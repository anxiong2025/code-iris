//! iris TUI — terminal UI entry point.
//!
//! Architecture:
//! ```text
//! tokio::main
//!   ├── agent_worker task  (owns Agent, streams AgentEvent back via unbounded channel)
//!   └── run_event_loop     (ratatui + crossterm EventStream + AgentEvent receiver)
//! ```

use std::io;

use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::prelude::*;
use ratatui::layout::Margin;
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use tokio::sync::mpsc;

mod app;
mod input;
mod markdown;
mod statusbar;
mod welcome;

use app::{AgentEvent, AgentState, App, AppMode, ChatRole};
use iris_core::agent::Agent;
use iris_core::config::user_env_path;
use iris_core::context::compress;
use iris_core::memory as iris_memory;
use iris_core::permissions::PermissionMode;
use iris_core::storage::Storage;

/// Commands sent from the TUI event loop to the agent worker.
enum WorkerCmd {
    /// User typed a message — run agent.chat_streaming().
    UserInput(String),
    /// /model <name> — switch model for next turn.
    SetModel(String),
    /// /compact — manually trigger context compression.
    Compact,
    /// /resume <id> — replace current session with a saved one.
    LoadSession(String),
    /// /cd <path> — change working directory for all tool calls.
    SetCwd(std::path::PathBuf),
    /// /cd — reset to process cwd.
    ResetCwd,
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    if let Ok(env_path) = user_env_path() {
        let _ = dotenvy::from_path(env_path);
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(io::stderr)
        .try_init()
        .ok();

    // TUI → agent worker
    let (tx_input, rx_input) = mpsc::channel::<WorkerCmd>(32);
    // agent worker → TUI
    let (tx_events, rx_events) = mpsc::unbounded_channel::<AgentEvent>();

    // Spawn agent worker if any provider key is available.
    let session_id = match Agent::from_env() {
        Ok(agent) => {
            let id = agent.session.id.clone();
            tokio::spawn(agent_worker(agent, rx_input, tx_events));
            Some(id)
        }
        Err(_) => None,
    };

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_event_loop(&mut terminal, tx_input, rx_events, session_id).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = result {
        eprintln!("Error: {err}");
    }
    Ok(())
}

// ── Agent worker ──────────────────────────────────────────────────────────────

async fn agent_worker(
    mut agent: Agent,
    mut rx_input: mpsc::Receiver<WorkerCmd>,
    tx_events: mpsc::UnboundedSender<AgentEvent>,
) {
    agent = agent.with_permissions(PermissionMode::Auto);

    while let Some(cmd) = rx_input.recv().await {
        match cmd {
            WorkerCmd::SetModel(model) => {
                agent = agent.with_model(&model);
                let _ = tx_events.send(AgentEvent::System(format!("Model switched to: {model}")));
            }
            WorkerCmd::SetCwd(path) => {
                *agent.cwd.lock().unwrap() = Some(path.clone());
                let _ = tx_events.send(AgentEvent::System(
                    format!("Working directory: {}", path.display())
                ));
            }
            WorkerCmd::ResetCwd => {
                *agent.cwd.lock().unwrap() = None;
                let _ = tx_events.send(AgentEvent::System("Working directory reset.".to_string()));
            }
            WorkerCmd::LoadSession(id) => {
                match iris_core::storage::Storage::new().and_then(|s| s.load(&id)) {
                    Ok(session) => {
                        let msg_count = session.messages.len();
                        agent.session = session;
                        let _ = tx_events.send(AgentEvent::System(format!(
                            "Resumed session {id} — {msg_count} messages loaded."
                        )));
                    }
                    Err(e) => {
                        let _ = tx_events.send(AgentEvent::Error(format!(
                            "Failed to load session '{id}': {e}"
                        )));
                    }
                }
            }
            WorkerCmd::Compact => {
                let cfg = iris_core::context::ContextConfig::default();
                let changed = compress(&mut agent.session.messages, &cfg);
                let msg = if changed {
                    format!("Compacted — {} messages kept.", agent.session.messages.len())
                } else {
                    "Context is within limits, nothing to compact.".to_string()
                };
                let _ = tx_events.send(AgentEvent::System(msg));
            }
            WorkerCmd::UserInput(user_input) => {
                let tx = tx_events.clone();
                let result = agent
                    .chat_streaming(&user_input, move |chunk| {
                        let _ = tx.send(AgentEvent::TextChunk(chunk.to_string()));
                    })
                    .await;

                match result {
                    Ok(resp) => {
                        for tool in &resp.tool_calls {
                            let _ = tx_events.send(AgentEvent::ToolCall(tool.clone()));
                        }
                        let _ = tx_events.send(AgentEvent::Done {
                            _tool_calls: resp.tool_calls,
                            usage: resp.usage,
                        });
                    }
                    Err(err) => {
                        let _ = tx_events.send(AgentEvent::Error(err.to_string()));
                    }
                }
            }
        }
    }
}

// ── Event loop ────────────────────────────────────────────────────────────────

async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    tx_input: mpsc::Sender<WorkerCmd>,
    mut rx_events: mpsc::UnboundedReceiver<AgentEvent>,
    session_id: Option<String>,
) -> anyhow::Result<()> {
    let mut app = App::new(session_id);
    let mut key_stream = EventStream::new();

    loop {
        terminal.draw(|frame| render(frame, &app))?;

        tokio::select! {
            Some(Ok(event)) = key_stream.next() => {
                if let Event::Key(key) = event {
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
                    {
                        return Ok(());
                    }
                    match key.code {
                        KeyCode::Enter => {
                            let input = app.take_input();
                            if matches!(input.trim(), "exit" | "quit") {
                                return Ok(());
                            }
                            if !input.trim().is_empty() {
                                handle_user_input(&mut app, input, &tx_input).await;
                            }
                        }
                        KeyCode::Char(c) => app.push_char(c),
                        KeyCode::Backspace => app.pop_char(),
                        KeyCode::Up => app.scroll_up(),
                        KeyCode::Down => app.scroll_down(),
                        KeyCode::PageUp => { for _ in 0..10 { app.scroll_up(); } }
                        KeyCode::PageDown => { for _ in 0..10 { app.scroll_down(); } }
                        _ => {}
                    }
                }
            }

            Some(event) = rx_events.recv() => {
                match event {
                    AgentEvent::TextChunk(chunk) => app.append_assistant_chunk(&chunk),
                    AgentEvent::ToolCall(name) => app.push_tool_call(&name),
                    AgentEvent::Done { _tool_calls: _, usage } => app.finish_response(&usage),
                    AgentEvent::System(msg) => app.push_system(msg),
                    AgentEvent::Error(err) => {
                        app.push_system(format!("Error: {err}"));
                        app.agent_state = AgentState::Idle;
                    }
                }
            }
        }
    }
}

