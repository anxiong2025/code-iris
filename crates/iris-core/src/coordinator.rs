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
//! - Permissions inherited from the parent (never exceeds parent's mode)

use std::sync::Arc;

use anyhow::{bail, Result};
use futures::future::join_all;
use tokio::task::JoinHandle;

/// Returns the more restrictive of two permission modes.
///
/// Restrictiveness order (most → least): Plan > Default > Auto
fn most_restrictive(a: PermissionMode, b: PermissionMode) -> PermissionMode {
    fn rank(m: &PermissionMode) -> u8 {
        match m {
            PermissionMode::Plan => 0,
            PermissionMode::Default => 1,
            PermissionMode::Auto => 2,
            PermissionMode::Custom { .. } => 1,
        }
    }
    if rank(&a) <= rank(&b) { a } else { b }
}

use crate::agent::{Agent, AgentResponse};
use crate::agent_def::find_agent;
use crate::permissions::PermissionMode;
use crate::storage::new_session;
use crate::tools::send_message::{MessageBus, SendMessageTool};
use crate::tools::ToolRegistry;

/// Safety and concurrency limits for the coordinator.
#[derive(Debug, Clone)]
pub struct CoordinatorConfig {
    /// Maximum number of agent threads running concurrently (default: 6).
    pub max_threads: usize,
    /// Maximum nesting depth for sub-agents (default: 1 — direct children only).
    /// Depth 0 = root coordinator. Setting this above 1 risks exponential fan-out.
    pub max_depth: u8,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self { max_threads: 6, max_depth: 1 }
    }
}

/// A sub-task dispatched to one specialist agent.
#[derive(Debug, Clone)]
pub struct SubTask {
    /// Human-readable label (e.g. `"search"`, `"code-review"`).
    pub label: String,
    /// System prompt that specialises this sub-agent (appended after agent type instructions).
    pub system_prompt: String,
    /// The user-facing prompt for this sub-task.
    pub prompt: String,
    /// Optional agent type name: `"explorer"`, `"worker"`, `"reviewer"`, or any custom name.
    ///
    /// When set, the agent's sandbox mode, model, and instructions are loaded from the
    /// corresponding [`AgentDefinition`]. The coordinator's `permissions` still act as
    /// a ceiling — a `Full` agent in a `Plan`-mode coordinator runs as `Plan`.
    pub agent_type: Option<String>,
}

impl SubTask {
    /// Convenience constructor for a plain task with no agent type.
    pub fn plain(label: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            system_prompt: String::new(),
            prompt: prompt.into(),
            agent_type: None,
        }
    }

    /// Convenience constructor for a typed task.
    pub fn typed(
        label: impl Into<String>,
        agent_type: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            system_prompt: String::new(),
            prompt: prompt.into(),
            agent_type: Some(agent_type.into()),
        }
    }
}

/// Result from a single sub-agent.
#[derive(Debug)]
pub struct SubResult {
    pub label: String,
    pub response: AgentResponse,
}

/// How sub-agents obtain their LLM credentials.
#[derive(Clone)]
enum CoordAuth {
    /// Explicit Anthropic API key.
    ApiKey(String),
    /// Auto-detect from environment (same logic as `Agent::from_env()`).
    FromEnv,
}

/// Multi-agent coordinator.
pub struct Coordinator {
    auth: CoordAuth,
    model: String,
    bus: MessageBus,
    config: CoordinatorConfig,
    /// Current nesting depth (0 = root). Checked before spawning sub-agents.
    depth: u8,
    /// Permission mode inherited by all sub-agents.
    permissions: PermissionMode,
}

