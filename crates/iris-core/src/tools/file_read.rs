use async_trait::async_trait;
use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::{resolve_path, CwdRef, Tool};

pub struct FileReadTool {
    cwd: CwdRef,
}

impl FileReadTool {
    pub fn new(cwd: CwdRef) -> Self { Self { cwd } }
}

const MAX_LINES: usize = 2000;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str { "file_read" }

    fn description(&self) -> &str {
        "Read a UTF-8 text file with an optional inclusive line range."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The path to the file to read" },
                "start_line": { "type": "integer", "description": "1-based start line (optional)" },
                "end_line":   { "type": "integer", "description": "1-based end line inclusive (optional)" }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let raw_path = input["path"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?;
        let path = resolve_path(raw_path, &self.cwd);

        let content = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("failed to read file {}", path.display()))?;

        let all_lines: Vec<&str> = content.lines().collect();
        let total = all_lines.len();

        let start = input["start_line"].as_u64()
            .map(|n| (n as usize).saturating_sub(1)).unwrap_or(0);
        let end = input["end_line"].as_u64()
            .map(|n| (n as usize).min(total)).unwrap_or(total);
        let start = start.min(total);
        let end = end.max(start).min(total);

        let slice = &all_lines[start..end];
        let (lines_to_show, truncated) = if slice.len() > MAX_LINES {
            (&slice[..MAX_LINES], true)
        } else {
            (slice, false)
        };

        let mut output = String::new();
        for (i, line) in lines_to_show.iter().enumerate() {
            output.push_str(&format!("{:>4}\t{}\n", start + i + 1, line));
        }
        if truncated {
            output.push_str(&format!(
                "[truncated after {MAX_LINES} lines, {total} total lines]\n"
            ));
        }
        Ok(output)
    }
}
