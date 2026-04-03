//! Slash command handling — extracted from the event loop.

use std::path::{Path, PathBuf};

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
                     /init                  scan project, generate .iris/instructions.md\n  \
                     /skills                list available skills\n  \
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
                let mut lines = Vec::new();
                // Show instruction layers.
                let home = dirs::home_dir().unwrap_or_default();
                let cwd = std::env::current_dir().unwrap_or_default();
                let global = home.join(".code-iris").join("instructions.md");
                let project = cwd.join(".iris").join("instructions.md");
                let local = cwd.join(".iris").join("instructions_local.md");

                lines.push("Instruction layers:".to_string());
                lines.push(format!("  Global  ~/.code-iris/instructions.md  {}",
                    if global.exists() { "✓" } else { "✗ (not found)" }));
                lines.push(format!("  Project .iris/instructions.md         {}",
                    if project.exists() { "✓" } else { "✗ (not found)" }));
                lines.push(format!("  Local   .iris/instructions_local.md   {}",
                    if local.exists() { "✓" } else { "✗ (not found)" }));
                lines.push(String::new());

                // Show notes.
                match iris_memory::load_notes() {
                    Ok(notes) if notes.is_empty() => lines.push("No notes. Use /memory <text> to add.".to_string()),
                    Ok(notes) => lines.push(format!("Notes:\n{notes}")),
                    Err(e) => lines.push(format!("Error: {e}")),
                }
                app.push_system(lines.join("\n"));
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

            "/init" => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                match generate_project_instructions(&cwd) {
                    Ok(path) => {
                        app.push_system(format!("Project scanned. Instructions written to {}", path.display()));
                        // Notify worker to reload instructions.
                        if tx_input.send(WorkerCmd::ResetCwd).await.is_err() {
                            app.push_system("Agent worker stopped unexpectedly.");
                        }
                    }
                    Err(e) => app.push_system(format!("Error: {e}")),
                }
            }

            "/skills" => {
                let skills = load_skills();
                if skills.is_empty() {
                    app.push_system("No skills found. Add .md files to .iris/skills/ or ~/.code-iris/skills/");
                } else {
                    let list: Vec<String> = skills.iter()
                        .map(|(name, desc)| format!("  /{name}  — {desc}"))
                        .collect();
                    app.push_system(format!("Available skills:\n{}", list.join("\n")));
                }
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
                // Check if it's a skill invocation.
                let (skill_name, skill_args) = if let Some(space) = cmd.find(' ') {
                    (&cmd[1..space], cmd[space+1..].trim())
                } else {
                    (&cmd[1..], "")
                };
                if let Some(prompt) = load_skill_prompt(skill_name, skill_args) {
                    app.push_user(cmd);
                    if tx_input.send(WorkerCmd::UserInput(prompt)).await.is_err() {
                        app.push_system("Agent worker stopped unexpectedly.");
                    }
                } else {
                    app.push_system(format!("Unknown command: {cmd}  (type /help)"));
                }
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

// ── /init — project scanner ─────────────────────────────────────────────────

/// Scan the project directory and generate `.iris/instructions.md`.
fn generate_project_instructions(root: &Path) -> anyhow::Result<PathBuf> {
    let iris_dir = root.join(".iris");
    std::fs::create_dir_all(&iris_dir)?;
    let path = iris_dir.join("instructions.md");

    let mut sections: Vec<String> = Vec::new();

    // Language & framework detection.
    let mut langs: Vec<&str> = Vec::new();
    let mut build_cmds: Vec<&str> = Vec::new();
    let mut test_cmds: Vec<&str> = Vec::new();

    if root.join("Cargo.toml").exists() {
        langs.push("Rust");
        build_cmds.push("cargo build");
        test_cmds.push("cargo test");
    }
    if root.join("package.json").exists() {
        langs.push("Node.js/TypeScript");
        if root.join("pnpm-lock.yaml").exists() {
            build_cmds.push("pnpm install && pnpm build");
            test_cmds.push("pnpm test");
        } else if root.join("yarn.lock").exists() {
            build_cmds.push("yarn install && yarn build");
            test_cmds.push("yarn test");
        } else {
            build_cmds.push("npm install && npm run build");
            test_cmds.push("npm test");
        }
    }
    if root.join("pyproject.toml").exists() || root.join("setup.py").exists() {
        langs.push("Python");
        if root.join("pyproject.toml").exists() {
            build_cmds.push("pip install -e .");
        }
        test_cmds.push("pytest");
    }
    if root.join("go.mod").exists() {
        langs.push("Go");
        build_cmds.push("go build ./...");
        test_cmds.push("go test ./...");
    }
    if root.join("Makefile").exists() {
        build_cmds.push("make");
    }

    // Project name from directory.
    let project_name = root.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());

    sections.push(format!("# Project: {project_name}\n"));

    if !langs.is_empty() {
        sections.push(format!("**Languages:** {}", langs.join(", ")));
    }

    // Detect frameworks.
    let mut frameworks: Vec<&str> = Vec::new();
    if root.join("next.config.js").exists() || root.join("next.config.ts").exists() || root.join("next.config.mjs").exists() {
        frameworks.push("Next.js");
    }
    if root.join("vite.config.ts").exists() || root.join("vite.config.js").exists() {
        frameworks.push("Vite");
    }
    if root.join("tailwind.config.js").exists() || root.join("tailwind.config.ts").exists() {
        frameworks.push("Tailwind CSS");
    }
    if !frameworks.is_empty() {
        sections.push(format!("**Frameworks:** {}", frameworks.join(", ")));
    }

    // Directory structure (top-level only).
    let mut dirs: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || matches!(name.as_str(), "node_modules" | "target" | "__pycache__" | "dist" | "build") {
                continue;
            }
            if entry.path().is_dir() {
                dirs.push(name);
            }
        }
    }
    dirs.sort();
    if !dirs.is_empty() {
        sections.push(format!("\n## Directory Structure\n\n```\n{}\n```", dirs.join("/\n") + "/"));
    }

    // Build & test commands.
    if !build_cmds.is_empty() {
        sections.push(format!("\n## Build\n\n```sh\n{}\n```", build_cmds.join("\n")));
    }
    if !test_cmds.is_empty() {
        sections.push(format!("\n## Test\n\n```sh\n{}\n```", test_cmds.join("\n")));
    }

    // Workspace detection.
    if root.join("Cargo.toml").exists() {
        if let Ok(content) = std::fs::read_to_string(root.join("Cargo.toml")) {
            if content.contains("[workspace]") {
                let mut members: Vec<String> = Vec::new();
                let crates_dir = root.join("crates");
                if crates_dir.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(&crates_dir) {
                        for entry in entries.flatten() {
                            if entry.path().join("Cargo.toml").exists() {
                                members.push(entry.file_name().to_string_lossy().to_string());
                            }
                        }
                    }
                }
                members.sort();
                if !members.is_empty() {
                    sections.push(format!(
                        "\n## Workspace Crates\n\n{}",
                        members.iter().map(|m| format!("- `crates/{m}`")).collect::<Vec<_>>().join("\n")
                    ));
                }
            }
        }
    }

    // Git info.
    if root.join(".git").exists() {
        sections.push("\n## Git\n\nThis is a git repository.".to_string());
        if let Ok(output) = std::process::Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(root)
            .output()
        {
            let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !remote.is_empty() {
                sections.push(format!("Remote: `{remote}`"));
            }
        }
    }

    let content = sections.join("\n");
    std::fs::write(&path, &content)?;

    Ok(path)
}