impl Coordinator {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            auth: CoordAuth::ApiKey(api_key.into()),
            model: "claude-sonnet-4-6-20250514".to_string(),
            bus: MessageBus::new(),
            config: CoordinatorConfig::default(),
            depth: 0,
            permissions: PermissionMode::Auto,
        }
    }

    /// Create a coordinator that auto-detects the provider from environment variables.
    pub fn from_env() -> Self {
        Self {
            auth: CoordAuth::FromEnv,
            model: "claude-sonnet-4-6-20250514".to_string(),
            bus: MessageBus::new(),
            config: CoordinatorConfig::default(),
            depth: 0,
            permissions: PermissionMode::Auto,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Override concurrency / depth limits.
    pub fn with_config(mut self, config: CoordinatorConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the permission mode inherited by all sub-agents.
    ///
    /// Sub-agents will never receive a mode more permissive than this.
    /// For interactive callers, pass in the parent agent's current mode.
    pub fn with_permissions(mut self, mode: PermissionMode) -> Self {
        self.permissions = mode;
        self
    }

    /// Build a fresh sub-agent for one task.
    ///
    /// If `task.agent_type` is set, loads the [`AgentDefinition`] and applies its
    /// model and permissions — but the coordinator's `ceiling` always wins when more
    /// restrictive (e.g. a `Full` agent in a `Plan`-mode coordinator runs as `Plan`).
    fn make_agent(
        auth: &CoordAuth,
        default_model: &str,
        task: &SubTask,
        bus: MessageBus,
        agent_id: String,
        ceiling: PermissionMode,
    ) -> Result<Agent> {
        // Resolve agent definition (custom → built-in → fallback).
        let def = task.agent_type.as_deref().and_then(|t| find_agent(t, None));

        // Model: agent def > coordinator default.
        let model = def.as_ref()
            .and_then(|d| d.model.as_deref())
            .unwrap_or(default_model);

        // Permissions: most restrictive of (agent def, coordinator ceiling).
        let agent_perm = def.as_ref()
            .map(|d| d.permission_mode())
            .unwrap_or(ceiling.clone());
        let effective_perm = most_restrictive(agent_perm, ceiling);

        // System prompt: agent def instructions + task-specific system_prompt.
        let mut combined_instructions = String::new();
        if let Some(ref d) = def {
            combined_instructions.push_str(&d.instructions);
            if !task.system_prompt.is_empty() {
                combined_instructions.push_str("\n\n");
            }
        }
        combined_instructions.push_str(&task.system_prompt);

        let mut registry = ToolRegistry::minimal_registry();
        registry.register(Arc::new(SendMessageTool { bus, agent_id }));

        let mut agent = match auth {
            CoordAuth::ApiKey(key) => Agent::new(key)?,
            CoordAuth::FromEnv => Agent::from_env()?,
        };
        agent = agent
            .with_model(model)
            .with_system_prompt(&combined_instructions)
            .with_permissions(effective_perm)
            .with_session(new_session());
        agent.set_tools(registry);
        Ok(agent)
    }

    /// Run all sub-tasks concurrently and return their results in order.
    ///
    /// Respects `max_depth` and `max_threads` from [`CoordinatorConfig`].
    pub async fn run(&self, tasks: Vec<SubTask>) -> Result<Vec<SubResult>> {
        if self.depth >= self.config.max_depth {
            bail!(
                "coordinator depth limit reached (max_depth={}): refusing to spawn sub-agents",
                self.config.max_depth
            );
        }

        // Enforce max_threads: chunk tasks and run each chunk sequentially.
        let mut all_results: Vec<SubResult> = Vec::with_capacity(tasks.len());
        for chunk in tasks.chunks(self.config.max_threads) {
            let handles: Vec<JoinHandle<Result<SubResult>>> = chunk
                .iter()
                .cloned()
                .enumerate()
                .map(|(i, task)| {
                    let auth = self.auth.clone();
                    let model = self.model.clone();
                    let bus = self.bus.clone();
                    let ceiling = self.permissions.clone();
                    let agent_id = format!("sub-{}", self.depth * 100 + i as u8);

                    tokio::spawn(async move {
                        let mut agent = Self::make_agent(
                            &auth, &model, &task, bus, agent_id, ceiling,
                        )?;
                        let response = agent.chat(&task.prompt).await?;
                        Ok(SubResult { label: task.label, response })
                    })
                })
                .collect();

            let chunk_results = join_all(handles).await;
            for r in chunk_results {
                all_results.push(r.map_err(|e| anyhow::anyhow!("sub-agent panicked: {e}"))??);
            }
        }

        Ok(all_results)
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

        let mut synth_agent = match &self.auth {
            CoordAuth::ApiKey(key) => Agent::new(key)?,
            CoordAuth::FromEnv => Agent::from_env()?,
        };
        synth_agent = synth_agent
            .with_model(&self.model)
            .with_permissions(self.permissions.clone());

        synth_agent.chat(&synthesis_prompt).await
    }

    /// Run steps sequentially, injecting each step's output into the next step's context.
    ///
    /// This is the core "pipeline" pattern — unlike [`run`] (parallel fan-out),
    /// each step here receives all prior outputs as structured context in its
    /// system prompt before it runs.
    ///
    /// ```text
    /// Step 0 (explorer) → output_0
    ///   ↓ injected as context
    /// Step 1 (reviewer) → output_1
    ///   ↓ injected as context
    /// Step 2 (worker)   → final output
    /// ```
    ///
    /// Returns each step's output in order.
    pub async fn pipeline_run(&self, steps: Vec<PipelineStep>) -> Result<Vec<PipelineStepResult>> {
        if self.depth >= self.config.max_depth {
            bail!(
                "coordinator depth limit reached (max_depth={}): refusing to run pipeline",
                self.config.max_depth
            );
        }
        if steps.is_empty() {
            return Ok(vec![]);
        }

        let mut results: Vec<PipelineStepResult> = Vec::with_capacity(steps.len());

        for (i, step) in steps.into_iter().enumerate() {
            // Build context block from all previous step outputs.
            let prior_context = if results.is_empty() {
                String::new()
            } else {
                let mut ctx = String::from("# Prior step results\n\n");
                for prev in &results {
                    ctx.push_str(&format!("## {}\n\n{}\n\n---\n\n", prev.label, prev.text));
                }
                ctx
            };

            // Resolve agent definition.
            let def = step.agent_type.as_deref().and_then(|t| find_agent(t, None));

            let model = def.as_ref()
                .and_then(|d| d.model.as_deref())
                .unwrap_or(&self.model);

            let agent_perm = def.as_ref()
                .map(|d| d.permission_mode())
                .unwrap_or(self.permissions.clone());
            let effective_perm = most_restrictive(agent_perm, self.permissions.clone());

            // System prompt: prior context + agent instructions + step-specific prompt.
            let mut system = String::new();
            if !prior_context.is_empty() {
                system.push_str(&prior_context);
            }
            if let Some(ref d) = def {
                if !d.instructions.is_empty() {
                    if !system.is_empty() { system.push_str("\n\n"); }
                    system.push_str(&d.instructions);
                }
            }
            if !step.system_prompt.is_empty() {
                if !system.is_empty() { system.push_str("\n\n"); }
                system.push_str(&step.system_prompt);
            }

            let agent_id = format!("pipe-{i}");
            let mut registry = ToolRegistry::minimal_registry();
            registry.register(Arc::new(SendMessageTool {
                bus: self.bus.clone(),
                agent_id,
            }));

            let mut agent = match &self.auth {
                CoordAuth::ApiKey(key) => Agent::new(key)?,
                CoordAuth::FromEnv => Agent::from_env()?,
            };
            agent = agent
                .with_model(model)
                .with_system_prompt(&system)
                .with_permissions(effective_perm)
                .with_session(new_session());
            agent.set_tools(registry);

            let response = agent.chat(&step.prompt).await?;
            results.push(PipelineStepResult {
                label: step.label,
                text: response.text,
                usage: response.usage,
            });
        }

        Ok(results)
    }

    /// Access the shared message bus (e.g. to subscribe from the caller).
    pub fn bus(&self) -> &MessageBus {
        &self.bus
    }
}

// ── Pipeline types ────────────────────────────────────────────────────────────

/// One step in a [`Coordinator::pipeline_run`] sequence.
#[derive(Debug, Clone)]
pub struct PipelineStep {
    /// Human-readable label shown in output (e.g. `"explore"`, `"review"`).
    pub label: String,
    /// Optional agent type: `"explorer"`, `"worker"`, `"reviewer"`, or any custom name.
    pub agent_type: Option<String>,
    /// Additional system instructions appended after the agent type's built-in instructions.
    pub system_prompt: String,
    /// The user prompt for this step.
    pub prompt: String,
}

impl PipelineStep {
    pub fn new(label: impl Into<String>, agent_type: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            agent_type: Some(agent_type.into()),
            system_prompt: String::new(),
            prompt: prompt.into(),
        }
    }
}

/// Output from a single pipeline step.
#[derive(Debug)]
pub struct PipelineStepResult {
    pub label: String,
    pub text: String,
    pub usage: iris_llm::TokenUsage,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let cfg = CoordinatorConfig::default();
        assert_eq!(cfg.max_threads, 6);
        assert_eq!(cfg.max_depth, 1);
    }

    #[test]
    fn depth_limit_error() {
        // A coordinator already at max_depth should refuse to run tasks.
        let coord = Coordinator::from_env().with_config(CoordinatorConfig {
            max_threads: 6,
            max_depth: 0, // already at limit
        });
        // depth=0, max_depth=0 → should error immediately
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let err = rt.block_on(coord.run(vec![SubTask::plain("test", "hello")]));
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("depth limit"));
    }

    #[test]
    fn with_permissions_stored() {
        let coord = Coordinator::from_env().with_permissions(PermissionMode::Plan);
        assert!(matches!(coord.permissions, PermissionMode::Plan));
    }
}
