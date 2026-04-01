use async_trait::async_trait;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::time;

use super::{CwdRef, Tool};

pub struct BashTool {
    cwd: CwdRef,
}

impl BashTool {
    pub fn new(cwd: CwdRef) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "bash" }

    fn description(&self) -> &str {
        "Execute a shell command with a timeout and return stdout/stderr."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
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
        let duration = std::time::Duration::from_secs(timeout_secs);

        let cwd_path = self.cwd.lock().unwrap().clone();
        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(&command);
        if let Some(dir) = cwd_path {
            cmd.current_dir(dir);
        }

        match time::timeout(duration, cmd.output()).await {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let code = output.status.code().unwrap_or_default();
                let mut result = format!("exit_code: {code}\n");
                if !stdout.trim().is_empty() {
                    result.push_str("stdout:\n");
                    result.push_str(&stdout);
                    if !stdout.ends_with('\n') { result.push('\n'); }
                }
                if !stderr.trim().is_empty() {
                    result.push_str("stderr:\n");
                    result.push_str(&stderr);
                    if !stderr.ends_with('\n') { result.push('\n'); }
                }
                Ok(result)
            }
            Ok(Err(error)) => Err(error).context("failed to execute command"),
            Err(_) => Ok(format!(
                "exit_code: -1\nstderr:\ncommand timed out after {timeout_secs} seconds\n"
            )),
        }
    }
}
