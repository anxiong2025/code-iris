//! AgentTool — spawn a sub-agent to handle a self-contained sub-task.
//!
//! Mirrors Claude Code's `AgentTool` / `src/coordinator/` pattern:
//!
//! - The parent agent delegates a focused task to a child agent.
//! - The child runs with `PermissionMode::Auto` (no interactive prompts).
//! - The child gets its own fresh session and the standard tool set
//!   (excluding AgentTool itself to prevent unbound recursion).
//! - The parent receives the child's final response as a tool result string.
//!
//! Typical use case: the parent asks the sub-agent to perform a multi-step
//! file analysis or code edit while it continues planning at a higher level.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::Agent;
use crate::permissions::PermissionMode;
use super::Tool;

pub struct AgentTool {
    /// Anthropic API key forwarded from the parent agent's environment.
    api_key: String,
    /// Model to use for sub-agents (defaults to same as parent).
    model: String,
}

impl AgentTool {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
        }
    }

    /// Convenience constructor that reads the API key from the environment.
    pub fn from_env() -> Option<Self> {
        let key = std::env::var("ANTHROPIC_API_KEY").ok()?;
        if key.trim().is_empty() {
            return None;
        }
        Some(Self::new(key, "claude-sonnet-4-6-20250514"))
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        "agent"
    }

    fn description(&self) -> &str {
        "Spawn a sub-agent to complete a self-contained task. \
         The sub-agent has access to all standard tools (bash, file operations, web fetch, etc.) \
         and will work autonomously until the task is done or it reaches its turn limit. \
         Use this to parallelise independent sub-tasks or to delegate complex multi-step work."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Clear, self-contained description of the task for the sub-agent"
                },
                "system_prompt": {
                    "type": "string",
                    "description": "Optional additional system instructions for the sub-agent"
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override (e.g. claude-haiku-4-5-20251001 for fast tasks)"
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let task = input
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field: task"))?
            .to_string();

        let model = input
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.model)
            .to_string();

        let system_prompt = input
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        tracing::debug!(task = %task, model = %model, "spawning sub-agent");

        // Build sub-agent — Auto permissions (no interactive prompts in sub-tasks).
        let mut sub_agent = Agent::new(self.api_key.clone())?
            .with_model(model)
            .with_permissions(PermissionMode::Auto);

        if let Some(prompt) = system_prompt {
            sub_agent = sub_agent.with_system_prompt(prompt);
        }

        let response = sub_agent.chat(&task).await?;

        let summary = format!(
            "{}\n\n[sub-agent: {} tool calls, {} in / {} out tokens]",
            response.text,
            response.tool_calls.len(),
            response.usage.input_tokens,
            response.usage.output_tokens,
        );

        tracing::debug!(
            tool_calls = response.tool_calls.len(),
            tokens_in = response.usage.input_tokens,
            tokens_out = response.usage.output_tokens,
            "sub-agent finished"
        );

        Ok(summary)
    }
}
