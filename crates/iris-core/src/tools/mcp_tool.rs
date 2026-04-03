//! MCP tool integration — bridges MCP server tools into the core Tool trait.
//!
//! Each MCP tool is registered as an independent tool with its own name,
//! description, and JSON Schema. Tool names follow the pattern
//! `mcp__<server>__<tool>` for namespacing.
//!
//! At startup, `discover_mcp_tools()` connects to each configured server,
//! fetches `tools/list`, and returns a `Vec<McpIndividualTool>` — each
//! is an independent `Tool` impl with its own schema.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use iris_llm::{McpClient, ToolDefinition};
use serde_json::Value;

use super::Tool;

/// A single MCP tool registered independently with its own schema.
///
/// Registered as `mcp__<server>__<tool_name>` in the ToolRegistry.
pub struct McpIndividualTool {
    /// Full registry name: `mcp__<server>__<tool_name>`.
    pub registry_name: String,
    /// Original tool name on the MCP server.
    pub original_name: String,
    /// Tool description from the MCP server.
    pub tool_description: String,
    /// JSON Schema for the tool input.
    pub schema: Value,
    /// Shared MCP client connection.
    client: Arc<McpClient>,
}

impl McpIndividualTool {
    pub fn new(
        server_name: &str,
        def: ToolDefinition,
        client: Arc<McpClient>,
    ) -> Self {
        Self {
            registry_name: format!("mcp__{server_name}__{}", def.name),
            original_name: def.name,
            tool_description: def.description,
            schema: def.input_schema,
            client,
        }
    }
}

#[async_trait]
impl Tool for McpIndividualTool {
    fn name(&self) -> &str {
        &self.registry_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn input_schema(&self) -> Value {
        self.schema.clone()
    }

    async fn execute(&self, input: Value) -> Result<String> {
        self.client.call_tool(&self.original_name, input).await
    }
}

/// Connect to an MCP server and discover all its tools.
///
/// Returns a list of independently-registerable tools, or an error if
/// the server fails to start or respond.
pub async fn discover_mcp_tools(
    server_name: &str,
    client: Arc<McpClient>,
) -> Result<Vec<McpIndividualTool>> {
    let tool_defs = client.list_tools().await?;
    let tools: Vec<McpIndividualTool> = tool_defs
        .into_iter()
        .map(|def| McpIndividualTool::new(server_name, def, client.clone()))
        .collect();
    Ok(tools)
}
