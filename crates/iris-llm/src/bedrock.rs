use anyhow::{bail, Result};
use async_stream::stream;
use futures::Stream;
use hmac::{Hmac, Mac};
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::types::{ContentBlock, Message, ModelConfig, Role, StreamEvent, TokenUsage, ToolDefinition};

type HmacSha256 = Hmac<Sha256>;

/// Authentication method for AWS Bedrock.
pub enum BedrockAuth {
    /// Bearer token (e.g. `AWS_BEARER_TOKEN_BEDROCK` / ABSK keys).
    Bearer(SecretString),
    /// Standard IAM credentials (`AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY`, optional session token).
    SigV4 {
        access_key_id: String,
        secret_access_key: SecretString,
        session_token: Option<SecretString>,
    },
}

/// AWS Bedrock provider using the **InvokeModel** API (Anthropic Messages format).
///
/// Supports two auth modes:
/// 1. Bearer token (`AWS_BEARER_TOKEN_BEDROCK` — ABSK keys from Claude Code)
/// 2. Standard IAM SigV4 (`AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY`)
pub struct BedrockProvider {
    pub region: String,
    pub model: String,
    auth: BedrockAuth,
    pub client: Client,
}

impl BedrockProvider {
    /// Create with bearer token auth.
    pub fn new(api_key: impl Into<String>, region: impl Into<String>, model: impl Into<String>) -> Self {
        let client = Client::builder()
            .use_rustls_tls()
            .build()
            .expect("Failed to build reqwest client");
        Self {
            auth: BedrockAuth::Bearer(SecretString::new(api_key.into().into())),
            region: region.into(),
            model: model.into(),
            client,
        }
    }

    /// Create with standard IAM credentials (SigV4).
    pub fn with_iam(
        access_key_id: impl Into<String>,
        secret_access_key: impl Into<String>,
        session_token: Option<String>,
        region: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let client = Client::builder()
            .use_rustls_tls()
            .build()
            .expect("Failed to build reqwest client");
        Self {
            auth: BedrockAuth::SigV4 {
                access_key_id: access_key_id.into(),
                secret_access_key: SecretString::new(secret_access_key.into().into()),
                session_token: session_token.map(|t| SecretString::new(t.into())),
            },
            region: region.into(),
            model: model.into(),
            client,
        }
    }

    /// Detect from environment: tries bearer token, then env vars, then AWS config files.
    pub fn from_env(region: String, model: String) -> Option<Self> {
        // 1. Bearer token
        if let Ok(token) = std::env::var("AWS_BEARER_TOKEN_BEDROCK") {
            if !token.is_empty() {
                return Some(Self::new(token, region, model));
            }
        }
        // 2. Environment variables
        if let (Some(ak), Some(sk)) = (
            std::env::var("AWS_ACCESS_KEY_ID").ok().filter(|s| !s.is_empty()),
            std::env::var("AWS_SECRET_ACCESS_KEY").ok().filter(|s| !s.is_empty()),
        ) {
            let session_token = std::env::var("AWS_SESSION_TOKEN").ok().filter(|s| !s.is_empty());
            return Some(Self::with_iam(ak, sk, session_token, region, model));
        }
        // 3. AWS config files (~/.aws/credentials, ~/.aws/config)
        let profile = std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".to_string());
        if let Some((ak, sk, token, file_region)) = read_aws_credentials(&profile) {
            let region = if region == "us-west-2" {
                file_region.unwrap_or(region)
            } else {
                region
            };
            return Some(Self::with_iam(ak, sk, token, region, model));
        }
        None
    }

