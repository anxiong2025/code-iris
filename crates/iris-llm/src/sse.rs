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
                            // Emit ToolUseStart immediately so TUI can show tool name.
                            if !name.is_empty() {
                                yield Ok(StreamEvent::ToolUseStart { name: name.clone() });
                            }
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

/// Accumulator for streamed OpenAI tool calls (arguments arrive in chunks).
struct OaiToolAccum {
    id: String,
    name: String,
    args_buf: String,
}

/// Parse OpenAI-format SSE stream into typed StreamEvents.
///
/// Handles: data: {"choices":[{"delta":{"content":...,"tool_calls":[...]}}]}
pub fn parse_openai_sse(response: Response) -> impl Stream<Item = Result<StreamEvent>> {
    let debug = std::env::var("IRIS_DEBUG_SSE").map(|v| v == "1").unwrap_or(false);
    stream! {
        let mut event_stream = response.bytes_stream().eventsource();
        // Track in-progress tool calls by index.
        let mut tool_accums: Vec<OaiToolAccum> = Vec::new();

        while let Some(event) = futures::StreamExt::next(&mut event_stream).await {
            let event = match event {
                Ok(e) => e,
                Err(e) => {
                    tracing::debug!("SSE parse error: {e}");
                    yield Err(anyhow!("SSE stream error: {e}"));
                    return;
                }
            };

            if debug {
                eprintln!("[SSE] event={:?} data={}", event.event, event.data);
            }
            tracing::debug!(event_type = %event.event, data = %event.data, "SSE event");

            let data = &event.data;
            if data.is_empty() || data == "[DONE]" {
                if data == "[DONE]" {
                    // Flush any pending tool calls.
                    for acc in tool_accums.drain(..) {
                        let input: Value = if acc.args_buf.is_empty() {
                            Value::Object(Default::default())
                        } else {
                            serde_json::from_str(&acc.args_buf)
                                .unwrap_or(Value::String(acc.args_buf))
                        };
                        yield Ok(StreamEvent::ToolUse { id: acc.id, name: acc.name, input });
                    }
                    yield Ok(StreamEvent::MessageStop);
                }
                continue;
            }

            let v: Value = match serde_json::from_str(data) {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!("SSE JSON parse failed: {e}, data={data}");
                    continue;
                }
            };

            if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
                for choice in choices {
                    // Some providers send "message" instead of "delta" in SSE.
                    let delta = choice.get("delta")
                        .or_else(|| choice.get("message"));

                    // Process content and tool calls from delta (if present).
                    if let Some(delta) = delta {
                        // Text content
                        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                            if !content.is_empty() {
                                yield Ok(StreamEvent::TextDelta { text: content.to_string() });
                            }
                        }

                        // Tool calls (streamed in chunks)
                        if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                            for tc in tool_calls {
                                tracing::debug!("tool_call delta chunk: {tc}");
                                let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                                while tool_accums.len() <= idx {
                                    tool_accums.push(OaiToolAccum {
                                        id: String::new(),
                                        name: String::new(),
                                        args_buf: String::new(),
                                    });
                                }
                                if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                                    tool_accums[idx].id = id.to_string();
                                }
                                if let Some(name) = tc.pointer("/function/name").and_then(|n| n.as_str()) {
                                    if !name.is_empty() && tool_accums[idx].name.is_empty() {
                                        tool_accums[idx].name = name.to_string();
                                        yield Ok(StreamEvent::ToolUseStart { name: name.to_string() });
                                    }
                                }
                                if let Some(args) = tc.pointer("/function/arguments").and_then(|a| a.as_str()) {
                                    tool_accums[idx].args_buf.push_str(args);
                                }
                            }
                        }
                    }

                    // finish_reason check — MUST be outside the delta block.
                    // Some providers (e.g. Qwen) send finish_reason in a chunk
                    // without a delta field, so checking inside delta would miss it.
                    let finish = choice.get("finish_reason").and_then(|r| r.as_str());
                    if finish == Some("tool_calls") || finish == Some("function_call") {
                        for acc in tool_accums.drain(..) {
                            let input: Value = if acc.args_buf.is_empty() {
                                Value::Object(Default::default())
                            } else {
                                serde_json::from_str(&acc.args_buf)
                                    .unwrap_or(Value::String(acc.args_buf))
                            };
                            yield Ok(StreamEvent::ToolUse { id: acc.id, name: acc.name, input });
                        }
                        yield Ok(StreamEvent::MessageStop);
                    }
                    if finish == Some("stop") || finish == Some("length") {
                        for acc in tool_accums.drain(..) {
                            let input: Value = if acc.args_buf.is_empty() {
                                Value::Object(Default::default())
                            } else {
                                serde_json::from_str(&acc.args_buf)
                                    .unwrap_or(Value::String(acc.args_buf))
                            };
                            yield Ok(StreamEvent::ToolUse { id: acc.id, name: acc.name, input });
                        }
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
        // SSE stream ended without [DONE] or finish_reason — safety net.
        // Flush any pending tool calls and ensure MessageStop is always sent.
        for acc in tool_accums.drain(..) {
            let input: Value = if acc.args_buf.is_empty() {
                Value::Object(Default::default())
            } else {
                serde_json::from_str(&acc.args_buf)
                    .unwrap_or(Value::String(acc.args_buf))
            };
            yield Ok(StreamEvent::ToolUse { id: acc.id, name: acc.name, input });
        }
        yield Ok(StreamEvent::MessageStop);
    }
}

#[derive(Deserialize)]
struct RawUsage {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
}
