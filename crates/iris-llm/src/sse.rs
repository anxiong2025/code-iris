use anyhow::{anyhow, Result};
use async_stream::stream;
use eventsource_stream::Eventsource;
use futures::Stream;
use reqwest::Response;
use serde::Deserialize;
use serde_json::Value;

use crate::types::{StreamEvent, TokenUsage};

/// Parse Anthropic-format SSE stream into typed StreamEvents.
///
/// Handles: message_start, content_block_start, content_block_delta,
/// content_block_stop, message_delta, message_stop.
pub fn parse_anthropic_sse(response: Response) -> impl Stream<Item = Result<StreamEvent>> {
    // State for accumulating tool_use blocks
    struct ToolAccum {
        id: String,
        name: String,
        json_buf: String,
    }

    stream! {
        let mut event_stream = response.bytes_stream().eventsource();
        let mut tool_accum: Option<ToolAccum> = None;

        while let Some(event) = futures::StreamExt::next(&mut event_stream).await {
            let event = match event {
                Ok(e) => e,
                Err(e) => {
                    yield Err(anyhow!("SSE stream error: {e}"));
                    return;
                }
            };

            if event.event == "message_stop" {
                yield Ok(StreamEvent::MessageStop);
                return;
            }

            let data = &event.data;
            if data.is_empty() || data == "[DONE]" {
                continue;
            }

            let v: Value = match serde_json::from_str(data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            match event.event.as_str() {
                "message_start" => {
                    if let Some(usage) = v.pointer("/message/usage") {
                        if let Ok(u) = serde_json::from_value::<RawUsage>(usage.clone()) {
                            yield Ok(StreamEvent::Usage(TokenUsage {
                                input_tokens: u.input_tokens.unwrap_or(0),
                                output_tokens: u.output_tokens.unwrap_or(0),
                            }));
                        }
                    }
                }
                "content_block_start" => {
                    if let Some(block) = v.get("content_block") {
                        let t = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        if t == "tool_use" {
                            let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            tool_accum = Some(ToolAccum { id, name, json_buf: String::new() });
                        }
                    }
                }
                "content_block_delta" => {
                    if let Some(delta) = v.get("delta") {
                        let dtype = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match dtype {
                            "text_delta" => {
                                let text = delta.get("text").and_then(|t| t.as_str()).unwrap_or("");
                                if !text.is_empty() {
                                    yield Ok(StreamEvent::TextDelta { text: text.to_string() });
                                }
                            }
                            "thinking_delta" => {
                                let thinking = delta.get("thinking").and_then(|t| t.as_str()).unwrap_or("");
                                if !thinking.is_empty() {
                                    yield Ok(StreamEvent::ThinkingDelta { thinking: thinking.to_string() });
                                }
                            }
                            "input_json_delta" => {
                                let partial = delta.get("partial_json").and_then(|v| v.as_str()).unwrap_or("");
                                if let Some(ref mut acc) = tool_accum {
                                    acc.json_buf.push_str(partial);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "content_block_stop" => {
                    if let Some(acc) = tool_accum.take() {
                        let input: Value = if acc.json_buf.is_empty() {
                            Value::Object(Default::default())
                        } else {
                            serde_json::from_str(&acc.json_buf)
                                .unwrap_or(Value::String(acc.json_buf))
                        };
                        yield Ok(StreamEvent::ToolUse {
                            id: acc.id,
                            name: acc.name,
                            input,
                        });
                    }
                }
                "message_delta" => {
                    if let Some(usage) = v.get("usage") {
                        if let Ok(u) = serde_json::from_value::<RawUsage>(usage.clone()) {
                            yield Ok(StreamEvent::Usage(TokenUsage {
                                input_tokens: u.input_tokens.unwrap_or(0),
                                output_tokens: u.output_tokens.unwrap_or(0),
                            }));
                        }
                    }
                }
                _ => {}
            }
        }
        yield Ok(StreamEvent::MessageStop);
    }
}

/// Parse OpenAI-format SSE stream into typed StreamEvents.
///
/// Handles: data: {"choices":[{"delta":{"content":...}}]}
pub fn parse_openai_sse(response: Response) -> impl Stream<Item = Result<StreamEvent>> {
    stream! {
        let mut event_stream = response.bytes_stream().eventsource();

        while let Some(event) = futures::StreamExt::next(&mut event_stream).await {
            let event = match event {
                Ok(e) => e,
                Err(e) => {
                    yield Err(anyhow!("SSE stream error: {e}"));
                    return;
                }
            };

            let data = &event.data;
            if data.is_empty() || data == "[DONE]" {
                if data == "[DONE]" {
                    yield Ok(StreamEvent::MessageStop);
                }
                continue;
            }

            let v: Value = match serde_json::from_str(data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
                for choice in choices {
                    if let Some(delta) = choice.get("delta") {
                        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                            if !content.is_empty() {
                                yield Ok(StreamEvent::TextDelta { text: content.to_string() });
                            }
                        }
                    }
                    // finish_reason == "stop"
                    if choice.get("finish_reason").and_then(|r| r.as_str()) == Some("stop") {
                        yield Ok(StreamEvent::MessageStop);
                    }
                }
            }

            if let Some(usage) = v.get("usage") {
                if let Ok(u) = serde_json::from_value::<RawUsage>(usage.clone()) {
                    yield Ok(StreamEvent::Usage(TokenUsage {
                        input_tokens: u.input_tokens.unwrap_or(0),
                        output_tokens: u.output_tokens.unwrap_or(0),
                    }));
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct RawUsage {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
}
