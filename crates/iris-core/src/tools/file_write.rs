use async_trait::async_trait;
use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::Tool;

pub struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write content to a file, creating parent directories when needed."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path of the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let path = input["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?;

        let content = input["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: content"))?;

        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("failed to create directories for {path}"))?;
            }
        }

        tokio::fs::write(path, content)
            .await
            .with_context(|| format!("failed to write file {path}"))?;

        Ok(format!("Written {} bytes to {}", content.len(), path))
    }
}
