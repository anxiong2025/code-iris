use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 80)));

    let input_line = Line::from(vec![
        Span::styled("❯ ", Style::default().fg(Color::Rgb(100, 200, 100)).bold()),
        Span::styled(&app.input, Style::default().fg(Color::White)),
        Span::styled("█", Style::default().fg(Color::Rgb(100, 200, 100))),
    ]);

    let input_widget = Paragraph::new(input_line).block(block);
    frame.render_widget(input_widget, area);
}
