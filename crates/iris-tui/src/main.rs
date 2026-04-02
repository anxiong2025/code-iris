//! iris TUI — terminal UI entry point.
//!
//! Architecture:
//! ```text
//! tokio::main
//!   ├── agent_worker task  (owns Agent, streams AgentEvent back via unbounded channel)
//!   └── run_event_loop     (ratatui + crossterm EventStream + AgentEvent receiver)
//! ```

use std::io;

use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::prelude::*;
use ratatui::widgets::{
    Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
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
use iris_core::coordinator::{Coordinator, PipelineStep};
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
    /// /plan <prompt> — run three-step pipeline and stream step results.
    Plan(String),
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
    // cancel channel (TUI → agent worker)
    let (tx_cancel, rx_cancel) = mpsc::channel::<()>(4);

    // Spawn agent worker if any provider key is available.
    let session_id = match Agent::from_env() {
        Ok(agent) => {
            let id = agent.session.id.clone();
            tokio::spawn(agent_worker(agent, rx_input, rx_cancel, tx_events));
            Some(id)
        }
        Err(_) => None,
    };

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        crossterm::event::EnableBracketedPaste,
        crossterm::event::EnableMouseCapture,
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_event_loop(&mut terminal, tx_input, tx_cancel, rx_events, session_id).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        crossterm::event::DisableBracketedPaste,
        crossterm::event::DisableMouseCapture,
        LeaveAlternateScreen,
    )?;
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
    mut rx_cancel: mpsc::Receiver<()>,
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
                agent.reload_hooks_and_instructions();
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
                agent.cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                let tx = tx_events.clone();
                tokio::select! {
                    result = agent.chat_streaming(&user_input, move |chunk| {
                        let _ = tx.send(AgentEvent::TextChunk(chunk.to_string()));
                    }) => {
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
                    Some(_) = rx_cancel.recv() => {
                        agent.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        let _ = tx_events.send(AgentEvent::System("Interrupted.".to_string()));
                        let _ = tx_events.send(AgentEvent::Done {
                            _tool_calls: vec![],
                            usage: iris_llm::TokenUsage::default(),
                        });
                    }
                }
            }

            WorkerCmd::Plan(prompt) => {
                let steps = vec![
                    PipelineStep {
                        label: "product".to_string(),
                        agent_type: Some("explorer".to_string()),
                        system_prompt: String::new(),
                        prompt: format!(
                            "Analyse this requirement and output a structured breakdown:\n\
                             - Problem statement\n- Target users\n- Acceptance criteria\n- Constraints\n\n\
                             Requirement: {prompt}"
                        ),
                    },
                    PipelineStep {
                        label: "architecture".to_string(),
                        agent_type: Some("reviewer".to_string()),
                        system_prompt: String::new(),
                        prompt: "Based on the product analysis above, produce a technical architecture plan:\n\
                                 - Component breakdown\n- Interface design\n- Dependencies\n- Risks"
                            .to_string(),
                    },
                    PipelineStep {
                        label: "implementation".to_string(),
                        agent_type: Some("worker".to_string()),
                        system_prompt: String::new(),
                        prompt: "Based on the analysis and architecture above, generate the implementation:\n\
                                 - Write the code for each component\n- Include file paths\n- Add tests\n- Note follow-ups"
                            .to_string(),
                    },
                ];
                let total = steps.len();

                let coord = Coordinator::from_env();

                for (i, step) in steps.iter().enumerate() {
                    let _ = tx_events.send(AgentEvent::PipelineStep {
                        index: i,
                        total,
                        label: step.label.clone(),
                        done: false,
                        text: None,
                    });
                }

                match coord.pipeline_run(steps).await {
                    Ok(results) => {
                        let mut total_in = 0u32;
                        let mut total_out = 0u32;
                        for (i, result) in results.iter().enumerate() {
                            total_in += result.usage.input_tokens;
                            total_out += result.usage.output_tokens;
                            let _ = tx_events.send(AgentEvent::PipelineStep {
                                index: i,
                                total,
                                label: result.label.clone(),
                                done: true,
                                text: Some(result.text.clone()),
                            });
                        }
                        let _ = tx_events.send(AgentEvent::System(
                            format!("Plan complete. tokens in={total_in} out={total_out}")
                        ));
                    }
                    Err(e) => {
                        let _ = tx_events.send(AgentEvent::Error(format!("Pipeline error: {e}")));
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
    tx_cancel: mpsc::Sender<()>,
    mut rx_events: mpsc::UnboundedReceiver<AgentEvent>,
    session_id: Option<String>,
) -> anyhow::Result<()> {
    let mut app = App::new(session_id);
    let mut key_stream = EventStream::new();

    // Timer for animation ticks.
    let mut tick_interval = tokio::time::interval(std::time::Duration::from_millis(200));

    loop {
        terminal.draw(|frame| render(frame, &mut app))?;

        tokio::select! {
            _ = tick_interval.tick() => {
                // Send a tick event through the regular event channel via a local bump.
                app.tick = app.tick.wrapping_add(1);
            }

            Some(Ok(event)) = key_stream.next() => {
                // Bracketed paste — insert pasted text as-is.
                if let Event::Paste(text) = &event {
                    app.insert_str(text);
                    continue;
                }
                // Mouse scroll wheel → chat scroll.
                if let Event::Mouse(mouse) = &event {
                    use crossterm::event::{MouseEventKind};
                    match mouse.kind {
                        MouseEventKind::ScrollUp => app.scroll_up(),
                        MouseEventKind::ScrollDown => app.scroll_down(),
                        _ => {}
                    }
                    continue;
                }
                if let Event::Key(key) = event {
                    // Only process key-press events — on Windows crossterm
                    // fires both Press and Release, causing duplicate input.
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    // Ctrl+D → exit
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('d')
                    {
                        return Ok(());
                    }
                    // Ctrl+C → interrupt current turn, or exit if idle
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                    {
                        if app.agent_state != AgentState::Idle {
                            tx_cancel.try_send(()).ok();
                            app.clear_input();
                            continue;
                        } else {
                            // idle: exit like Ctrl+D
                            return Ok(());
                        }
                    }
                    // Ctrl+W → delete word before cursor
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('w')
                    {
                        app.delete_word_before();
                        continue;
                    }
                    // Ctrl+A → home
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('a')
                    {
                        app.cursor_home();
                        continue;
                    }
                    // Ctrl+E → end
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('e')
                    {
                        app.cursor_end();
                        continue;
                    }
                    // Ctrl+U → kill to start of line
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('u')
                    {
                        app.kill_to_start();
                        continue;
                    }
                    // Ctrl+K → kill to end of line
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('k')
                    {
                        app.kill_to_end();
                        continue;
                    }

                    // ── Completion menu intercepts ──────────────────────────
                    if app.completion.visible {
                        match key.code {
                            KeyCode::Up => { app.completion.select_prev(); continue; }
                            KeyCode::Down => { app.completion.select_next(); continue; }
                            KeyCode::Tab | KeyCode::Enter => {
                                if let Some(label) = app.completion.selected_label() {
                                    let is_enter = key.code == KeyCode::Enter;
                                    match app.completion.kind {
                                        app::CompletionKind::Command => {
                                            let needs_arg = matches!(label,
                                                "/model" | "/commit" | "/cd" | "/resume" |
                                                "/memory" | "/worktree" | "/plan"
                                            );
                                            if needs_arg {
                                                app.input = format!("{} ", label);
                                                app.cursor_pos = app.input.chars().count();
                                                app.completion.update(&app.input);
                                            } else {
                                                // No-arg command: fill and execute on Enter.
                                                app.input = label.to_string();
                                                app.completion.dismiss();
                                                if is_enter {
                                                    let input = app.take_input();
                                                    handle_user_input(&mut app, input, &tx_input).await;
                                                } else {
                                                    app.cursor_pos = app.input.chars().count();
                                                }
                                            }
                                        }
                                        app::CompletionKind::Model => {
                                            app.input = format!("/model {}", label);
                                            app.completion.dismiss();
                                            if is_enter {
                                                let input = app.take_input();
                                                handle_user_input(&mut app, input, &tx_input).await;
                                            } else {
                                                app.cursor_pos = app.input.chars().count();
                                            }
                                        }
                                    }
                                }
                                continue;
                            }
                            KeyCode::Esc => { app.completion.dismiss(); continue; }
                            _ => {} // fall through to normal handling
                        }
                    }

                    match key.code {
                        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                            app.insert_newline();
                        }
                        KeyCode::Enter => {
                            let input = app.take_input();
                            if matches!(input.trim(), "exit" | "quit") {
                                return Ok(());
                            }
                            if !input.trim().is_empty() {
                                handle_user_input(&mut app, input, &tx_input).await;
                            }
                        }
                        KeyCode::Tab => {} // no-op outside completion
                        KeyCode::Char(c) => app.push_char(c),
                        KeyCode::Backspace => app.pop_char(),
                        KeyCode::Delete => app.delete_forward(),
                        KeyCode::Esc => {
                            app.clear_input();
                        }
                        KeyCode::Up => {
                            if !app.input.is_empty() || app.history_idx.is_some() {
                                app.history_prev();
                            } else {
                                app.scroll_up();
                            }
                        }
                        KeyCode::Down => {
                            if app.history_idx.is_some() {
                                app.history_next();
                            } else {
                                app.scroll_down();
                            }
                        }
                        KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL)
                            || key.modifiers.contains(KeyModifiers::ALT) => {
                            app.cursor_word_left();
                        }
                        KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL)
                            || key.modifiers.contains(KeyModifiers::ALT) => {
                            app.cursor_word_right();
                        }
                        KeyCode::Left => app.cursor_left(),
                        KeyCode::Right => app.cursor_right(),
                        KeyCode::Home => app.cursor_home(),
                        KeyCode::End => app.cursor_end(),
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
                        // Extract a human-readable message from JSON errors.
                        let msg = extract_error_message(&err);
                        app.push_system(format!("Error: {msg}"));
                        app.agent_state = AgentState::Idle;
                    }
                    AgentEvent::PipelineStep { index, total, label, done, text } => {
                        let step_label = match label.as_str() {
                            "product" => "Product",
                            "architecture" => "Architecture",
                            "implementation" => "Implementation",
                            other => other,
                        };
                        if !done {
                            app.push_system(format!(
                                "  [{}/{}] * {} ...",
                                index + 1, total, step_label
                            ));
                            app.agent_state = AgentState::Thinking;
                        } else {
                            app.push_system(format!(
                                "  [{}/{}] + {} done",
                                index + 1, total, step_label
                            ));
                            if let Some(t) = text {
                                app.chat_history.push(crate::app::ChatEntry {
                                    role: crate::app::ChatRole::Assistant,
                                    content: t,
                                });
                            }
                            if index + 1 == total {
                                app.agent_state = AgentState::Idle;
                            }
                        }
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
                     /cd <path>  /pwd  /worktree <branch>\n  \
                     /agents                list available agent types\n  \
                     /plan <prompt>         run 3-step product→arch→impl pipeline\n  \
                     exit|quit"
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
                if tx_input.send(WorkerCmd::ResetCwd).await.is_err() {}
                let cwd = std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "unknown".to_string());
                app.push_system(format!("Process cwd: {cwd}"));
            }
            "/commit" => {
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
            "/agents" => {
                use iris_core::agent_def::{builtin_agents, load_custom_agents, SandboxMode};
                let mut lines = vec!["Available agent types:\n".to_string()];
                let custom = load_custom_agents(None);
                if !custom.is_empty() {
                    lines.push("  Custom:".to_string());
                    for def in &custom {
                        let mode = match def.sandbox_mode {
                            SandboxMode::ReadOnly => "read-only",
                            SandboxMode::Full => "full",
                        };
                        let model_hint = def.model.as_deref().unwrap_or("inherit");
                        lines.push(format!("  • {} [{}] ({}) — {}", def.name, mode, model_hint, def.description));
                    }
                    lines.push(String::new());
                }
                lines.push("  Built-in:".to_string());
                for def in builtin_agents() {
                    let mode = match def.sandbox_mode {
                        SandboxMode::ReadOnly => "read-only",
                        SandboxMode::Full => "full",
                    };
                    let model_hint = def.model.as_deref().unwrap_or("inherit");
                    lines.push(format!("  • {} [{}] ({}) — {}", def.name, mode, model_hint, def.description));
                }
                lines.push(String::new());
                lines.push("Use in CLI: iris run --pipeline --sub \"step@explorer:prompt\"".to_string());
                app.push_system(lines.join("\n"));
            }

            "/buddy" => {
                let buddy = app::roll_buddy();
                app.buddy = Some(buddy);
                let (r, g, b) = buddy.rarity.color();
                let rarity_label = buddy.rarity.label();
                app.push_system(format!(
                    "\n  {} {} ({})  [{}] {}\n\n  \"{} is now watching your code.\"\n",
                    buddy.face, buddy.name, buddy.name_cn,
                    rarity_label, buddy.trait_name,
                    buddy.name,
                ));
                let _ = (r, g, b); // used by statusbar rendering
            }

            _ if cmd.starts_with("/plan ") => {
                let prompt = cmd.trim_start_matches("/plan ").trim().to_string();
                if prompt.is_empty() {
                    app.push_system("Usage: /plan <requirement>");
                } else {
                    app.push_user(format!("/plan {prompt}"));
                    if tx_input.send(WorkerCmd::Plan(prompt)).await.is_err() {
                        app.push_system("Agent worker stopped unexpectedly.");
                    }
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

fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let input_h = app.input_height();
    let layout = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(input_h),
        Constraint::Length(1),
    ])
    .split(area);

    match app.mode {
        AppMode::Welcome => welcome::render(frame, layout[0], app),
        AppMode::Chat => render_chat(frame, layout[0], app),
    }
    input::render(frame, layout[1], app);
    statusbar::render(frame, layout[2], app);

    // ── Completion popup (rendered last as overlay) ───────────────────────
    if app.completion.visible && !app.completion.items.is_empty() {
        render_completion(frame, layout[1], app);
    }
}

/// Render completion menu above the input box.
fn render_completion(frame: &mut Frame, input_area: Rect, app: &App) {
    use ratatui::widgets::{Block, Borders, Clear};

    let item_count = app.completion.items.len().min(10) as u16;
    let menu_h = item_count + 2; // +2 for borders

    // Position above the input area.
    let menu_y = input_area.y.saturating_sub(menu_h);
    let menu_w = 60u16.min(input_area.width);
    let menu_area = Rect::new(input_area.x + 2, menu_y, menu_w, menu_h);

    // Clear the area behind the popup.
    frame.render_widget(Clear, menu_area);

    let lines: Vec<Line> = app.completion.items.iter().enumerate().map(|(i, (label, desc))| {
        let is_selected = i == app.completion.selected;
        let (name_style, desc_style) = if is_selected {
            (
                Style::default().fg(Color::White).bg(Color::Rgb(60, 60, 100)).bold(),
                Style::default().fg(Color::Rgb(180, 180, 180)).bg(Color::Rgb(60, 60, 100)),
            )
        } else {
            (
                Style::default().fg(Color::Rgb(100, 200, 100)),
                Style::default().fg(Color::Rgb(120, 120, 120)),
            )
        };
        let padded_name = format!("  {:<28}", label);
        if desc.is_empty() {
            Line::from(Span::styled(padded_name, name_style))
        } else {
            Line::from(vec![
                Span::styled(padded_name, name_style),
                Span::styled(*desc, desc_style),
            ])
        }
    }).collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 80)))
        .style(Style::default().bg(Color::Rgb(25, 25, 40)));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, menu_area);
}

