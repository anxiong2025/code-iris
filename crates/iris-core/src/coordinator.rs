//! Multi-agent Coordinator — mirrors Claude Code's multi-agent dispatch layer.
//!
//! The [`Coordinator`] fans out a high-level task to N specialist sub-agents
//! running concurrently, collects their results, and optionally feeds them to a
//! "synthesis" agent that produces a final consolidated answer.
//!
//! ```text
//! Coordinator::run(task)
//!   ├── spawn sub-agent 0  (tokio task)
//!   ├── spawn sub-agent 1  (tokio task)
//!   ├── …
//!   └── synthesis agent    (receives all sub-results via MessageBus)
//! ```
//!
//! Each sub-agent gets:
//! - A unique `agent_id` (e.g. `"sub-0"`)
//! - A `SendMessageTool` wired to the shared `MessageBus`
//! - The `minimal_registry()` (no nested AgentTool to prevent runaway recursion)
//! - `PermissionMode::Auto` (non-interactive)

use std::sync::Arc;

use anyhow::Result;
use futures::future::join_all;
use tokio::task::JoinHandle;

use crate::agent::{Agent, AgentResponse};
use crate::permissions::PermissionMode;
use crate::storage::new_session;
use crate::tools::send_message::{MessageBus, SendMessageTool};
use crate::tools::ToolRegistry;

/// A sub-task dispatched to one specialist agent.
#[derive(Debug, Clone)]
pub struct SubTask {
    /// Human-readable label (e.g. `"search"`, `"code-review"`).
    pub label: String,
    /// System prompt that specialises this sub-agent.
    pub system_prompt: String,
    /// The user-facing prompt for this sub-task.
    pub prompt: String,
}

/// Result from a single sub-agent.
#[derive(Debug)]
pub struct SubResult {
    pub label: String,
    pub response: AgentResponse,
}

/// Multi-agent coordinator.
pub struct Coordinator {
    api_key: String,
    model: String,
    bus: MessageBus,
}

impl Coordinator {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: "claude-sonnet-4-6-20250514".to_string(),
            bus: MessageBus::new(),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Run all sub-tasks concurrently and return their results in order.
    pub async fn run(&self, tasks: Vec<SubTask>) -> Result<Vec<SubResult>> {
        let handles: Vec<JoinHandle<Result<SubResult>>> = tasks
            .into_iter()
            .enumerate()
            .map(|(i, task)| {
                let api_key = self.api_key.clone();
                let model = self.model.clone();
                let bus = self.bus.clone();
                let agent_id = format!("sub-{i}");

                tokio::spawn(async move {
                    let mut registry = ToolRegistry::minimal_registry();
                    registry.register(Arc::new(SendMessageTool {
                        bus,
                        agent_id: agent_id.clone(),
                    }));

                    let mut agent = Agent::new(&api_key)?
                        .with_model(model)
                        .with_system_prompt(&task.system_prompt)
                        .with_permissions(PermissionMode::Auto)
                        .with_session(new_session());

                    // Override the tool registry so it includes SendMessageTool.
                    agent.set_tools(registry);

                    let response = agent.chat(&task.prompt).await?;
                    Ok(SubResult { label: task.label, response })
                })
            })
            .collect();

        let results = join_all(handles).await;
        results
            .into_iter()
            .map(|r| r.map_err(|e| anyhow::anyhow!("sub-agent panicked: {e}"))?)
            .collect()
    }

    /// Run all sub-tasks, then synthesise a final answer from their outputs.
    pub async fn run_with_synthesis(
        &self,
        tasks: Vec<SubTask>,
        synthesis_prompt_template: &str,
    ) -> Result<AgentResponse> {
        let sub_results = self.run(tasks).await?;

        // Build a combined context for the synthesis agent.
        let combined = sub_results
            .iter()
            .map(|r| format!("## {}\n\n{}", r.label, r.response.text))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        let synthesis_prompt = synthesis_prompt_template.replace("{results}", &combined);

        let mut synth_agent = Agent::new(&self.api_key)?
            .with_model(&self.model)
            .with_permissions(PermissionMode::Auto);

        synth_agent.chat(&synthesis_prompt).await
    }

    /// Access the shared message bus (e.g. to subscribe from the caller).
    pub fn bus(&self) -> &MessageBus {
        &self.bus
    }
}
