//! Task management tools — create, update, list, and inspect tasks.
//!
//! These tools mirror Claude Code's `TaskCreate / TaskUpdate / TaskList / TaskGet`
//! and give the agent a way to plan and track multi-step work within a single session.
//!
//! Tasks are persisted to `~/.code-iris/tasks/<session_id>.json` when a session
//! ID is supplied at construction time. Without a session ID they fall back to
//! in-memory only (useful for ephemeral sub-agents).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::Tool;

// ── Data model ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Pending => write!(f, "pending"),
            TaskStatus::InProgress => write!(f, "in_progress"),
            TaskStatus::Completed => write!(f, "completed"),
            TaskStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: TaskStatus,
    pub output: Option<String>,
}

// ── TaskStore internals ──────────────────────────────────────────────────────

struct Inner {
    tasks: HashMap<String, Task>,
    /// When set, every mutation is persisted here.
    path: Option<PathBuf>,
}

impl Inner {
    fn save(&self) {
        if let Some(ref path) = self.path {
            if let Ok(json) = serde_json::to_string_pretty(&self.tasks) {
                // Best-effort — ignore I/O errors (don't break the agent turn).
                let _ = std::fs::write(path, json);
            }
        }
    }
}

/// Shared task store, injected into each task tool.
///
/// Cheap to clone — backed by `Arc<Mutex<Inner>>`.
#[derive(Clone, Default)]
pub struct TaskStore(Arc<Mutex<Inner>>);

impl Default for Inner {
    fn default() -> Self {
        Self { tasks: HashMap::new(), path: None }
    }
}

impl TaskStore {
    /// In-memory only (no persistence).
    pub fn new() -> Self {
        Self::default()
    }

    /// Persistent store tied to a session.
    ///
    /// Existing tasks are loaded from disk if the file exists.
    /// All future mutations are auto-saved to the same file.
    pub fn for_session(session_id: &str) -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
        let dir = home.join(".code-iris").join("tasks");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{session_id}.json"));

        let tasks: HashMap<String, Task> = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            HashMap::new()
        };

        Ok(Self(Arc::new(Mutex::new(Inner { tasks, path: Some(path) }))))
    }

    fn next_id(&self) -> String {
        let guard = self.0.lock().unwrap();
        format!("task_{}", guard.tasks.len() + 1)
    }

    fn insert(&self, task: Task) {
        let mut guard = self.0.lock().unwrap();
        guard.tasks.insert(task.id.clone(), task);
        guard.save();
    }

    fn get(&self, id: &str) -> Option<Task> {
        self.0.lock().unwrap().tasks.get(id).cloned()
    }

    fn update_status(&self, id: &str, status: TaskStatus, output: Option<String>) -> bool {
        let mut guard = self.0.lock().unwrap();
        if let Some(task) = guard.tasks.get_mut(id) {
            task.status = status;
            if let Some(out) = output {
                task.output = Some(out);
            }
            guard.save();
            true
        } else {
            false
        }
    }

    fn list_all(&self) -> Vec<Task> {
        let guard = self.0.lock().unwrap();
        let mut tasks: Vec<Task> = guard.tasks.values().cloned().collect();
        tasks.sort_by(|a, b| a.id.cmp(&b.id));
        tasks
    }
}

// ── TaskCreate ───────────────────────────────────────────────────────────────

pub struct TaskCreateTool(pub TaskStore);

#[async_trait]
impl Tool for TaskCreateTool {
    fn name(&self) -> &str { "task_create" }

    fn description(&self) -> &str {
        "Create a new task to track a piece of work. Returns the task ID."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "Short title for the task" },
                "description": { "type": "string", "description": "Detailed description of what needs to be done" }
            },
            "required": ["title"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let title = input.get("title").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field: title"))?
            .to_string();
        let description = input.get("description").and_then(|v| v.as_str())
            .unwrap_or("").to_string();

        let id = self.0.next_id();
        self.0.insert(Task {
            id: id.clone(),
            title,
            description,
            status: TaskStatus::Pending,
            output: None,
        });

        Ok(format!("Created task `{id}`"))
    }
}

// ── TaskUpdate ───────────────────────────────────────────────────────────────

pub struct TaskUpdateTool(pub TaskStore);

#[async_trait]
impl Tool for TaskUpdateTool {
    fn name(&self) -> &str { "task_update" }

    fn description(&self) -> &str {
        "Update the status of an existing task. Valid statuses: pending, in_progress, completed, cancelled."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Task ID returned by task_create" },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed", "cancelled"],
                    "description": "New status"
                },
                "output": { "type": "string", "description": "Optional result or notes to attach to the task" }
            },
            "required": ["id", "status"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let id = input.get("id").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field: id"))?;
        let status_str = input.get("status").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field: status"))?;
        let output = input.get("output").and_then(|v| v.as_str()).map(|s| s.to_string());

        let status = match status_str {
            "pending" => TaskStatus::Pending,
            "in_progress" => TaskStatus::InProgress,
            "completed" => TaskStatus::Completed,
            "cancelled" => TaskStatus::Cancelled,
            other => return Err(anyhow::anyhow!("unknown status: {other}")),
        };

        if self.0.update_status(id, status.clone(), output) {
            Ok(format!("Task `{id}` → {status}"))
        } else {
            Err(anyhow::anyhow!("task not found: {id}"))
        }
    }
}

