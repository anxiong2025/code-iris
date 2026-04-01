//! Google Gemini provider — streamGenerateContent API.
//!
//! Auth: `GOOGLE_API_KEY` passed as `?key=` query parameter.
//! Endpoint: `https://generativelanguage.googleapis.com/v1beta/models/{model}:streamGenerateContent`
//!
//! Response format differs from OpenAI — each SSE chunk is a full JSON object
//! (not delta-encoded), so we diff against the previous text to emit deltas.

use anyhow::Result;
use async_stream::stream;
use futures::Stream;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::retry::RetryPolicy;
use crate::types::{Message, ModelConfig, Role, StreamEvent, ToolDefinition, TokenUsage};

// ── Response types ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    content: Option<GeminiContent>,
    #[serde(rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiContent {
    parts: Option<Vec<GeminiPart>>,
}

#[derive(Debug, Deserialize)]
struct GeminiPart {
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiUsage {
    #[serde(rename = "promptTokenCount", default)]
    prompt_token_count: u32,
    #[serde(rename = "candidatesTokenCount", default)]
    candidates_token_count: u32,
}

// ── Provider ──────────────────────────────────────────────────────────────────

pub struct GoogleProvider {
    api_key: SecretString,
    client: reqwest::Client,
    base_url: String,
    retry: RetryPolicy,
}

impl GoogleProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .expect("failed to build reqwest client");
        Self {
            api_key: SecretString::new(api_key.into().into()),
            client,
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
            retry: RetryPolicy::default(),
        }
    }

    /// Convert our Message list to Gemini `contents` array.
    fn build_contents(messages: &[Message], _config: &ModelConfig) -> Value {
        let mut contents: Vec<Value> = Vec::new();

        // Gemini uses a "system_instruction" field, not a user message.
        // That's handled separately below.
        for msg in messages {
            let role = match msg.role {
                Role::User | Role::Tool => "user",
                Role::Assistant => "model",
            };
            let text: String = msg
                .content
                .iter()
                .filter_map(|b| match b {
                    crate::types::ContentBlock::Text { text } => Some(text.as_str()),
                    crate::types::ContentBlock::ToolResult { content, .. } => {
                        Some(content.as_str())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");

            if !text.is_empty() {
                contents.push(json!({
                    "role": role,
                    "parts": [{"text": text}]
                }));
            }
        }

        // Gemini requires alternating user/model turns; merge consecutive same-role messages.
        let mut merged: Vec<Value> = Vec::new();
        for item in contents {
            let role = item["role"].as_str().unwrap_or("").to_string();
            let text = item["parts"][0]["text"].as_str().unwrap_or("").to_string();
            if let Some(last) = merged.last_mut() {
                if last["role"].as_str() == Some(&role) {
                    let prev = last["parts"][0]["text"].as_str().unwrap_or("").to_string();
                    last["parts"][0]["text"] = json!(format!("{prev}\n{text}"));
                    continue;
                }
            }
            merged.push(item);
        }

        json!(merged)
    }

    pub async fn chat_stream(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        config: &ModelConfig,
    ) -> Result<impl Stream<Item = Result<StreamEvent>>> {
        let url = format!(
            "{}/models/{}:streamGenerateContent?key={}&alt=sse",
            self.base_url,
            config.model,
            self.api_key.expose_secret()
        );

        let contents = Self::build_contents(messages, config);
        let mut body = json!({
            "contents": contents,
            "generationConfig": {
                "maxOutputTokens": config.max_tokens,
            }
        });

        if let Some(ref sys) = config.system_prompt {
            body["system_instruction"] = json!({
                "parts": [{"text": sys}]
            });
        }
        if let Some(temp) = config.temperature {
            body["generationConfig"]["temperature"] = json!(temp);
        }

        // Retry on transient errors.
        let response = self
            .retry
            .run(|| {
                let client = self.client.clone();
                let url = url.clone();
                let body = body.clone();
                async move {
                    let resp = client
                        .post(&url)
                        .header("Content-Type", "application/json")
                        .json(&body)
                        .send()
                        .await
                        .map_err(|e| (0u16, e.to_string()))?;
                    let status = resp.status().as_u16();
                    if !resp.status().is_success() {
                        let msg = resp.text().await.unwrap_or_default();
                        return Err((status, msg));
                    }
                    Ok(resp)
                }
            })
            .await
            .map_err(|e| anyhow::anyhow!("Google API error: {e}"))?;

        // Parse the SSE stream — each `data:` line is a full GeminiResponse JSON.
        let byte_stream = response.bytes_stream();
        let s = stream! {
            use futures::StreamExt;
            use eventsource_stream::Eventsource;

            let mut event_stream = byte_stream.eventsource();
            let mut emitted_text = String::new();

            while let Some(event) = event_stream.next().await {
                let event = match event {
                    Ok(e) => e,
                    Err(e) => { yield Err(anyhow::anyhow!("SSE error: {e}")); break; }
                };

                if event.data == "[DONE]" { break; }
                if event.data.is_empty() { continue; }

                let chunk: GeminiResponse = match serde_json::from_str(&event.data) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                // Emit token usage.
                if let Some(usage) = chunk.usage_metadata {
                    yield Ok(StreamEvent::Usage(TokenUsage {
                        input_tokens: usage.prompt_token_count,
                        output_tokens: usage.candidates_token_count,
                    }));
                }

                for candidate in chunk.candidates.unwrap_or_default() {
                    // Check finish reason.
                    if candidate.finish_reason.as_deref() == Some("STOP") {
                        yield Ok(StreamEvent::MessageStop);
                        return;
                    }
                    // Extract text parts and emit deltas.
                    if let Some(content) = candidate.content {
                        for part in content.parts.unwrap_or_default() {
                            if let Some(full_text) = part.text {
                                // Gemini returns cumulative text, so emit the delta.
                                if full_text.len() > emitted_text.len() {
                                    let delta = full_text[emitted_text.len()..].to_string();
                                    emitted_text = full_text;
                                    yield Ok(StreamEvent::TextDelta { text: delta });
                                }
                            }
                        }
                    }
                }
            }
            yield Ok(StreamEvent::MessageStop);
        };

        Ok(s)
    }
}
