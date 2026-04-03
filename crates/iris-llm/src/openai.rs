use anyhow::{bail, Result};
use async_stream::stream;
use futures::Stream;
use reqwest::Response;
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;

use crate::sse::parse_openai_sse;
use crate::types::{Message, ModelConfig, StreamEvent, TokenUsage, ToolDefinition};

/// Parse a non-streaming OpenAI-format JSON response into a single-item Stream.
async fn parse_openai_json(response: Response) -> Result<impl Stream<Item = Result<StreamEvent>>> {
    let text = response.text().await?;
    tracing::debug!("Non-streaming JSON body: {text}");
    let v: serde_json::Value = serde_json::from_str(&text)?;

    let content = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    let usage = v.get("usage").and_then(|u| {
        Some(TokenUsage {
            input_tokens: u.get("prompt_tokens")?.as_u64()? as u32,
            output_tokens: u.get("completion_tokens")?.as_u64()? as u32,
        })
    });

    Ok(stream! {
        if !content.is_empty() {
            yield Ok(StreamEvent::TextDelta { text: content });
        }
        if let Some(u) = usage {
            yield Ok(StreamEvent::Usage(u));
        }
        yield Ok(StreamEvent::MessageStop);
    })
}

/// Metadata about a single LLM provider.
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    /// Short identifier, e.g. "deepseek"
    pub name: &'static str,
    /// Environment variable holding the API key, e.g. "DEEPSEEK_API_KEY"
    pub env_key: &'static str,
    /// OpenAI-compatible base URL (empty string → Anthropic native provider)
    pub base_url: &'static str,
    /// Default model to use when none is specified
    pub default_model: &'static str,
    /// Human-readable label (bilingual for Chinese providers)
    pub label: &'static str,
}

/// All 15 supported providers — ported from code-robin providers.py.
pub static PROVIDERS: &[ProviderInfo] = &[
    // --- International ---
    ProviderInfo {
        name: "anthropic",
        env_key: "ANTHROPIC_API_KEY",
        base_url: "",
        default_model: "claude-sonnet-4-6-20250514",
        label: "Anthropic (Claude)",
    },
    ProviderInfo {
        name: "openai",
        env_key: "OPENAI_API_KEY",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o",
        label: "OpenAI (GPT)",
    },
    ProviderInfo {
        name: "google",
        env_key: "GOOGLE_API_KEY",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        default_model: "gemini-2.0-flash",
        label: "Google (Gemini)",
    },
    ProviderInfo {
        name: "groq",
        env_key: "GROQ_API_KEY",
        base_url: "https://api.groq.com/openai/v1",
        default_model: "llama-3.3-70b-versatile",
        label: "Groq",
    },
    ProviderInfo {
        name: "openrouter",
        env_key: "OPENROUTER_API_KEY",
        base_url: "https://openrouter.ai/api/v1",
        default_model: "anthropic/claude-sonnet-4",
        label: "OpenRouter (多模型聚合)",
    },
    // --- China ---
    ProviderInfo {
        name: "deepseek",
        env_key: "DEEPSEEK_API_KEY",
        base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
        label: "DeepSeek (深度求索)",
    },
    ProviderInfo {
        name: "zhipu",
        env_key: "ZHIPU_API_KEY",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        default_model: "glm-4-flash",
        label: "智谱 AI (GLM)",
    },
    ProviderInfo {
        name: "qwen",
        env_key: "DASHSCOPE_API_KEY",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        default_model: "qwen-plus",
        label: "通义千问 (Qwen/百炼)",
    },
    ProviderInfo {
        name: "moonshot",
        env_key: "MOONSHOT_API_KEY",
        base_url: "https://api.moonshot.cn/v1",
        default_model: "moonshot-v1-8k",
        label: "月之暗面 (Kimi)",
    },
    ProviderInfo {
        name: "baichuan",
        env_key: "BAICHUAN_API_KEY",
        base_url: "https://api.baichuan-ai.com/v1",
        default_model: "Baichuan4-Air",
        label: "百川智能",
    },
    ProviderInfo {
        name: "minimax",
        env_key: "MINIMAX_API_KEY",
        base_url: "https://api.minimax.chat/v1",
        default_model: "MiniMax-Text-01",
        label: "MiniMax (稀宇)",
    },
    ProviderInfo {
        name: "yi",
        env_key: "YI_API_KEY",
        base_url: "https://api.lingyiwanwu.com/v1",
        default_model: "yi-lightning",
        label: "零一万物 (Yi)",
    },
    ProviderInfo {
        name: "siliconflow",
        env_key: "SILICONFLOW_API_KEY",
        base_url: "https://api.siliconflow.cn/v1",
        default_model: "deepseek-ai/DeepSeek-V3",
        label: "硅基流动 (SiliconFlow)",
    },
    ProviderInfo {
        name: "stepfun",
        env_key: "STEPFUN_API_KEY",
        base_url: "https://api.stepfun.com/v1",
        default_model: "step-2-16k",
        label: "阶跃星辰 (StepFun)",
    },
    ProviderInfo {
        name: "spark",
        env_key: "SPARK_API_KEY",
        base_url: "https://spark-api-open.xf-yun.com/v1",
        default_model: "generalv3.5",
        label: "讯飞星火 (Spark)",
    },
];

