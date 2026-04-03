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

        // Read existing content for diff (empty if new file).
        let old_content = tokio::fs::read_to_string(&path).await.unwrap_or_default();

        tokio::fs::write(&path, content)
            .await
            .with_context(|| format!("failed to write file {}", path.display()))?;

        let diff = crate::tools::file_edit::generate_unified_diff(
            &path.display().to_string(),
            &old_content,
            content,
        );
        if diff.is_empty() {
            Ok(format!("Written {} bytes to {} (unchanged)", content.len(), path.display()))
        } else {
            Ok(format!("Written {} bytes to {}\n\n{diff}", content.len(), path.display()))
        }
    }
}
