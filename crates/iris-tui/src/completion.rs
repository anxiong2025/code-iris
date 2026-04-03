//! Slash command and model name completion.

// ── Slash command definitions ────────────────────────────────────────────────

pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
}

pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand { name: "/help",      description: "Show available commands" },
    SlashCommand { name: "/clear",     description: "Clear chat history" },
    SlashCommand { name: "/model",     description: "Switch or show current model" },
    SlashCommand { name: "/compact",   description: "Compress context to save tokens" },
    SlashCommand { name: "/commit",    description: "Git commit with message" },
    SlashCommand { name: "/plan",      description: "Run product→arch→impl pipeline" },
    SlashCommand { name: "/cd",        description: "Change working directory" },
    SlashCommand { name: "/pwd",       description: "Show current working directory" },
    SlashCommand { name: "/session",   description: "Show current session ID" },
    SlashCommand { name: "/sessions",  description: "List saved sessions" },
    SlashCommand { name: "/resume",    description: "Resume a saved session" },
    SlashCommand { name: "/memory",    description: "View or add memory notes" },
    SlashCommand { name: "/worktree",  description: "Create git worktree and switch" },
    SlashCommand { name: "/agents",    description: "List available agent types" },
    SlashCommand { name: "/buddy",     description: "Summon your coding buddy" },
    SlashCommand { name: "/init",      description: "Scan project and generate instructions" },
    SlashCommand { name: "/skills",    description: "List available skills" },
];

/// Common model names for completion (grouped by provider).
pub const KNOWN_MODELS: &[&str] = &[
    // Qwen / DashScope
    "qwen-plus", "qwen-max", "qwen-turbo", "qwen-long",
    "qwen2.5-72b-instruct", "qwen2.5-32b-instruct", "qwen2.5-14b-instruct",
    // Anthropic
    "claude-sonnet-4-6-20250514", "claude-opus-4-6", "claude-haiku-4-5-20251001",
    // OpenAI
    "gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "o1", "o1-mini", "o3-mini",
    // DeepSeek
    "deepseek-chat", "deepseek-reasoner",
    // Google
    "gemini-2.0-flash", "gemini-2.0-pro", "gemini-1.5-pro",
    // Groq
    "llama-3.3-70b-versatile", "mixtral-8x7b-32768",
    // Moonshot
    "moonshot-v1-8k", "moonshot-v1-32k", "moonshot-v1-128k",
    // Zhipu
    "glm-4-flash", "glm-4-plus", "glm-4",
];

// ── Completion state ─────────────────────────────────────────────────────────

pub struct CompletionState {
    /// Filtered items: (label, description).
    pub items: Vec<(&'static str, &'static str)>,
    /// Currently highlighted index within `items`.
    pub selected: usize,
    /// Whether the menu is visible.
    pub visible: bool,
    /// What kind of completion is active.
    pub kind: CompletionKind,
}

#[derive(Clone, PartialEq)]
pub enum CompletionKind {
    Command,
    Model,
}

impl CompletionState {
    pub fn new() -> Self {
        Self { items: Vec::new(), selected: 0, visible: false, kind: CompletionKind::Command }
    }

    /// Update completion list based on current input.
    pub fn update(&mut self, input: &str) {
        if !input.starts_with('/') {
            self.visible = false;
            self.items.clear();
            return;
        }

        // Model name completion: "/model <partial>"
        if input.starts_with("/model ") {
            let partial = input.trim_start_matches("/model ").trim();
            self.kind = CompletionKind::Model;
            self.items = KNOWN_MODELS
                .iter()
                .filter(|m| partial.is_empty() || m.starts_with(partial))
                .map(|m| (*m, ""))
                .collect();
            self.visible = !self.items.is_empty();
            if self.selected >= self.items.len() {
                self.selected = 0;
            }
            return;
        }

        // Don't show command completion if there's a space (other commands with args).
        if input.contains(' ') {
            self.visible = false;
            self.items.clear();
            return;
        }

        // Slash command completion.
        self.kind = CompletionKind::Command;
        let partial = input;
        self.items = SLASH_COMMANDS
            .iter()
            .filter(|cmd| cmd.name.starts_with(partial) || partial == "/")
            .map(|cmd| (cmd.name, cmd.description))
            .collect();
        self.visible = !self.items.is_empty();
        if self.selected >= self.items.len() {
            self.selected = 0;
        }
    }

    pub fn select_prev(&mut self) {
        if !self.items.is_empty() {
            self.selected = self.selected.checked_sub(1).unwrap_or(self.items.len() - 1);
        }
    }

    pub fn select_next(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + 1) % self.items.len();
        }
    }

    /// Return the currently selected item label.
    pub fn selected_label(&self) -> Option<&'static str> {
        self.items.get(self.selected).map(|(label, _)| *label)
    }

    pub fn dismiss(&mut self) {
        self.visible = false;
        self.items.clear();
    }
}
