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
mod buddy;
mod commands;
mod completion;
mod input;
mod markdown;
mod statusbar;
mod welcome;

use app::{AgentEvent, AgentState, App, AppMode, ChatRole};
use iris_core::agent::Agent;
use iris_core::config::user_env_path;
use iris_core::context::compress;
use iris_core::coordinator::{Coordinator, PipelineStep};
use iris_core::permissions::PermissionMode;

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
                let (actual, switched_provider, error) = agent.switch_model(&model);
                let _ = tx_events.send(AgentEvent::ModelSwitched { actual_model: actual.clone() });
                if let Some(err) = error {
                    let _ = tx_events.send(AgentEvent::Error(err));
                } else {
                    let msg = match switched_provider {
                        Some(provider) => format!("Model switched to: {actual} (provider → {provider})"),
                        None if actual != model => format!("Model switched to: {actual} (mapped from {model})"),
                        None => format!("Model switched to: {model}"),
                    };
                    let _ = tx_events.send(AgentEvent::System(msg));
                }
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
                let tx_tool = tx_events.clone();
                tokio::select! {
                    result = agent.chat_streaming(&user_input, move |chunk| {
                        let _ = tx.send(AgentEvent::TextChunk(chunk.to_string()));
                    }, move |tool_name| {
                        let _ = tx_tool.send(AgentEvent::ToolCall(tool_name.to_string()));
                    }) => {
                        match result {
                            Ok(resp) => {
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

    let mut last_tick = tokio::time::Instant::now();
    let tick_rate = std::time::Duration::from_millis(200);
    // Fixed poll interval — short enough for smooth streaming, long enough to save CPU.
    let poll_interval = std::time::Duration::from_millis(16); // ~60fps

    loop {
        // ── 1. Drain ALL pending agent events ────────────────────────────────
        while let Ok(event) = rx_events.try_recv() {
            handle_agent_event(&mut app, event);
        }

        // ── 2. Animation tick (every 200ms) ──────────────────────────────────
        if last_tick.elapsed() >= tick_rate {
            last_tick = tokio::time::Instant::now();
            app.tick = app.tick.wrapping_add(1);
            app.buddy_tick();
            if app.agent_state == AgentState::Idle
                && app.buddy_reaction.is_none()
                && app.tick % 60 == 0
            {
                app.buddy_react(buddy::BuddyEvent::Idle);
            }
        }

        // ── 3. Draw ──────────────────────────────────────────────────────────
        terminal.draw(|frame| render(frame, &mut app))?;

        // ── 4. Wait for keyboard OR short poll timeout ──────────────────────
        tokio::select! {
            // Poll timeout — guarantees redraw even with no keyboard input.
            _ = tokio::time::sleep(poll_interval) => {}

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
                                        completion::CompletionKind::Command => {
                                            let needs_arg = matches!(label,
                                                "/model" | "/commit" | "/cd" | "/resume" |
                                                "/memory" | "/worktree" | "/plan"
                                            );
                                            if needs_arg {
                                                app.input = format!("{} ", label);
                                                app.cursor_pos = app.input.chars().count();
                                                app.completion.update(&app.input);
                                            } else {
                                                app.input = label.to_string();
                                                app.completion.dismiss();
                                                if is_enter {
                                                    let input = app.take_input();
                                                    commands::handle_user_input(&mut app, input, &tx_input).await;
                                                } else {
                                                    app.cursor_pos = app.input.chars().count();
                                                }
                                            }
                                        }
                                        completion::CompletionKind::Model => {
                                            app.input = format!("/model {}", label);
                                            app.completion.dismiss();
                                            if is_enter {
                                                let input = app.take_input();
                                                commands::handle_user_input(&mut app, input, &tx_input).await;
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
                                commands::handle_user_input(&mut app, input, &tx_input).await;
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

        }
    }
}

fn handle_agent_event(app: &mut App, event: AgentEvent) {
    match event {
        AgentEvent::TextChunk(chunk) => {
            app.append_assistant_chunk(&chunk);
        }
        AgentEvent::ToolCall(name) => {
            app.push_tool_call(&name);
            app.buddy_react(buddy::BuddyEvent::ToolCall);
        }
        AgentEvent::Done { _tool_calls: _, usage } => {
            app.finish_response(&usage);
            app.buddy_react(buddy::BuddyEvent::Done);
        }
        AgentEvent::System(msg) => app.push_system(msg),
        AgentEvent::ModelSwitched { actual_model } => {
            app.model_name = actual_model;
        }
        AgentEvent::Error(err) => {
            let msg = extract_error_message(&err);
            app.push_system(format!("Error: {msg}"));
            app.agent_state = AgentState::Idle;
            app.buddy_react(buddy::BuddyEvent::Error);
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
                    "  [{}/{}] * {} ...", index + 1, total, step_label
                ));
                app.agent_state = AgentState::Thinking;
            } else {
                app.push_system(format!(
                    "  [{}/{}] + {} done", index + 1, total, step_label
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

// ── Render ────────────────────────────────────────────────────────────────────

fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let input_h = app.input_height_for_width(area.width);
    let thinking_h = if app.agent_state != AgentState::Idle { 1 } else { 0 };
    let layout = Layout::vertical([
        Constraint::Min(1),                   // chat area
        Constraint::Length(thinking_h as u16), // thinking indicator (0 when idle)
        Constraint::Length(input_h),           // input box
        Constraint::Length(1),                 // status bar
        Constraint::Length(2),                 // bottom padding
    ])
    .split(area);

    match app.mode {
        AppMode::Welcome => welcome::render(frame, layout[0], app),
        AppMode::Chat => render_chat(frame, layout[0], app),
    }
    // ── Thinking indicator (fixed above input) ───────────────────────
    if app.agent_state != AgentState::Idle {
        render_thinking(frame, layout[1], app);
    }
    input::render(frame, layout[2], app);
    statusbar::render(frame, layout[3], app);

    // ── Buddy sprite overlay (bottom-right corner of chat) ───────────
    if app.buddy.is_some() {
        render_buddy_sprite(frame, layout[0], app);
    }

    // ── Completion popup (rendered last as overlay) ───────────────────
    if app.completion.visible && !app.completion.items.is_empty() {
        render_completion(frame, layout[2], app);
    }
}

/// Render completion menu above the input box.
fn render_completion(frame: &mut Frame, input_area: Rect, app: &App) {
    use ratatui::widgets::{Block, Borders, Clear};

    let item_count = app.completion.items.len().min(10) as u16;
    let menu_h = item_count + 2; // +2 for borders

    let menu_y = input_area.y.saturating_sub(menu_h);
    let menu_w = 60u16.min(input_area.width);
    let menu_area = Rect::new(input_area.x + 2, menu_y, menu_w, menu_h);

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

/// Render buddy sprite as ambient overlay in the bottom-right corner,
/// with name label below and optional speech bubble to the left.
/// Only renders when idle — hides during streaming to avoid text overlap.
fn render_buddy_sprite(frame: &mut Frame, chat_area: Rect, app: &App) {
    use ratatui::widgets::Clear;

    let Some(companion) = &app.buddy else { return };

    let sprite_lines = buddy::render_sprite(&companion.bones, app.tick);
    let sprite_h = sprite_lines.len() as u16;
    let sprite_w: u16 = sprite_lines.iter()
        .map(|l| l.len() as u16)
        .max()
        .unwrap_or(12)
        + 2; // minimal padding

    let name_line = 1u16;
    let total_h = sprite_h + name_line;

    // Position: bottom-right of chat area, with margin.
    let x = chat_area.right().saturating_sub(sprite_w + 1);
    let y = chat_area.bottom().saturating_sub(total_h + 1);

    if sprite_w > chat_area.width / 3 || total_h + 2 > chat_area.height {
        return;
    }

    let (r, g, b) = companion.bones.rarity.color();
    let style = Style::default().fg(Color::Rgb(r, g, b));
    let dim_style = Style::default().fg(Color::Rgb(r / 2, g / 2, b / 2));

    // ── Sprite ───────────────────────────────────────────────────────────────
    let sprite_area = Rect::new(x, y, sprite_w, sprite_h);
    frame.render_widget(Clear, sprite_area);

    let lines: Vec<Line> = sprite_lines.iter()
        .map(|l| Line::from(Span::styled(format!(" {l}"), style)))
        .collect();
    frame.render_widget(Paragraph::new(lines), sprite_area);

    // ── Name label ───────────────────────────────────────────────────────────
    let name = &companion.soul.name;
    let name_w = (name.chars().count() as u16 + 2).min(sprite_w);
    let name_x = x + (sprite_w.saturating_sub(name_w)) / 2;
    let name_area = Rect::new(name_x, y + sprite_h, name_w, 1);
    frame.render_widget(Clear, name_area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            name.to_string(),
            dim_style.italic(),
        )).alignment(Alignment::Center)),
        name_area,
    );

    // ── Speech bubble (if reaction active) ───────────────────────────────────
    if let Some(reaction) = &app.buddy_reaction {
        if reaction.is_visible() {
            let text = &reaction.text;
            let bubble_inner_w = text.chars().count().min(24);
            let bubble_w = bubble_inner_w as u16 + 4;
            let bubble_h = 3u16;

            let bx = x.saturating_sub(bubble_w + 1);
            let by = y + (sprite_h.saturating_sub(bubble_h)) / 2;

            if bx >= chat_area.x && by + bubble_h <= chat_area.bottom() {
                let bubble_area = Rect::new(bx, by, bubble_w, bubble_h);
                frame.render_widget(Clear, bubble_area);

                let bubble_style = if reaction.is_fading() {
                    Style::default().fg(Color::Rgb(60, 60, 60))
                } else {
                    Style::default().fg(Color::Rgb(140, 140, 110))
                };

                let truncated: String = text.chars().take(bubble_inner_w).collect();
                let inner_w = bubble_w as usize - 4;
                let top = format!("╭{}╮", "─".repeat(inner_w + 2));
                let mid = format!("│ {:<w$} │", truncated, w = inner_w);
                let bot = format!("╰{}╯", "─".repeat(inner_w + 2));

                let bubble_lines = vec![
                    Line::from(Span::styled(top, bubble_style)),
                    Line::from(Span::styled(mid, bubble_style)),
                    Line::from(Span::styled(bot, bubble_style)),
                ];
                frame.render_widget(Paragraph::new(bubble_lines), bubble_area);
            }
        }
    }
}

/// Fun rotating verbs for the thinking indicator — like Claude Code.
const THINKING_VERBS: &[&str] = &[
    "Thinking", "Pondering", "Contemplating", "Reasoning",
    "Gesticulating", "Cogitating", "Ruminating", "Deliberating",
    "Cooking", "Brewing", "Conjuring", "Composing",
];

/// Render the thinking/streaming indicator as a fixed line above input.
fn render_thinking(frame: &mut Frame, area: Rect, app: &App) {
    let elapsed = app.turn_started_at
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);

    // Pick verb based on turn hash so it stays stable within a turn.
    let verb_seed = app.turn_started_at
        .map(|t| t.elapsed().as_millis() as usize / 10000)
        .unwrap_or(0);
    let verb = THINKING_VERBS[verb_seed.wrapping_mul(7) % THINKING_VERBS.len()];

    let star = if app.tick % 2 == 0 { "✦" } else { "✱" };
    let color = Style::default().fg(Color::Rgb(230, 130, 60));
    let dim = Style::default().fg(Color::Rgb(100, 100, 110));

    let mut spans = vec![
        Span::styled(format!("{star} "), color),
        Span::styled(format!("{verb}…"), color),
    ];

    if elapsed > 0 {
        spans.push(Span::styled(format!(" ({elapsed}s)"), dim));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_chat(frame: &mut Frame, area: Rect, app: &mut App) {
    let mut lines: Vec<Line> = Vec::new();
    let mut prev_role: Option<ChatRole> = None;

    for entry in &app.chat_history {
        match entry.role {
            ChatRole::User => {
                lines.push(Line::from(vec![
                    Span::styled("❯ ", Style::default().fg(Color::Rgb(100, 200, 100)).bold()),
                    Span::styled(entry.content.as_str(), Style::default().fg(Color::White).bold()),
                ]));
            }
            ChatRole::Assistant => {
                // Only show "iris" header if this is the first assistant message
                // in a turn (not after a tool call in the same turn).
                let show_header = prev_role.as_ref() != Some(&ChatRole::Assistant)
                    && prev_role.as_ref() != Some(&ChatRole::Tool);
                if show_header {
                    lines.push(Line::from(Span::styled(
                        "iris",
                        Style::default().fg(Color::Rgb(255, 140, 60)).bold(),
                    )));
                }
                for md_line in markdown::render_markdown(&entry.content) {
                    let mut indented = vec![Span::raw("  ")];
                    indented.extend(md_line.spans);
                    lines.push(Line::from(indented));
                }
            }
            ChatRole::Tool => {
                let raw = entry.content.trim_start_matches('⚙').trim();
                let (tool_name, preview) = if let Some(nl) = raw.find('\n') {
                    (&raw[..nl], raw[nl + 1..].trim())
                } else {
                    (raw, "")
                };
                // Dim, compact tool indicator — "thinking" process, not the answer.
                let dim = Style::default().fg(Color::Rgb(90, 90, 100));
                let mut spans = vec![
                    Span::styled("  ⎿ ", dim),
                    Span::styled(tool_name.to_string(), dim.bold()),
                ];
                if !preview.is_empty() {
                    let truncated: String = preview.chars().take(60).collect();
                    let ellipsis = if preview.chars().count() > 60 { "…" } else { "" };
                    spans.push(Span::styled(
                        format!(" {truncated}{ellipsis}"),
                        dim.italic(),
                    ));
                }
                lines.push(Line::from(spans));
            }
            ChatRole::System => {
                for sys_line in entry.content.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {sys_line}"),
                        Style::default().fg(Color::Rgb(200, 80, 80)).italic(),
                    )));
                }
            }
        }
        prev_role = Some(entry.role.clone());
        // Minimal spacing: only add a blank line after Assistant blocks.
        if entry.role == ChatRole::Assistant {
            lines.push(Line::from(""));
        }
    }

    // Use ratatui's exact line_count for accurate scroll (requires unstable feature).
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let total_lines = paragraph.line_count(area.width);
    let visible = area.height as usize;
    let max_scroll = total_lines.saturating_sub(visible);
    app.last_max_scroll = max_scroll;

    // Auto-scroll: stick to bottom unless user manually scrolled up.
    let scroll = if !app.user_scrolled {
        max_scroll
    } else {
        app.scroll_offset.min(max_scroll)
    };

    let paragraph = paragraph.scroll((scroll as u16, 0));
    frame.render_widget(paragraph, area);

    if total_lines > visible {
        let mut scroll_state = ScrollbarState::new(max_scroll).position(scroll);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, area, &mut scroll_state);
    }
}

/// Extract a human-readable message from API error strings.
fn extract_error_message(raw: &str) -> String {
    if let Some(start) = raw.find("\"message\":\"") {
        let after = &raw[start + 11..];
        if let Some(end) = after.find('"') {
            let msg = &after[..end];
            if !msg.is_empty() {
                return msg.to_string();
            }
        }
    }
    raw.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(raw)
        .trim()
        .to_string()
}
