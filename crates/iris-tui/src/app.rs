//! Application state for the iris TUI.
//!
//! The App struct is owned by the main event loop. The agent runs in a
//! dedicated tokio task and communicates back via an unbounded channel.

use std::path::PathBuf;

use iris_llm::TokenUsage;

// ── Events sent from the agent worker to the TUI ─────────────────────────────

/// Events the agent worker sends back to the TUI event loop.
#[derive(Debug)]
pub enum AgentEvent {
    /// A streamed text chunk from the LLM.
    TextChunk(String),
    /// A tool was called (name).
    ToolCall(String),
    /// The agent finished a full exchange.
    Done { _tool_calls: Vec<String>, usage: TokenUsage },
    /// The agent encountered an error.
    Error(String),
}

// ── Chat history ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ChatEntry {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChatRole {
    User,
    Assistant,
    Tool,
    System,
}

// ── App mode / agent state ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum AppMode {
    Welcome,
    Chat,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentState {
    Idle,
    Thinking,
    Streaming,
}

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    pub mode: AppMode,
    pub agent_state: AgentState,
    pub input: String,
    pub chat_history: Vec<ChatEntry>,
    pub scroll_offset: usize,
    pub user_scrolled: bool,

    pub model_name: String,
    pub total_tokens: u32,
    pub working_dir: PathBuf,
    pub git_branch: Option<String>,
    pub project_type: Option<String>,
    pub file_count: usize,
    pub has_api_key: bool,
    pub session_id: Option<String>,
}

impl App {
    pub fn new(session_id: Option<String>) -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let git_branch = detect_git_branch(&working_dir);
        let project_type = detect_project_type(&working_dir);
        let file_count = count_source_files(&working_dir);
        let has_api_key = std::env::var("ANTHROPIC_API_KEY")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);

        Self {
            mode: AppMode::Welcome,
            agent_state: AgentState::Idle,
            input: String::new(),
            chat_history: Vec::new(),
            scroll_offset: 0,
            user_scrolled: false,
            model_name: "claude-sonnet-4.6".to_string(),
            total_tokens: 0,
            working_dir,
            git_branch,
            project_type,
            file_count,
            has_api_key,
            session_id,
        }
    }

    pub fn push_char(&mut self, c: char) {
        self.input.push(c);
        self.mode = AppMode::Chat;
    }

    pub fn pop_char(&mut self) {
        self.input.pop();
        if self.input.is_empty() && self.chat_history.is_empty() {
            self.mode = AppMode::Welcome;
        }
    }

    pub fn take_input(&mut self) -> String {
        std::mem::take(&mut self.input)
    }

    pub fn push_user(&mut self, text: impl Into<String>) {
        self.chat_history.push(ChatEntry { role: ChatRole::User, content: text.into() });
        self.mode = AppMode::Chat;
        self.agent_state = AgentState::Thinking;
        self.user_scrolled = false;
    }

    pub fn append_assistant_chunk(&mut self, chunk: &str) {
        self.agent_state = AgentState::Streaming;
        match self.chat_history.last_mut() {
            Some(e) if e.role == ChatRole::Assistant => e.content.push_str(chunk),
            _ => self.chat_history.push(ChatEntry {
                role: ChatRole::Assistant,
                content: chunk.to_string(),
            }),
        }
        if !self.user_scrolled {
            self.scroll_to_bottom();
        }
    }

    pub fn push_tool_call(&mut self, name: &str) {
        self.chat_history.push(ChatEntry {
            role: ChatRole::Tool,
            content: format!("⚙  {name}"),
        });
    }

    pub fn push_system(&mut self, text: impl Into<String>) {
        self.chat_history.push(ChatEntry { role: ChatRole::System, content: text.into() });
    }

    pub fn finish_response(&mut self, usage: &TokenUsage) {
        self.agent_state = AgentState::Idle;
        self.total_tokens += usage.input_tokens + usage.output_tokens;
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(3);
        self.user_scrolled = true;
    }

    pub fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(3);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = usize::MAX / 2;
    }

    pub fn working_dir_short(&self) -> String {
        let home = dirs::home_dir().unwrap_or_default();
        let path = self.working_dir.display().to_string();
        let home_str = home.display().to_string();
        if path.starts_with(&home_str) {
            format!("~{}", &path[home_str.len()..])
        } else {
            path
        }
    }
}

fn detect_git_branch(dir: &PathBuf) -> Option<String> {
    std::fs::read_to_string(dir.join(".git/HEAD"))
        .ok()?
        .trim()
        .strip_prefix("ref: refs/heads/")
        .map(|s| s.to_string())
}

fn detect_project_type(dir: &PathBuf) -> Option<String> {
    if dir.join("Cargo.toml").exists() { return Some("Rust".into()); }
    if dir.join("pyproject.toml").exists() || dir.join("setup.py").exists() { return Some("Python".into()); }
    if dir.join("package.json").exists() { return Some("Node.js".into()); }
    if dir.join("go.mod").exists() { return Some("Go".into()); }
    None
}

fn count_source_files(dir: &PathBuf) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut n = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s.starts_with('.') || matches!(s.as_ref(), "node_modules" | "target" | "__pycache__") {
            continue;
        }
        let path = entry.path();
        if path.is_file() { n += 1; }
        else if path.is_dir() { n += count_source_files(&path); }
    }
    n
}
