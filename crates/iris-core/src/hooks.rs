//! Hooks system — run shell commands before/after tool execution.
//!
//! Inspired by Claude Code's hooks, but with typed TOML config and
//! structured JSON context injected via stdin.
//!
//! # Config file
//!
//! Place `.iris/hooks.toml` in the project root (or `~/.code-iris/hooks.toml`
//! for user-level hooks applied to every project):
//!
//! ```toml
//! # Block bash commands that contain "rm -rf"
//! [[hooks]]
//! event = "PreToolUse"
//! matcher = "bash"
//! command = "jq -e '.tool_input.command | test(\"rm -rf\") | not' > /dev/null"
//!
//! # Log every file write
//! [[hooks]]
//! event = "PostToolUse"
//! matcher = "file_write"
//! command = "jq -r '\"wrote: \" + .tool_input.path' >> ~/.iris-writes.log"
//!
//! # Desktop notification when the agent finishes
//! [[hooks]]
//! event = "Notification"
//! command = "osascript -e 'display notification \"$IRIS_MESSAGE\" with title \"iris\"'"
//! ```
//!
//! # Hook stdin contract
//!
//! Every hook receives a JSON object on stdin:
//!
//! ```json
//! {
//!   "event":       "PreToolUse",
//!   "tool_name":   "bash",
//!   "tool_input":  { ... },      // original tool arguments
//!   "tool_output": null           // filled for PostToolUse
//! }
//! ```
//!
//! # PreToolUse exit codes
//!
//! - `0`  → allow the tool call
//! - any other → block; hook's stdout is returned to the LLM as the tool error

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time;

// ── Config types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    Notification,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HookConfig {
    pub event: HookEvent,
    /// Optional glob pattern for tool name (`"bash"`, `"file_*"`, `"*"`).
    /// `None` matches every tool.
    pub matcher: Option<String>,
    /// Shell command executed via `sh -c`.
    pub command: String,
    /// Timeout in milliseconds (default: 10 000).
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
}

fn default_timeout() -> u64 {
    10_000
}

// ── Context sent to hooks ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct HookContext {
    pub event: String,
    pub tool_name: String,
    pub tool_input: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<String>,
}

// ── Decision ──────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum HookDecision {
    /// Allow the tool call to proceed.
    Allow,
    /// Block it; the string is returned to the LLM as the tool error message.
    Block(String),
}

// ── Runner ────────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct HookRunner {
    hooks: Vec<HookConfig>,
}

#[derive(Deserialize)]
struct HooksFile {
    #[serde(default)]
    hooks: Vec<HookConfig>,
}

impl HookRunner {
    /// Load hooks from (in order):
    /// 1. `~/.code-iris/hooks.toml` (user-level)
    /// 2. `<project_root>/.iris/hooks.toml` (project-level, appended last → higher priority)
    pub fn load(project_root: Option<&Path>) -> Self {
        let mut hooks: Vec<HookConfig> = Vec::new();

        // User-level
        if let Some(home) = dirs::home_dir() {
            load_file(home.join(".code-iris").join("hooks.toml"), &mut hooks);
        }
        // Project-level
        if let Some(root) = project_root {
            load_file(root.join(".iris").join("hooks.toml"), &mut hooks);
        }

        Self { hooks }
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// Run all matching `PreToolUse` hooks serially.
    ///
    /// Returns `Block(msg)` as soon as one hook exits with a non-zero code.
    pub async fn run_pre_tool(&self, tool_name: &str, input: &Value) -> HookDecision {
        let ctx = HookContext {
            event: "PreToolUse".to_string(),
            tool_name: tool_name.to_string(),
            tool_input: input.clone(),
            tool_output: None,
        };

        for hook in &self.hooks {
            if hook.event != HookEvent::PreToolUse {
                continue;
            }
            if !matches_pattern(hook.matcher.as_deref(), tool_name) {
                continue;
            }
            match run_hook(hook, &ctx).await {
                Ok(out) if out.success => {}
                Ok(out) => {
                    let msg = if out.stdout.trim().is_empty() {
                        format!("PreToolUse hook blocked `{tool_name}`")
                    } else {
                        out.stdout
                    };
                    return HookDecision::Block(msg);
                }
                Err(e) => {
                    tracing::warn!(tool = tool_name, "PreToolUse hook error: {e}");
                    // Don't block on hook infrastructure errors.
                }
            }
        }
        HookDecision::Allow
    }

    /// Spawn `PostToolUse` hooks as fire-and-forget tasks.
    pub fn run_post_tool(&self, tool_name: &str, input: &Value, output: &str) {
        let matching = self.matching(HookEvent::PostToolUse, tool_name);
        if matching.is_empty() {
            return;
        }
        let ctx = HookContext {
            event: "PostToolUse".to_string(),
            tool_name: tool_name.to_string(),
            tool_input: input.clone(),
            tool_output: Some(output.to_string()),
        };
        for hook in matching {
            let ctx = ctx.clone();
            tokio::spawn(async move {
                if let Err(e) = run_hook(&hook, &ctx).await {
                    tracing::warn!("PostToolUse hook error: {e}");
                }
            });
        }
    }

    /// Spawn `Notification` hooks as fire-and-forget tasks.
    pub fn run_notification(&self, message: &str) {
        let matching = self.matching_event(HookEvent::Notification);
        if matching.is_empty() {
            return;
        }
        let ctx = HookContext {
            event: "Notification".to_string(),
            tool_name: String::new(),
            tool_input: Value::Null,
            tool_output: Some(message.to_string()),
        };
        for hook in matching {
            let ctx = ctx.clone();
            let msg = message.to_string();
            tokio::spawn(async move {
                let child = Command::new("sh")
                    .arg("-c")
                    .arg(&hook.command)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .env("IRIS_EVENT", "Notification")
                    .env("IRIS_MESSAGE", &msg)
                    .kill_on_drop(true)
                    .spawn();
                if let Ok(mut c) = child {
                    let _ = c.wait().await;
                }
                let _ = ctx; // keep ctx alive
            });
        }
    }

    fn matching(&self, event: HookEvent, tool_name: &str) -> Vec<HookConfig> {
        self.hooks
            .iter()
            .filter(|h| h.event == event && matches_pattern(h.matcher.as_deref(), tool_name))
            .cloned()
            .collect()
    }

    fn matching_event(&self, event: HookEvent) -> Vec<HookConfig> {
        self.hooks.iter().filter(|h| h.event == event).cloned().collect()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn load_file(path: impl AsRef<Path>, out: &mut Vec<HookConfig>) {
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Ok(f) = toml::from_str::<HooksFile>(&text) {
            out.extend(f.hooks);
        }
    }
}

fn matches_pattern(pattern: Option<&str>, tool_name: &str) -> bool {
    match pattern {
        None => true,
        Some("*") => true,
        Some(p) if p.ends_with('*') => tool_name.starts_with(&p[..p.len() - 1]),
        Some(p) => p == tool_name,
    }
}

struct HookOutput {
    success: bool,
    stdout: String,
}

async fn run_hook(hook: &HookConfig, ctx: &HookContext) -> Result<HookOutput> {
    let json = serde_json::to_string(ctx)?;
    let timeout = Duration::from_millis(hook.timeout_ms);

    let result = time::timeout(timeout, async {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&hook.command)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .env("IRIS_EVENT", &ctx.event)
            .env("IRIS_TOOL", &ctx.tool_name)
            .env("IRIS_MESSAGE", ctx.tool_output.as_deref().unwrap_or(""))
            .kill_on_drop(true)
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(json.as_bytes()).await;
        }

        let out = child.wait_with_output().await?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok::<_, anyhow::Error>(HookOutput { success: out.status.success(), stdout })
    })
    .await;

