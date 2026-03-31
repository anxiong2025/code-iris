//! Tool system — extensible tool framework modelled after Claude Code's 40+ tools.
//!
//! Each tool is a self-contained module with:
//! - An `input_schema()` (JSON Schema, validated by the LLM at call time)
//! - A `description()` used verbatim in the system prompt
//! - An async `execute()` that returns a `String` result
//!
//! The [`ToolRegistry`] owns all registered tools and hands them to the agent loop.

pub mod agent_tool;
pub mod bash;
pub mod file_edit;
pub mod file_read;
pub mod file_write;
pub mod glob;
pub mod grep;
pub mod send_message;
pub mod task;
pub mod web_fetch;
pub mod web_search;

pub use agent_tool::AgentTool;
pub use bash::BashTool;
pub use file_edit::FileEditTool;
pub use file_read::FileReadTool;
pub use file_write::FileWriteTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use send_message::{MessageBus, SendMessageTool};
pub use task::{TaskCreateTool, TaskGetTool, TaskListTool, TaskStore, TaskUpdateTool};
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use iris_llm::ToolDefinition;
use serde_json::Value;

/// A single agent tool.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    async fn execute(&self, input: Value) -> Result<String>;
}

/// Registry of all available tools.
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: HashMap::new() }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn all(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools: Vec<_> = self.tools.values().cloned().collect();
        tools.sort_by_key(|t| t.name().to_string());
        tools
    }

    pub fn all_definitions(&self) -> Vec<ToolDefinition> {
        self.all()
            .into_iter()
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect()
    }

    /// Full registry including AgentTool (for top-level agents).
    ///
    /// Reads `ANTHROPIC_API_KEY` from the environment for AgentTool.
    pub fn default_registry() -> Self {
        let store = TaskStore::new();
        let mut r = Self::new();
        r.register(Arc::new(BashTool));
        r.register(Arc::new(FileReadTool));
        r.register(Arc::new(FileWriteTool));
        r.register(Arc::new(FileEditTool));
        r.register(Arc::new(GrepTool));
        r.register(Arc::new(GlobTool));
        r.register(Arc::new(WebFetchTool));
        r.register(Arc::new(WebSearchTool));
        r.register(Arc::new(TaskCreateTool(store.clone())));
        r.register(Arc::new(TaskUpdateTool(store.clone())));
        r.register(Arc::new(TaskListTool(store.clone())));
        r.register(Arc::new(TaskGetTool(store)));
        if let Some(agent_tool) = AgentTool::from_env() {
            r.register(Arc::new(agent_tool));
        }
        r
    }

    /// Minimal registry for sub-agents — excludes AgentTool to prevent unbounded recursion.
    pub fn minimal_registry() -> Self {
        let store = TaskStore::new();
        let mut r = Self::new();
        r.register(Arc::new(BashTool));
        r.register(Arc::new(FileReadTool));
        r.register(Arc::new(FileWriteTool));
        r.register(Arc::new(FileEditTool));
        r.register(Arc::new(GrepTool));
        r.register(Arc::new(GlobTool));
        r.register(Arc::new(WebFetchTool));
        r.register(Arc::new(WebSearchTool));
        r.register(Arc::new(TaskCreateTool(store.clone())));
        r.register(Arc::new(TaskUpdateTool(store.clone())));
        r.register(Arc::new(TaskListTool(store.clone())));
        r.register(Arc::new(TaskGetTool(store)));
        r
    }
}