// ── TaskList ─────────────────────────────────────────────────────────────────

pub struct TaskListTool(pub TaskStore);

#[async_trait]
impl Tool for TaskListTool {
    fn name(&self) -> &str { "task_list" }

    fn description(&self) -> &str {
        "List all tasks in the current session with their statuses."
    }

    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn execute(&self, _input: Value) -> Result<String> {
        let tasks = self.0.list_all();
        if tasks.is_empty() {
            return Ok("No tasks yet.".to_string());
        }

        let mut out = String::from("Tasks:\n");
        for task in &tasks {
            let icon = match task.status {
                TaskStatus::Pending => "○",
                TaskStatus::InProgress => "◉",
                TaskStatus::Completed => "✓",
                TaskStatus::Cancelled => "✗",
            };
            out.push_str(&format!("  {icon} [{}] {}\n", task.id, task.title));
        }
        Ok(out)
    }
}

// ── TaskGet ───────────────────────────────────────────────────────────────────

pub struct TaskGetTool(pub TaskStore);

#[async_trait]
impl Tool for TaskGetTool {
    fn name(&self) -> &str { "task_get" }

    fn description(&self) -> &str {
        "Get details of a specific task, including its output if set."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Task ID" }
            },
            "required": ["id"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let id = input.get("id").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field: id"))?;

        let task = self.0.get(id)
            .ok_or_else(|| anyhow::anyhow!("task not found: {id}"))?;

        let mut out = format!(
            "Task: {}\nID: {}\nStatus: {}\nDescription: {}\n",
            task.title, task.id, task.status, task.description
        );
        if let Some(output) = &task.output {
            out.push_str(&format!("Output:\n{output}\n"));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_get_task() {
        let store = TaskStore::new();
        store.insert(Task {
            id: "task_1".to_string(),
            title: "Test task".to_string(),
            description: "desc".to_string(),
            status: TaskStatus::Pending,
            output: None,
        });
        let task = store.get("task_1").expect("task should exist");
        assert_eq!(task.title, "Test task");
        assert_eq!(task.status, TaskStatus::Pending);
    }

    #[test]
    fn update_status() {
        let store = TaskStore::new();
        store.insert(Task {
            id: "task_1".to_string(),
            title: "T".to_string(),
            description: String::new(),
            status: TaskStatus::Pending,
            output: None,
        });
        let updated = store.update_status("task_1", TaskStatus::Completed, Some("done".to_string()));
        assert!(updated);
        let task = store.get("task_1").unwrap();
        assert_eq!(task.status, TaskStatus::Completed);
        assert_eq!(task.output.as_deref(), Some("done"));
    }

    #[test]
    fn update_nonexistent_returns_false() {
        let store = TaskStore::new();
        assert!(!store.update_status("ghost", TaskStatus::Completed, None));
    }

    #[test]
    fn list_all_sorted() {
        let store = TaskStore::new();
        for (id, title) in [("task_2", "B"), ("task_1", "A"), ("task_3", "C")] {
            store.insert(Task {
                id: id.to_string(),
                title: title.to_string(),
                description: String::new(),
                status: TaskStatus::Pending,
                output: None,
            });
        }
        let tasks = store.list_all();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].id, "task_1");
        assert_eq!(tasks[2].id, "task_3");
    }

    #[test]
    fn next_id_increments() {
        let store = TaskStore::new();
        let id1 = store.next_id();
        assert_eq!(id1, "task_1");
        store.insert(Task {
            id: id1,
            title: "x".to_string(),
            description: String::new(),
            status: TaskStatus::Pending,
            output: None,
        });
        assert_eq!(store.next_id(), "task_2");
    }

    #[test]
    fn task_status_display() {
        assert_eq!(TaskStatus::Pending.to_string(), "pending");
        assert_eq!(TaskStatus::InProgress.to_string(), "in_progress");
        assert_eq!(TaskStatus::Completed.to_string(), "completed");
        assert_eq!(TaskStatus::Cancelled.to_string(), "cancelled");
    }

    #[test]
    fn clone_shares_state() {
        let store = TaskStore::new();
        let clone = store.clone();
        store.insert(Task {
            id: "task_1".to_string(),
            title: "shared".to_string(),
            description: String::new(),
            status: TaskStatus::Pending,
            output: None,
        });
        // Clone should see the same task (Arc-backed).
        assert!(clone.get("task_1").is_some());
    }
}
