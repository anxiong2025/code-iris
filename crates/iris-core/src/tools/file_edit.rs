use async_trait::async_trait;
use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::Tool;

pub struct FileEditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "file_edit"
    }

    fn description(&self) -> &str {
        "Replace an exact string in a file. The target must appear exactly once."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path of the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact string to find and replace"
                },
                "new_string": {
                    "type": "string",
                    "description": "The string to replace it with"
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let path = input["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?;

        let old_string = input["old_string"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: old_string"))?;

        let new_string = input["new_string"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: new_string"))?;

        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read file {path}"))?;

        if !content.contains(old_string) {
            anyhow::bail!("old_string not found in {path}");
        }

        let count = content.matches(old_string).count();
        if count != 1 {
            anyhow::bail!("old_string found {count} times in {path}");
        }

        let new_content = content.replacen(old_string, new_string, 1);

        tokio::fs::write(path, &new_content)
            .await
            .with_context(|| format!("failed to write file {path}"))?;

        Ok(format!("Replaced in {}", path))
    }
}
