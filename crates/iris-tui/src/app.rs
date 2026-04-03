//! Application state for the iris TUI.
//!
//! The App struct is owned by the main event loop. The agent runs in a
//! dedicated tokio task and communicates back via an unbounded channel.

use std::path::PathBuf;
use std::time::Instant;

use iris_llm::TokenUsage;

pub use crate::buddy::{Companion, ReactionState};
pub use crate::completion::CompletionState;

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
    /// A system/status message from the worker (model switch, compact result, etc.).
    System(String),
    /// The model (and possibly provider) was switched — update status bar.
    ModelSwitched { actual_model: String },
    /// The agent encountered an error.
    Error(String),
    /// A pipeline step started or completed.
    PipelineStep {
        index: usize,
        total: usize,
        label: String,
        /// false = started, true = completed
        done: bool,
        text: Option<String>,
    },
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
    /// Absolute cursor position within `input` (in chars).
    pub cursor_pos: usize,
    pub chat_history: Vec<ChatEntry>,
    pub scroll_offset: usize,
    pub user_scrolled: bool,

    /// Command history (oldest first).
    pub input_history: Vec<String>,
    /// Index into input_history while navigating (None = not navigating).
    pub history_idx: Option<usize>,

    /// Animation tick counter (incremented on AgentEvent::Tick).
    pub tick: u64,

    /// Short display path for cwd (e.g. ~/project/code-iris).
    pub cwd_short: String,

    pub model_name: String,
    pub total_tokens: u32,
    pub working_dir: PathBuf,
    pub git_branch: Option<String>,
    pub project_type: Option<String>,
    pub file_count: usize,
    pub has_api_key: bool,
    pub session_id: Option<String>,
    /// When the current turn started (for elapsed time display).
    pub turn_started_at: Option<Instant>,
    /// Last known max_scroll (updated by render_chat so scroll_up works correctly).
    pub last_max_scroll: usize,

    /// Slash command completion menu state.
    pub completion: CompletionState,

    /// Active buddy companion (None until /buddy is used).
    pub buddy: Option<Companion>,
    /// Current buddy speech bubble reaction.
    pub buddy_reaction: Option<ReactionState>,
}

impl App {
    pub fn new(session_id: Option<String>) -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let git_branch = detect_git_branch(&working_dir);
        let project_type = detect_project_type(&working_dir);
        let file_count = count_source_files(&working_dir);
        let detected = iris_llm::detect_provider();
        let has_api_key = detected.is_some()
            || iris_llm::load_credentials().is_some();
        let model_name = detected
            .map(|p| p.default_model.to_string())
            .unwrap_or_else(|| "no model".to_string());

