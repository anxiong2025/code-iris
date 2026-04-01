//! Claude OAuth 2.0 — login with claude.ai account, no API key required.
//!
//! Flow (PKCE / S256):
//! ```text
//! 1. generate code_verifier + code_challenge (SHA-256 / base64url)
//! 2. open browser → https://claude.ai/oauth/authorize?...
//! 3. user approves → redirect to localhost callback with ?code=...
//! 4. exchange code → access_token + refresh_token
//! 5. persist to ~/.code-iris/credentials.json (atomic write)
//! ```
//!
//! Token refresh happens automatically in `AnthropicProvider` when the
//! access token is within 60 s of expiry.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ── PKCE helpers ──────────────────────────────────────────────────────────────

/// Generate a cryptographically random code verifier (43–128 chars, base64url).
pub fn generate_code_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Derive code challenge from verifier using S256 method.
pub fn derive_code_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

/// Generate a random state parameter for CSRF protection.
pub fn generate_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

// ── OAuth constants ───────────────────────────────────────────────────────────

pub const OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const OAUTH_REDIRECT_URI: &str = "http://localhost:54321/oauth/callback";
pub const OAUTH_SCOPE: &str = "org:create_api_key user:profile user:inference";
pub const OAUTH_AUTH_URL: &str = "https://claude.ai/oauth/authorize";
pub const OAUTH_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";

// ── Token types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokenSet {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix timestamp (seconds) when the access token expires.
    pub expires_at: u64,
    pub scope: String,
}

impl OAuthTokenSet {
    /// Returns true if the token expires within the next 60 seconds.
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now + 60 >= self.expires_at
    }
}

// ── Credential persistence ────────────────────────────────────────────────────

fn credentials_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".code-iris")
        .join("credentials.json")
}

/// Persist OAuth credentials using an atomic temp-file + rename.
pub fn save_credentials(tokens: &OAuthTokenSet) -> Result<()> {
    let path = credentials_path();
    std::fs::create_dir_all(path.parent().unwrap())?;

    let json = serde_json::to_string_pretty(tokens)?;

    // Atomic write: write to temp file then rename.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Load persisted OAuth credentials, if any.
pub fn load_credentials() -> Option<OAuthTokenSet> {
    let path = credentials_path();
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Delete stored credentials (logout).
pub fn clear_credentials() -> Result<()> {
    let path = credentials_path();
    if path.exists() {
        std::fs::remove_file(&path).context("failed to remove credentials")?;
    }
    Ok(())
}

// ── Authorization URL builder ─────────────────────────────────────────────────

/// Build the authorization URL to open in the user's browser.
pub fn authorization_url(code_challenge: &str, state: &str) -> String {
    format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        OAUTH_AUTH_URL,
        OAUTH_CLIENT_ID,
        urlencoding::encode(OAUTH_REDIRECT_URI),
        urlencoding::encode(OAUTH_SCOPE),
        code_challenge,
        state,
    )
}

// ── Token exchange ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    scope: Option<String>,
}

/// Exchange an authorization code for tokens.
pub async fn exchange_code(
    client: &reqwest::Client,
    code: &str,
    code_verifier: &str,
) -> Result<OAuthTokenSet> {
    let resp = client
        .post(OAUTH_TOKEN_URL)
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": OAUTH_CLIENT_ID,
            "code": code,
            "redirect_uri": OAUTH_REDIRECT_URI,
            "code_verifier": code_verifier,
        }))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("OAuth token exchange failed {status}: {body}");
    }

    let tr: TokenResponse = resp.json().await?;
    let expires_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + tr.expires_in.unwrap_or(3600);

    Ok(OAuthTokenSet {
        access_token: tr.access_token,
        refresh_token: tr.refresh_token.unwrap_or_default(),
        expires_at,
        scope: tr.scope.unwrap_or_else(|| OAUTH_SCOPE.to_string()),
    })
}

/// Refresh an existing token set.
pub async fn refresh_token(
    client: &reqwest::Client,
    tokens: &OAuthTokenSet,
) -> Result<OAuthTokenSet> {
    let resp = client
        .post(OAUTH_TOKEN_URL)
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": OAUTH_CLIENT_ID,
            "refresh_token": tokens.refresh_token,
            "scope": OAUTH_SCOPE,
        }))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("OAuth token refresh failed {status}: {body}");
    }

    let tr: TokenResponse = resp.json().await?;
    let expires_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + tr.expires_in.unwrap_or(3600);

    Ok(OAuthTokenSet {
        access_token: tr.access_token,
        refresh_token: tr.refresh_token.unwrap_or_else(|| tokens.refresh_token.clone()),
        expires_at,
        scope: tr.scope.unwrap_or_else(|| tokens.scope.clone()),
    })
}

// ── Local callback server ─────────────────────────────────────────────────────

/// Spin up a one-shot HTTP server on localhost:54321 to capture the OAuth callback.
/// Returns `(code, state)`.
pub async fn wait_for_callback() -> Result<(String, String)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:54321").await
        .context("failed to bind OAuth callback port 54321")?;

    let (mut stream, _) = listener.accept().await?;

    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Parse GET /oauth/callback?code=...&state=...
    let first_line = request.lines().next().unwrap_or("");
    let path = first_line.split_whitespace().nth(1).unwrap_or("");
    let query = path.splitn(2, '?').nth(1).unwrap_or("");

    let mut code = String::new();
    let mut state = String::new();
    for pair in query.split('&') {
        if let Some(v) = pair.strip_prefix("code=") { code = urlencoding::decode(v)?.into_owned(); }
        if let Some(v) = pair.strip_prefix("state=") { state = urlencoding::decode(v)?.into_owned(); }
    }

    // Send a friendly response to the browser.
    let body = "<html><body><h2>Login successful — you can close this tab.</h2></body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body
    );
    stream.write_all(response.as_bytes()).await?;

    if code.is_empty() {
        anyhow::bail!("OAuth callback missing code parameter");
    }
    Ok((code, state))
}

// ── High-level login flow ─────────────────────────────────────────────────────

/// Full interactive OAuth login:
/// 1. Generate PKCE + state
/// 2. Print / open auth URL
/// 3. Wait for callback
/// 4. Exchange code → tokens
/// 5. Persist tokens
///
/// Returns the access token.
pub async fn login(client: &reqwest::Client) -> Result<OAuthTokenSet> {
    let verifier = generate_code_verifier();
    let challenge = derive_code_challenge(&verifier);
    let state = generate_state();

    let url = authorization_url(&challenge, &state);

    println!("\nOpening Claude login in your browser…");
    println!("If the browser doesn't open, visit:\n\n  {url}\n");

    // Try to open the browser; ignore errors (user can open manually).
    let _ = open_browser(&url);

    println!("Waiting for login callback on localhost:54321 …");

    let (code, returned_state) = wait_for_callback().await?;

    if returned_state != state {
        anyhow::bail!("OAuth state mismatch — possible CSRF attack");
    }

    let tokens = exchange_code(client, &code, &verifier).await?;
    save_credentials(&tokens)?;

    println!("Login successful. Credentials saved to ~/.code-iris/credentials.json");
    Ok(tokens)
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    std::process::Command::new("open").arg(url).spawn()?;
    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open").arg(url).spawn()?;
    #[cfg(target_os = "windows")]
    std::process::Command::new("cmd").args(["/c", "start", url]).spawn()?;
    Ok(())
}
