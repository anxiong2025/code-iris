//! WebFetchTool — fetch a URL and return its text content.
//!
//! HTML responses are stripped to plain text (tags removed, whitespace normalised).
//! JSON/plain-text responses are returned as-is, truncated to `MAX_CHARS`.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use super::Tool;

/// Maximum characters returned per fetch (≈ 50 k tokens).
const MAX_CHARS: usize = 200_000;

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch the content of a URL. Returns plain text stripped of HTML markup. \
         Useful for reading documentation, release notes, or any web page."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch (http:// or https://)"
                },
                "max_length": {
                    "type": "integer",
                    "description": "Maximum characters to return (default 200000)"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let url = input
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field: url"))?;

        let max_len = input
            .get("max_length")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(MAX_CHARS);

        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (compatible; iris/0.1; +https://github.com/anxiong2025/code-iris)")
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let response = client.get(url).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Ok(format!("HTTP {status}: {url}"));
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body = response.text().await?;

        let text = if content_type.contains("text/html") {
            strip_html(&body)
        } else {
            body
        };

        let truncated: String = text.chars().take(max_len).collect();
        let suffix = if text.len() > max_len {
            format!("\n\n[… truncated, {} chars omitted]", text.len() - max_len)
        } else {
            String::new()
        };

        Ok(format!("{truncated}{suffix}"))
    }
}

/// Minimal HTML-to-text: removes tags, decodes common entities, collapses whitespace.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut in_script = false;
    let mut tag_buf = String::new();

    let mut chars = html.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '<' => {
                // Peek at what follows to detect <script> / <style>.
                tag_buf.clear();
                in_tag = true;
            }
            '>' if in_tag => {
                let tag = tag_buf.trim().to_lowercase();
                if tag.starts_with("script") || tag.starts_with("style") {
                    in_script = true;
                } else if tag.starts_with("/script") || tag.starts_with("/style") {
                    in_script = false;
                }
                // Block-level elements add newlines.
                if matches!(
                    tag.split_whitespace().next().unwrap_or(""),
                    "p" | "/p" | "br" | "br/" | "div" | "/div"
                    | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                    | "li" | "tr" | "td" | "th"
                ) {
                    out.push('\n');
                }
                in_tag = false;
            }
            _ if in_tag => tag_buf.push(ch),
            _ if in_script => {}
            '&' => {
                // Collect entity.
                let mut entity = String::new();
                for ec in chars.by_ref() {
                    if ec == ';' { break; }
                    entity.push(ec);
                }
                out.push(decode_entity(&entity));
            }
            _ => out.push(ch),
        }
    }

    // Collapse runs of whitespace / blank lines.
    let mut result = String::with_capacity(out.len());
    let mut prev_blank = false;
    for line in out.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !prev_blank {
                result.push('\n');
            }
            prev_blank = true;
        } else {
            result.push_str(trimmed);
            result.push('\n');
            prev_blank = false;
        }
    }
    result
}

fn decode_entity(entity: &str) -> char {
    match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" | "#39" => '\'',
        "nbsp" | "#160" => ' ',
        e if e.starts_with("#x") => {
            u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32)
                .unwrap_or(' ')
        }
        e if e.starts_with('#') => {
            e[1..].parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .unwrap_or(' ')
        }
        _ => ' ',
    }
}
