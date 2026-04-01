use async_trait::async_trait;
use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::{resolve_path, CwdRef, Tool};

pub struct FileWriteTool {
    cwd: CwdRef,
}

impl FileWriteTool {
    pub fn new(cwd: CwdRef) -> Self { Self { cwd } }
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str { "file_write" }

    fn description(&self) -> &str {
        "Write content to a file, creating parent directories when needed."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path":    { "type": "string", "description": "The path of the file to write" },
                "content": { "type": "string", "description": "The content to write" }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let raw_path = input["path"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?;
        let content = input["content"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: content"))?;

        let path = resolve_path(raw_path, &self.cwd);

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("failed to create directories for {}", path.display()))?;
            }
        }

        tokio::fs::write(&path, content)
            .await
            .with_context(|| format!("failed to write file {}", path.display()))?;

        Ok(format!("Written {} bytes to {}", content.len(), path.display()))
    }
}
