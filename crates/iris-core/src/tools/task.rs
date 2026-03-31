//! Task management tools — create, update, list, and inspect in-session tasks.
//!
//! These tools mirror Claude Code's `TaskCreate / TaskUpdate / TaskList / TaskGet`
//! and give the agent a way to plan and track multi-step work within a single session.
//!
//! Tasks are stored in a thread-local `TaskStore` so they are lightweight and
//! require no additional I/O. They are NOT persisted across sessions.

use std::collections::HashMap;
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

/// Shared in-memory task store, injected into each task tool.
#[derive(Debug, Default, Clone)]
pub struct TaskStore(Arc<Mutex<HashMap<String, Task>>>);

impl TaskStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_id(&self) -> String {
        let guard = self.0.lock().unwrap();
        format!("task_{}", guard.len() + 1)
    }

    fn insert(&self, task: Task) {
        self.0.lock().unwrap().insert(task.id.clone(), task);
    }

    fn get(&self, id: &str) -> Option<Task> {
        self.0.lock().unwrap().get(id).cloned()
    }

    fn update_status(&self, id: &str, status: TaskStatus, output: Option<String>) -> bool {
        let mut guard = self.0.lock().unwrap();
        if let Some(task) = guard.get_mut(id) {
            task.status = status;
            if let Some(out) = output {
                task.output = Some(out);
            }
            true
        } else {
            false
        }
    }

    fn list_all(&self) -> Vec<Task> {
        let guard = self.0.lock().unwrap();
        let mut tasks: Vec<Task> = guard.values().cloned().collect();
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
