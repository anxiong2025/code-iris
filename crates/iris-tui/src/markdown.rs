//! Lightweight Markdown → ratatui Lines renderer.
//!
//! Handles the most common patterns in LLM output:
//!
//! - `# / ## / ###` headings
//! - `**bold**` and `*italic*`
//! - `` `inline code` ``
//! - ` ```code blocks``` ` (fenced)
//! - `- / * / 1.` list items
//! - `>` blockquotes
//! - Plain text (with inline span parsing)

use ratatui::prelude::*;

// ── Colour palette ────────────────────────────────────────────────────────────

const C_HEADING1: Color = Color::Rgb(255, 200, 80);
const C_HEADING2: Color = Color::Rgb(230, 180, 60);
const C_HEADING3: Color = Color::Rgb(200, 160, 50);
const C_CODE_BG: Color = Color::Rgb(40, 40, 50);
const C_CODE_FG: Color = Color::Rgb(180, 230, 180);
const C_QUOTE: Color = Color::Rgb(150, 150, 180);
const C_BULLET: Color = Color::Rgb(100, 200, 100);
const C_BOLD: Color = Color::Rgb(255, 255, 255);
const C_ITALIC: Color = Color::Rgb(200, 200, 230);
const C_INLINE_CODE: Color = Color::Rgb(180, 230, 180);
const C_TEXT: Color = Color::Rgb(220, 220, 220);

// ── Public API ────────────────────────────────────────────────────────────────

/// Convert a Markdown string into a list of ratatui [`Line`]s.
pub fn render_markdown(text: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut in_code_block = false;
    let mut code_lang = String::new();

    for raw_line in text.lines() {
        // ── Fenced code block toggle ──────────────────────────────────────────
        if raw_line.starts_with("```") {
            if in_code_block {
                in_code_block = false;
                code_lang.clear();
                // closing fence — add a blank line for breathing room
                lines.push(Line::from(""));
            } else {
                in_code_block = true;
                code_lang = raw_line.trim_start_matches('`').to_string();
            }
            continue;
        }

        if in_code_block {
            lines.push(Line::from(vec![Span::styled(
                format!("  {raw_line}"),
                Style::default().fg(C_CODE_FG).bg(C_CODE_BG),
            )]));
            continue;
        }

        // ── Headings ──────────────────────────────────────────────────────────
        if let Some(t) = raw_line.strip_prefix("### ") {
            lines.push(Line::from(vec![Span::styled(
                format!("  ▸ {t}"),
                Style::default().fg(C_HEADING3).bold(),
            )]));
            continue;
        }
        if let Some(t) = raw_line.strip_prefix("## ") {
            lines.push(Line::from(vec![Span::styled(
                format!("◆ {t}"),
                Style::default().fg(C_HEADING2).bold(),
            )]));
            continue;
        }
        if let Some(t) = raw_line.strip_prefix("# ") {
            lines.push(Line::from(vec![Span::styled(
                format!("◈ {t}"),
                Style::default().fg(C_HEADING1).bold(),
            )]));
            continue;
        }

        // ── Blockquote ────────────────────────────────────────────────────────
        if let Some(t) = raw_line.strip_prefix("> ") {
            let mut spans = vec![Span::styled("│ ", Style::default().fg(C_QUOTE))];
            spans.extend(parse_inline(t));
            lines.push(Line::from(spans));
            continue;
        }

        // ── List items ────────────────────────────────────────────────────────
        let trimmed = raw_line.trim_start();
        let indent = raw_line.len() - trimmed.len();
        let indent_str = " ".repeat(indent);

        if let Some(t) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let mut spans = vec![
                Span::raw(indent_str),
                Span::styled("• ", Style::default().fg(C_BULLET)),
            ];
            spans.extend(parse_inline(t));
            lines.push(Line::from(spans));
            continue;
        }

        // Numbered list: "1. " "2. " etc.
        if let Some(pos) = trimmed.find(". ") {
            if pos < 3 && trimmed[..pos].chars().all(|c| c.is_ascii_digit()) {
                let num = &trimmed[..pos + 1];
                let rest = &trimmed[pos + 2..];
                let mut spans = vec![
                    Span::raw(indent_str),
                    Span::styled(format!("{num} "), Style::default().fg(C_BULLET).bold()),
                ];
                spans.extend(parse_inline(rest));
                lines.push(Line::from(spans));
                continue;
            }
        }

        // ── Horizontal rule ───────────────────────────────────────────────────
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            lines.push(Line::from(Span::styled(
                "─────────────────────────────────────────",
                Style::default().fg(Color::Rgb(80, 80, 80)),
            )));
            continue;
        }

        // ── Blank line ────────────────────────────────────────────────────────
        if raw_line.trim().is_empty() {
            lines.push(Line::from(""));
            continue;
        }

        // ── Plain text with inline spans ──────────────────────────────────────
        lines.push(Line::from(parse_inline(raw_line)));
    }

    lines
}

