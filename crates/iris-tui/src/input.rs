use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthChar;

use crate::app::App;

/// Threshold: if pasted content exceeds this many lines, collapse it.
const COLLAPSE_LINES: usize = 10;
/// Threshold: if pasted content exceeds this many chars, collapse it.
const COLLAPSE_CHARS: usize = 500;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 80)));

    // Detect long paste — collapse to summary.
    let input_lines_count = app.input.lines().count();
    let input_char_count = app.input.chars().count();
    let is_collapsed = input_lines_count > COLLAPSE_LINES || input_char_count > COLLAPSE_CHARS;

    if is_collapsed {
        let summary = format!(
            "({} lines, {} chars pasted)",
            input_lines_count, input_char_count,
        );
        let lines = vec![
            Line::from(vec![
                Span::styled("❯ ", Style::default().fg(Color::Rgb(100, 200, 100)).bold()),
                Span::styled(summary, Style::default().fg(Color::Rgb(180, 180, 180)).italic()),
                Span::styled("█", Style::default().fg(Color::Rgb(100, 200, 100))),
            ]),
        ];
        let input_widget = Paragraph::new(lines).block(block);
        frame.render_widget(input_widget, area);
        return;
    }

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
                let cursor_str = ch.to_string();
                spans.push(Span::styled(
                    cursor_str,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Rgb(100, 200, 100)),
                ));
                let rest_after: String = first_after.chars().skip(1).collect();
                if !rest_after.is_empty() {
                    spans.push(Span::styled(rest_after, Style::default().fg(Color::White)));
                }
                let _ = width;
            } else {
                spans.push(Span::styled(
                    "█",
                    Style::default().fg(Color::Rgb(100, 200, 100)),
                ));
            }

            lines.push(Line::from(spans));

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

    let input_widget = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(input_widget, area);
}
