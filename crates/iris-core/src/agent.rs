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
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures::StreamExt;
use iris_llm::{
    AnthropicProvider, ContentBlock, Message, ModelConfig, OpenAiCompatProvider, Role,
    StreamEvent, TokenUsage,
};
use tokio::task::JoinSet;

use crate::context::{autocompact, compress, ContextConfig};
use crate::permissions::{format_preview, PermissionMode};
use crate::storage::{new_session, Session, Storage};
use crate::tools::ToolRegistry;

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

/// Unified LLM backend — either Anthropic native or OpenAI-compatible.
pub enum LlmProvider {
    Anthropic(AnthropicProvider),
    OpenAiCompat(OpenAiCompatProvider),
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
}

impl Agent {
    /// Create an agent using the Anthropic provider with sensible defaults.
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        Ok(Self {
            provider: LlmProvider::Anthropic(AnthropicProvider::new(api_key)),
            config: ModelConfig::new("claude-sonnet-4-6-20250514"),
            tools: ToolRegistry::default_registry(),
            context_cfg: ContextConfig::default(),
            permissions: PermissionMode::Default,
            session: new_session(),
            storage: Storage::new()?,
        })
    }

    /// Create an agent using an OpenAI-compatible provider (Qwen, DeepSeek, etc.).
    pub fn new_openai_compat(provider: OpenAiCompatProvider) -> Result<Self> {
        let default_model = provider.default_model.clone();
        Ok(Self {
            provider: LlmProvider::OpenAiCompat(provider),
            config: ModelConfig::new(default_model),
            tools: ToolRegistry::default_registry(),
            context_cfg: ContextConfig::default(),
            permissions: PermissionMode::Default,
            session: new_session(),
            storage: Storage::new()?,
        })
    }

    /// Auto-detect provider from environment variables and create an agent.
    ///
    /// Priority: OAuth credentials → ANTHROPIC_API_KEY → first set env key among all providers.
    pub fn from_env() -> Result<Self> {
        use iris_llm::{detect_provider, AuthSource, PROVIDERS};

        // 1. OAuth / Anthropic API key
        if let Some(auth) = AuthSource::from_env() {
            return Ok(Self {
                provider: LlmProvider::Anthropic(AnthropicProvider::with_auth_pub(auth)),
                config: ModelConfig::new("claude-sonnet-4-6-20250514"),
                tools: ToolRegistry::default_registry(),
                context_cfg: ContextConfig::default(),
                permissions: PermissionMode::Default,
                session: new_session(),
                storage: Storage::new()?,
            });
        }

        // 2. Any other configured provider
        if let Some(info) = detect_provider() {
            let key = std::env::var(info.env_key).unwrap_or_default();
            // Skip "anthropic" here (already handled above)
            if info.name != "anthropic" && !key.is_empty() {
                let compat = OpenAiCompatProvider::new(
                    info.name,
                    key,
                    info.base_url,
                    info.default_model,
                );
                return Self::new_openai_compat(compat);
            }
        }

        anyhow::bail!(
            "No API key found. Set one of: {}\nor run `iris configure`.",
            PROVIDERS.iter().map(|p| p.env_key).collect::<Vec<_>>().join(", ")
        )
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
        self.session.messages.push(Message::user(user_input));

        let mut response_text = String::new();
        let mut tool_calls: Vec<String> = Vec::new();
        let mut usage = TokenUsage::default();

        for turn in 0..MAX_TURNS {
            // ── [1] Context compression ──────────────────────────────────────
            if compress(&mut self.session.messages, &self.context_cfg) {
                tracing::debug!(turn, "context compressed (levels 1-3)");
            }
            // Level 4: autocompact via LLM when levels 1–3 are still over budget.
            if crate::context::count_tokens(&self.session.messages) > self.context_cfg.max_tokens {
                if let LlmProvider::Anthropic(ref mut p) = self.provider {
                    match autocompact(&mut self.session.messages, p, &self.context_cfg).await {
                        Ok(true) => tracing::info!(turn, "autocompact: conversation summarised"),
                        Ok(false) => {}
                        Err(e) => tracing::warn!(turn, "autocompact failed: {e}"),
                    }
                }
            }

            // ── [2] Stream LLM response ───────────────────────────────────────
            let mut assistant_text = String::new();
            // (tool_use_id, name, input)
            let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();

            {
                let messages = self.session.messages.clone();
                let defs = self.tools.all_definitions();
                let config = self.config.clone();

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
                    };

                while let Some(event) = stream.next().await {
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
                return Ok(AgentResponse { text: response_text, tool_calls, usage });
            }

            // ── [5] Permission checks (serial — may prompt the user) ──────────
            let mut approved: Vec<(String, String, serde_json::Value)> = Vec::new();
            let mut denied_ids: Vec<String> = Vec::new();

            for (id, name, input) in &tool_uses {
                tool_calls.push(name.clone());
                let preview = format_preview(name, input);
                if self.permissions.request(name, &preview) {
                    approved.push((id.clone(), name.clone(), input.clone()));
                } else {
                    denied_ids.push(id.clone());
                }
            }

            // Append denied results immediately.
            for (id, name, _) in tool_uses.iter().filter(|(id, _, _)| denied_ids.contains(id)) {
                self.session.messages.push(Message::tool_result(
                    id,
                    format!("Permission denied for tool `{name}`"),
                    true,
                ));
            }

            // ── [6] Parallel tool execution ───────────────────────────────────
            //
            // Each approved tool runs concurrently via tokio::task::JoinSet.
            // Results are collected into a HashMap keyed by tool_use_id and then
            // appended to session.messages in the original LLM-response order
            // (required by the Anthropic API).

            let mut join_set: JoinSet<(String, Result<String>)> = JoinSet::new();

            for (id, name, input) in approved {
                let tool = self.tools.get(&name);
                join_set.spawn(async move {
                    let result = match tool {
                        Some(t) => {
                            tracing::debug!(tool = %name, "executing");
                            t.execute(input).await
                        }
                        None => Err(anyhow::anyhow!("Unknown tool: `{name}`")),
                    };
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
                if denied_ids.contains(id) {
                    continue; // already appended above
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

            tracing::debug!(turn, approved = tool_uses.len() - denied_ids.len(), "continuing");
        }

        tracing::warn!("reached MAX_TURNS ({MAX_TURNS})");
        self.touch_and_save()?;
        Ok(AgentResponse { text: response_text, tool_calls, usage })
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn touch_and_save(&mut self) -> Result<()> {
        self.session.updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.storage.save(&self.session)
    }
}