// ── Inline span parser ────────────────────────────────────────────────────────

/// Parse inline Markdown (`**bold**`, `*italic*`, `` `code` ``) into Spans.
fn parse_inline(text: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut buf = String::new();

    macro_rules! flush {
        () => {
            if !buf.is_empty() {
                spans.push(Span::styled(
                    buf.clone(),
                    Style::default().fg(C_TEXT),
                ));
                buf.clear();
            }
        };
    }

    while i < chars.len() {
        // `` `inline code` ``
        if chars[i] == '`' {
            flush!();
            i += 1;
            let start = i;
            while i < chars.len() && chars[i] != '`' {
                i += 1;
            }
            let code: String = chars[start..i].iter().collect();
            spans.push(Span::styled(
                format!(" {code} "),
                Style::default().fg(C_INLINE_CODE).bg(C_CODE_BG),
            ));
            if i < chars.len() { i += 1; }
            continue;
        }

        // `**bold**`
        if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '*' {
            flush!();
            i += 2;
            let start = i;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '*') {
                i += 1;
            }
            let bold: String = chars[start..i].iter().collect();
            spans.push(Span::styled(
                bold,
                Style::default().fg(C_BOLD).bold(),
            ));
            if i + 1 < chars.len() { i += 2; }
            continue;
        }

        // `*italic*`
        if chars[i] == '*' {
            flush!();
            i += 1;
            let start = i;
            while i < chars.len() && chars[i] != '*' {
                i += 1;
            }
            let italic: String = chars[start..i].iter().collect();
            spans.push(Span::styled(
                italic,
                Style::default().fg(C_ITALIC).italic(),
            ));
            if i < chars.len() { i += 1; }
            continue;
        }

        buf.push(chars[i]);
        i += 1;
    }

    flush!();
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), Style::default().fg(C_TEXT)));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_empty_string() {
        assert!(render_markdown("").is_empty());
    }

    #[test]
    fn render_plain_text_produces_one_line() {
        let lines = render_markdown("hello world");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn render_heading1() {
        let lines = render_markdown("# Title");
        assert_eq!(lines.len(), 1);
        let text = lines[0].spans.iter().map(|s| s.content.as_ref()).collect::<String>();
        assert!(text.contains("Title"), "{text}");
    }

    #[test]
    fn render_fenced_code_block() {
        let src = "```rust\nfn main() {}\n```";
        let lines = render_markdown(src);
        // Opening fence is consumed; code line rendered; closing fence adds blank
        assert!(!lines.is_empty());
        let code_line = lines.iter().find(|l| {
            l.spans.iter().any(|s| s.content.contains("fn main"))
        });
        assert!(code_line.is_some(), "code line not found");
    }

    #[test]
    fn render_unordered_list() {
        let lines = render_markdown("- item one\n- item two");
        assert_eq!(lines.len(), 2);
        // Each line should contain the bullet character
        for line in &lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(text.contains('•'), "bullet missing in: {text}");
        }
    }

    #[test]
    fn render_blank_line() {
        let lines = render_markdown("a\n\nb");
        // a, blank, b
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn render_horizontal_rule() {
        let lines = render_markdown("---");
        assert_eq!(lines.len(), 1);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('─'), "{text}");
    }

    #[test]
    fn parse_inline_bold() {
        let spans = parse_inline("**bold text**");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.trim(), "bold text");
    }

    #[test]
    fn parse_inline_inline_code() {
        let spans = parse_inline("`code`");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("code"), "{text}");
    }

    #[test]
    fn parse_inline_plain_text() {
        let spans = parse_inline("hello world");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "hello world");
    }
}
