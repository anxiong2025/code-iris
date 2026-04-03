//! Agent loop — the core engine that drives LLM API calls and tool execution.
//!
//! Architecture mirrors Claude Code's `QueryEngine.ts` (46 k lines condensed):
//!
//! ```text
//! chat(user_input)
//!   └── loop (max MAX_TURNS)
//!         ├── [1] context compression     (context.rs)
//!         ├── [2] callModel()             stream LLM response
//!         ├── [3] permission checks       serial (interactive-safe)
//!         ├── [4] parallel tool execution tokio::JoinSet
//!         └── [5] needsFollowUp?          → continue / exit
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures::StreamExt;
use iris_llm::{
    AnthropicProvider, BedrockProvider, ContentBlock, GoogleProvider, Message, ModelConfig,
    OpenAiCompatProvider, Role, StreamEvent, TokenUsage,
};
use tokio::task::JoinSet;

use crate::context::{compress, ContextConfig};
use crate::hooks::{HookDecision, HookRunner};
use crate::instructions;
use crate::permissions::{format_preview, PermissionMode, PermissionRules};
use crate::storage::{new_session, Session, Storage};
use crate::tools::{CwdRef, ToolRegistry};

/// Maximum tool-call rounds per user turn (prevents infinite loops).
const MAX_TURNS: usize = 20;

/// Result returned from [`Agent::chat`] / [`Agent::chat_streaming`].
#[derive(Debug)]
pub struct AgentResponse {
    /// Accumulated assistant text across all turns.
    pub text: String,
    /// Names of every tool called during this exchange.
    pub tool_calls: Vec<String>,
    /// Accumulated token usage.
    pub usage: TokenUsage,
}

/// Unified LLM backend — Anthropic native, OpenAI-compatible, Google Gemini, or AWS Bedrock.
pub enum LlmProvider {
    Anthropic(AnthropicProvider),
    OpenAiCompat(OpenAiCompatProvider),
    Google(GoogleProvider),
    Bedrock(BedrockProvider),
}


/// The agent — owns the provider, tool registry, permission policy, and session.
pub struct Agent {
    provider: LlmProvider,
    config: ModelConfig,
    tools: ToolRegistry,
    context_cfg: ContextConfig,
    pub permissions: PermissionMode,
    pub session: Session,
    storage: Storage,
    /// Shared working-directory reference — injected into all I/O tools.
    pub cwd: CwdRef,
    /// Set to true to interrupt the current streaming turn.
    pub cancel: Arc<AtomicBool>,
    /// Hooks loaded from `.iris/hooks.toml` / `~/.code-iris/hooks.toml`.
    pub hooks: HookRunner,
    /// Extra instructions prepended to the system prompt (from `.iris/instructions.md`).
    pub instructions: Option<String>,
    /// Per-tool permission rules loaded from `.iris/permissions.toml`.
    pub permission_rules: PermissionRules,
}

