use async_trait::async_trait;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::process::Command;

use super::Tool;

pub struct GrepTool;

const MAX_RESULTS: usize = 100;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search for a pattern using grep -rn and return matching lines."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "The directory or file to search in (default: current directory)"
                },
                "file_glob": {
                    "type": "string",
                    "description": "Optional glob pattern to filter which files to search (e.g. '*.rs')"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let pattern = input["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: pattern"))?;

        let search_path = input["path"].as_str().unwrap_or(".");
        let file_glob = input["file_glob"].as_str();

        let mut cmd = Command::new("grep");
        cmd.arg("-rn");
        cmd.arg("--color=never");

        if let Some(glob) = file_glob {
            cmd.arg(format!("--include={}", glob));
        }

        cmd.arg(pattern);
        cmd.arg(search_path);

        let output = cmd.output().await.context("failed to run grep")?;

        if output.status.code() == Some(2) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("grep error: {}", stderr.trim()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = stdout.lines().collect();

        if lines.is_empty() {
            return Ok("No matches found.".to_string());
        }

        let (to_show, truncated) = if lines.len() > MAX_RESULTS {
            (&lines[..MAX_RESULTS], true)
        } else {
            (lines.as_slice(), false)
        };

        let mut result = to_show.join("\n");
        if truncated {
            result.push_str(&format!(
                "\n[truncated, showing {} of {} matches]",
                MAX_RESULTS,
                lines.len()
            ));
        }

        Ok(result)
    }
}
