use std::time::SystemTime;

use async_trait::async_trait;
use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::Tool;

pub struct GlobTool;

const MAX_RESULTS: usize = 200;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern sorted by most recent modification time."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The glob pattern to match (e.g. '**/*.rs', 'src/*.py')"
                },
                "path": {
                    "type": "string",
                    "description": "The base directory to search from (default: current directory)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let pattern = input["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: pattern"))?;

        let base_path = input["path"].as_str().unwrap_or(".");
        let full_pattern = if base_path == "." {
            pattern.to_string()
        } else {
            format!("{}/{}", base_path.trim_end_matches('/'), pattern)
        };

        let mut paths = glob::glob(&full_pattern)
            .with_context(|| format!("invalid glob pattern {full_pattern}"))?
            .filter_map(|entry| entry.ok())
            .map(|path| {
                let modified = std::fs::metadata(&path)
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                (path, modified)
            })
            .collect::<Vec<_>>();

        if paths.is_empty() {
            return Ok("No files found matching the pattern.".to_string());
        }

        paths.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

        let total = paths.len();
        let (to_show, truncated) = if total > MAX_RESULTS {
            (&paths[..MAX_RESULTS], true)
        } else {
            (paths.as_slice(), false)
        };

        let mut result = to_show
            .iter()
            .map(|(path, _)| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        if truncated {
            result.push_str(&format!(
                "\n[truncated, showing {} of {} matches]",
                MAX_RESULTS,
                total
            ));
        }

        Ok(result)
    }
}