        let mut app = Self {
            mode: AppMode::Welcome,
            agent_state: AgentState::Idle,
            input: String::new(),
            cursor_pos: 0,
            chat_history: Vec::new(),
            scroll_offset: 0,
            user_scrolled: false,
            input_history: Vec::new(),
            history_idx: None,
            tick: 0,
            cwd_short: String::new(),
            model_name,
            total_tokens: 0,
            working_dir,
            git_branch,
            project_type,
            file_count,
            has_api_key,
            session_id,
            turn_started_at: None,
            last_max_scroll: 0,
            completion: CompletionState::new(),
            buddy: crate::buddy::load_companion(),
            buddy_reaction: None,
        };
        app.cwd_short = app.working_dir_short();
        app
    }

    // ── Input editing ─────────────────────────────────────────────────────────

    /// Insert `c` at the current cursor position.
    pub fn push_char(&mut self, c: char) {
        let byte_pos = self.char_to_byte(self.cursor_pos);
        self.input.insert(byte_pos, c);
        self.cursor_pos += 1;
        self.mode = AppMode::Chat;
        self.completion.update(&self.input);
    }

    /// Delete the character before the cursor (Backspace).
    pub fn pop_char(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let end_byte = self.char_to_byte(self.cursor_pos);
        let start_byte = self.char_to_byte(self.cursor_pos - 1);
        self.input.drain(start_byte..end_byte);
        self.cursor_pos -= 1;
        if self.input.is_empty() && self.chat_history.is_empty() {
            self.mode = AppMode::Welcome;
        }
        self.completion.update(&self.input);
    }

    /// Move cursor one character to the left.
    pub fn cursor_left(&mut self) {
        self.cursor_pos = self.cursor_pos.saturating_sub(1);
    }

    /// Move cursor one character to the right.
    pub fn cursor_right(&mut self) {
        let len = self.input.chars().count();
        self.cursor_pos = (self.cursor_pos + 1).min(len);
    }

    /// Move cursor to start of input (Ctrl+A).
    pub fn cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    /// Move cursor to end of input (Ctrl+E).
    pub fn cursor_end(&mut self) {
        self.cursor_pos = self.input.chars().count();
    }

    /// Delete word before cursor (Ctrl+W).
    pub fn delete_word_before(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let chars: Vec<char> = self.input.chars().collect();
        let mut pos = self.cursor_pos;
        while pos > 0 && chars[pos - 1] == ' ' {
            pos -= 1;
        }
        while pos > 0 && chars[pos - 1] != ' ' {
            pos -= 1;
        }
        let start_byte = self.char_to_byte(pos);
        let end_byte = self.char_to_byte(self.cursor_pos);
        self.input.drain(start_byte..end_byte);
        self.cursor_pos = pos;
    }

    /// Delete the character after the cursor (Delete key).
    pub fn delete_forward(&mut self) {
        let len = self.input.chars().count();
        if self.cursor_pos >= len {
            return;
        }
        let start_byte = self.char_to_byte(self.cursor_pos);
        let end_byte = self.char_to_byte(self.cursor_pos + 1);
        self.input.drain(start_byte..end_byte);
    }

    /// Kill from cursor to start of line (Ctrl+U).
    pub fn kill_to_start(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let byte_pos = self.char_to_byte(self.cursor_pos);
        self.input.drain(..byte_pos);
        self.cursor_pos = 0;
    }

    /// Kill from cursor to end of line (Ctrl+K).
    pub fn kill_to_end(&mut self) {
        let byte_pos = self.char_to_byte(self.cursor_pos);
        self.input.truncate(byte_pos);
    }

    /// Move cursor one word to the left (Ctrl+Left / Alt+B).
    pub fn cursor_word_left(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let chars: Vec<char> = self.input.chars().collect();
        let mut pos = self.cursor_pos;
        while pos > 0 && chars[pos - 1] == ' ' {
            pos -= 1;
        }
        while pos > 0 && chars[pos - 1] != ' ' {
            pos -= 1;
        }
        self.cursor_pos = pos;
    }

    /// Move cursor one word to the right (Ctrl+Right / Alt+F).
    pub fn cursor_word_right(&mut self) {
        let chars: Vec<char> = self.input.chars().collect();
        let len = chars.len();
        let mut pos = self.cursor_pos;
        while pos < len && chars[pos] != ' ' {
            pos += 1;
        }
        while pos < len && chars[pos] == ' ' {
            pos += 1;
        }
        self.cursor_pos = pos;
    }

    /// Insert a string at the current cursor position (for paste).
    pub fn insert_str(&mut self, s: &str) {
        let byte_pos = self.char_to_byte(self.cursor_pos);
        self.input.insert_str(byte_pos, s);
        self.cursor_pos += s.chars().count();
        self.mode = AppMode::Chat;
    }

    /// Clear the entire input and reset cursor.
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.cursor_pos = 0;
        self.completion.dismiss();
    }

    /// Insert a newline at cursor position (Shift+Enter).
    pub fn insert_newline(&mut self) {
        let byte_pos = self.char_to_byte(self.cursor_pos);
        self.input.insert(byte_pos, '\n');
        self.cursor_pos += 1;
        self.mode = AppMode::Chat;
    }

    /// How many visual lines the current input occupies (accounting for wrap).
    ///
    /// `term_width` is the terminal width for word-wrap estimation.
    pub fn input_line_count_wrapped(&self, term_width: u16) -> u16 {
        // Collapsed long paste — always 1 line.
        let line_count = self.input.lines().count();
        let char_count = self.input.chars().count();
        if line_count > 10 || char_count > 500 {
            return 1;
        }

        // Account for word-wrap per logical line.
        let wrap_width = (term_width.saturating_sub(4)).max(1) as usize; // subtract border + prompt
        let mut total: usize = 0;
        for logical_line in self.input.split('\n') {
            let display_width: usize = logical_line.chars()
                .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(1))
                .sum();
            total += if display_width == 0 { 1 } else { (display_width / wrap_width) + 1 };
        }
        (total.max(1) as u16).min(6)
    }

    /// Height for the input widget (border + lines, max 8).
    pub fn input_height(&self) -> u16 {
        // Use a reasonable default width; actual width is applied during render.
        self.input_line_count_wrapped(80) + 2
    }

    /// Height for the input widget with known terminal width.
    pub fn input_height_for_width(&self, term_width: u16) -> u16 {
        self.input_line_count_wrapped(term_width) + 2
    }

    // ── History navigation ────────────────────────────────────────────────────

    /// Navigate to the previous history entry (Up key).
    pub fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let new_idx = match self.history_idx {
            None => self.input_history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_idx = Some(new_idx);
        self.input = self.input_history[new_idx].clone();
        self.cursor_pos = self.input.chars().count();
    }

    /// Navigate to the next history entry (Down key).
    pub fn history_next(&mut self) {
        match self.history_idx {
            None => {}
            Some(i) if i + 1 >= self.input_history.len() => {
                self.history_idx = None;
                self.input.clear();
                self.cursor_pos = 0;
            }
            Some(i) => {
                self.history_idx = Some(i + 1);
                self.input = self.input_history[i + 1].clone();
                self.cursor_pos = self.input.chars().count();
            }
        }
    }

    // ── Misc ──────────────────────────────────────────────────────────────────

    /// Take the current input, push it to history, reset cursor.
    pub fn take_input(&mut self) -> String {
        let s = std::mem::take(&mut self.input);
        if !s.trim().is_empty() {
            self.input_history.push(s.clone());
        }
        self.cursor_pos = 0;
        self.history_idx = None;
        s
    }

    pub fn push_user(&mut self, text: impl Into<String>) {
        self.chat_history.push(ChatEntry { role: ChatRole::User, content: text.into() });
        self.mode = AppMode::Chat;
        self.agent_state = AgentState::Thinking;
        self.user_scrolled = false;
        self.turn_started_at = Some(Instant::now());
        self.buddy_react(crate::buddy::BuddyEvent::Thinking);
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
        if !self.user_scrolled {
            self.scroll_to_bottom();
        }
    }

    pub fn push_system(&mut self, text: impl Into<String>) {
        self.chat_history.push(ChatEntry { role: ChatRole::System, content: text.into() });
        if !self.user_scrolled {
            self.scroll_to_bottom();
        }
    }

    pub fn finish_response(&mut self, usage: &TokenUsage) {
        self.agent_state = AgentState::Idle;
        self.total_tokens += usage.input_tokens + usage.output_tokens;
        if let Some(started) = self.turn_started_at.take() {
            let secs = started.elapsed().as_secs();
            self.push_system(format!("✦ Responded in {secs}s"));
        }
    }

    /// Try to trigger a buddy reaction for the given event.
    pub fn buddy_react(&mut self, event: crate::buddy::BuddyEvent) {
        let Some(companion) = &self.buddy else { return };
        if let Some(text) = crate::buddy::pick_reaction(event, &companion.bones, self.tick) {
            self.buddy_reaction = Some(ReactionState::new(text));
        }
    }

    /// Tick the buddy reaction timer.
    pub fn buddy_tick(&mut self) {
        if let Some(ref mut r) = self.buddy_reaction {
            r.tick();
            if !r.is_visible() {
                self.buddy_reaction = None;
            }
        }
    }

    pub fn scroll_up(&mut self) {
        if self.scroll_offset > self.last_max_scroll {
            self.scroll_offset = self.last_max_scroll;
        }
        self.scroll_offset = self.scroll_offset.saturating_sub(3);
        self.user_scrolled = true;
    }

    pub fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(3).min(self.last_max_scroll);
        if self.scroll_offset >= self.last_max_scroll {
            self.user_scrolled = false;
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = usize::MAX / 2;
        self.user_scrolled = false;
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

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Convert a char-index to a byte-index in `self.input`.
    fn char_to_byte(&self, char_idx: usize) -> usize {
        self.input
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.input.len())
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