async fn handle_user_input(app: &mut App, input: String, tx_input: &mpsc::Sender<WorkerCmd>) {
    // ── Slash commands ────────────────────────────────────────────────────────
    if input.starts_with('/') {
        let cmd = input.trim();
        match cmd {
            "/help" => {
                app.push_system(
                    "/help  /clear  /session  /sessions  /resume <id>\n  \
                     /model [name]  /compact  /commit [msg]  /memory [note]\n  \
                     /cd <path>  /pwd  /worktree <branch>  exit|quit"
                );
            }
            "/clear" => {
                app.chat_history.clear();
                app.scroll_offset = 0;
                app.push_system("Chat history cleared.");
            }
            "/session" => {
                if let Some(id) = &app.session_id {
                    app.push_system(format!("Session: {id}"));
                } else {
                    app.push_system("No active session.");
                }
            }
            "/model" => {
                app.push_system(format!("Current model: {}", app.model_name));
            }
            "/sessions" => {
                match Storage::new().and_then(|s| s.list()) {
                    Ok(ids) if ids.is_empty() => app.push_system("No saved sessions."),
                    Ok(ids) => {
                        let list = ids.join("\n  ");
                        app.push_system(format!("Saved sessions:\n  {list}"));
                    }
                    Err(e) => app.push_system(format!("Error listing sessions: {e}")),
                }
            }
            "/compact" => {
                // Send to worker — it will compress and reply via AgentEvent::System
                if tx_input.send(WorkerCmd::Compact).await.is_err() {
                    app.push_system("Agent worker stopped unexpectedly.");
                }
            }
            "/memory" => {
                match iris_memory::load_notes() {
                    Ok(notes) if notes.is_empty() => app.push_system("No notes saved yet. Use /memory <text> to add one."),
                    Ok(notes) => app.push_system(format!("Notes:\n{notes}")),
                    Err(e) => app.push_system(format!("Error reading notes: {e}")),
                }
            }
            "/pwd" => {
                // Ask the worker for current cwd.
                if tx_input.send(WorkerCmd::ResetCwd).await.is_err() {
                    // If it fails, just show process cwd.
                }
                let cwd = std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "unknown".to_string());
                app.push_system(format!("Process cwd: {cwd}"));
            }
            "/commit" => {
                // git commit with no message — show status
                let output = std::process::Command::new("git")
                    .args(["status", "--short"])
                    .output();
                match output {
                    Ok(o) => {
                        let s = String::from_utf8_lossy(&o.stdout);
                        if s.trim().is_empty() {
                            app.push_system("Nothing to commit (working tree clean).");
                        } else {
                            app.push_system(format!("Staged/unstaged changes:\n{s}\nUse /commit <message> to commit."));
                        }
                    }
                    Err(e) => app.push_system(format!("git error: {e}")),
                }
            }
            _ if cmd.starts_with("/memory ") => {
                let note = cmd.trim_start_matches("/memory ").trim();
                match iris_memory::add_note(note) {
                    Ok(()) => app.push_system(format!("Note saved: {note}")),
                    Err(e) => app.push_system(format!("Error saving note: {e}")),
                }
            }
            _ if cmd.starts_with("/commit ") => {
                let msg = cmd.trim_start_matches("/commit ").trim().to_string();
                let output = std::process::Command::new("git")
                    .args(["add", "-A"])
                    .output()
                    .and_then(|_| {
                        std::process::Command::new("git")
                            .args(["commit", "-m", &msg])
                            .output()
                    });
                match output {
                    Ok(o) => {
                        let stdout = String::from_utf8_lossy(&o.stdout);
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        let out = if !stdout.trim().is_empty() { stdout.as_ref() } else { stderr.as_ref() };
                        app.push_system(format!("git commit:\n{}", out.trim()));
                    }
                    Err(e) => app.push_system(format!("git error: {e}")),
                }
            }
            _ if cmd.starts_with("/cd ") => {
                let path_str = cmd.trim_start_matches("/cd ").trim();
                let path = std::path::PathBuf::from(path_str);
                if path.is_dir() {
                    let abs = path.canonicalize().unwrap_or(path);
                    if tx_input.send(WorkerCmd::SetCwd(abs)).await.is_err() {
                        app.push_system("Agent worker stopped unexpectedly.");
                    }
                } else {
                    app.push_system(format!("Not a directory: {path_str}"));
                }
            }
            "/cd" => {
                if tx_input.send(WorkerCmd::ResetCwd).await.is_err() {
                    app.push_system("Agent worker stopped unexpectedly.");
                }
            }
            _ if cmd.starts_with("/worktree ") => {
                let branch = cmd.trim_start_matches("/worktree ").trim().to_string();
                let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
                let wt_path = home.join(".code-iris").join("worktrees").join(&branch);
                let create = std::process::Command::new("git")
                    .args(["worktree", "add", &wt_path.to_string_lossy(), "-b", &branch])
                    .output();
                match create {
                    Ok(o) if o.status.success() => {
                        if tx_input.send(WorkerCmd::SetCwd(wt_path.clone())).await.is_err() {
                            app.push_system("Agent worker stopped unexpectedly.");
                        } else {
                            app.push_system(format!("Worktree created and cwd set to {}", wt_path.display()));
                        }
                    }
                    Ok(o) => {
                        let e = String::from_utf8_lossy(&o.stderr);
                        app.push_system(format!("git worktree add failed: {}", e.trim()));
                    }
                    Err(e) => app.push_system(format!("git error: {e}")),
                }
            }
            _ if cmd.starts_with("/resume ") => {
                let id = cmd.trim_start_matches("/resume ").trim().to_string();
                if tx_input.send(WorkerCmd::LoadSession(id)).await.is_err() {
                    app.push_system("Agent worker stopped unexpectedly.");
                }
            }
            _ if cmd.starts_with("/model ") => {
                let m = cmd.trim_start_matches("/model ").trim().to_string();
                app.model_name = m.clone();
                // Send to worker so it takes effect on next turn
                if tx_input.send(WorkerCmd::SetModel(m)).await.is_err() {
                    app.push_system("Agent worker stopped unexpectedly.");
                }
            }
            _ => {
                app.push_system(format!("Unknown command: {cmd}  (type /help)"));
            }
        }
        return;
    }

    if !app.has_api_key {
        app.push_system("No API key found — set ANTHROPIC_API_KEY or DASHSCOPE_API_KEY etc.");
        return;
    }
    app.push_user(&input);
    if tx_input.send(WorkerCmd::UserInput(input)).await.is_err() {
        app.push_system("Agent worker stopped unexpectedly.");
        app.agent_state = AgentState::Idle;
    }
}

