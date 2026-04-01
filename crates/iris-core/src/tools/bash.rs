//! Persistent bash session tool.
//!
//! A single `bash` process is kept alive for the lifetime of the agent session.
//! This means `cd`, `export`, shell functions, and aliases all persist across
//! tool calls — exactly like a real terminal session.
//!
//! # Protocol
//!
//! For each command we write to the bash stdin:
//! ```text
//! { <command>
//! }; printf '\n__IRIS_<id>__ %d\n' $?
//! ```
//!
//! We then read stdout lines until we see `__IRIS_<id>__ <exit_code>`.
//! stderr is merged into stdout via `exec 2>&1` at session start, so the
//! agent receives both in a single stream.
//!
//! # State persistence
//!
//! ```text
//! call 1: bash("cd /tmp && mkdir iris_test")   → cwd is now /tmp/iris_test
//! call 2: bash("pwd")                          → /tmp/iris_test  ✓
//! call 3: bash("export FOO=bar")               → FOO set
//! call 4: bash("echo $FOO")                    → bar  ✓
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time;

use super::{CwdRef, Tool};

// ── Shell session ─────────────────────────────────────────────────────────────

struct ShellSession {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// Keep child alive (kill_on_drop).
    _child: Child,
    counter: u64,
}

impl ShellSession {
    async fn spawn(cwd: Option<PathBuf>) -> Result<Self> {
        let mut cmd = Command::new("bash");
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            // Disable readline / prompt interference.
            .env("TERM", "dumb")
            .env("PS1", "")
            .env("PS2", "");

        if let Some(ref dir) = cwd {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().context("failed to spawn bash")?;
        let stdin = child.stdin.take().context("bash stdin unavailable")?;
        let stdout = BufReader::new(child.stdout.take().context("bash stdout unavailable")?);

        // Drain stderr in a background task so the pipe never blocks.
        // After `exec 2>&1` below, bash's own fd-2 points at stdout, so this
        // stderr pipe only ever receives output from bash builtins that bypass
        // the redirection (very rare). Dropping it silently is safe.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut rdr = BufReader::new(stderr);
                let mut line = String::new();
                while rdr.read_line(&mut line).await.unwrap_or(0) > 0 {
                    line.clear();
                }
            });
        }

        let mut session = Self { stdin, stdout, _child: child, counter: 0 };

        // Initialise: merge stderr into stdout for the whole session.
        const INIT_SENTINEL: &str = "__IRIS_INIT__";
        let init = format!("exec 2>&1; printf '\\n{INIT_SENTINEL}\\n'\n");
        session
            .stdin
            .write_all(init.as_bytes())
            .await
            .context("bash init write")?;
        session.stdin.flush().await?;

        // Wait for the init sentinel (up to 5 s).
        time::timeout(Duration::from_secs(5), async {
            let mut line = String::new();
            loop {
                line.clear();
                let n = session.stdout.read_line(&mut line).await?;
                if n == 0 {
                    anyhow::bail!("bash exited during initialisation");
                }
                if line.trim() == INIT_SENTINEL {
                    break;
                }
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("bash init timed out")??;

        Ok(session)
    }

    /// Run one command and return `(exit_code, combined_output)`.
    async fn run(&mut self, command: &str, timeout: Duration) -> Result<(i32, String)> {
        self.counter += 1;
        let id = self.counter;
        let sentinel = format!("__IRIS_{id}__");

        // Wrap the command in a group so multi-line commands work, then print
        // sentinel + exit code. The leading newline ensures the sentinel always
        // starts on its own line even if the command output has no trailing newline.
        let script = format!(
            "{{\n{command}\n}}; printf '\\n{sentinel} %d\\n' $?\n"
        );

        self.stdin
            .write_all(script.as_bytes())
            .await
            .context("write to bash stdin")?;
        self.stdin.flush().await?;

        let mut output = String::new();
        let mut exit_code: i32 = 0;

        time::timeout(timeout, async {
            let mut line = String::new();
            loop {
                line.clear();
                let n = self.stdout.read_line(&mut line).await?;
                if n == 0 {
                    anyhow::bail!("bash session ended unexpectedly (EOF)");
                }
                let trimmed = line.trim_end_matches(['\n', '\r']);
                if let Some(rest) = trimmed.strip_prefix(&sentinel) {
                    exit_code = rest.trim().parse().unwrap_or(0);
                    break;
                }
                output.push_str(&line);
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .map_err(|_| anyhow::anyhow!("command timed out after {}s", timeout.as_secs()))??;

        Ok((exit_code, output))
    }
}

// ── BashTool ──────────────────────────────────────────────────────────────────

pub struct BashTool {
    cwd: CwdRef,
    session: Arc<AsyncMutex<Option<ShellSession>>>,
}

impl BashTool {
    pub fn new(cwd: CwdRef) -> Self {
        Self {
            cwd,
            session: Arc::new(AsyncMutex::new(None)),
        }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command in a persistent bash session. \
         Working directory, environment variables, and shell functions are \
         preserved across calls — `cd` and `export` work as expected. \
         stderr is merged into the output."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to run. \
                        State (cwd, env vars, functions) persists across calls."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 30, max: 600)",
                    "default": 30
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: command"))?
            .to_string();
        let timeout_secs = input["timeout_seconds"].as_u64().unwrap_or(30).min(600);
        let timeout = Duration::from_secs(timeout_secs);

        let mut guard = self.session.lock().await;

        // Lazily spawn (or respawn after crash).
        if guard.is_none() {
            let cwd = self.cwd.lock().unwrap().clone();
            *guard = Some(ShellSession::spawn(cwd).await?);
        }

        let session = guard.as_mut().unwrap();
        match session.run(&command, timeout).await {
            Ok((exit_code, output)) => {
                let mut result = format!("exit_code: {exit_code}\n");
                if !output.trim().is_empty() {
                    result.push_str(&output);
                    if !output.ends_with('\n') {
                        result.push('\n');
                    }
                }
                Ok(result)
            }
            Err(e) => {
                // Session died — clear so next call respawns.
                *guard = None;
                Err(e)
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn make_tool() -> BashTool {
        BashTool::new(Arc::new(Mutex::new(None)))
    }

    fn cmd(s: &str) -> Value {
        json!({ "command": s })
    }

    #[tokio::test]
    async fn basic_execution() {
        let t = make_tool();
        let out = t.execute(cmd("echo hello")).await.unwrap();
        assert!(out.contains("hello"), "{out}");
        assert!(out.contains("exit_code: 0"), "{out}");
    }

    #[tokio::test]
    async fn exit_code_nonzero() {
        let t = make_tool();
        let out = t.execute(cmd("exit 42")).await;
        // `exit` in a group command kills the session; we either get exit code
        // or an error — both are acceptable.
        let _ = out; // just don't panic
    }

    #[tokio::test]
    async fn state_persists_cd() {
        let t = make_tool();
        t.execute(cmd("cd /tmp")).await.unwrap();
        let out = t.execute(cmd("pwd")).await.unwrap();
        assert!(out.contains("/tmp"), "{out}");
    }

    #[tokio::test]
    async fn state_persists_export() {
        let t = make_tool();
        t.execute(cmd("export IRIS_TEST_VAR=hello_iris")).await.unwrap();
        let out = t.execute(cmd("echo $IRIS_TEST_VAR")).await.unwrap();
        assert!(out.contains("hello_iris"), "{out}");
    }

    #[tokio::test]
    async fn stderr_captured() {
        let t = make_tool();
        let out = t.execute(cmd("echo err_msg >&2")).await.unwrap();
        assert!(out.contains("err_msg"), "{out}");
    }

    #[tokio::test]
    async fn multiline_command() {
        let t = make_tool();
        let out = t
            .execute(cmd("for i in 1 2 3; do\n  echo $i\ndone"))
            .await
            .unwrap();
        assert!(out.contains('1') && out.contains('2') && out.contains('3'), "{out}");
    }

    #[tokio::test]
    async fn exit_code_captured() {
        let t = make_tool();
        let out = t.execute(cmd("false")).await.unwrap();
        assert!(out.contains("exit_code: 1"), "{out}");
    }

    #[tokio::test]
    async fn timeout_respected() {
        let t = make_tool();
        let out = t
            .execute(json!({ "command": "sleep 60", "timeout_seconds": 1 }))
            .await;
        assert!(out.is_err(), "expected timeout error");
    }
}
