//! Permission system — controls whether tool calls require user confirmation.
//!
//! Mirrors Claude Code's `src/hooks/toolPermission/` with four modes:
//!
//! | Mode    | Behavior                                          |
//! |---------|---------------------------------------------------|
//! | Default | Prompt for dangerous tools (Bash, FileWrite, …)  |
//! | Plan    | Read-only tools allowed; writes require confirm   |
//! | Auto    | All tools auto-approved (--dangerously-skip-permissions) |
//! | Custom  | Per-tool allow/deny list                          |

use std::collections::HashSet;
use std::io::{self, Write};

use serde::{Deserialize, Serialize};

/// Tools that can mutate state and therefore require confirmation in Default mode.
const DANGEROUS_TOOLS: &[&str] = &[
    "bash",
    "file_write",
    "file_edit",
];

/// How the agent handles permission checks before executing a tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Interactive confirmation for dangerous tools (default).
    #[default]
    Default,
    /// Read-only tools auto-approved; anything that mutates requires confirmation.
    Plan,
    /// All tools auto-approved — use only in trusted environments.
    Auto,
    /// Allow only the listed tools; deny all others.
    Custom { allowed: HashSet<String> },
}

impl PermissionMode {
    /// Returns `true` if the tool may proceed without prompting the user.
    pub fn is_allowed(&self, tool_name: &str) -> bool {
        match self {
            PermissionMode::Auto => true,
            PermissionMode::Plan => !DANGEROUS_TOOLS.contains(&tool_name),
            PermissionMode::Default => !DANGEROUS_TOOLS.contains(&tool_name),
            PermissionMode::Custom { allowed } => allowed.contains(tool_name),
        }
    }

    /// Prompt the user interactively and return `true` if they approve.
    ///
    /// Returns `true` automatically in [`PermissionMode::Auto`].
    /// Returns `false` automatically in [`PermissionMode::Plan`] for dangerous tools.
    pub fn request(&self, tool_name: &str, preview: &str) -> bool {
        if self.is_allowed(tool_name) {
            return true;
        }

        // Plan mode: never prompt, just deny writes.
        if *self == PermissionMode::Plan {
            eprintln!(
                "[plan mode] Tool `{tool_name}` requires write access — denied in plan mode."
            );
            return false;
        }

        // Default / Custom: interactive prompt.
        println!();
        println!("  Tool: \x1b[33m{tool_name}\x1b[0m");
        if !preview.is_empty() {
            println!("  ─────────────────────────────────────────");
            for line in preview.lines().take(10) {
                println!("  {line}");
            }
            println!("  ─────────────────────────────────────────");
        }
        print!("  Allow? [y/N] ");
        io::stdout().flush().ok();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            return false;
        }
        matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
    }
}

/// Formats a short preview string from a JSON tool input value for display during confirmation.
pub fn format_preview(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "file_write" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let content = input
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("write to: {path}\n{}", &content[..content.len().min(200)])
        }
        "file_edit" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let old = input
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new = input
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("edit: {path}\n- {}\n+ {}", old.lines().next().unwrap_or(""), new.lines().next().unwrap_or(""))
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_allows_all_tools() {
        let mode = PermissionMode::Auto;
        assert!(mode.is_allowed("bash"));
        assert!(mode.is_allowed("file_write"));
        assert!(mode.is_allowed("file_edit"));
        assert!(mode.is_allowed("grep"));
    }

    #[test]
    fn plan_blocks_dangerous_tools() {
        let mode = PermissionMode::Plan;
        assert!(!mode.is_allowed("bash"));
        assert!(!mode.is_allowed("file_write"));
        assert!(!mode.is_allowed("file_edit"));
    }

    #[test]
    fn plan_allows_read_tools() {
        let mode = PermissionMode::Plan;
        assert!(mode.is_allowed("grep"));
        assert!(mode.is_allowed("file_read"));
        assert!(mode.is_allowed("glob"));
    }

    #[test]
    fn default_blocks_dangerous_tools() {
        let mode = PermissionMode::Default;
        assert!(!mode.is_allowed("bash"));
        assert!(!mode.is_allowed("file_write"));
    }

    #[test]
    fn default_allows_read_tools() {
        let mode = PermissionMode::Default;
        assert!(mode.is_allowed("grep"));
        assert!(mode.is_allowed("file_read"));
    }

    #[test]
    fn custom_allows_only_listed() {
        let mut allowed = std::collections::HashSet::new();
        allowed.insert("bash".to_string());
        let mode = PermissionMode::Custom { allowed };
        assert!(mode.is_allowed("bash"));
        assert!(!mode.is_allowed("file_write"));
        assert!(!mode.is_allowed("grep"));
    }

    #[test]
    fn format_preview_bash() {
        let input = serde_json::json!({ "command": "ls -la" });
        let preview = format_preview("bash", &input);
        assert_eq!(preview, "ls -la");
    }

    #[test]
    fn format_preview_unknown_tool() {
        let preview = format_preview("some_tool", &serde_json::json!({}));
        assert!(preview.is_empty());
    }
}
