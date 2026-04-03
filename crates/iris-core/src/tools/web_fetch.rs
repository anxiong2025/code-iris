//! WebFetchTool — fetch a URL and return clean Markdown content.
//!
//! Primary: Jina Reader (`r.jina.ai`) — handles JS rendering, returns Markdown.
//! Fallback: raw `reqwest` + HTML tag stripping.
//!
//! Features:
//! - `prompt` parameter for targeted content extraction
//! - 15-minute in-memory cache (same URL won't be re-fetched)
//! - Smart truncation: when content is large and a prompt is provided,
//!   extract the most relevant sections instead of blind truncation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use super::Tool;

/// Maximum characters returned per fetch.
const MAX_CHARS: usize = 200_000;
/// Cache entries expire after 15 minutes.
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
/// Above this threshold, apply smart extraction when a prompt is provided.
const EXTRACTION_THRESHOLD: usize = 50_000;

struct CacheEntry {
    markdown: String,
    fetched_at: Instant,
}

pub struct WebFetchTool {
    cache: Arc<Mutex<HashMap<String, CacheEntry>>>,
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a URL and return its content as clean Markdown. Handles JavaScript-rendered \
         pages. Provide a `prompt` to describe what information you need — this helps \
         extract the most relevant sections from large pages.\n\n\
         Use for: reading documentation, API references, release notes, blog posts, \
         GitHub issues, or any web page."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch (http:// or https://)"
                },
                "prompt": {
                    "type": "string",
                    "description": "What information to extract from the page (e.g. 'find the API for creating users'). When provided, large pages will be intelligently trimmed to the most relevant sections."
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

        let prompt = input.get("prompt").and_then(|v| v.as_str()).unwrap_or("");

        let max_len = input
            .get("max_length")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(MAX_CHARS);

        // ── Cache check ──────────────────────────────────────────────────────
        {
            let mut cache = self.cache.lock().unwrap();
            // Evict expired entries.
            cache.retain(|_, entry| entry.fetched_at.elapsed() < CACHE_TTL);

            if let Some(entry) = cache.get(url) {
                tracing::debug!(url, "web_fetch cache hit");
                let content = maybe_extract(&entry.markdown, prompt, max_len);
                return Ok(content);
            }
        }

        // ── Fetch ────────────────────────────────────────────────────────────
        let markdown = match fetch_with_jina(url).await {
            Ok(md) => md,
            Err(e) => {
                tracing::debug!(url, error = %e, "Jina Reader failed, falling back to raw fetch");
                fetch_raw(url).await?
            }
        };

        // ── Cache store ──────────────────────────────────────────────────────
        {
            let mut cache = self.cache.lock().unwrap();
            cache.insert(url.to_string(), CacheEntry {
                markdown: markdown.clone(),
                fetched_at: Instant::now(),
            });
        }

        Ok(maybe_extract(&markdown, prompt, max_len))
    }
}

// ── Jina Reader ──────────────────────────────────────────────────────────────

/// Fetch a URL via Jina Reader, which handles JS rendering and returns Markdown.
async fn fetch_with_jina(url: &str) -> Result<String> {
    let jina_url = format!("https://r.jina.ai/{url}");

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; iris/0.1; +https://github.com/anxiong2025/code-iris)")
        .timeout(Duration::from_secs(30))
        .build()?;

    let response = client
        .get(&jina_url)
        .header("Accept", "text/markdown")
        .header("X-No-Cache", "true")
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("Jina Reader HTTP {status}");
    }

    let body = response.text().await?;
    if body.trim().is_empty() {
        anyhow::bail!("Jina Reader returned empty content");
    }

    Ok(body)
}

// ── Raw fallback ─────────────────────────────────────────────────────────────

/// Direct HTTP fetch with basic HTML-to-text stripping.
async fn fetch_raw(url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; iris/0.1; +https://github.com/anxiong2025/code-iris)")
        .timeout(Duration::from_secs(30))
        .build()?;

    let response = client.get(url).send().await?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {status}: {url}");
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body = response.text().await?;

    if content_type.contains("text/html") {
        Ok(strip_html(&body))
    } else {
        Ok(body)
    }
}

