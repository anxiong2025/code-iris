use async_trait::async_trait;
use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::Tool;

pub struct FileReadTool;

const MAX_LINES: usize = 2000;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read a UTF-8 text file with an optional inclusive line range."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path to the file to read"
                },
                "start_line": {
                    "type": "integer",
                    "description": "The 1-based line number to start reading from (optional)"
                },
                "end_line": {
                    "type": "integer",
                    "description": "The 1-based line number to stop reading at (inclusive, optional)"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let path = input["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?;

        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read file {path}"))?;

        let all_lines: Vec<&str> = content.lines().collect();
        let total = all_lines.len();

        // Determine the range (1-based input → 0-based index)
        let start = input["start_line"]
            .as_u64()
            .map(|n| (n as usize).saturating_sub(1))
            .unwrap_or(0);

        let end = input["end_line"]
            .as_u64()
            .map(|n| (n as usize).min(total))
            .unwrap_or(total);

        let start = start.min(total);
        let end = end.max(start).min(total);

        let slice = &all_lines[start..end];

        // Truncate to MAX_LINES
        let (lines_to_show, truncated) = if slice.len() > MAX_LINES {
            (&slice[..MAX_LINES], true)
        } else {
            (slice, false)
        };

        let mut output = String::new();
        for (i, line) in lines_to_show.iter().enumerate() {
            let lineno = start + i + 1;
            output.push_str(&format!("{:>4}\t{}\n", lineno, line));
        }

        if truncated {
            output.push_str(&format!("[truncated after {MAX_LINES} lines, {total} total lines]\n"));
        }

        Ok(output)
    }
}
