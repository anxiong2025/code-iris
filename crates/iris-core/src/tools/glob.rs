use std::time::SystemTime;

use async_trait::async_trait;
use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::{resolve_path, CwdRef, Tool};

pub struct GlobTool {
    cwd: CwdRef,
}

impl GlobTool {
    pub fn new(cwd: CwdRef) -> Self { Self { cwd } }
}

const MAX_RESULTS: usize = 200;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str { "glob" }

    fn description(&self) -> &str {
        "Find files matching a glob pattern sorted by most recent modification time."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern e.g. '**/*.rs'" },
                "path":    { "type": "string", "description": "Base directory (default: .)" }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let pattern = input["pattern"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: pattern"))?;

        let raw_base = input["path"].as_str().unwrap_or(".");
        let base = resolve_path(raw_base, &self.cwd);
        let base_str = base.to_string_lossy();
        let full_pattern = if raw_base == "." && !pattern.starts_with("./") {
            pattern.to_string()
        } else {
            format!("{}/{}", base_str.trim_end_matches('/'), pattern)
        };

        let mut paths = glob::glob(&full_pattern)
            .with_context(|| format!("invalid glob pattern {full_pattern}"))?
            .filter_map(|e| e.ok())
            .map(|path| {
                let modified = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                (path, modified)
            })
            .collect::<Vec<_>>();

        if paths.is_empty() {
            return Ok("No files found matching the pattern.".to_string());
        }

        paths.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let total = paths.len();
        let (to_show, truncated) = if total > MAX_RESULTS {
            (&paths[..MAX_RESULTS], true)
        } else {
            (paths.as_slice(), false)
        };

        let mut result = to_show.iter()
            .map(|(p, _)| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        if truncated {
            result.push_str(&format!(
                "\n[truncated, showing {MAX_RESULTS} of {total} matches]"
            ));
        }
        Ok(result)
    }
}
