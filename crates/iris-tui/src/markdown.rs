//! Lightweight Markdown → ratatui Lines renderer with syntax highlighting.
//!
//! Handles the most common patterns in LLM output:
//!
//! - `# / ## / ###` headings
//! - `**bold**` and `*italic*`
//! - `` `inline code` ``
//! - ` ```lang\n...\n``` ` fenced code blocks (with syntect highlighting)
//! - `- / * / 1.` list items
//! - `>` blockquotes
//! - `---` horizontal rules
//! - Plain text (with inline span parsing)

use std::sync::LazyLock;

use ratatui::prelude::*;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

// ── Syntect globals (initialised once) ───────────────────────────────────────

static SS: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static TS: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

// ── Colour palette ────────────────────────────────────────────────────────────

const C_HEADING1: Color = Color::Rgb(255, 200, 80);
const C_HEADING2: Color = Color::Rgb(230, 180, 60);
const C_HEADING3: Color = Color::Rgb(200, 160, 50);
const C_CODE_BG: Color = Color::Rgb(30, 32, 40);
const C_CODE_FG: Color = Color::Rgb(180, 230, 180);
const C_CODE_BORDER: Color = Color::Rgb(70, 75, 100);
const C_CODE_LANG: Color = Color::Rgb(130, 140, 200);
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
    let mut code_buf: Vec<String> = Vec::new();

    for raw_line in text.lines() {
        // ── Fenced code block toggle ──────────────────────────────────────────
        if raw_line.starts_with("```") {
            if in_code_block {
                // Flush buffered code block with highlighting.
                lines.extend(render_code_block(&code_lang, &code_buf));
                code_buf.clear();
                code_lang.clear();
                in_code_block = false;
            } else {
                in_code_block = true;
                code_lang = raw_line.trim_start_matches('`').trim().to_string();
            }
            continue;
        }

        if in_code_block {
            code_buf.push(raw_line.to_string());
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

    // Unclosed code block at end of streamed text — flush as-is.
    if in_code_block && !code_buf.is_empty() {
        lines.extend(render_code_block(&code_lang, &code_buf));
    }

    lines
}

// ── Code block renderer ───────────────────────────────────────────────────────

fn render_code_block(lang: &str, code_lines: &[String]) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();

    // Try syntect highlighting.
    let highlighted = try_highlight(lang, code_lines);

    if let Some(hl_lines) = highlighted {
        for hl in hl_lines {
            let mut spans = vec![Span::styled("  │ ", Style::default().fg(C_CODE_BORDER))];
            spans.extend(hl);
            out.push(Line::from(spans));
        }
    } else {
        for line in code_lines {
            out.push(Line::from(vec![
                Span::styled("  │ ", Style::default().fg(C_CODE_BORDER)),
                Span::styled(line.clone(), Style::default().fg(C_CODE_FG)),
            ]));
        }
    }

    out.push(Line::from(""));
    out
}

/// Attempt syntect highlighting. Returns `None` if the language is unknown.
fn try_highlight(lang: &str, code_lines: &[String]) -> Option<Vec<Vec<Span<'static>>>> {
    let syntax = if lang.is_empty() {
        return None;
    } else {
        SS.find_syntax_by_token(lang)?
    };

    let theme = TS.themes.get("base16-ocean.dark")?;
    let mut h = HighlightLines::new(syntax, theme);
    let mut result: Vec<Vec<Span<'static>>> = Vec::new();

    for raw in code_lines {
        let line_with_nl = format!("{raw}\n");
        let ranges = h.highlight_line(&line_with_nl, &SS).ok()?;

        let spans: Vec<Span<'static>> = ranges
            .iter()
            .filter_map(|(style, text)| {
                let t = text.trim_end_matches('\n');
                if t.is_empty() {
                    return None;
                }
                let fg = Color::Rgb(
                    style.foreground.r,
                    style.foreground.g,
                    style.foreground.b,
                );
                Some(Span::styled(
                    t.to_string(),
                    Style::default().fg(fg).bg(C_CODE_BG),
                ))
            })
            .collect();

        result.push(spans);
    }

    Some(result)
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
                spans.push(Span::styled(buf.clone(), Style::default().fg(C_TEXT)));
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
            if i < chars.len() {
                i += 1;
            }
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
            spans.push(Span::styled(bold, Style::default().fg(C_BOLD).bold()));
            if i + 1 < chars.len() {
                i += 2;
            }
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
            spans.push(Span::styled(italic, Style::default().fg(C_ITALIC).italic()));
            if i < chars.len() {
                i += 1;
            }
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
        // header + code line + footer + blank
        assert!(!lines.is_empty());
        let all_text: String = lines.iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
            .collect();
        assert!(all_text.contains("fn main"), "code not found in: {all_text}");
    }

    #[test]
    fn render_code_block_has_header_footer() {
        let lines = render_code_block("rust", &["let x = 1;".to_string()]);
        let all: String = lines.iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
            .collect();
        assert!(all.contains('╭'), "missing header: {all}");
        assert!(all.contains('╰'), "missing footer: {all}");
    }

    #[test]
    fn render_unordered_list() {
        let lines = render_markdown("- item one\n- item two");
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(text.contains('•'), "bullet missing in: {text}");
        }
    }

    #[test]
    fn render_blank_line() {
        let lines = render_markdown("a\n\nb");
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

    #[test]
    fn unclosed_code_block_at_eof() {
        // Streaming text may end without closing fence — should not panic.
        let src = "```rust\nfn foo() {";
        let lines = render_markdown(src);
        assert!(!lines.is_empty());
    }
}
