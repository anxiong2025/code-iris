//! McpToolWrapper — bridges an MCP server tool into the core Tool trait.
//!
//! On first `execute()` call the wrapper fetches the server's tool list
//! and caches it. Subsequent calls go directly to `client.call_tool()`.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use iris_llm::McpClient;
use serde_json::{json, Value};
use tokio::sync::OnceCell;

use super::Tool;

/// A single MCP server registered as a pseudo-tool.
///
/// When the LLM calls `mcp__<server>__<tool>`, the agent loop dispatches
/// here and we forward to the MCP server via JSON-RPC.
pub struct McpToolWrapper {
    pub server_name: String,
    client: Arc<McpClient>,
    /// Lazy-initialised list of tool names exposed by this server.
    tool_names: OnceCell<Vec<String>>,
}

impl McpToolWrapper {
    pub fn new(server_name: impl Into<String>, client: Arc<McpClient>) -> Self {
        Self {
            server_name: server_name.into(),
            client,
            tool_names: OnceCell::new(),
        }
    }
}

#[async_trait]
impl Tool for McpToolWrapper {
    fn name(&self) -> &str {
        &self.server_name
    }

    fn description(&self) -> &str {
        "MCP server proxy — forwards tool calls to the configured MCP server."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tool": {
                    "type": "string",
                    "description": "The MCP tool name to call."
                },
                "input": {
                    "type": "object",
                    "description": "Arguments to pass to the MCP tool."
                }
            },
            "required": ["tool"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let tool_name = input
            .get("tool")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field: tool"))?;

        let tool_input = input
            .get("input")
            .cloned()
            .unwrap_or_else(|| json!({}));

        self.client.call_tool(tool_name, tool_input).await
    }
}
