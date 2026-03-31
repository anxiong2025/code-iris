use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::App;

/// ASCII art logo for Code Iris — an eye/iris symbol
const LOGO: &str = r#"
    ██  ██  ██
  ██░░██░░██░░██
  ██░░░░░░░░░░██
  ██░░██░░██░░██
  ██░░░░░░░░░░██
    ██░░░░░░██
      ██████
"#;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    // Outer container with orange border
    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(255, 140, 60)))
        .title(Line::from(vec![
            Span::styled(" Code Iris ", Style::default().fg(Color::Rgb(255, 140, 60)).bold()),
            Span::styled("v0.1.0 ", Style::default().fg(Color::DarkGray)),
        ]))
        .title_alignment(Alignment::Left);

    let inner = outer_block.inner(area);
    frame.render_widget(outer_block, area);

    // Split into left (welcome + logo) and right (tips + activity)
    let columns = Layout::horizontal([
        Constraint::Percentage(45),
        Constraint::Percentage(55),
    ]).split(inner);

    render_left_panel(frame, columns[0], app);
    render_right_panel(frame, columns[1], app);
}

fn render_left_panel(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(2),   // Welcome text
        Constraint::Length(9),   // Logo
        Constraint::Length(1),   // Spacer
        Constraint::Min(2),     // Info
    ]).split(area);

    // Welcome text
    let welcome = Paragraph::new(Line::from(vec![
        Span::styled("  Welcome!", Style::default().fg(Color::White).bold()),
    ]));
    frame.render_widget(welcome, layout[0]);

    // Logo with color
    let logo_lines: Vec<Line> = LOGO.lines().map(|line| {
        let styled: Vec<Span> = line.chars().map(|c| {
            match c {
                '█' => Span::styled("█", Style::default().fg(Color::Rgb(255, 140, 60))),
                '░' => Span::styled("░", Style::default().fg(Color::Rgb(180, 80, 30))),
                _ => Span::styled(c.to_string(), Style::default()),
            }
        }).collect();
        Line::from(styled)
    }).collect();

    let logo = Paragraph::new(logo_lines).alignment(Alignment::Center);
    frame.render_widget(logo, layout[1]);

    // Provider + path info
    let info_lines = vec![
        Line::from(vec![
            Span::styled(
                format!("  anthropic · {}", app.model_name),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                format!("  {}", app.working_dir_short()),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];
    let info = Paragraph::new(info_lines);
    frame.render_widget(info, layout[3]);
}

fn render_right_panel(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(5),   // Tips
        Constraint::Length(1),   // Divider
        Constraint::Min(3),     // Recent activity
    ]).split(area);

    // Tips section
    let tips = get_contextual_tips(app);
    let tip_lines: Vec<Line> = std::iter::once(
        Line::from(Span::styled(
            " Tips",
            Style::default().fg(Color::Rgb(255, 140, 60)).bold(),
        ))
    ).chain(
        tips.iter().map(|tip| {
            Line::from(Span::styled(
                format!(" {tip}"),
                Style::default().fg(Color::Rgb(180, 180, 180)),
            ))
        })
    ).collect();

    let tips_widget = Paragraph::new(tip_lines).wrap(Wrap { trim: false });
    frame.render_widget(tips_widget, layout[0]);

    // Divider
    let divider = Paragraph::new(Line::from(Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(Color::Rgb(60, 60, 80)),
    )));
    frame.render_widget(divider, layout[1]);

    // Recent activity
    let activity_lines = vec![
        Line::from(Span::styled(
            " Recent activity",
            Style::default().fg(Color::Rgb(255, 140, 60)).bold(),
        )),
        Line::from(Span::styled(
            " No recent activity",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let activity = Paragraph::new(activity_lines);
    frame.render_widget(activity, layout[2]);
}

/// Generate tips based on detected project context (OPT-12)
fn get_contextual_tips(app: &App) -> Vec<String> {
    let mut tips = Vec::new();

    match app.project_type.as_deref() {
        Some("Python") => tips.push("Try `scan .` to analyze your Python architecture".to_string()),
        Some("Rust") => tips.push("Try `scan .` to analyze your Rust crate structure".to_string()),
        Some("Node.js") => tips.push("Try `scan .` to analyze your Node.js project".to_string()),
        Some("Go") => tips.push("Try `scan .` to analyze your Go module".to_string()),
        _ => tips.push("Try `scan .` to analyze the current project".to_string()),
    }

    tips.push("Type `help` for all available commands".to_string());

    if app.file_count > 0 {
        tips.push(format!("Detected {} files in project", app.file_count));
    }

    tips
}
