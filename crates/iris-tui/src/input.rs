use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::app::App;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 80)));

    let chars: Vec<char> = app.input.chars().collect();
    let cursor = app.cursor_pos.min(chars.len());
    let before: String = chars[..cursor].iter().collect();
    let after: String = chars[cursor..].iter().collect();

    let before_lines: Vec<&str> = before.split('\n').collect();
    let after_lines: Vec<&str> = after.split('\n').collect();

    let mut lines: Vec<Line> = Vec::new();

    let total_before = before_lines.len();
    let _total_after = after_lines.len();

    for (i, bl) in before_lines.iter().enumerate() {
        let is_last_before = i + 1 == total_before;
        if is_last_before {
            let first_after = after_lines.first().copied().unwrap_or("");
            let mut spans = Vec::new();
            if i == 0 {
                spans.push(Span::styled(
                    "❯ ",
                    Style::default().fg(Color::Rgb(100, 200, 100)).bold(),
                ));
            } else {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(*bl, Style::default().fg(Color::White)));

            // Render cursor block — must cover the correct display width.
            let next_char = first_after.chars().next();
            if let Some(ch) = next_char {
                let width = ch.width().unwrap_or(1);
                // Cursor highlight over the next character.
                let cursor_str = ch.to_string();
                spans.push(Span::styled(
                    cursor_str,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Rgb(100, 200, 100)),
                ));
                // Rest of the first after-line.
                let rest_after: String = first_after.chars().skip(1).collect();
                if !rest_after.is_empty() {
                    spans.push(Span::styled(rest_after, Style::default().fg(Color::White)));
                }
                let _ = width; // width is handled correctly by ratatui's Span rendering
            } else {
                // Cursor at end — show a block cursor.
                // Use a full-width block if the previous char was CJK for visual consistency.
                spans.push(Span::styled(
                    "█",
                    Style::default().fg(Color::Rgb(100, 200, 100)),
                ));
            }

            lines.push(Line::from(spans));

            // Remaining after-lines (multi-line input).
            for al in after_lines.iter().skip(1) {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(*al, Style::default().fg(Color::White)),
                ]));
            }
        } else {
            let prefix = if i == 0 {
                Span::styled("❯ ", Style::default().fg(Color::Rgb(100, 200, 100)).bold())
            } else {
                Span::raw("  ")
            };
            lines.push(Line::from(vec![
                prefix,
                Span::styled(*bl, Style::default().fg(Color::White)),
            ]));
        }
    }

    let input_widget = Paragraph::new(lines).block(block);
    frame.render_widget(input_widget, area);
}
