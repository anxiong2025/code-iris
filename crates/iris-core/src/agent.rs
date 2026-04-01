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
    AnthropicProvider, ContentBlock, GoogleProvider, Message, ModelConfig, OpenAiCompatProvider,
    Role, StreamEvent, TokenUsage,
};
use tokio::task::JoinSet;

use crate::context::{compress, ContextConfig};
use crate::hooks::{HookDecision, HookRunner};
use crate::instructions;
use crate::permissions::{format_preview, PermissionMode};
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

/// Unified LLM backend — Anthropic native, OpenAI-compatible, or Google Gemini.
pub enum LlmProvider {
    Anthropic(AnthropicProvider),
    OpenAiCompat(OpenAiCompatProvider),
    Google(GoogleProvider),
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
        };
        agent.reload_hooks_and_instructions();
        Ok(agent)
    }

    /// Auto-detect provider from environment variables and create an agent.
    ///
    /// Priority: OAuth credentials → ANTHROPIC_API_KEY → first set env key among all providers.
    /// Also loads MCP servers from ~/.code-iris/config.toml and registers their tools.
    pub fn from_env() -> Result<Self> {
        use iris_llm::{detect_provider, AuthSource, PROVIDERS};

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
                }
            // 3. Any other OpenAI-compat provider
            } else if info.name != "anthropic" && !key.is_empty() {
                let compat = OpenAiCompatProvider::new(
                    info.name, key, info.base_url, info.default_model,
                );
                Self::new_openai_compat(compat)?
            } else {
                anyhow::bail!(
                    "No API key found. Set one of: {}\nor run `iris configure`.",
                    PROVIDERS.iter().map(|p| p.env_key).collect::<Vec<_>>().join(", ")
                )
            }
        } else {
            anyhow::bail!(
                "No API key found. Set one of: {}\nor run `iris configure`.",
                PROVIDERS.iter().map(|p| p.env_key).collect::<Vec<_>>().join(", ")
            )
        };

        // 3. Load MCP servers from config and register their tools.
        agent.load_mcp_tools();
        Ok(agent)
    }

    /// Load MCP server configs from ~/.code-iris/config.toml and register tools.
    ///
    /// Errors are logged as warnings — a missing/broken MCP server should not
    /// prevent the agent from starting.
    fn load_mcp_tools(&mut self) {
        use crate::config::load_config;
        use crate::tools::mcp_tool::McpToolWrapper;
        use iris_llm::McpClient;
        use std::sync::Arc;

        let config = match load_config() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("failed to load config: {e}");
                return;
            }
        };

        for server_cfg in &config.mcp_servers {
            let transport = match server_cfg.to_transport() {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(server = %server_cfg.name, "invalid MCP transport: {e}");
                    continue;
                }
            };
            let client = Arc::new(McpClient::new(transport));
            // We can't await here (sync context), so we register a lazy wrapper
            // that will fetch tool definitions on first use.
            let wrapper = McpToolWrapper::new(server_cfg.name.clone(), client);
            self.tools.register(Arc::new(wrapper));
            tracing::info!(server = %server_cfg.name, "registered MCP server");
        }
    }

    // ── Builder methods ───────────────────────────────────────────────────────

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.config.model = model.into();
        self
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
        self.instructions = instructions::load(root.as_deref());
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
        self.chat_streaming(user_input, |_| {}).await
    }

    /// Like [`chat`], but calls `on_text` with each streamed text delta.
    ///
    /// `on_text` is a sync callback — safe to call from async context using
    /// `tokio::sync::mpsc::UnboundedSender::send` or simple stdout writes.
    pub async fn chat_streaming(
        &mut self,
        user_input: &str,
        mut on_text: impl FnMut(&str),
    ) -> Result<AgentResponse> {
        self.cancel.store(false, Ordering::Relaxed);
        self.session.messages.push(Message::user(user_input));

        let mut response_text = String::new();
        let mut tool_calls: Vec<String> = Vec::new();
        let mut usage = TokenUsage::default();

        // Inject layered instructions into the system prompt for this exchange.
        let effective_config = if let Some(ref instr) = self.instructions {
            let merged = match self.config.system_prompt.as_deref() {
                Some(existing) => format!("{instr}\n\n---\n\n{existing}"),
                None => instr.clone(),
            };
            self.config.clone().with_system(merged)
        } else {
            self.config.clone()
        };

        for turn in 0..MAX_TURNS {
            // ── [1] Context compression ──────────────────────────────────────
            // L4 autocompact at 80 % of the context window — proactive, before
            // the window fills, matching Claude Code's behaviour.
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

            {
                let messages = self.session.messages.clone();
                let defs = self.tools.all_definitions();
                let config = effective_config.clone();

                let mut stream: std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<StreamEvent>> + Send>> =
                    match &mut self.provider {
                        LlmProvider::Anthropic(p) => Box::pin(
                            p.chat_stream(&messages, &defs, &config)
                                .await
                                .context("LLM stream failed")?,
                        ),
                        LlmProvider::OpenAiCompat(p) => Box::pin(
                            p.chat_stream(&messages, &defs, &config)
                                .await
                                .context("LLM stream failed")?,
                        ),
                        LlmProvider::Google(p) => Box::pin(
                            p.chat_stream(&messages, &defs, &config)
                                .await
                                .context("LLM stream failed")?,
                        ),
                    };

                while let Some(event) = stream.next().await {
                    if self.cancel.load(Ordering::Relaxed) {
                        self.cancel.store(false, Ordering::Relaxed);
                        break;
                    }
                    match event? {
                        StreamEvent::TextDelta { text } => {
                            on_text(&text);
                            assistant_text.push_str(&text);
                        }
                        StreamEvent::ThinkingDelta { .. } => {}
                        StreamEvent::ToolUse { id, name, input } => {
                            tool_uses.push((id, name, input));
                        }
                        StreamEvent::Usage(u) => usage.accumulate(&u),
                        StreamEvent::MessageStop => break,
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
                // Permission gate
                let preview = format_preview(name, input);
                if !self.permissions.request(name, &preview) {
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
                        Message::tool_result(id, output, false)
                    }
                    Some(Err(err)) => {
                        tracing::warn!(tool = %name, error = %err, "failed");
                        Message::tool_result(id, err.to_string(), true)
                    }
                    None => Message::tool_result(
                        id,
                        format!("Tool `{name}` produced no result"),
                        true,
                    ),
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