impl Agent {
    /// Create an agent using the Anthropic provider with sensible defaults.
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        let session = new_session();
        let cwd: CwdRef = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut agent = Self {
            provider: LlmProvider::Anthropic(AnthropicProvider::new(api_key)),
            config: ModelConfig::new("claude-sonnet-4-6-20250514"),
            tools: ToolRegistry::default_registry_for(Some(&session.id), cwd.clone()),
            context_cfg: ContextConfig::default(),
            permissions: PermissionMode::Default,
            session,
            storage: Storage::new()?,
            cwd,
            cancel: Arc::new(AtomicBool::new(false)),
            hooks: HookRunner::default(),
            instructions: None,
            permission_rules: PermissionRules::default(),
        };
        agent.reload_hooks_and_instructions();
        Ok(agent)
    }

    /// Create an agent using an OpenAI-compatible provider (Qwen, DeepSeek, etc.).
    pub fn new_openai_compat(provider: OpenAiCompatProvider) -> Result<Self> {
        let default_model = provider.default_model.clone();
        let session = new_session();
        let cwd: CwdRef = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut agent = Self {
            provider: LlmProvider::OpenAiCompat(provider),
            config: ModelConfig::new(default_model),
            tools: ToolRegistry::default_registry_for(Some(&session.id), cwd.clone()),
            context_cfg: ContextConfig::default(),
            permissions: PermissionMode::Default,
            session,
            storage: Storage::new()?,
            cwd,
            cancel: Arc::new(AtomicBool::new(false)),
            hooks: HookRunner::default(),
            instructions: None,
            permission_rules: PermissionRules::default(),
        };
        agent.reload_hooks_and_instructions();
        Ok(agent)
    }

    /// Auto-detect provider from environment variables and create an agent.
    ///
    /// Priority: OAuth credentials → ANTHROPIC_API_KEY → first set env key among all providers.
    /// Also loads MCP servers from ~/.code-iris/config.toml and registers their tools.
    pub async fn from_env() -> Result<Self> {
        use iris_llm::{detect_provider, AuthSource};

        // 1. OAuth / Anthropic API key
        let mut agent = if let Some(auth) = AuthSource::from_env() {
            let session = new_session();
            let cwd: CwdRef = std::sync::Arc::new(std::sync::Mutex::new(None));
            Self {
                provider: LlmProvider::Anthropic(AnthropicProvider::with_auth_pub(auth)),
                config: ModelConfig::new("claude-sonnet-4-6-20250514"),
                tools: ToolRegistry::default_registry_for(Some(&session.id), cwd.clone()),
                context_cfg: ContextConfig::default(),
                permissions: PermissionMode::Default,
                session,
                storage: Storage::new()?,
                cwd,
                cancel: Arc::new(AtomicBool::new(false)),
                hooks: HookRunner::default(),
                instructions: None,
            permission_rules: PermissionRules::default(),
            }
        } else if let Some(info) = detect_provider() {
            // 2. Google Gemini
            let key = std::env::var(info.env_key).unwrap_or_default();
            if info.name == "google" && !key.is_empty() {
                let session = new_session();
                let cwd: CwdRef = std::sync::Arc::new(std::sync::Mutex::new(None));
                Self {
                    provider: LlmProvider::Google(GoogleProvider::new(key)),
                    config: ModelConfig::new(info.default_model),
                    tools: ToolRegistry::default_registry_for(Some(&session.id), cwd.clone()),
                    context_cfg: ContextConfig::default(),
                    permissions: PermissionMode::Default,
                    session,
                    storage: Storage::new()?,
                    cwd,
                    cancel: Arc::new(AtomicBool::new(false)),
                    hooks: HookRunner::default(),
                    instructions: None,
            permission_rules: PermissionRules::default(),
                }
            // 3. Any other OpenAI-compat provider
            } else if info.name != "anthropic" && !key.is_empty() {
                let compat = OpenAiCompatProvider::new(
                    info.name, key, info.base_url, info.default_model,
                );
                Self::new_openai_compat(compat)?
            } else {
                Self::try_bedrock()?
            }
        } else {
            Self::try_bedrock()?
        };

        // 3. Load MCP servers from config and register their tools.
        agent.load_mcp_tools().await;
        Ok(agent)
    }

    /// Try AWS Bedrock as last-resort provider.
    ///
    /// Checks (in order): `AWS_BEARER_TOKEN_BEDROCK`, then standard IAM
    /// credentials (`AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY`).
    fn try_bedrock() -> Result<Self> {
        use iris_llm::PROVIDERS;

        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-west-2".to_string());
        let model = std::env::var("BEDROCK_MODEL")
            .or_else(|_| std::env::var("ANTHROPIC_DEFAULT_SONNET_MODEL"))
            .unwrap_or_else(|_| "us.anthropic.claude-sonnet-4-5-20251001-v1:0".to_string());

        if let Some(provider) = BedrockProvider::from_env(region.clone(), model.clone()) {
            tracing::debug!(%region, %model, "Using AWS Bedrock Converse API");
            let session = new_session();
            let cwd: CwdRef = std::sync::Arc::new(std::sync::Mutex::new(None));
            return Ok(Self {
                provider: LlmProvider::Bedrock(provider),
                config: ModelConfig::new(&model).with_max_tokens(8192),
                tools: ToolRegistry::default_registry_for(Some(&session.id), cwd.clone()),
                context_cfg: ContextConfig::default(),
                permissions: PermissionMode::Default,
                session,
                storage: Storage::new()?,
                cwd,
                cancel: Arc::new(AtomicBool::new(false)),
                hooks: HookRunner::default(),
                instructions: None,
            permission_rules: PermissionRules::default(),
            });
        }

        anyhow::bail!(
            "No API key found. Set one of: {}, AWS_ACCESS_KEY_ID+AWS_SECRET_ACCESS_KEY\nor run `iris configure`.",
            PROVIDERS.iter().map(|p| p.env_key).collect::<Vec<_>>().join(", ")
        )
    }

    /// Load MCP server configs and discover + register individual tools.
    ///
    /// Loads from both global `~/.code-iris/config.toml` and project-level
    /// `.iris/mcp.toml`. Project-level configs override global ones with
    /// the same server name.
    ///
    /// Each MCP tool is registered independently with its own schema,
    /// as `mcp__<server>__<tool_name>`.
    async fn load_mcp_tools(&mut self) {
        use crate::config::load_config;
        use iris_llm::{McpClient, McpServerConfig};
        use std::collections::HashMap;
        use std::sync::Arc;

        let mut servers: HashMap<String, McpServerConfig> = HashMap::new();

        // Global config.
        if let Ok(config) = load_config() {
            for s in config.mcp_servers {
                servers.insert(s.name.clone(), s);
            }
        }

        // Project-level .iris/mcp.toml (overrides global).
        let project_root = {
            let guard = self.cwd.lock().unwrap();
            guard.clone().or_else(|| std::env::current_dir().ok())
        };
        if let Some(ref root) = project_root {
            let mcp_path = root.join(".iris").join("mcp.toml");
            if mcp_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&mcp_path) {
                    #[derive(serde::Deserialize)]
                    struct McpConfig { #[serde(default)] servers: Vec<McpServerConfig> }
                    if let Ok(cfg) = toml::from_str::<McpConfig>(&content) {
                        for s in cfg.servers {
                            servers.insert(s.name.clone(), s);
                        }
                    }
                }
            }
        }

        // Discover tools from each server.
        for (_, server_cfg) in &servers {
            let transport = match server_cfg.to_transport() {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(server = %server_cfg.name, "invalid MCP transport: {e}");
                    continue;
                }
            };
            let client = Arc::new(McpClient::new(transport));
            match crate::tools::mcp_tool::discover_mcp_tools(&server_cfg.name, client).await {
                Ok(tools) => {
                    let count = tools.len();
                    for tool in tools {
                        tracing::debug!(tool = %tool.registry_name, "registered MCP tool");
                        self.tools.register(Arc::new(tool));
                    }
                    tracing::info!(server = %server_cfg.name, count, "MCP tools discovered");
                }
                Err(e) => {
                    tracing::warn!(server = %server_cfg.name, "MCP tool discovery failed: {e}");
                }
            }
        }
    }

    // ── Builder methods ───────────────────────────────────────────────────────

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.config.model = self.normalize_model(model.into());
        self
    }

    pub fn set_model(&mut self, model: impl Into<String>) {
        self.config.model = self.normalize_model(model.into());
    }

    /// Switch model **and** auto-switch provider if the model belongs to a
    /// different provider than the currently active one.
    ///
    /// Returns `(actual_model, switched_provider_name, error_message)`.
    pub fn switch_model(&mut self, model: impl Into<String>) -> (String, Option<&'static str>, Option<String>) {
        let model: String = model.into();
        let targets = Self::infer_provider_candidates(&model);
        let current = self.provider_name();

        // Same provider family — just update model name.
        if targets.contains(&current) {
            self.config.model = self.normalize_model(model);
            return (self.config.model.clone(), None, None);
        }

        // Try each candidate provider in order.
        for &target in &targets {
            if let Some((new_provider, actual_model)) = self.try_build_provider(target, &model) {
                self.provider = new_provider;
                // Re-normalize now that provider may have changed (e.g. Bedrock mapping).
                self.config.model = self.normalize_model(actual_model);
                return (self.config.model.clone(), Some(target), None);
            }
        }

        // None of the candidates worked — stay on current provider, do NOT change model.
        let tried = targets.iter().map(|t| *t).collect::<Vec<_>>().join(", ");
        let err = format!(
            "Cannot switch to {model}: no credentials for [{tried}]. Model unchanged."
        );
        tracing::warn!("{err}");
        (self.config.model.clone(), None, Some(err))
    }

    /// Short name of the currently active provider.
    pub fn provider_name(&self) -> &'static str {
        match &self.provider {
            LlmProvider::Anthropic(_) => "anthropic",
            LlmProvider::OpenAiCompat(p) => {
                // Leak a &'static str from the heap name — fine, it's a small
                // set of provider names that live for the process lifetime.
                // But we can just match common names.
                match p.name.as_str() {
                    "deepseek" => "deepseek",
                    "qwen" => "qwen",
                    "openai" => "openai",
                    "groq" => "groq",
                    "openrouter" => "openrouter",
                    "moonshot" => "moonshot",
                    "zhipu" => "zhipu",
                    "baichuan" => "baichuan",
                    "minimax" => "minimax",
                    "yi" => "yi",
                    "siliconflow" => "siliconflow",
                    "stepfun" => "stepfun",
                    "spark" => "spark",
                    _ => "openai_compat",
                }
            }
            LlmProvider::Google(_) => "google",
            LlmProvider::Bedrock(_) => "bedrock",
        }
    }

    /// Return candidate providers for a model name, in priority order.
    ///
    /// Claude models try `anthropic` first, then fall back to `bedrock`.
    fn infer_provider_candidates(model: &str) -> Vec<&'static str> {
        let m = model.to_lowercase();
        if m.contains("claude") || m.contains("anthropic") {
            if m.contains('.') || m.contains(':') {
                return vec!["bedrock"];
            }
            // Short Claude name: try Anthropic API first, then Bedrock.
            return vec!["anthropic", "bedrock"];
        }
        if m.contains("gpt") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") {
            return vec!["openai"];
        }
        if m.contains("gemini") { return vec!["google"]; }
        if m.contains("qwen") { return vec!["qwen"]; }
        if m.contains("deepseek") { return vec!["deepseek"]; }
        if m.contains("llama") || m.contains("mixtral") { return vec!["groq"]; }
        if m.contains("moonshot") { return vec!["moonshot"]; }
        if m.contains("glm") { return vec!["zhipu"]; }
        if m.contains("baichuan") { return vec!["baichuan"]; }
        if m.contains("minimax") { return vec!["minimax"]; }
        if m.starts_with("yi-") { return vec!["yi"]; }
        if m.starts_with("step-") { return vec!["stepfun"]; }
        if m.contains("general") || m.contains("spark") { return vec!["spark"]; }
        vec![]
    }

    /// Try to build a new LlmProvider for the given target provider + model.
    /// Returns None if the required API key is missing.
    fn try_build_provider(&self, target: &str, model: &str) -> Option<(LlmProvider, String)> {
        use iris_llm::{get_provider, AnthropicProvider, AuthSource};

        match target {
            "anthropic" => {
                let auth = AuthSource::from_env()?;
                let p = AnthropicProvider::with_auth_pub(auth);
                Some((LlmProvider::Anthropic(p), model.to_string()))
            }
            "bedrock" => {
                let region = std::env::var("AWS_REGION")
                    .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
                    .unwrap_or_else(|_| "us-west-2".to_string());
                let bp = BedrockProvider::from_env(region, model.to_string())?;
                Some((LlmProvider::Bedrock(bp), model.to_string()))
            }
            "google" => {
                let key = std::env::var("GOOGLE_API_KEY").ok().filter(|k| !k.is_empty())?;
                Some((LlmProvider::Google(iris_llm::GoogleProvider::new(key)), model.to_string()))
            }
            _ => {
                // OpenAI-compat providers
                let info = get_provider(target)?;
                let key = std::env::var(info.env_key).ok().filter(|k| !k.is_empty())?;
                let p = OpenAiCompatProvider::from_info(info, key);
                Some((LlmProvider::OpenAiCompat(p), model.to_string()))
            }
        }
    }

    /// When the active provider is Bedrock, map short Claude model names
    /// to their full Bedrock model IDs.
    ///
    /// Priority:
    /// 1. `ANTHROPIC_DEFAULT_OPUS_MODEL` / `SONNET` / `HAIKU` env vars
    /// 2. `BEDROCK_MODEL` env var (generic override)
    /// 3. Hardcoded fallback `us.anthropic.<name>-v1:0`
    fn normalize_model(&self, model: String) -> String {
        if !matches!(self.provider, LlmProvider::Bedrock(_)) {
            return model;
        }
        // Already a Bedrock-qualified ID (contains a dot) — leave it alone.
        if model.contains('.') {
            return model;
        }
        let m = model.to_lowercase();
        // Check env vars first — user knows their exact Bedrock model IDs.
        let env_mapped = if m.contains("opus") {
            std::env::var("ANTHROPIC_DEFAULT_OPUS_MODEL").ok()
        } else if m.contains("sonnet") {
            std::env::var("ANTHROPIC_DEFAULT_SONNET_MODEL").ok()
        } else if m.contains("haiku") {
            std::env::var("ANTHROPIC_DEFAULT_HAIKU_MODEL").ok()
        } else {
            None
        };
        if let Some(mapped) = env_mapped.filter(|s| !s.is_empty()) {
            tracing::info!(short = %model, bedrock = %mapped, "mapped model via env var");
            return mapped;
        }
        // Fallback: wrap as us.anthropic.<name>-v1:0
        let fallback = format!("us.anthropic.{model}-v1:0");
        tracing::info!(short = %model, bedrock = %fallback, "auto-mapped model name for Bedrock (fallback)");
        fallback
    }

    pub fn current_model(&self) -> &str {
        &self.config.model
    }

    pub fn with_permissions(mut self, mode: PermissionMode) -> Self {
        self.permissions = mode;
        self
    }

    pub fn with_context_config(mut self, cfg: ContextConfig) -> Self {
        self.context_cfg = cfg;
        self
    }

    pub fn with_session(mut self, session: Session) -> Self {
        self.session = session;
        self
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.config.system_prompt = Some(prompt.into());
        self
    }

    /// Replace the tool registry (used by Coordinator to inject extra tools).
    pub fn set_tools(&mut self, tools: ToolRegistry) {
        self.tools = tools;
    }

    /// (Re)load hooks and instructions from the current working directory.
    ///
    /// Called automatically on construction. Call again after `/cd` if the
    /// project root changes.
    pub fn reload_hooks_and_instructions(&mut self) {
        let root = {
            let guard = self.cwd.lock().unwrap();
            guard.clone().or_else(|| std::env::current_dir().ok())
        };
        self.hooks = HookRunner::load(root.as_deref());
        let cwd_dir = { self.cwd.lock().unwrap().clone() };
        self.instructions = instructions::load_with_cwd(
            root.as_deref(),
            cwd_dir.as_deref(),
        );
        self.permission_rules = PermissionRules::load(root.as_deref());
        if !self.hooks.is_empty() {
            tracing::debug!("hooks loaded from {:?}", root);
        }
        if self.instructions.is_some() {
            tracing::debug!("instructions loaded from {:?}", root);
        }
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Send a user message, execute any tool calls, and return the full response.
    pub async fn chat(&mut self, user_input: &str) -> Result<AgentResponse> {
        self.chat_streaming(user_input, |_| {}, |_| {}, |_, _, _| {}, |_| {}).await
    }

    /// Like [`chat`], but calls `on_text` with each streamed text delta.
    ///
    /// Callbacks:
    /// - `on_text(chunk)` — streamed text delta
    /// - `on_tool(name)` — tool call started
    /// - `on_tool_result(name, result, is_error)` — tool finished
    /// - `on_thinking(chunk)` — extended thinking delta (Claude only)
    pub async fn chat_streaming(
        &mut self,
        user_input: &str,
        mut on_text: impl FnMut(&str),
        mut on_tool: impl FnMut(&str),
        mut on_tool_result: impl FnMut(&str, &str, bool),
        mut on_thinking: impl FnMut(&str),
    ) -> Result<AgentResponse> {
        self.cancel.store(false, Ordering::Relaxed);
        self.session.messages.push(Message::user(user_input));

        let mut response_text = String::new();
        let mut tool_calls: Vec<String> = Vec::new();
        let mut usage = TokenUsage::default();

        // Inject layered instructions into the system prompt for this exchange.
        let cwd_str = {
            let guard = self.cwd.lock().unwrap();
            guard.clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
                .display()
                .to_string()
        };
        let date_line = format!(
            "Today's date is {}.",
            chrono::Local::now().format("%Y-%m-%d")
        );
        let default_system = format!(
            "You are an AI coding agent. {date_line}\n\
             Working directory: {cwd_str}\n\n\
             You have tools: bash, file_read, file_write, file_edit, grep, glob, \
             lsp, web_fetch, web_search, task_create/update/list/get, agent_tool.\n\n\
             Rules:\n\
             - You MUST use tools to answer questions about the project. Never guess or make up information.\n\
             - After using tools, summarize findings concisely. Do not reproduce raw file contents.\n\
             - Do not output code blocks containing tool invocations or file contents.\n\
             - Be concise. Lead with the answer. Skip preamble."
        );
        let effective_config = {
            let base = match self.config.system_prompt.as_deref() {
                Some(existing) => format!("{default_system}\n\n{existing}"),
                None => default_system,
            };
            let merged = if let Some(ref instr) = self.instructions {
                format!("{instr}\n\n---\n\n{base}")
            } else {
                base
            };
            self.config.clone().with_system(merged)
        };

        for turn in 0..MAX_TURNS {
            // ── [1] Context compression ──────────────────────────────────────

            // Always evict old tool results — cheap and saves the most tokens.
            // This runs every turn regardless of token count.
            crate::context::evict_old_tool_results(
                &mut self.session.messages,
                self.context_cfg.keep_recent_turns,
            );

            // L4 autocompact at 60 % of the context window — proactive, before
            // the window fills, keeps total cost down.
            let token_count = crate::context::count_tokens(&self.session.messages);
            if token_count >= self.context_cfg.autocompact_at() {
                match self.autocompact_with_provider().await {
                    Ok(true) => {
                        tracing::info!(turn, "autocompact: conversation summarised");
                        on_text("\n[Context compacted — conversation summarised to save space]\n");
                    }
                    Ok(false) => {}
                    Err(e) => tracing::warn!(turn, "autocompact failed: {e}"),
                }
            } else if compress(&mut self.session.messages, &self.context_cfg) {
                // L1–L3 local compression at 90 %.
                tracing::debug!(turn, "context compressed (levels 1-3)");
            }

            // ── [2] Stream LLM response ───────────────────────────────────────
            let mut assistant_text = String::new();
            // (tool_use_id, name, input)
            let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();
            // Track tool names that already had ToolUseStart events.
            let mut started_tools: Vec<String> = Vec::new();

            {
                let messages = self.session.messages.clone();
                let defs = self.tools.all_definitions();
                let config = effective_config.clone();

                let mut stream: std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<StreamEvent>> + Send>> =
                    match &mut self.provider {
                        LlmProvider::Anthropic(p) => Box::pin(
                            p.chat_stream(&messages, &defs, &config)
                                .await
                                .map_err(|e| anyhow::anyhow!("LLM stream failed: {e:#}"))?,
                        ),
                        LlmProvider::OpenAiCompat(p) => Box::pin(
                            p.chat_stream(&messages, &defs, &config)
                                .await
                                .map_err(|e| anyhow::anyhow!("LLM stream failed: {e:#}"))?,
                        ),
                        LlmProvider::Google(p) => Box::pin(
                            p.chat_stream(&messages, &defs, &config)
                                .await
                                .map_err(|e| anyhow::anyhow!("LLM stream failed: {e:#}"))?,
                        ),
                        LlmProvider::Bedrock(p) => Box::pin(
                            p.chat_stream(&messages, &defs, &config)
                                .await
                                .map_err(|e| anyhow::anyhow!("LLM stream failed: {e:#}"))?,
                        ),
                    };

                // Read stream events with a per-event timeout.
                // Some providers keep the SSE connection open indefinitely after
                // sending all data — the timeout breaks us out of that hang.
                loop {
                    if self.cancel.load(Ordering::Relaxed) {
                        self.cancel.store(false, Ordering::Relaxed);
                        break;
                    }
                    let next = tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        stream.next(),
                    ).await;
                    match next {
                        Ok(Some(event)) => match event? {
                            StreamEvent::TextDelta { text } => {
                                on_text(&text);
                                assistant_text.push_str(&text);
                            }
                            StreamEvent::ThinkingDelta { thinking } => {
                                on_thinking(&thinking);
                            }
                            StreamEvent::ToolUseStart { name } => {
                                // Immediately notify TUI that a tool call is starting.
                                on_tool(&name);
                                started_tools.push(name);
                            }
                            StreamEvent::ToolUse { id, name, input } => {
                                // Fallback: if no ToolUseStart was sent (Bedrock/Gemini),
                                // fire on_tool now so the TUI shows the tool name.
                                if !started_tools.iter().any(|s| s == &name) {
                                    on_tool(&name);
                                }
                                tool_uses.push((id, name, input));
                            }
                            StreamEvent::Usage(u) => usage.accumulate(&u),
                            StreamEvent::MessageStop => break,
                        },
                        Ok(None) => break, // stream ended
                        Err(_) => {
                            // 30s timeout — likely provider didn't close connection.
                            tracing::warn!(turn, "stream read timed out after 30s — breaking");
                            break;
                        }
                    }
                }
            }

            // ── [3] Append assistant message ─────────────────────────────────
            let mut content: Vec<ContentBlock> = Vec::new();
            if !assistant_text.is_empty() {
                response_text.push_str(&assistant_text);
                content.push(ContentBlock::Text { text: assistant_text });
            }
            for (id, name, input) in &tool_uses {
                content.push(ContentBlock::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
            }
            if !content.is_empty() {
                self.session.messages.push(Message { role: Role::Assistant, content });
            }

            // ── [4] No tool calls → done ──────────────────────────────────────
            if tool_uses.is_empty() {
                self.touch_and_save()?;
                if !response_text.is_empty() {
                    self.hooks.run_notification(&response_text);
                }
                return Ok(AgentResponse { text: response_text, tool_calls, usage });
            }

            // ── [5] Permission checks + PreToolUse hooks ──────────────────────
            //
            // Both run serially so the user can be prompted and hooks can block
            // before any parallel execution begins.
            let mut approved: Vec<(String, String, serde_json::Value)> = Vec::new();
            // Tracks ids whose tool_result message has already been pushed.
            let mut skip_ids: Vec<String> = Vec::new();

            for (id, name, input) in &tool_uses {
                tool_calls.push(name.clone());
                // Permission gate — per-tool rules take priority.
                let rule_result = self.permissions.is_allowed_with_rules(
                    name, input, &self.permission_rules,
                );
                let allowed = match rule_result {
                    Some(true) => true,
                    Some(false) => false, // explicit deny
                    None => {
                        // Needs interactive confirm.
                        let preview = format_preview(name, input);
                        self.permissions.request(name, &preview)
                    }
                };
                if !allowed {
                    self.session.messages.push(Message::tool_result(
                        id,
                        format!("Permission denied for tool `{name}`"),
                        true,
                    ));
                    skip_ids.push(id.clone());
                    continue;
                }
                // PreToolUse hook gate
                match self.hooks.run_pre_tool(name, input).await {
                    HookDecision::Allow => {
                        approved.push((id.clone(), name.clone(), input.clone()));
                    }
                    HookDecision::Block(msg) => {
                        tracing::info!(tool = %name, "blocked by PreToolUse hook");
                        self.session.messages.push(Message::tool_result(id, msg, true));
                        skip_ids.push(id.clone());
                    }
                }
            }

            // ── [6] Parallel tool execution ───────────────────────────────────
            //
            // Each approved tool runs concurrently via tokio::task::JoinSet.
            // Results are collected into a HashMap keyed by tool_use_id and then
            // appended to session.messages in the original LLM-response order
            // (required by the Anthropic API).

            let hooks = self.hooks.clone();
            let mut join_set: JoinSet<(String, Result<String>)> = JoinSet::new();

            for (id, name, input) in approved {
                let tool = self.tools.get(&name);
                let hooks = hooks.clone();
                join_set.spawn(async move {
                    let result = match tool {
                        Some(t) => {
                            tracing::debug!(tool = %name, "executing");
                            t.execute(input.clone()).await
                        }
                        None => Err(anyhow::anyhow!("Unknown tool: `{name}`")),
                    };
                    // PostToolUse hook — fire-and-forget
                    if let Ok(ref output) = result {
                        hooks.run_post_tool(&name, &input, output);
                    }
                    (id, result)
                });
            }

            // Collect into map (order non-deterministic from JoinSet).
            let mut results: HashMap<String, Result<String>> = HashMap::new();
            while let Some(join_result) = join_set.join_next().await {
                match join_result {
                    Ok((id, tool_result)) => {
                        results.insert(id, tool_result);
                    }
                    Err(join_err) => {
                        tracing::error!("tool task panicked: {join_err}");
                    }
                }
            }

            // Append tool_result messages in original tool_use order.
            for (id, name, _) in &tool_uses {
                if skip_ids.contains(id) {
                    continue; // already appended above (denied or hook-blocked)
                }
                let msg = match results.remove(id) {
                    Some(Ok(output)) => {
                        tracing::debug!(tool = %name, "succeeded");
                        on_tool_result(name, &output, false);
                        Message::tool_result(id, output, false)
                    }
                    Some(Err(err)) => {
                        tracing::warn!(tool = %name, error = %err, "failed");
                        on_tool_result(name, &err.to_string(), true);
                        Message::tool_result(id, err.to_string(), true)
                    }
                    None => {
                        on_tool_result(name, &format!("Tool `{name}` produced no result"), true);
                        Message::tool_result(
                            id,
                            format!("Tool `{name}` produced no result"),
                            true,
                        )
                    }
                };
                self.session.messages.push(msg);
            }

            tracing::debug!(turn, approved = tool_uses.len() - skip_ids.len(), "continuing");
        }

        tracing::warn!("reached MAX_TURNS ({MAX_TURNS})");
        self.touch_and_save()?;
        Ok(AgentResponse { text: response_text, tool_calls, usage })
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Level-4 autocompact using whichever provider is active.
    ///
    /// Uses the cheapest/fastest available model per provider.
    async fn autocompact_with_provider(&mut self) -> Result<bool> {
        use crate::context::count_tokens;
        use futures::StreamExt;
        use iris_llm::{ModelConfig, StreamEvent};

        if count_tokens(&self.session.messages) <= self.context_cfg.max_tokens {
            return Ok(false);
        }

        let mut transcript = String::new();
        for msg in &self.session.messages {
            let role = match msg.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::Tool => "Tool",
            };
            for block in &msg.content {
                match block {
                    ContentBlock::Text { text } => {
                        transcript.push_str(&format!("{role}: {text}\n\n"));
                    }
                    ContentBlock::ToolUse { name, .. } => {
                        transcript.push_str(&format!("{role}: [called tool: {name}]\n\n"));
                    }
                    ContentBlock::ToolResult { content, .. } => {
                        let preview: String = content.chars().take(200).collect();
                        transcript.push_str(&format!("Tool result: {preview}…\n\n"));
                    }
                    _ => {}
                }
            }
        }

        let prompt = format!(
            "Summarise this conversation concisely, preserving all important context, \
             decisions, file paths, code changes, and open questions.\n\n{transcript}"
        );
        let summary_messages = vec![Message::user(&prompt)];

        // Pick a fast/cheap model per provider.
        let compact_model = match &self.provider {
            LlmProvider::Anthropic(_) => "claude-haiku-4-5-20251001",
            LlmProvider::OpenAiCompat(p) => p.default_model.as_str(),
            LlmProvider::Google(_) => "gemini-2.0-flash",
            LlmProvider::Bedrock(p) => p.model.as_str(),
        };
        let summary_config = ModelConfig::new(compact_model).with_max_tokens(2048);

        let mut stream: std::pin::Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send>> =
            match &mut self.provider {
                LlmProvider::Anthropic(p) => Box::pin(
                    p.chat_stream(&summary_messages, &[], &summary_config).await?,
                ),
                LlmProvider::OpenAiCompat(p) => Box::pin(
                    p.chat_stream(&summary_messages, &[], &summary_config).await?,
                ),
                LlmProvider::Google(p) => Box::pin(
                    p.chat_stream(&summary_messages, &[], &summary_config).await?,
                ),
                LlmProvider::Bedrock(p) => Box::pin(
                    p.chat_stream(&summary_messages, &[], &summary_config).await?,
                ),
            };

        let mut summary = String::new();
        while let Some(event) = stream.next().await {
            if let Ok(StreamEvent::TextDelta { text }) = event {
                summary.push_str(&text);
            }
        }

        if summary.trim().is_empty() {
            return Ok(false);
        }

        let keep_n = self.context_cfg.keep_recent_turns * 2;
        let recent: Vec<Message> = self.session.messages
            .iter().rev().take(keep_n).cloned()
            .collect::<Vec<_>>().into_iter().rev().collect();

        self.session.messages.clear();
        self.session.messages.push(Message::assistant(format!(
            "[Conversation summary]\n\n{summary}"
        )));
        self.session.messages.extend(recent);

        tracing::info!("autocompact: compressed to {} messages", self.session.messages.len());
        Ok(true)
    }

    fn touch_and_save(&mut self) -> Result<()> {
        self.session.updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.storage.save(&self.session)
    }
}