// ── Smart extraction ─────────────────────────────────────────────────────────

/// When content is large and a prompt is provided, extract the most relevant
/// sections based on keyword matching. Otherwise, simple truncation.
fn maybe_extract(content: &str, prompt: &str, max_len: usize) -> String {
    // Short content — return as-is.
    if content.len() <= max_len {
        return content.to_string();
    }

    // No prompt or short content — simple truncation.
    if prompt.is_empty() || content.len() <= EXTRACTION_THRESHOLD {
        return truncate(content, max_len);
    }

    // Smart extraction: score each section by keyword relevance.
    let keywords = extract_keywords(prompt);
    let sections = split_sections(content);

    if sections.len() <= 1 {
        return truncate(content, max_len);
    }

    // Score sections by keyword density.
    let mut scored: Vec<(usize, f32, &str)> = sections
        .iter()
        .enumerate()
        .map(|(i, section)| {
            let lower = section.to_lowercase();
            let score: f32 = keywords
                .iter()
                .map(|kw| lower.matches(kw).count() as f32)
                .sum();
            // Boost early sections (usually contain overview/summary).
            let position_boost = if i < 3 { 2.0 } else { 1.0 };
            (i, score * position_boost, *section)
        })
        .collect();

    // Sort by score descending, but keep original order for top sections.
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Take top sections until we hit max_len, preserving original order.
    let mut selected: Vec<(usize, &str)> = Vec::new();
    let mut total_len = 0;
    for (idx, _score, section) in &scored {
        if total_len + section.len() > max_len {
            // Add a partial last section if there's room.
            let remaining = max_len - total_len;
            if remaining > 200 {
                let partial: String = section.chars().take(remaining).collect();
                selected.push((*idx, Box::leak(partial.into_boxed_str())));
            }
            break;
        }
        selected.push((*idx, section));
        total_len += section.len();
    }

    // Re-sort by original position.
    selected.sort_by_key(|(idx, _)| *idx);

    let mut result: String = selected.iter().map(|(_, s)| *s).collect::<Vec<_>>().join("\n\n---\n\n");

    if total_len < content.len() {
        result.push_str(&format!(
            "\n\n[… {}/{} chars shown, filtered by relevance to: \"{}\"]",
            total_len,
            content.len(),
            prompt
        ));
    }

    result
}

/// Split content into sections by Markdown headings or double newlines.
fn split_sections(content: &str) -> Vec<&str> {
    let mut sections = Vec::new();
    let mut last = 0;

    for (i, _) in content.match_indices("\n## ") {
        if i > last {
            let section = content[last..i].trim();
            if !section.is_empty() {
                sections.push(section);
            }
        }
        last = i;
    }

    if last < content.len() {
        let section = content[last..].trim();
        if !section.is_empty() {
            sections.push(section);
        }
    }

    // If no headings found, split by double newlines into larger chunks.
    if sections.len() <= 1 {
        sections = content
            .split("\n\n\n")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
    }

    sections
}

/// Extract lowercase keywords from a prompt, filtering stop words.
fn extract_keywords(prompt: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "could",
        "should", "may", "might", "can", "shall", "to", "of", "in", "for",
        "on", "with", "at", "by", "from", "as", "into", "about", "between",
        "through", "after", "before", "and", "but", "or", "not", "no", "if",
        "then", "than", "that", "this", "it", "what", "which", "who", "how",
        "find", "get", "show", "me", "i", "you", "we", "they", "my", "your",
        "extract", "look", "search", "want", "need", "page", "information",
    ];

    prompt
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2 && !STOP_WORDS.contains(w))
        .map(String::from)
        .collect()
}

fn truncate(content: &str, max_len: usize) -> String {
    let truncated: String = content.chars().take(max_len).collect();
    if content.len() > max_len {
        format!(
            "{truncated}\n\n[… truncated, {} chars omitted]",
            content.len() - max_len
        )
    } else {
        truncated
    }
}

// ── HTML stripping (fallback) ────────────────────────────────────────────────

fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut in_script = false;
    let mut tag_buf = String::new();

    let mut chars = html.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '<' => {
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
