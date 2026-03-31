//! WebSearchTool — search the web via DuckDuckGo Instant Answer API.
//!
//! Uses DuckDuckGo's HTML search (no API key required) as the default backend.
//! Falls back gracefully when results are empty.
//!
//! Result format: numbered list of `title · url\nsummary` blocks, max 10 results.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use super::Tool;

const MAX_RESULTS: usize = 10;
const DDG_URL: &str = "https://html.duckduckgo.com/html/";

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for up-to-date information. Returns a ranked list of results \
         with titles, URLs, and summaries. Use for current events, documentation, \
         library versions, or any information not in your training data."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default 10, max 10)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field: query"))?
            .to_string();

        let max = input
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|v| (v as usize).min(MAX_RESULTS))
            .unwrap_or(MAX_RESULTS);

        search_ddg(&query, max).await
    }
}

async fn search_ddg(query: &str, max: usize) -> Result<String> {
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; iris/0.1)")
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let response = client
        .post(DDG_URL)
        .form(&[("q", query), ("kl", "us-en")])
        .send()
        .await?;

    if !response.status().is_success() {
        return Ok(format!("Search failed: HTTP {}", response.status()));
    }

    let html = response.text().await?;
    let results = parse_ddg_html(&html, max);

    if results.is_empty() {
        return Ok(format!("No results found for: {query}"));
    }

    let mut out = format!("Search results for: {query}\n\n");
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!(
            "{}. {}\n   {}\n   {}\n\n",
            i + 1,
            r.title,
            r.url,
            r.snippet
        ));
    }
    Ok(out.trim_end().to_string())
}

struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

/// Parse DuckDuckGo HTML response — extracts result blocks.
fn parse_ddg_html(html: &str, max: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // Each result is wrapped in <div class="result ..."> ... </div>
    // We do a lightweight scan without a full HTML parser.
    let mut remaining = html;

    while results.len() < max {
        // Find result block start.
        let Some(block_start) = remaining.find("class=\"result__body\"") else { break };
        remaining = &remaining[block_start..];

        // Title: <a class="result__a" href="...">TITLE</a>
        let title = extract_between(remaining, "class=\"result__a\"", "</a>")
            .map(strip_tags)
            .unwrap_or_default();

        // URL: <a class="result__url" href="...">URL</a>
        let url = extract_attr(remaining, "result__url", "href")
            .or_else(|| extract_between(remaining, "class=\"result__url\"", "</a>").map(strip_tags))
            .unwrap_or_default();

        // Snippet: <a class="result__snippet" ...>SNIPPET</a>
        let snippet = extract_between(remaining, "class=\"result__snippet\"", "</a>")
            .map(strip_tags)
            .unwrap_or_default();

        if !title.is_empty() && !url.is_empty() {
            results.push(SearchResult {
                title: title.trim().to_string(),
                url: clean_url(&url),
                snippet: snippet.trim().to_string(),
            });
        }

        // Advance past this block.
        let Some(end) = remaining[1..].find("class=\"result__body\"") else { break };
        remaining = &remaining[end + 1..];
    }

    results
}

fn extract_between<'a>(html: &'a str, start_marker: &str, end_marker: &str) -> Option<&'a str> {
    let start = html.find(start_marker)?;
    let after_start = &html[start + start_marker.len()..];
    // Skip to the end of the opening tag.
    let tag_end = after_start.find('>')?;
    let content = &after_start[tag_end + 1..];
    let end = content.find(end_marker)?;
    Some(&content[..end])
}

fn extract_attr(html: &str, class_name: &str, attr: &str) -> Option<String> {
    let marker = format!("class=\"{class_name}\"");
    let pos = html.find(&marker)?;
    let region = &html[pos.saturating_sub(200)..pos + marker.len() + 300];
    let attr_marker = format!("{attr}=\"");
    let attr_pos = region.find(&attr_marker)?;
    let after = &region[attr_pos + attr_marker.len()..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    // Decode common HTML entities.
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

fn clean_url(url: &str) -> String {
    // DuckDuckGo wraps URLs in redirect links — try to extract the real URL.
    if let Some(pos) = url.find("uddg=") {
        let encoded = &url[pos + 5..];
        let end = encoded.find('&').unwrap_or(encoded.len());
        let decoded = percent_decode(&encoded[..end]);
        if decoded.starts_with("http") {
            return decoded;
        }
    }
    url.trim().to_string()
}

fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte as char);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}
