use anyhow::{bail, Result};
use async_stream::stream;
use futures::Stream;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};

use crate::types::{ContentBlock, Message, ModelConfig, Role, StreamEvent, TokenUsage, ToolDefinition};

/// AWS Bedrock provider using the Converse API with bearer token (ABSK... keys).
pub struct BedrockProvider {
    pub region: String,
    pub model: String,
    pub api_key: SecretString,
    pub client: Client,
}

impl BedrockProvider {
    pub fn new(api_key: impl Into<String>, region: impl Into<String>, model: impl Into<String>) -> Self {
        let client = Client::builder()
            .use_rustls_tls()
            .build()
            .expect("Failed to build reqwest client");
        Self {
            api_key: SecretString::new(api_key.into().into()),
            region: region.into(),
            model: model.into(),
            client,
        }
    }

    pub async fn chat_stream(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        config: &ModelConfig,
    ) -> Result<impl Stream<Item = Result<StreamEvent>>> {
        // Build Bedrock Converse API request
        let bedrock_messages: Vec<Value> = messages.iter().map(|msg| {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "user",
            };
            let text: String = msg.content.iter().filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            }).collect::<Vec<_>>().join("\n");
            json!({
                "role": role,
                "content": [{"text": text}]
            })
        }).collect();

        let mut body = json!({
            "messages": bedrock_messages,
            "inferenceConfig": {
                "maxTokens": config.max_tokens
            }
        });

        if let Some(ref sys) = config.system_prompt {
            body["system"] = json!([{"text": sys}]);
        }

        let model_encoded = urlencoding::encode(&self.model).into_owned();
        let url = format!(
            "https://bedrock-runtime.{}.amazonaws.com/model/{}/converse",
            self.region, model_encoded
        );

        tracing::debug!(%url, model = %self.model, "Bedrock Converse request");

        let response = self.client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key.expose_secret()))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        tracing::debug!(%status, "Bedrock Converse response");

        if !status.is_success() {
            let err_body = response.text().await.unwrap_or_default();
            bail!("Bedrock error {status}: {err_body}");
        }

        let resp: Value = response.json().await?;
        tracing::debug!("Bedrock response JSON: {resp}");

        let text = resp
            .pointer("/output/message/content/0/text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let usage = TokenUsage {
            input_tokens: resp.pointer("/usage/inputTokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            output_tokens: resp.pointer("/usage/outputTokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        };

        Ok(stream! {
            if !text.is_empty() {
                yield Ok(StreamEvent::TextDelta { text });
            }
            yield Ok(StreamEvent::Usage(usage));
            yield Ok(StreamEvent::MessageStop);
        })
    }
}
