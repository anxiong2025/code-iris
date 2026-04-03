//! Slash command handling — extracted from the event loop.

use tokio::sync::mpsc;

use crate::app::{AgentState, App};
use crate::buddy;
use crate::WorkerCmd;
use iris_core::memory as iris_memory;
use iris_core::storage::Storage;

pub async fn handle_user_input(app: &mut App, input: String, tx_input: &mpsc::Sender<WorkerCmd>) {
    // ── Slash commands ──────────────────────────────────────────────────────
    if input.starts_with('/') {
        let cmd = input.trim();
        match cmd {
            "/help" => {
                app.push_system(
                    "/help  /clear  /session  /sessions  /resume <id>\n  \
                     /model [name]  /compact  /commit [msg]  /memory [note]\n  \
                     /cd <path>  /pwd  /worktree <branch>\n  \
                     /agents                list available agent types\n  \
                     /plan <prompt>         run 3-step product→arch→impl pipeline\n  \
                     exit|quit"
                );
            }
            "/clear" => {
                app.chat_history.clear();
                app.scroll_offset = 0;
                app.push_system("Chat history cleared.");
            }
            "/session" => {
                if let Some(id) = &app.session_id {
                    app.push_system(format!("Session: {id}"));
                } else {
                    app.push_system("No active session.");
                }
            }
            "/model" => {
                app.push_system(format!("Current model: {}", app.model_name));
            }
            "/sessions" => {
                match Storage::new().and_then(|s| s.list()) {
                    Ok(ids) if ids.is_empty() => app.push_system("No saved sessions."),
                    Ok(ids) => {
                        let list = ids.join("\n  ");
                        app.push_system(format!("Saved sessions:\n  {list}"));
                    }
                    Err(e) => app.push_system(format!("Error listing sessions: {e}")),
                }
            }
            "/compact" => {
                if tx_input.send(WorkerCmd::Compact).await.is_err() {
                    app.push_system("Agent worker stopped unexpectedly.");
                }
            }
            "/memory" => {
                match iris_memory::load_notes() {
                    Ok(notes) if notes.is_empty() => app.push_system("No notes saved yet. Use /memory <text> to add one."),
                    Ok(notes) => app.push_system(format!("Notes:\n{notes}")),
                    Err(e) => app.push_system(format!("Error reading notes: {e}")),
                }
            }
            "/pwd" => {
                if tx_input.send(WorkerCmd::ResetCwd).await.is_err() {}
                let cwd = std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "unknown".to_string());
                app.push_system(format!("Process cwd: {cwd}"));
            }
            "/commit" => {
                let output = std::process::Command::new("git")
                    .args(["status", "--short"])
                    .output();
                match output {
                    Ok(o) => {
                        let s = String::from_utf8_lossy(&o.stdout);
                        if s.trim().is_empty() {
                            app.push_system("Nothing to commit (working tree clean).");
                        } else {
                            app.push_system(format!("Staged/unstaged changes:\n{s}\nUse /commit <message> to commit."));
                        }
                    }
                    Err(e) => app.push_system(format!("git error: {e}")),
                }
            }
            _ if cmd.starts_with("/memory ") => {
                let note = cmd.trim_start_matches("/memory ").trim();
                match iris_memory::add_note(note) {
                    Ok(()) => app.push_system(format!("Note saved: {note}")),
                    Err(e) => app.push_system(format!("Error saving note: {e}")),
                }
            }
            _ if cmd.starts_with("/commit ") => {
                let msg = cmd.trim_start_matches("/commit ").trim().to_string();
                let output = std::process::Command::new("git")
                    .args(["add", "-A"])
                    .output()
                    .and_then(|_| {
                        std::process::Command::new("git")
                            .args(["commit", "-m", &msg])
                            .output()
                    });
                match output {
                    Ok(o) => {
                        let stdout = String::from_utf8_lossy(&o.stdout);
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        let out = if !stdout.trim().is_empty() { stdout.as_ref() } else { stderr.as_ref() };
                        app.push_system(format!("git commit:\n{}", out.trim()));
                    }
                    Err(e) => app.push_system(format!("git error: {e}")),
                }
            }
            _ if cmd.starts_with("/cd ") => {
                let path_str = cmd.trim_start_matches("/cd ").trim();
                let path = std::path::PathBuf::from(path_str);
                if path.is_dir() {
                    let abs = path.canonicalize().unwrap_or(path);
                    if tx_input.send(WorkerCmd::SetCwd(abs)).await.is_err() {
                        app.push_system("Agent worker stopped unexpectedly.");
                    }
                } else {
                    app.push_system(format!("Not a directory: {path_str}"));
                }
            }
            "/cd" => {
                if tx_input.send(WorkerCmd::ResetCwd).await.is_err() {
                    app.push_system("Agent worker stopped unexpectedly.");
                }
            }
            _ if cmd.starts_with("/worktree ") => {
                let branch = cmd.trim_start_matches("/worktree ").trim().to_string();
                let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
                let wt_path = home.join(".code-iris").join("worktrees").join(&branch);
                let create = std::process::Command::new("git")
                    .args(["worktree", "add", &wt_path.to_string_lossy(), "-b", &branch])
                    .output();
                match create {
                    Ok(o) if o.status.success() => {
                        if tx_input.send(WorkerCmd::SetCwd(wt_path.clone())).await.is_err() {
                            app.push_system("Agent worker stopped unexpectedly.");
                        } else {
                            app.push_system(format!("Worktree created and cwd set to {}", wt_path.display()));
                        }
                    }
                    Ok(o) => {
                        let e = String::from_utf8_lossy(&o.stderr);
                        app.push_system(format!("git worktree add failed: {}", e.trim()));
                    }
                    Err(e) => app.push_system(format!("git error: {e}")),
                }
            }
            "/agents" => {
                use iris_core::agent_def::{builtin_agents, load_custom_agents, SandboxMode};
                let mut lines = vec!["Available agent types:\n".to_string()];
                let custom = load_custom_agents(None);
                if !custom.is_empty() {
                    lines.push("  Custom:".to_string());
                    for def in &custom {
                        let mode = match def.sandbox_mode {
                            SandboxMode::ReadOnly => "read-only",
                            SandboxMode::Full => "full",
                        };
                        let model_hint = def.model.as_deref().unwrap_or("inherit");
                        lines.push(format!("  • {} [{}] ({}) — {}", def.name, mode, model_hint, def.description));
                    }
                    lines.push(String::new());
                }
                lines.push("  Built-in:".to_string());
                for def in builtin_agents() {
                    let mode = match def.sandbox_mode {
                        SandboxMode::ReadOnly => "read-only",
                        SandboxMode::Full => "full",
                    };
                    let model_hint = def.model.as_deref().unwrap_or("inherit");
                    lines.push(format!("  • {} [{}] ({}) — {}", def.name, mode, model_hint, def.description));
                }
                lines.push(String::new());
                lines.push("Use in CLI: iris run --pipeline --sub \"step@explorer:prompt\"".to_string());
                app.push_system(lines.join("\n"));
            }

            "/buddy" => {
                let (companion, _seed) = buddy::roll_and_save();
                let card = buddy::format_reveal_card(&companion);
                app.buddy = Some(companion);
                app.push_system(card);
            }

            _ if cmd.starts_with("/plan ") => {
                let prompt = cmd.trim_start_matches("/plan ").trim().to_string();
                if prompt.is_empty() {
                    app.push_system("Usage: /plan <requirement>");
                } else {
                    app.push_user(format!("/plan {prompt}"));
                    if tx_input.send(WorkerCmd::Plan(prompt)).await.is_err() {
                        app.push_system("Agent worker stopped unexpectedly.");
                    }
                }
            }

            _ if cmd.starts_with("/resume ") => {
                let id = cmd.trim_start_matches("/resume ").trim().to_string();
                if tx_input.send(WorkerCmd::LoadSession(id)).await.is_err() {
                    app.push_system("Agent worker stopped unexpectedly.");
                }
            }
            _ if cmd.starts_with("/model ") => {
                let m = cmd.trim_start_matches("/model ").trim().to_string();
                if tx_input.send(WorkerCmd::SetModel(m)).await.is_err() {
                    app.push_system("Agent worker stopped unexpectedly.");
                }
            }
            _ => {
                app.push_system(format!("Unknown command: {cmd}  (type /help)"));
            }
        }
        return;
    }

    if !app.has_api_key {
        app.push_system("No API key found — set ANTHROPIC_API_KEY or DASHSCOPE_API_KEY etc.");
        return;
    }
    app.push_user(&input);
    if tx_input.send(WorkerCmd::UserInput(input)).await.is_err() {
        app.push_system("Agent worker stopped unexpectedly.");
        app.agent_state = AgentState::Idle;
    }
}
