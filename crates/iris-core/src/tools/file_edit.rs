use async_trait::async_trait;
use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::{resolve_path, CwdRef, Tool};

pub struct FileEditTool {
    cwd: CwdRef,
}

impl FileEditTool {
    pub fn new(cwd: CwdRef) -> Self { Self { cwd } }
}

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str { "file_edit" }

    fn description(&self) -> &str {
        "Replace an exact string in a file. The target must appear exactly once."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path":       { "type": "string", "description": "The path of the file to edit" },
                "old_string": { "type": "string", "description": "The exact string to find and replace" },
                "new_string": { "type": "string", "description": "The string to replace it with" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let raw_path = input["path"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?;
        let old_string = input["old_string"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: old_string"))?;
        let new_string = input["new_string"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: new_string"))?;

        let path = resolve_path(raw_path, &self.cwd);

        let content = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("failed to read file {}", path.display()))?;

        if !content.contains(old_string) {
            anyhow::bail!("old_string not found in {}", path.display());
        }
        let count = content.matches(old_string).count();
        if count != 1 {
            anyhow::bail!("old_string found {count} times in {}", path.display());
        }

        let new_content = content.replacen(old_string, new_string, 1);
        tokio::fs::write(&path, &new_content)
            .await
            .with_context(|| format!("failed to write file {}", path.display()))?;

        // Generate unified diff for display.
        let diff = generate_unified_diff(
            &path.display().to_string(),
            &content,
            &new_content,
        );
        Ok(format!("Replaced in {}\n\n{diff}", path.display()))
    }
}

/// Generate a unified diff between old and new content.
///
/// Output format matches `diff -u` — parseable by the TUI for colored rendering.
pub fn generate_unified_diff(path: &str, old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut hunks: Vec<String> = Vec::new();

    // Simple diff: find changed region by scanning from both ends.
    let common_prefix = old_lines.iter().zip(new_lines.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let common_suffix = old_lines.iter().rev().zip(new_lines.iter().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(old_lines.len() - common_prefix)
        .min(new_lines.len() - common_prefix);

    let old_start = common_prefix;
    let old_end = old_lines.len() - common_suffix;
    let new_start = common_prefix;
    let new_end = new_lines.len() - common_suffix;

    if old_start == old_end && new_start == new_end {
        return String::new(); // no changes
    }

    // Context lines around the change.
    let ctx = 3;
    let hunk_old_start = old_start.saturating_sub(ctx);
    let hunk_old_end = (old_end + ctx).min(old_lines.len());
    let hunk_new_start = new_start.saturating_sub(ctx);
    let hunk_new_end = (new_end + ctx).min(new_lines.len());

    hunks.push(format!(
        "@@ -{},{} +{},{} @@",
        hunk_old_start + 1,
        hunk_old_end - hunk_old_start,
        hunk_new_start + 1,
        hunk_new_end - hunk_new_start,
    ));

    // Leading context.
    for i in hunk_old_start..old_start {
        hunks.push(format!(" {}", old_lines[i]));
    }
    // Removed lines.
    for i in old_start..old_end {
        hunks.push(format!("-{}", old_lines[i]));
    }
    // Added lines.
    for i in new_start..new_end {
        hunks.push(format!("+{}", new_lines[i]));
    }
    // Trailing context.
    for i in old_end..hunk_old_end {
        hunks.push(format!(" {}", old_lines[i]));
    }

    format!("--- {path}\n+++ {path}\n{}", hunks.join("\n"))
}
