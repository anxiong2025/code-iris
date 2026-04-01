//! LSP Tool — query Language Server Protocol servers for code intelligence.
//!
//! Supports: hover, go-to-definition, find-references, diagnostics.
//!
//! Auto-detects the appropriate server by file extension:
//! - `.rs`  → rust-analyzer
//! - `.py`  → pylsp (python-lsp-server)
//! - `.ts` / `.tsx` → typescript-language-server --stdio
//! - `.js` / `.jsx` → typescript-language-server --stdio
//! - `.go`  → gopls
//! - `.c` / `.cpp` / `.h` → clangd
//!
//! Each call spawns a fresh server, performs the handshake, runs the request,
//! then shuts down. This is simple and stateless (no persistent connection).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::{self, Duration};

use super::{resolve_path, CwdRef, Tool};

// ── Server detection ──────────────────────────────────────────────────────────

struct ServerInfo {
    bin: &'static str,
    args: &'static [&'static str],
}

fn detect_server(file: &Path) -> Option<ServerInfo> {
    let ext = file.extension()?.to_str()?;
    Some(match ext {
        "rs" => ServerInfo { bin: "rust-analyzer", args: &[] },
        "py" => ServerInfo { bin: "pylsp", args: &[] },
        "ts" | "tsx" => ServerInfo { bin: "typescript-language-server", args: &["--stdio"] },
        "js" | "jsx" | "mjs" | "cjs" => ServerInfo { bin: "typescript-language-server", args: &["--stdio"] },
        "go" => ServerInfo { bin: "gopls", args: &[] },
        "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" => ServerInfo { bin: "clangd", args: &[] },
        _ => return None,
    })
}

// ── JSON-RPC framing ──────────────────────────────────────────────────────────