// ── Skills system ───────────────────────────────────────────────────────────

/// Discover available skills from `.iris/skills/` and `~/.code-iris/skills/`.
fn load_skills() -> Vec<(String, String)> {
    let mut skills = Vec::new();

    // Built-in skills.
    skills.push(("review".to_string(), "Code review the recent changes".to_string()));
    skills.push(("doc".to_string(), "Generate documentation for a file or function".to_string()));

    // User skills from directories.
    for dir in skill_dirs() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        let desc = read_skill_description(&path)
                            .unwrap_or_else(|| "Custom skill".to_string());
                        skills.push((stem.to_string(), desc));
                    }
                }
            }
        }
    }

    skills.sort_by(|a, b| a.0.cmp(&b.0));
    skills.dedup_by(|a, b| a.0 == b.0);
    skills
}

/// Load a skill's prompt template and expand with arguments.
fn load_skill_prompt(name: &str, args: &str) -> Option<String> {
    // Built-in skills.
    let builtin = match name {
        "review" => Some(
            "Review the recent code changes. Look at `git diff` and provide:\n\
             1. Summary of changes\n\
             2. Potential bugs or issues\n\
             3. Suggestions for improvement\n\
             Be concise and actionable.".to_string()
        ),
        "doc" => {
            let target = if args.is_empty() { "the current project" } else { args };
            Some(format!(
                "Generate documentation for {target}. Include:\n\
                 1. Overview / purpose\n\
                 2. Usage examples\n\
                 3. API reference (if applicable)\n\
                 Output as Markdown."
            ))
        }
        _ => None,
    };
    if builtin.is_some() {
        return builtin;
    }

    // User-defined skills from .md files.
    for dir in skill_dirs() {
        let path = dir.join(format!("{name}.md"));
        if let Ok(content) = std::fs::read_to_string(&path) {
            // Replace {{args}} placeholder with actual arguments.
            let prompt = content.replace("{{args}}", args);
            return Some(prompt);
        }
    }

    None
}

/// Read the first line (after optional frontmatter) as skill description.
fn read_skill_description(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    // Skip frontmatter if present.
    let start = if lines.first() == Some(&"---") {
        lines.iter().skip(1).position(|l| *l == "---").map(|i| i + 2).unwrap_or(0)
    } else {
        0
    };
    // Find first non-empty line as description.
    for line in &lines[start..] {
        let trimmed = line.trim().trim_start_matches('#').trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Directories to search for skill files.
fn skill_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    // Global skills.
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".code-iris").join("skills"));
    }
    // Project-level skills.
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join(".iris").join("skills"));
    }
    dirs
}