/// Spinner frames for the thinking indicator.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn render_chat(frame: &mut Frame, area: Rect, app: &mut App) {
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
                    let mut indented = vec![Span::raw("  ")];
                    indented.extend(md_line.spans);
                    lines.push(Line::from(indented));
                }
            }
            ChatRole::Tool => {
                // Parse tool name and content preview from "⚙  tool_name" format.
                let raw = entry.content.trim_start_matches('⚙').trim();
                // raw is e.g. "bash" or "bash\narg..." depending on push_tool_call
                let (tool_name, preview) = if let Some(nl) = raw.find('\n') {
                    (&raw[..nl], raw[nl + 1..].trim())
                } else {
                    (raw, "")
                };
                let mut spans = vec![
                    Span::raw("  "),
                    Span::styled("⟩ ", Style::default().fg(Color::Rgb(100, 150, 255))),
                    Span::styled(
                        tool_name.to_string(),
                        Style::default().fg(Color::Rgb(100, 150, 255)).bold(),
                    ),
                ];
                if !preview.is_empty() {
                    let truncated: String = preview.chars().take(80).collect();
                    let ellipsis = if preview.chars().count() > 80 { "…" } else { "" };
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        format!("{truncated}{ellipsis}"),
                        Style::default().fg(Color::Rgb(120, 120, 120)).italic(),
                    ));
                }
                lines.push(Line::from(spans));
            }
            ChatRole::System => {
                lines.push(Line::from(Span::styled(
                    format!("  {}", entry.content),
                    Style::default().fg(Color::Rgb(200, 80, 80)).italic(),
                )));
            }
        }
        // No blank line after tool calls — they stack compactly.
        if entry.role != ChatRole::Tool {
            lines.push(Line::from(""));
        }
    }

    match app.agent_state {
        AgentState::Thinking => {
            let frame_char = SPINNER[app.tick as usize % SPINNER.len()];
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{frame_char} thinking…"),
                    Style::default().fg(Color::Rgb(150, 150, 150)).italic(),
                ),
            ]));
        }
        AgentState::Streaming => {
            let frame_char = SPINNER[app.tick as usize % SPINNER.len()];
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(frame_char, Style::default().fg(Color::Rgb(255, 140, 60))),
                Span::styled(" ▋", Style::default().fg(Color::Rgb(255, 140, 60))),
            ]));
        }
        AgentState::Idle => {}
    }

    let total_lines = lines.len();
    let visible = area.height.saturating_sub(2) as usize;
    let max_scroll = total_lines.saturating_sub(visible);
    app.last_max_scroll = max_scroll;
    let scroll = app.scroll_offset.min(max_scroll);

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));

    frame.render_widget(paragraph, area);

    if total_lines > visible {
        let mut scroll_state = ScrollbarState::new(max_scroll).position(scroll);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, area, &mut scroll_state);
    }
}

/// Extract a human-readable message from API error strings.
///
/// Many providers return JSON with `"message":"..."`.  This pulls out
/// just the message text without requiring serde_json.
fn extract_error_message(raw: &str) -> String {
    // Look for "message":"<text>" pattern in JSON error responses.
    if let Some(start) = raw.find("\"message\":\"") {
        let after = &raw[start + 11..]; // skip `"message":"`
        if let Some(end) = after.find('"') {
            let msg = &after[..end];
            if !msg.is_empty() {
                return msg.to_string();
            }
        }
    }
    // Fallback: use the last meaningful line.
    raw.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(raw)
        .trim()
        .to_string()
}