async fn write_message(stdin: &mut tokio::process::ChildStdin, msg: &Value) -> Result<()> {
    let body = serde_json::to_string(msg)?;
    let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    stdin.write_all(frame.as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

async fn read_message(reader: &mut BufReader<tokio::process::ChildStdout>) -> Result<Value> {
    // Read headers.
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let line = line.trim();
        if line.is_empty() { break; }
        if let Some(rest) = line.strip_prefix("Content-Length: ") {
            content_length = rest.trim().parse().context("bad Content-Length")?;
        }
    }
    if content_length == 0 {
        bail!("LSP response had no Content-Length");
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

// ── LSP session ───────────────────────────────────────────────────────────────

async fn lsp_query(
    file: &Path,
    root: &Path,
    operation: &str,
    line: u32,
    character: u32,
    timeout: Duration,
) -> Result<String> {
    let info = detect_server(file).with_context(|| {
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("?");
        format!("No LSP server configured for .{ext} files")
    })?;

    // Verify the server binary is installed.
    let which = std::process::Command::new("which")
        .arg(info.bin)
        .output()
        .ok();
    if which.map(|o| !o.status.success()).unwrap_or(true) {
        bail!(
            "LSP server `{}` not found in PATH. Install it first.\n\
             Hint: cargo install {} / npm install -g {} / ...",
            info.bin, info.bin, info.bin
        );
    }

    let mut child = Command::new(info.bin)
        .args(info.args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to spawn {}", info.bin))?;

    let mut stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let mut reader = BufReader::new(stdout);

    let file_uri = format!("file://{}", file.display());
    let root_uri = format!("file://{}", root.display());
    let pid = std::process::id();

    let result = time::timeout(timeout, async {
        // ── Initialize ────────────────────────────────────────────────────────
        write_message(&mut stdin, &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": pid,
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "hover": { "contentFormat": ["plaintext", "markdown"] },
                        "definition": {},
                        "references": {},
                        "publishDiagnostics": {}
                    }
                },
                "clientInfo": { "name": "iris", "version": "0.1" }
            }
        })).await?;

        // Drain until we get the initialize response (id=1).
        loop {
            let msg = read_message(&mut reader).await?;
            if msg.get("id") == Some(&json!(1)) { break; }
        }

        // ── Initialized notification ──────────────────────────────────────────
        write_message(&mut stdin, &json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        })).await?;

        // ── Open the file ─────────────────────────────────────────────────────
        let source = std::fs::read_to_string(file)
            .with_context(|| format!("cannot read {}", file.display()))?;

        let lang_id = match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "rs" => "rust",
            "py" => "python",
            "ts" | "tsx" => "typescript",
            "js" | "jsx" | "mjs" => "javascript",
            "go" => "go",
            "c" | "h" => "c",
            "cpp" | "cc" | "cxx" | "hpp" => "cpp",
            other => other,
        };

        write_message(&mut stdin, &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": file_uri,
                    "languageId": lang_id,
                    "version": 1,
                    "text": source
                }
            }
        })).await?;

        // ── Dispatch operation ────────────────────────────────────────────────
        let position = json!({ "line": line, "character": character });
        let text_doc = json!({ "uri": file_uri });

        let (method, params) = match operation {
            "hover" => (
                "textDocument/hover",
                json!({ "textDocument": text_doc, "position": position }),
            ),
            "definition" => (
                "textDocument/definition",
                json!({ "textDocument": text_doc, "position": position }),
            ),
            "references" => (
                "textDocument/references",
                json!({
                    "textDocument": text_doc,
                    "position": position,
                    "context": { "includeDeclaration": true }
                }),
            ),
            "diagnostics" => {
                // Diagnostics are pushed, not pulled — wait for publishDiagnostics.
                // Give the server 3s to push, then shut down.
                let deadline = time::Instant::now() + Duration::from_secs(3);
                loop {
                    if time::Instant::now() >= deadline { break; }
                    let msg = match time::timeout(
                        Duration::from_millis(500),
                        read_message(&mut reader),
                    ).await {
                        Ok(Ok(m)) => m,
                        _ => break,
                    };
                    if msg.get("method") == Some(&json!("textDocument/publishDiagnostics")) {
                        let diags = &msg["params"]["diagnostics"];
                        return Ok(format_diagnostics(diags, file));
                    }
                }
                return Ok(format!("No diagnostics received from {} within 3s.", info.bin));
            }
            other => bail!("unknown operation: {other}"),
        };

        write_message(&mut stdin, &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": method,
            "params": params
        })).await?;

        // Read until we get the response (id=2), skip notifications.
        let response = loop {
            let msg = read_message(&mut reader).await?;
            if msg.get("id") == Some(&json!(2)) { break msg; }
        };

        // ── Shutdown ──────────────────────────────────────────────────────────
        let _ = write_message(&mut stdin, &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "shutdown",
            "params": null
        })).await;
        let _ = write_message(&mut stdin, &json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        })).await;

        if let Some(err) = response.get("error") {
            bail!("LSP error: {}", err);
        }

        Ok(format_response(operation, &response["result"], file))
    })
    .await;

    let _ = child.kill().await;

    match result {
        Ok(r) => r,
        Err(_) => bail!("LSP request timed out after {}s", timeout.as_secs()),
    }
}

// ── Response formatters ───────────────────────────────────────────────────────