    match result {
        Ok(r) => r,
        Err(_) => anyhow::bail!("hook timed out after {}ms", hook.timeout_ms),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_none_matches_all() {
        assert!(matches_pattern(None, "bash"));
        assert!(matches_pattern(None, "file_read"));
    }

    #[test]
    fn pattern_exact_match() {
        assert!(matches_pattern(Some("bash"), "bash"));
        assert!(!matches_pattern(Some("bash"), "grep"));
    }

    #[test]
    fn pattern_wildcard_prefix() {
        assert!(matches_pattern(Some("file_*"), "file_read"));
        assert!(matches_pattern(Some("file_*"), "file_write"));
        assert!(!matches_pattern(Some("file_*"), "bash"));
    }

    #[test]
    fn pattern_star_matches_all() {
        assert!(matches_pattern(Some("*"), "anything"));
    }

    #[test]
    fn hook_runner_empty_by_default() {
        let r = HookRunner::default();
        assert!(r.is_empty());
    }

    #[test]
    fn parse_hooks_toml() {
        let toml_str = r#"
[[hooks]]
event = "PreToolUse"
matcher = "bash"
command = "echo ok"

[[hooks]]
event = "PostToolUse"
command = "true"
timeout_ms = 5000
"#;
        let f: HooksFile = toml::from_str(toml_str).unwrap();
        assert_eq!(f.hooks.len(), 2);
        assert_eq!(f.hooks[0].event, HookEvent::PreToolUse);
        assert_eq!(f.hooks[0].matcher.as_deref(), Some("bash"));
        assert_eq!(f.hooks[1].event, HookEvent::PostToolUse);
        assert_eq!(f.hooks[1].timeout_ms, 5000);
    }

    #[tokio::test]
    async fn pre_tool_allow_when_hook_exits_zero() {
        let runner = HookRunner {
            hooks: vec![HookConfig {
                event: HookEvent::PreToolUse,
                matcher: Some("bash".to_string()),
                command: "exit 0".to_string(),
                timeout_ms: 3000,
            }],
        };
        let decision = runner.run_pre_tool("bash", &serde_json::json!({})).await;
        assert!(matches!(decision, HookDecision::Allow));
    }

    #[tokio::test]
    async fn pre_tool_block_when_hook_exits_nonzero() {
        let runner = HookRunner {
            hooks: vec![HookConfig {
                event: HookEvent::PreToolUse,
                matcher: Some("bash".to_string()),
                command: "echo 'blocked by policy'; exit 1".to_string(),
                timeout_ms: 3000,
            }],
        };
        let decision = runner.run_pre_tool("bash", &serde_json::json!({})).await;
        assert!(matches!(decision, HookDecision::Block(_)));
        if let HookDecision::Block(msg) = decision {
            assert!(msg.contains("blocked by policy"), "{msg}");
        }
    }

    #[tokio::test]
    async fn pre_tool_skips_non_matching_tools() {
        let runner = HookRunner {
            hooks: vec![HookConfig {
                event: HookEvent::PreToolUse,
                matcher: Some("bash".to_string()),
                command: "exit 1".to_string(), // would block
                timeout_ms: 3000,
            }],
        };
        // "file_read" doesn't match "bash" pattern → should allow
        let decision = runner.run_pre_tool("file_read", &serde_json::json!({})).await;
        assert!(matches!(decision, HookDecision::Allow));
    }
}