    pub async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &ModelConfig,
    ) -> Result<impl Stream<Item = Result<StreamEvent>>> {
        // Build Anthropic Messages API body for Bedrock InvokeModel.
        let mut anthropic_messages: Vec<Value> = Vec::new();
        for msg in messages {
            let role = match msg.role {
                Role::User | Role::Tool => "user",
                Role::Assistant => "assistant",
            };
            let content: Vec<Value> = msg.content.iter().filter_map(|b| match b {
                ContentBlock::Text { text } => Some(json!({"type": "text", "text": text})),
                ContentBlock::ToolUse { id, name, input } => {
                    let safe_id = sanitize_tool_id(id);
                    Some(json!({"type": "tool_use", "id": safe_id, "name": name, "input": input}))
                }
                ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                    let safe_id = sanitize_tool_id(tool_use_id);
                    let mut r = json!({
                        "type": "tool_result",
                        "tool_use_id": safe_id,
                        "content": content,
                    });
                    if *is_error == Some(true) {
                        r["is_error"] = json!(true);
                    }
                    Some(r)
                }
                _ => None,
            }).collect();
            if !content.is_empty() {
                anthropic_messages.push(json!({"role": role, "content": content}));
            }
        }

        // Merge consecutive messages with the same role (Anthropic API requires alternation).
        let anthropic_messages = merge_consecutive_roles(anthropic_messages);

        let mut body = json!({
            "anthropic_version": "bedrock-2023-05-31",
            "max_tokens": config.max_tokens,
            "messages": anthropic_messages,
        });
        if let Some(ref sys) = config.system_prompt {
            body["system"] = json!(sys);
        }
        // Tool definitions in Anthropic format.
        if !tools.is_empty() {
            let tool_defs: Vec<Value> = tools.iter().map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            }).collect();
            body["tools"] = json!(tool_defs);
        }

        let model_encoded = urlencoding::encode(&config.model).into_owned();
        let url = format!(
            "https://bedrock-runtime.{}.amazonaws.com/model/{}/invoke",
            self.region, model_encoded
        );

        tracing::debug!(%url, model = %config.model, "Bedrock InvokeModel request");

        let body_bytes = serde_json::to_vec(&body)?;

        let request = match &self.auth {
            BedrockAuth::Bearer(token) => {
                self.client
                    .post(&url)
                    .header("Authorization", format!("Bearer {}", token.expose_secret()))
                    .header("Content-Type", "application/json")
                    .body(body_bytes)
            }
            BedrockAuth::SigV4 { access_key_id, secret_access_key, session_token } => {
                let body_json = serde_json::to_vec(&body)?;
                let headers = sign_v4(
                    "POST",
                    &url,
                    &body_json,
                    &self.region,
                    "bedrock",
                    access_key_id,
                    secret_access_key.expose_secret(),
                    session_token.as_ref().map(|t| t.expose_secret().as_ref()),
                )?;
                let mut req = self.client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .body(body_json);
                for (k, v) in &headers {
                    req = req.header(k.as_str(), v.as_str());
                }
                req
            }
        };

        let response = request.send().await?;
        let status = response.status();
        tracing::debug!(%status, "Bedrock InvokeModel response");

        if !status.is_success() {
            let err_body = response.text().await.unwrap_or_default();
            bail!("Bedrock error {status}: {err_body}");
        }

        // Anthropic Messages API response format.
        let resp: Value = response.json().await?;
        tracing::debug!("Bedrock response: {resp}");

        // Parse content blocks: text + tool_use.
        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_uses: Vec<(String, String, Value)> = Vec::new();

        if let Some(content) = resp.get("content").and_then(|c| c.as_array()) {
            for block in content {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                            if !t.is_empty() {
                                text_parts.push(t.to_string());
                            }
                        }
                    }
                    Some("tool_use") => {
                        let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let input = block.get("input").cloned().unwrap_or(json!({}));
                        tool_uses.push((id, name, input));
                    }
                    _ => {}
                }
            }
        }

        let usage = TokenUsage {
            input_tokens: resp.pointer("/usage/input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            output_tokens: resp.pointer("/usage/output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        };

        Ok(stream! {
            for text in text_parts {
                yield Ok(StreamEvent::TextDelta { text });
            }
            for (id, name, input) in tool_uses {
                yield Ok(StreamEvent::ToolUse { id, name, input });
            }
            yield Ok(StreamEvent::Usage(usage));
            yield Ok(StreamEvent::MessageStop);
        })
    }
}