fn format_response(operation: &str, result: &Value, _file: &Path) -> String {
    match operation {
        "hover" => {
            if result.is_null() {
                return "No hover information available at this position.".to_string();
            }
            let content = &result["contents"];
            // MarkupContent { kind, value } or MarkedString or array
            if let Some(v) = content.get("value").and_then(|v| v.as_str()) {
                return v.to_string();
            }
            if let Some(s) = content.as_str() {
                return s.to_string();
            }
            if let Some(arr) = content.as_array() {
                return arr.iter()
                    .filter_map(|m| m.get("value").or(Some(m)).and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            format!("{result}")
        }
        "definition" => {
            if result.is_null() {
                return "Definition not found.".to_string();
            }
            format_locations(result)
        }
        "references" => {
            if result.is_null() {
                return "No references found.".to_string();
            }
            format_locations(result)
        }
        _ => format!("{result}"),
    }
}

fn format_locations(result: &Value) -> String {
    let locs: Vec<&Value> = if let Some(arr) = result.as_array() {
        arr.iter().collect()
    } else {
        vec![result]
    };

    if locs.is_empty() {
        return "No locations found.".to_string();
    }

    locs.iter().filter_map(|loc| {
        let uri = loc.get("uri")
            .or_else(|| loc.get("targetUri"))
            .and_then(|v| v.as_str())?;
        let range = loc.get("range")
            .or_else(|| loc.get("targetSelectionRange"))?;
        let line = range["start"]["line"].as_u64()? + 1;
        let col  = range["start"]["character"].as_u64()? + 1;
        let path = uri.strip_prefix("file://").unwrap_or(uri);
        Some(format!("{}:{}:{}", path, line, col))
    }).collect::<Vec<_>>().join("\n")
}

fn format_diagnostics(diags: &Value, file: &Path) -> String {
    let arr = match diags.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return format!("No diagnostics in {}", file.display()),
    };

    let mut lines = vec![format!("Diagnostics for {}:", file.display())];
    for d in arr {
        let severity = match d["severity"].as_u64() {
            Some(1) => "error",
            Some(2) => "warning",
            Some(3) => "info",
            Some(4) => "hint",
            _       => "note",
        };
        let line = d["range"]["start"]["line"].as_u64().unwrap_or(0) + 1;
        let col  = d["range"]["start"]["character"].as_u64().unwrap_or(0) + 1;
        let msg  = d["message"].as_str().unwrap_or("?");
        lines.push(format!("  {}:{} [{}] {}", line, col, severity, msg));
    }
    lines.join("\n")
}

// ── Tool impl ─────────────────────────────────────────────────────────────────

pub struct LspTool {
    cwd: CwdRef,
}

impl LspTool {
    pub fn new(cwd: CwdRef) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str { "lsp" }

    fn description(&self) -> &str {
        "Query a Language Server for code intelligence: hover info, go-to-definition, \
         find references, or diagnostics. Supported languages: Rust, Python, TypeScript, \
         JavaScript, Go, C/C++. Requires the language server to be installed."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["hover", "definition", "references", "diagnostics"],
                    "description": "hover: type/doc at position; definition: jump-to-def; references: find all usages; diagnostics: errors/warnings in file"
                },
                "file": {
                    "type": "string",
                    "description": "Path to the source file (absolute or relative to cwd)"
                },
                "line": {
                    "type": "integer",
                    "description": "Zero-based line number (required for hover/definition/references)"
                },
                "character": {
                    "type": "integer",
                    "description": "Zero-based character offset within the line (required for hover/definition/references)"
                },
                "timeout_seconds": {
                    "type": "integer",
                    "description": "Request timeout in seconds (default: 15, max: 60)",
                    "default": 15
                }
            },
            "required": ["operation", "file"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let operation = input["operation"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing field: operation"))?;
        let file_str = input["file"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing field: file"))?;
        let line = input["line"].as_u64().unwrap_or(0) as u32;
        let character = input["character"].as_u64().unwrap_or(0) as u32;
        let timeout_secs = input["timeout_seconds"].as_u64().unwrap_or(15).min(60);

        let file: PathBuf = {
            let raw = resolve_path(file_str, &self.cwd);
            raw.canonicalize().unwrap_or(raw)
        };

        // Use file's parent dir or cwd as root.
        let root: PathBuf = {
            let cwd_val = self.cwd.lock().unwrap().clone();
            cwd_val
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."))
        };

        lsp_query(&file, &root, operation, line, character, Duration::from_secs(timeout_secs)).await
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_rust_server() {
        let s = detect_server(Path::new("main.rs")).unwrap();
        assert_eq!(s.bin, "rust-analyzer");
    }

    #[test]
    fn detect_typescript_server() {
        let s = detect_server(Path::new("app.ts")).unwrap();
        assert_eq!(s.bin, "typescript-language-server");
    }

    #[test]
    fn detect_go_server() {
        let s = detect_server(Path::new("main.go")).unwrap();
        assert_eq!(s.bin, "gopls");
    }

    #[test]
    fn unknown_extension_returns_none() {
        assert!(detect_server(Path::new("file.xyz")).is_none());
    }

    #[test]
    fn format_diagnostics_empty() {
        let result = format_diagnostics(&json!([]), Path::new("test.rs"));
        assert!(result.contains("No diagnostics"));
    }

    #[test]
    fn format_diagnostics_with_errors() {
        let diags = json!([{
            "severity": 1,
            "message": "expected ;",
            "range": { "start": { "line": 4, "character": 8 }, "end": { "line": 4, "character": 9 } }
        }]);
        let result = format_diagnostics(&diags, Path::new("main.rs"));
        assert!(result.contains("error"));
        assert!(result.contains("expected ;"));
        assert!(result.contains("5:")); // 1-based
    }
}
