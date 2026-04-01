//! Anthropic provider — streaming Messages API with retry + OAuth support.
//!
//! Auth priority:
//! 1. OAuth `Bearer` token (from `~/.code-iris/credentials.json`) — no API key needed
//! 2. `x-api-key` header (classic `ANTHROPIC_API_KEY`)

use anyhow::Result;
use futures::Stream;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};

use crate::oauth::{load_credentials, refresh_token, OAuthTokenSet};
use crate::retry::RetryPolicy;
use crate::sse::parse_anthropic_sse;
use crate::types::{Message, ModelConfig, StreamEvent, ToolDefinition};

// ── Auth source ───────────────────────────────────────────────────────────────

/// How the provider authenticates with the Anthropic API.
pub enum AuthSource {
    /// Classic API key (`ANTHROPIC_API_KEY`).
    ApiKey(SecretString),
    /// OAuth Bearer token (claude.ai login, no API key required).
    OAuth(OAuthTokenSet),
}

impl AuthSource {
    /// Resolve from environment: OAuth credentials file first, then API key env var.
    pub fn from_env() -> Option<Self> {
        if let Some(tokens) = load_credentials() {
            return Some(Self::OAuth(tokens));
        }
        let key = std::env::var("ANTHROPIC_API_KEY").ok()?;
        if key.trim().is_empty() { return None; }
        Some(Self::ApiKey(SecretString::new(key.into())))
    }
}

// ── Provider ──────────────────────────────────────────────────────────────────

pub struct AnthropicProvider {
    auth: AuthSource,
    pub client: reqwest::Client,
    pub base_url: String,
    pub retry: RetryPolicy,
}

impl AnthropicProvider {
    /// Create with an explicit API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_auth(AuthSource::ApiKey(SecretString::new(api_key.into().into())))
    }

    /// Create from environment (OAuth first, then API key).
    pub fn from_env() -> Option<Self> {
        AuthSource::from_env().map(Self::with_auth)
    }

    fn with_auth(auth: AuthSource) -> Self {
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .expect("Failed to build reqwest client with rustls");
        Self {
            auth,
            client,
            base_url: "https://api.anthropic.com".to_string(),
            retry: RetryPolicy::default(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    /// Returns true if the provider is using OAuth (no API key required).
    pub fn is_oauth(&self) -> bool {
        matches!(self.auth, AuthSource::OAuth(_))
    }

    /// Ensure the OAuth token is fresh, refreshing if needed.
    async fn ensure_fresh_token(&mut self) -> Result<()> {
        if let AuthSource::OAuth(ref tokens) = self.auth {
            if tokens.is_expired() {
                tracing::info!("OAuth token expired, refreshing…");
                let new_tokens = refresh_token(&self.client, tokens).await?;
                crate::oauth::save_credentials(&new_tokens)?;
                self.auth = AuthSource::OAuth(new_tokens);
            }
        }
        Ok(())
    }


    /// Stream a chat completion via Anthropic Messages API (with retry).
    pub async fn chat_stream(
        &mut self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &ModelConfig,
    ) -> Result<impl Stream<Item = Result<StreamEvent>>> {
        self.ensure_fresh_token().await?;

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

        // Retry loop — only non-streaming status check benefits from retry;
        // once we have a 200 response we stream directly.
        let policy = self.retry.clone();
        let client = self.client.clone();
        let auth_source = self.auth.clone();

        let response = policy
            .run(|| {
                let client = client.clone();
                let auth = auth_source.clone();
                let url = url.clone();
                let body = body.clone();
                async move {
                    let rb = client
                        .post(&url)
                        .header("content-type", "application/json")
                        .json(&body);
                    let rb = match &auth {
                        AuthSource::ApiKey(key) => rb
                            .header("x-api-key", key.expose_secret())
                            .header("anthropic-version", "2023-06-01"),
                        AuthSource::OAuth(tokens) => rb
                            .header("Authorization", format!("Bearer {}", tokens.access_token))
                            .header("anthropic-version", "2023-06-01"),
                    };
                    let resp = rb.send().await.map_err(|e| (0u16, e.to_string()))?;
                    let status = resp.status().as_u16();
                    if !resp.status().is_success() {
                        let body = resp.text().await.unwrap_or_default();
                        return Err((status, body));
                    }
                    Ok(resp)
                }
            })
            .await
            .map_err(|e| anyhow::anyhow!("Anthropic API error: {e}"))?;

        Ok(parse_anthropic_sse(response))
    }
}

// Make AuthSource cloneable for the retry closure.
impl Clone for AuthSource {
    fn clone(&self) -> Self {
        match self {
            Self::ApiKey(k) => Self::ApiKey(SecretString::new(k.expose_secret().to_string().into())),
            Self::OAuth(t) => Self::OAuth(t.clone()),
        }
    }
}