/// Sanitize a tool ID to match Anthropic's required pattern `^[a-zA-Z0-9_-]+$`.
/// IDs from other providers (e.g. OpenAI's `call_xxx`) may contain characters
/// that Anthropic rejects. Empty IDs get a generated fallback.
fn sanitize_tool_id(id: &str) -> String {
    let cleaned: String = id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    if cleaned.is_empty() {
        format!("tool_{:08x}", rand::random::<u32>())
    } else {
        cleaned
    }
}

/// Merge consecutive messages with the same role into one
/// (Anthropic API requires strict user/assistant alternation).
fn merge_consecutive_roles(messages: Vec<Value>) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::new();
    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("").to_string();
        let content = msg.get("content").cloned().unwrap_or(json!([]));
        if let Some(last) = merged.last_mut() {
            let last_role = last.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if last_role == role {
                if let (Some(existing), Some(new)) = (
                    last.get_mut("content").and_then(|c| c.as_array_mut()),
                    content.as_array(),
                ) {
                    existing.extend(new.iter().cloned());
                    continue;
                }
            }
        }
        merged.push(json!({"role": role, "content": content}));
    }
    merged
}

// ── AWS SigV4 signing ────────────────────────────────────────────────────────

fn sign_v4(
    method: &str,
    url: &str,
    body: &[u8],
    region: &str,
    service: &str,
    access_key: &str,
    secret_key: &str,
    session_token: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let now = chrono::Utc::now();
    let datestamp = now.format("%Y%m%d").to_string();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

    let parsed = reqwest::Url::parse(url)?;
    let host = parsed.host_str().unwrap_or("");
    let canonical_uri = parsed.path().to_string();
    let canonical_querystring = parsed.query().unwrap_or("").to_string();

    let payload_hash = hex_sha256(body);

    let mut signed_header_names = vec!["content-type", "host", "x-amz-date"];
    let mut canonical_headers = format!(
        "content-type:application/json\nhost:{host}\nx-amz-date:{amz_date}\n"
    );
    if session_token.is_some() {
        signed_header_names.push("x-amz-security-token");
        canonical_headers.push_str(&format!(
            "x-amz-security-token:{}\n",
            session_token.unwrap()
        ));
    }
    signed_header_names.sort();
    let signed_headers = signed_header_names.join(";");

    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_querystring}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );

    let credential_scope = format!("{datestamp}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        hex_sha256(canonical_request.as_bytes())
    );

    let k_date = hmac_sha256(format!("AWS4{secret_key}").as_bytes(), datestamp.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");

    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}"
    );

    let mut headers = vec![
        ("Authorization".into(), authorization),
        ("x-amz-date".into(), amz_date),
        ("x-amz-content-sha256".into(), payload_hash),
    ];
    if let Some(token) = session_token {
        headers.push(("x-amz-security-token".into(), token.into()));
    }
    Ok(headers)
}

fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
}

// ── AWS config file reader ───────────────────────────────────────────────────

fn read_aws_credentials(profile: &str) -> Option<(String, String, Option<String>, Option<String>)> {
    let home = dirs::home_dir()?;
    let mut access_key = None;
    let mut secret_key = None;
    let mut session_token = None;
    let mut region = None;

    for filename in &["credentials", "config"] {
        let path = home.join(".aws").join(filename);
        let content = std::fs::read_to_string(&path).ok()?;
        let section_names: Vec<String> = if *filename == "config" {
            if profile == "default" {
                vec!["[default]".to_string()]
            } else {
                vec![format!("[profile {profile}]"), format!("[{profile}]")]
            }
        } else {
            vec![format!("[{profile}]")]
        };

        let mut in_section = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_section = section_names.iter().any(|s| trimmed == s);
                continue;
            }
            if !in_section { continue; }
            if let Some((key, val)) = trimmed.split_once('=') {
                let key = key.trim();
                let val = val.trim();
                match key {
                    "aws_access_key_id" => access_key = Some(val.to_string()),
                    "aws_secret_access_key" => secret_key = Some(val.to_string()),
                    "aws_session_token" => session_token = Some(val.to_string()),
                    "region" => region = Some(val.to_string()),
                    _ => {}
                }
            }
        }
        if access_key.is_some() && secret_key.is_some() {
            break;
        }
    }

    let ak = access_key?;
    let sk = secret_key?;
    Some((ak, sk, session_token, region))
}