/// Return the first provider that has an API key set in the environment.
pub fn detect_provider() -> Option<&'static ProviderInfo> {
    PROVIDERS.iter().find(|p| {
        std::env::var(p.env_key).map(|v| !v.is_empty()).unwrap_or(false)
    })
}

/// Look up a provider by name.
pub fn get_provider(name: &str) -> Option<&'static ProviderInfo> {
    PROVIDERS.iter().find(|p| p.name == name)
}

/// OpenAI-compatible streaming provider.
///
/// Covers: OpenAI, DeepSeek, Groq, OpenRouter, all Chinese providers.
pub struct OpenAiCompatProvider {
    pub name: String,
    pub api_key: SecretString,
    pub client: reqwest::Client,
    pub base_url: String,
    pub default_model: String,
}

impl OpenAiCompatProvider {
    pub fn new(
        name: impl Into<String>,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        default_model: impl Into<String>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .expect("Failed to build reqwest client with rustls");

        Self {
            name: name.into(),
            api_key: SecretString::new(api_key.into().into()),
            client,
            base_url: base_url.into(),
            default_model: default_model.into(),
        }
    }

    /// Build from a ProviderInfo + API key.
    pub fn from_info(info: &ProviderInfo, api_key: impl Into<String>) -> Self {
        Self::new(info.name, api_key, info.base_url, info.default_model)
    }

    /// Stream a chat completion.
    pub async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &ModelConfig,
    ) -> Result<Box<dyn Stream<Item = Result<StreamEvent>> + Unpin + Send>> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let mut openai_messages: Vec<serde_json::Value> = Vec::new();
        if let Some(ref sys) = config.system_prompt {
            openai_messages.push(json!({"role": "system", "content": sys}));
        }
        for msg in messages {
            match msg.role {
                crate::types::Role::User => {
                    // User messages may contain text or tool_results.
                    // Tool results must be sent as separate "tool" role messages.
                    for block in &msg.content {
                        match block {
                            crate::types::ContentBlock::Text { text } => {
                                openai_messages.push(json!({"role": "user", "content": text}));
                            }
                            crate::types::ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                                let mut m = json!({
                                    "role": "tool",
                                    "tool_call_id": tool_use_id,
                                    "content": content,
                                });
                                if *is_error == Some(true) {
                                    // Some providers support error indication
                                    m["name"] = json!("error");
                                }
                                openai_messages.push(m);
                            }
                            _ => {}
                        }
                    }
                }
                crate::types::Role::Assistant => {
                    // Assistant messages may contain text + tool_use blocks.
                    let mut text_parts: Vec<String> = Vec::new();
                    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
                    for block in &msg.content {
                        match block {
                            crate::types::ContentBlock::Text { text } => {
                                text_parts.push(text.clone());
                            }
                            crate::types::ContentBlock::ToolUse { id, name, input } => {
                                tool_calls.push(json!({
                                    "id": id,
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": input.to_string(),
                                    }
                                }));
                            }
                            _ => {}
                        }
                    }
                    let mut m = json!({"role": "assistant"});
                    let content_str = text_parts.join("\n");
                    if !content_str.is_empty() {
                        m["content"] = json!(content_str);
                    }
                    if !tool_calls.is_empty() {
                        m["tool_calls"] = json!(tool_calls);
                    }
                    openai_messages.push(m);
                }
                crate::types::Role::Tool => {
                    // Legacy path — shouldn't normally be reached
                    let content: String = msg.content.iter().filter_map(|b| match b {
                        crate::types::ContentBlock::Text { text } => Some(text.clone()),
                        crate::types::ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                        _ => None,
                    }).collect::<Vec<_>>().join("\n");
                    openai_messages.push(json!({"role": "assistant", "content": content}));
                }
            }
        }

        // Convert tool definitions to OpenAI function calling format.
        let openai_tools: Vec<serde_json::Value> = tools.iter().map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                }
            })
        }).collect();

        let mut body = json!({
            "model": config.model,
            "max_tokens": config.max_tokens,
            "messages": openai_messages,
            "stream": true,
        });
        if !openai_tools.is_empty() {
            body["tools"] = json!(openai_tools);
            body["tool_choice"] = json!("auto");
        }

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key.expose_secret()))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        tracing::debug!(provider = self.name, %status, %content_type, "LLM response status");
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("Provider {} error {status}: {body}", self.name);
        }

        // Debug mode: dump raw body instead of streaming (set IRIS_DEBUG_BODY=1)
        if std::env::var("IRIS_DEBUG_BODY").as_deref() == Ok("1") {
            let raw = response.text().await.unwrap_or_default();
            eprintln!("[DEBUG BODY] provider={} content-type={}\n{}", self.name, content_type, raw);
            bail!("Debug body dump complete — unset IRIS_DEBUG_BODY to resume normal operation");
        }

        // If content-type is JSON (not event-stream), parse as non-streaming response.
        if content_type.contains("application/json") && !content_type.contains("event-stream") {
            return Ok(Box::new(Box::pin(parse_openai_json(response).await?)));
        }

        Ok(Box::new(Box::pin(parse_openai_sse(response))))
    }
}
