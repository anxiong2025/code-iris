use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::{AgentState, App};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let branch = app.git_branch.as_deref().unwrap_or("no-git");
    let agent_indicator = match app.agent_state {
        AgentState::Idle => "",
        AgentState::Thinking => " * thinking",
        AgentState::Streaming => " * streaming",
    };

    let mut segments: Vec<Span> = vec![
        Span::styled(
            format!(" git:({branch})"),
            Style::default().fg(Color::Rgb(100, 200, 100)),
        ),
        Span::styled("  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            app.cwd_short.as_str(),
            Style::default().fg(Color::Rgb(180, 180, 180)),
        ),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            app.model_name.as_str(),
            Style::default().fg(Color::Rgb(200, 200, 100)),
        ),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}tok", fmt_count(app.total_tokens as usize)),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}files", app.file_count),
            Style::default().fg(Color::Rgb(100, 180, 255)).bold(),
        ),
    ];

    // Show buddy if active.
    if let Some(buddy) = &app.buddy {
        let (r, g, b) = buddy.rarity.color();
        segments.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
        segments.push(Span::styled(
            format!("{} {}", buddy.face, buddy.name),
            Style::default().fg(Color::Rgb(r, g, b)),
        ));
    }

    segments.push(Span::styled(
        agent_indicator,
        Style::default().fg(Color::Rgb(255, 200, 80)).italic(),
    ));

    frame.render_widget(
        Paragraph::new(Line::from(segments))
            .style(Style::default().bg(Color::Rgb(20, 20, 30))),
        area,
    );
}

fn fmt_count(n: usize) -> String {
    if n >= 1_000 { format!("{:.1}k", n as f64 / 1000.0) } else { n.to_string() }
}
