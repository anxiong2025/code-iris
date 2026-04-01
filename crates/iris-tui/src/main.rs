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
mod statusbar;
mod welcome;

use app::{AgentEvent, AgentState, App, AppMode, ChatRole};
use iris_core::agent::Agent;
use iris_core::context::compress;
use iris_core::permissions::PermissionMode;

/// Commands sent from the TUI event loop to the agent worker.
enum WorkerCmd {
    /// User typed a message — run agent.chat_streaming().
    UserInput(String),
    /// /model <name> — switch model for next turn.
    SetModel(String),
    /// /compact — manually trigger context compression.
    Compact,
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
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
                app.push_system("/help  /clear  /session  /model [name]  /compact  exit|quit");
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
            "/compact" => {
                // Send to worker — it will compress and reply via AgentEvent::System
                if tx_input.send(WorkerCmd::Compact).await.is_err() {
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
                for line in entry.content.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {line}"),
                        Style::default().fg(Color::Rgb(220, 220, 220)),
                    )));
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
