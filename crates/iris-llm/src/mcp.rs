//! MCP (Model Context Protocol) client — connect to external tool servers.
//!
//! Supports the two most common transports:
//! - **Stdio** — spawn a local process, communicate over stdin/stdout (JSON-RPC)
//! - **SSE**   — connect to a remote HTTP server using Server-Sent Events
//!
//! The client discovers available tools via `tools/list`, then exposes them
//! as [`ToolDefinition`]s so the agent loop can include them in LLM requests.
//! When the LLM calls an MCP tool, `call_tool()` forwards the request over
//! the transport and returns the result string.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::types::ToolDefinition;

// ── Transport ─────────────────────────────────────────────────────────────────

/// How to connect to an MCP server.
#[derive(Debug, Clone)]
pub enum McpTransport {
    /// Spawn a local process (`command` + `args`), talk over stdin/stdout.
    Stdio {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    },
    /// Connect to a remote HTTP+SSE endpoint.
    Sse { url: String, headers: HashMap<String, String> },
}

// ── JSON-RPC types ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    params: Value,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    id: Option<Value>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

// ── MCP tool list response ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct McpTool {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "inputSchema", default)]
    input_schema: Value,
}

#[derive(Debug, Deserialize)]
struct McpToolsListResult {
    tools: Vec<McpTool>,
}

// ── MCP tool call response ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct McpContent {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpCallResult {
    content: Vec<McpContent>,
    #[serde(rename = "isError", default)]
    is_error: bool,
}

// ── Stdio transport implementation ───────────────────────────────────────────

async fn stdio_call(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
    request: &JsonRpcRequest,
) -> Result<JsonRpcResponse> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::process::Command;

    let mut child = Command::new(command)
        .args(args)
        .envs(env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn MCP server: {command}"))?;

    let stdin = child.stdin.as_mut().context("no stdin")?;
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await?;

    let stdout = child.stdout.take().context("no stdout")?;
    let mut reader = BufReader::new(stdout).lines();
    if let Some(response_line) = reader.next_line().await? {
        let resp: JsonRpcResponse = serde_json::from_str(&response_line)
            .context("failed to parse MCP JSON-RPC response")?;
        return Ok(resp);
    }
    anyhow::bail!("MCP stdio server returned no response");
}

// ── McpClient ────────────────────────────────────────────────────────────────

/// A connected MCP client. Use [`McpClient::connect`] to initialise.
pub struct McpClient {
    transport: McpTransport,
    http_client: reqwest::Client,
    request_id: std::sync::atomic::AtomicU64,
}

impl McpClient {
    pub fn new(transport: McpTransport) -> Self {
        Self {
            transport,
            http_client: reqwest::Client::builder()
                .use_rustls_tls()
                .build()
                .expect("reqwest client"),
            request_id: std::sync::atomic::AtomicU64::new(1),
        }
    }

    fn next_id(&self) -> u64 {
        self.request_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Send a JSON-RPC request and return the parsed response.
    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: self.next_id(),
            method: method.to_string(),
            params,
        };

        let resp = match &self.transport {
            McpTransport::Stdio { command, args, env } => {
                stdio_call(command, args, env, &req).await?
            }
            McpTransport::Sse { url, headers } => {
                let mut rb = self.http_client.post(url).json(&req);
                for (k, v) in headers {
                    rb = rb.header(k, v);
                }
                let response = rb.send().await?;
                let text = response.text().await?;
                serde_json::from_str(&text)?
            }
        };

        if let Some(err) = resp.error {
            anyhow::bail!("MCP error {}: {}", err.code, err.message);
        }
        Ok(resp.result.unwrap_or(Value::Null))
    }

    /// Fetch the list of tools this server exposes.
    pub async fn list_tools(&self) -> Result<Vec<ToolDefinition>> {
        let result = self.rpc("tools/list", json!({})).await?;
        let mcp_result: McpToolsListResult = serde_json::from_value(result)
            .context("failed to parse tools/list response")?;

        Ok(mcp_result
            .tools
            .into_iter()
            .map(|t| ToolDefinition {
                name: t.name,
                description: t.description,
                input_schema: if t.input_schema.is_null() {
                    json!({"type": "object", "properties": {}})
                } else {
                    t.input_schema
                },
            })
            .collect())
    }

    /// Call a tool by name with the given input.
    pub async fn call_tool(&self, name: &str, input: Value) -> Result<String> {
        let result = self
            .rpc("tools/call", json!({ "name": name, "arguments": input }))
            .await?;

        let call_result: McpCallResult = serde_json::from_value(result)
            .context("failed to parse tools/call response")?;

        if call_result.is_error {
            let msg = call_result
                .content
                .iter()
                .filter_map(|c| c.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!("MCP tool error: {msg}");
        }

        let output = call_result
            .content
            .into_iter()
            .filter_map(|c| {
                if c.kind == "text" { c.text } else { None }
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(output)
    }
}

// ── McpTool wrapper (implements core Tool trait externally) ───────────────────

/// Metadata for a dynamically-registered MCP tool.
///
/// Wrap in `Arc` and register with `ToolRegistry` via `McpToolWrapper`
/// (defined in `iris-core/src/tools/mcp_tool.rs`).
#[derive(Debug, Clone)]
pub struct McpToolMeta {
    pub definition: ToolDefinition,
    pub server_name: String,
}

// ── Config helpers ────────────────────────────────────────────────────────────

/// A single MCP server entry in the config file.
///
/// Example TOML:
/// ```toml
/// [[mcp_servers]]
/// name = "filesystem"
/// command = "npx"
/// args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    /// For stdio transport.
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// For SSE transport.
    pub url: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

impl McpServerConfig {
    pub fn to_transport(&self) -> Result<McpTransport> {
        if let Some(ref cmd) = self.command {
            return Ok(McpTransport::Stdio {
                command: cmd.clone(),
                args: self.args.clone(),
                env: self.env.clone(),
            });
        }
        if let Some(ref url) = self.url {
            return Ok(McpTransport::Sse {
                url: url.clone(),
                headers: self.headers.clone(),
            });
        }
        anyhow::bail!("MCP server '{}' needs either 'command' or 'url'", self.name);
    }
}