// ── Render ────────────────────────────────────────────────────────────────────

fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let layout = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);

    match app.mode {
        AppMode::Welcome => welcome::render(frame, layout[0], app),
        AppMode::Chat => render_chat(frame, layout[0], app),
    }
    input::render(frame, layout[1], app);
    statusbar::render(frame, layout[2], app);
}

fn render_chat(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();

    for entry in &app.chat_history {
        match entry.role {
            ChatRole::User => {
                lines.push(Line::from(vec![
                    Span::styled("❯ ", Style::default().fg(Color::Rgb(100, 200, 100)).bold()),
                    Span::styled(entry.content.as_str(), Style::default().fg(Color::White).bold()),
                ]));
            }
            ChatRole::Assistant => {
                lines.push(Line::from(Span::styled(
                    "iris",
                    Style::default().fg(Color::Rgb(255, 140, 60)).bold(),
                )));
                for md_line in markdown::render_markdown(&entry.content) {
                    // indent by 2 spaces
                    let mut indented = vec![Span::raw("  ")];
                    indented.extend(md_line.spans);
                    lines.push(Line::from(indented));
                }
            }
            ChatRole::Tool => {
                lines.push(Line::from(Span::styled(
                    format!("  {}", entry.content),
                    Style::default().fg(Color::Rgb(150, 150, 255)).italic(),
                )));
            }
            ChatRole::System => {
                lines.push(Line::from(Span::styled(
                    format!("  {}", entry.content),
                    Style::default().fg(Color::Rgb(200, 80, 80)).italic(),
                )));
            }
        }
        lines.push(Line::from(""));
    }

    match app.agent_state {
        AgentState::Thinking => {
            lines.push(Line::from(Span::styled(
                "  thinking…",
                Style::default().fg(Color::Rgb(150, 150, 150)).italic(),
            )));
        }
        AgentState::Streaming => {
            lines.push(Line::from(Span::styled(
                "  ▋",
                Style::default().fg(Color::Rgb(255, 140, 60)),
            )));
        }
        AgentState::Idle => {}
    }

    let total_lines = lines.len();
    let visible = area.height.saturating_sub(2) as usize;
    let max_scroll = total_lines.saturating_sub(visible);
    let scroll = app.scroll_offset.min(max_scroll);

    let title = app
        .session_id
        .as_deref()
        .map(|id| format!(" iris · {} ", &id[..8.min(id.len())]))
        .unwrap_or_else(|| " iris ".to_string());

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(255, 140, 60)))
        .title(title)
        .title_style(Style::default().fg(Color::Rgb(255, 140, 60)).bold());

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));

    frame.render_widget(paragraph, area);

    if total_lines > visible {
        let mut scroll_state = ScrollbarState::new(max_scroll).position(scroll);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let inner = area.inner(Margin { horizontal: 1, vertical: 1 });
        frame.render_stateful_widget(scrollbar, inner, &mut scroll_state);
    }
}
