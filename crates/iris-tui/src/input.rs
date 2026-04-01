use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 80)));

    // Build multi-line content with an inline cursor block.
    // Split at cursor_pos to insert the cursor character.
    let chars: Vec<char> = app.input.chars().collect();
    let cursor = app.cursor_pos.min(chars.len());
    let before: String = chars[..cursor].iter().collect();
    let after: String = chars[cursor..].iter().collect();

    // Split before/after on newlines to build ratatui Lines.
    let before_lines: Vec<&str> = before.split('\n').collect();
    let after_lines: Vec<&str> = after.split('\n').collect();

    let mut lines: Vec<Line> = Vec::new();

    let total_before = before_lines.len();
    let total_after = after_lines.len();

    for (i, bl) in before_lines.iter().enumerate() {
        let is_last_before = i + 1 == total_before;
        if is_last_before {
            // This line also contains the cursor and start of `after`.
            let first_after = after_lines.first().copied().unwrap_or("");
            let mut spans = Vec::new();
            if i == 0 {
                // First line: prepend the prompt arrow.
                spans.push(Span::styled(
                    "❯ ",
                    Style::default().fg(Color::Rgb(100, 200, 100)).bold(),
                ));
            } else {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(*bl, Style::default().fg(Color::White)));
            // Cursor block
            let cursor_char = if first_after.is_empty() && total_after == 1 {
                // cursor at end of last line
                " "
            } else {
                let next_char = first_after.chars().next().unwrap_or(' ');
                // We'll render the cursor as a highlighted block over next char.
                // We handle this below with two spans.
                let _ = next_char;
                ""
            };
            if cursor_char == " " {
                // cursor at very end
                spans.push(Span::styled(
                    "█",
                    Style::default().fg(Color::Rgb(100, 200, 100)),
                ));
            } else {
                // cursor sits on top of first char of `after`
                let next_char = first_after.chars().next().unwrap_or(' ').to_string();
                let rest_after: String = first_after.chars().skip(1).collect();
                spans.push(Span::styled(
                    next_char,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Rgb(100, 200, 100)),
                ));
                spans.push(Span::styled(rest_after, Style::default().fg(Color::White)));
            }

            // Remaining after_lines (if multi-line in after part)
            lines.push(Line::from(spans));

            for (j, al) in after_lines.iter().skip(1).enumerate() {
                let prefix = if j == 0 { "  " } else { "  " };
                lines.push(Line::from(vec![
                    Span::raw(prefix),
                    Span::styled(*al, Style::default().fg(Color::White)),
                ]));
            }
        } else {
            // Lines entirely in `before` (no cursor).
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
