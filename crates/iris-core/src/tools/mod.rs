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
pub mod lsp;
pub mod mcp_tool;
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
pub use lsp::LspTool;
pub use send_message::{MessageBus, SendMessageTool};
pub use task::{TaskCreateTool, TaskGetTool, TaskListTool, TaskStore, TaskUpdateTool};
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use iris_llm::ToolDefinition;
use serde_json::Value;

/// Shared working-directory reference — injected into tools that do I/O.
///
/// When `None`, tools use the process's current working directory.
/// Set via `Agent::set_cwd()` or the `/cd` slash command.
pub type CwdRef = Arc<Mutex<Option<PathBuf>>>;

/// Resolve `path` relative to `cwd` (if it is not already absolute).
pub fn resolve_path(path: &str, cwd: &CwdRef) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        return p;
    }
    if let Some(ref base) = *cwd.lock().unwrap() {
        return base.join(p);
    }
    p
}

// ── Tool trait ────────────────────────────────────────────────────────────────

/// A single agent tool.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    async fn execute(&self, input: Value) -> Result<String>;
}

// ── Tool registry ─────────────────────────────────────────────────────────────

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

    /// Full registry including AgentTool.
    ///
    /// - `session_id` enables task persistence.
    /// - `cwd` is a shared working-directory reference injected into all I/O tools.
    pub fn default_registry_for(session_id: Option<&str>, cwd: CwdRef) -> Self {
        let store = session_id
            .and_then(|id| TaskStore::for_session(id).ok())
            .unwrap_or_default();
        let mut r = Self::new();
        r.register(Arc::new(BashTool::new(cwd.clone())));
        r.register(Arc::new(FileReadTool::new(cwd.clone())));
        r.register(Arc::new(FileWriteTool::new(cwd.clone())));
        r.register(Arc::new(FileEditTool::new(cwd.clone())));
        r.register(Arc::new(GrepTool::new(cwd.clone())));
        r.register(Arc::new(GlobTool::new(cwd.clone())));
        r.register(Arc::new(LspTool::new(cwd)));
        r.register(Arc::new(WebFetchTool::new()));
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

    /// Full registry with no persistence and default cwd (backward-compat).
    pub fn default_registry() -> Self {
        Self::default_registry_for(None, Arc::new(Mutex::new(None)))
    }

    /// Minimal registry for sub-agents — excludes AgentTool to prevent unbounded recursion.
    pub fn minimal_registry() -> Self {
        let store = TaskStore::new();
        let cwd: CwdRef = Arc::new(Mutex::new(None));
        let mut r = Self::new();
        r.register(Arc::new(BashTool::new(cwd.clone())));
        r.register(Arc::new(FileReadTool::new(cwd.clone())));
        r.register(Arc::new(FileWriteTool::new(cwd.clone())));
        r.register(Arc::new(FileEditTool::new(cwd.clone())));
        r.register(Arc::new(GrepTool::new(cwd.clone())));
        r.register(Arc::new(GlobTool::new(cwd.clone())));
        r.register(Arc::new(LspTool::new(cwd)));
        r.register(Arc::new(WebFetchTool::new()));
        r.register(Arc::new(WebSearchTool));
        r.register(Arc::new(TaskCreateTool(store.clone())));
        r.register(Arc::new(TaskUpdateTool(store.clone())));
        r.register(Arc::new(TaskListTool(store.clone())));
        r.register(Arc::new(TaskGetTool(store)));
        r
    }
}
