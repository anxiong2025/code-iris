use anyhow::{bail, Result};
use futures::Stream;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};

use crate::sse::parse_anthropic_sse;
use crate::types::{Message, ModelConfig, StreamEvent, ToolDefinition};

pub struct AnthropicProvider {
    pub api_key: SecretString,
    pub client: reqwest::Client,
    pub base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .expect("Failed to build reqwest client with rustls");

        Self {
            api_key: SecretString::new(api_key.into().into()),
            client,
            base_url: "https://api.anthropic.com".to_string(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Stream a chat completion via Anthropic Messages API.
    pub async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &ModelConfig,
    ) -> Result<impl Stream<Item = Result<StreamEvent>>> {
        let url = format!("{}/v1/messages", self.base_url);

        let mut body = json!({
            "model": config.model,
            "max_tokens": config.max_tokens,
            "messages": messages,
            "stream": true,
        });

        if let Some(ref sys) = config.system_prompt {
            body["system"] = json!(sys);
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })).collect::<Vec<Value>>());
        }
        if let Some(temp) = config.temperature {
            body["temperature"] = json!(temp);
        }

        let response = self
            .client
            .post(&url)
            .header("x-api-key", self.api_key.expose_secret())
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Anthropic API error {status}: {body}");
        }

        Ok(parse_anthropic_sse(response))
    }
}
