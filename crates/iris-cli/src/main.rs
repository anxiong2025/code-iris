//! `iris` — See through your code.
//!
//! A Rust-native AI coding agent and project analyser.
//!
//! ```text
//! iris scan [path]        — project manifest
//! iris arch [path]        — architecture report
//! iris deps [path]        — dependency graph
//! iris stats [path]       — statistics
//! iris configure          — interactive API-key setup
//! iris models             — list all LLM providers
//! iris chat               — interactive AI agent (streaming)
//! ```

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use iris_core::agent::Agent;
use iris_core::config::{configure_interactive, load_config, user_env_path};
use iris_core::coordinator::{Coordinator, SubTask};
use iris_core::memory as iris_memory;
use iris_core::context::{compress, ContextConfig};
use iris_core::permissions::PermissionMode;
use iris_core::reporter::Reporter;
use iris_core::storage::Storage;
use iris_llm::{clear_credentials, login, PROVIDERS};

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Debug, Parser)]
#[command(
    name = "iris",
    version,
    about = "See through your code — Rust-powered AI coding agent",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Scan a project and print a structured manifest.
    Scan {
        /// Path to the project root (defaults to current directory).
        path: Option<PathBuf>,
    },

    /// Generate a full architecture report.
    Arch {
        /// Path to the project root.
        path: Option<PathBuf>,
        /// Write report to a file instead of stdout.
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
    },

    /// Analyse and print module dependencies.
    Deps {
        path: Option<PathBuf>,
    },

    /// Print project statistics.
    Stats {
        path: Option<PathBuf>,
    },

    /// Interactive API-key configuration wizard.
    Configure,

    /// Login with your Claude.ai account (OAuth — no API key required).
    Login,

    /// Logout and remove stored OAuth credentials.
    Logout,

    /// List all supported LLM providers and their configuration status.
    Models,

    /// Run a multi-agent coordinator task (parallel sub-agents + synthesis).
    Run {
        /// High-level goal for the coordinator.
        prompt: String,
        /// Sub-task in "label:prompt" format (repeatable). If omitted, runs a single agent.
        #[arg(short, long = "sub", value_name = "LABEL:PROMPT")]
        subs: Vec<String>,
        /// Override the model used for all sub-agents and synthesis.
        #[arg(short, long)]
        model: Option<String>,
    },

    /// Start an interactive AI agent session (streaming).
    Chat {
        /// Override the model (e.g. `claude-opus-4-6-20250514`).
        #[arg(short, long)]
        model: Option<String>,
        /// Resume a previous session by ID.
        #[arg(short = 'r', long, value_name = "SESSION_ID")]
        resume: Option<String>,
        /// Skip permission prompts for all tools (use with care).
        #[arg(long)]
        auto: bool,
        /// Read-only mode — deny any tool that writes or executes.
        #[arg(long)]
        plan: bool,
    },
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    // Also load ~/.code-iris/.env (keys saved via `iris configure`).
    if let Ok(env_path) = user_env_path() {
        let _ = dotenvy::from_path(env_path);
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(io::stderr)
        .try_init()
        .ok();

    match Cli::parse().command {
        Command::Scan { path } => cmd_scan(resolve_path(path))?,
        Command::Arch { path, output } => cmd_arch(resolve_path(path), output)?,
        Command::Deps { path } => cmd_deps(resolve_path(path))?,
        Command::Stats { path } => cmd_stats(resolve_path(path))?,
        Command::Configure => configure_interactive()?,
        Command::Login => cmd_login().await?,
        Command::Logout => cmd_logout()?,
        Command::Models => cmd_models(),
        Command::Run { prompt, subs, model } => cmd_run(prompt, subs, model).await?,
        Command::Chat { model, resume, auto, plan } => {
            cmd_chat(model, resume, auto, plan).await?
        }
    }

    Ok(())
}

// ── Subcommand implementations ────────────────────────────────────────────────

fn cmd_scan(path: PathBuf) -> Result<()> {
    let reporter = Reporter::from_path(&path)?;
    println!("{}", reporter.render_manifest());
    Ok(())
}

fn cmd_arch(path: PathBuf, output: Option<PathBuf>) -> Result<()> {
    let reporter = Reporter::from_path(&path)?;
    let report = reporter.render_full_report();
    match output {
        Some(out_path) => {
            std::fs::write(&out_path, &report)
                .with_context(|| format!("Failed to write to {}", out_path.display()))?;
            eprintln!("Report written to {}", out_path.display());
        }
        None => println!("{report}"),
    }
    Ok(())
}

fn cmd_deps(path: PathBuf) -> Result<()> {
    let reporter = Reporter::from_path(&path)?;
    println!("{}", reporter.render_dependencies());
    Ok(())
}

fn cmd_stats(path: PathBuf) -> Result<()> {
    let reporter = Reporter::from_path(&path)?;
    println!("{}", reporter.render_stats());
    Ok(())
}

fn cmd_models() {
    let config = load_config().ok();
    let default_provider = config
        .as_ref()
        .and_then(|c| c.default_provider.as_deref())
        .unwrap_or("none");

    println!("Supported LLM Providers\n");
    println!("{:<20} {:<35} {}", "Provider", "Label", "Status");
    println!("{}", "-".repeat(75));

    for p in PROVIDERS {
        let configured = std::env::var(p.env_key)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        let status = if configured {
            format!("\x1b[32m✓\x1b[0m  ({})", p.env_key)
        } else {
            format!("\x1b[90m✗\x1b[0m  ({})", p.env_key)
        };
        let marker = if p.name == default_provider { " ◀ default" } else { "" };
        println!("{:<20} {:<35} {}{}", p.name, p.label, status, marker);
    }

    println!("\nTotal: {} providers", PROVIDERS.len());
}

async fn cmd_run(
    prompt: String,
    subs: Vec<String>,
    model: Option<String>,
) -> Result<()> {
    let mut coord = Coordinator::from_env();
    if let Some(m) = model {
        coord = coord.with_model(m);
    }

    if subs.is_empty() {
        // Single-agent one-shot mode.
        let tasks = vec![SubTask {
            label: "main".to_string(),
            system_prompt: String::new(),
            prompt: prompt.clone(),
        }];
        eprintln!("Running single agent task…");
        let results = coord.run(tasks).await?;
        if let Some(r) = results.into_iter().next() {
            println!("{}", r.response.text);
            eprintln!("\x1b[90m[tokens in={} out={}]\x1b[0m",
                r.response.usage.input_tokens, r.response.usage.output_tokens);
        }
        return Ok(());
    }

    // Multi-agent mode: parse "label:prompt" pairs.
    let tasks: Vec<SubTask> = subs
        .iter()
        .map(|s| {
            if let Some((label, sub_prompt)) = s.split_once(':') {
                SubTask {
                    label: label.trim().to_string(),
                    system_prompt: String::new(),
                    prompt: sub_prompt.trim().to_string(),
                }
            } else {
                SubTask {
                    label: s.clone(),
                    system_prompt: String::new(),
                    prompt: s.clone(),
                }
            }
        })
        .collect();

    eprintln!("Running {} sub-agents in parallel…", tasks.len());

    let synthesis_template = format!(
        "{prompt}\n\nSub-agent results:\n\n{{results}}\n\nPlease synthesise a final answer."
    );

    let response = coord.run_with_synthesis(tasks, &synthesis_template).await?;

    println!("{}", response.text);
    eprintln!(
        "\x1b[90m[tokens in={} out={}]\x1b[0m",
        response.usage.input_tokens, response.usage.output_tokens
    );

    Ok(())
}

async fn cmd_chat(
    model: Option<String>,
    resume: Option<String>,
    auto: bool,
    plan: bool,
) -> Result<()> {
    let mut agent = Agent::from_env()
        .context("No API key found. Set ANTHROPIC_API_KEY / DASHSCOPE_API_KEY / DEEPSEEK_API_KEY etc., or run `iris configure`.")?;

    if let Some(m) = model {
        agent = agent.with_model(m);
    }

    let perm = if auto {
        PermissionMode::Auto
    } else if plan {
        PermissionMode::Plan
    } else {
        PermissionMode::Default
    };
    agent = agent.with_permissions(perm.clone());

    if let Some(session_id) = resume {
        let storage = Storage::new()?;
        let session = storage
            .load(&session_id)
            .with_context(|| format!("Session `{session_id}` not found"))?;
        let msg_count = session.messages.len();
        agent = agent.with_session(session);
        eprintln!("Resumed session {session_id} ({msg_count} messages)");
    }

    let mode_label = match &perm {
        PermissionMode::Auto => " \x1b[31m[auto — all tools approved]\x1b[0m",
        PermissionMode::Plan => " \x1b[33m[plan — read-only]\x1b[0m",
        _ => "",
    };
    eprintln!(
        "\x1b[1miris\x1b[0m · session {}{}\n\
         /help for commands · Ctrl-D to exit\n",
        agent.session.id, mode_label
    );

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    loop {
        print!("\x1b[32m❯\x1b[0m ");
        io::stdout().flush()?;

        let Some(line) = lines.next() else { break };
        let input = line.context("stdin read error")?;
        let input = input.trim().to_string();

        if input.is_empty() {
            continue;
        }

        if let Some(result) = handle_slash_command(&input, &mut agent) {
            if result == "__quit__" {
                eprintln!("Session saved: {}", agent.session.id);
                break;
            }
            println!("{result}");
            continue;
        }

        print!("\x1b[33miris\x1b[0m  ");
        io::stdout().flush()?;

        let response = agent
            .chat_streaming(&input, |chunk| {
                print!("{chunk}");
                io::stdout().flush().ok();
            })
            .await?;

        if !response.text.ends_with('\n') {
            println!();
        }
        if !response.tool_calls.is_empty() {
            eprintln!("\x1b[90m  [tools: {}]\x1b[0m", response.tool_calls.join(", "));
        }
        eprintln!(
            "\x1b[90m  [tokens in={} out={}]\x1b[0m\n",
            response.usage.input_tokens, response.usage.output_tokens
        );
    }

    Ok(())
}

// ── Slash command handler ─────────────────────────────────────────────────────

fn handle_slash_command(input: &str, agent: &mut Agent) -> Option<String> {
    if !input.starts_with('/') {
        return None;
    }

    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let cmd = parts[0];

    Some(match cmd {
        "/quit" | "/exit" | "/q" => "__quit__".to_string(),

        "/session" => format!("Session: {}", agent.session.id),

        "/messages" | "/history" => {
            format!("{} messages in session", agent.session.messages.len())
        }

        "/compact" => {
            let before = agent.session.messages.len();
            let cfg = ContextConfig::default();
            compress(&mut agent.session.messages, &cfg);
            let after = agent.session.messages.len();
            format!("Compacted: {before} → {after} messages")
        }

        "/clear" => {
            agent.session.messages.clear();
            "Conversation cleared.".to_string()
        }

        "/memory" => {
            match iris_memory::load_notes() {
                Ok(notes) if notes.is_empty() => "No notes saved yet. Use /memory <text> to add one.".to_string(),
                Ok(notes) => format!("Notes:\n{notes}"),
                Err(e) => format!("Error reading notes: {e}"),
            }
        }

        "/commit" => {
            match std::process::Command::new("git").args(["status", "--short"]).output() {
                Ok(o) => {
                    let s = String::from_utf8_lossy(&o.stdout);
                    if s.trim().is_empty() {
                        "Nothing to commit (working tree clean).".to_string()
                    } else {
                        format!("Changes:\n{s}\nUse /commit <message> to commit.")
                    }
                }
                Err(e) => format!("git error: {e}"),
            }
        }

        "/pwd" => {
            let cwd = agent.cwd.lock().unwrap().clone();
            match cwd {
                Some(p) => format!("cwd: {}", p.display()),
                None => std::env::current_dir()
                    .map(|p| format!("cwd: {}", p.display()))
                    .unwrap_or_else(|_| "cwd: unknown".to_string()),
            }
        }

        "/help" => "\
Slash commands:
  /quit, /q            Exit (session auto-saved)
  /session             Print session ID
  /messages            Show message count
  /compact             Run context compression now
  /clear               Clear conversation history
  /commit [message]    git add -A && git commit (no msg shows status)
  /memory [note]       Save or show notes
  /pwd                 Show current working directory
  /cd <path>           Set working directory for tool calls
  /worktree <branch>   Create a git worktree and cd into it
  /help                Show this help"
            .to_string(),

        other if other.starts_with("/memory ") => {
            let note = other.trim_start_matches("/memory ").trim();
            match iris_memory::add_note(note) {
                Ok(()) => format!("Note saved: {note}"),
                Err(e) => format!("Error saving note: {e}"),
            }
        }

        other if other.starts_with("/commit ") => {
            let msg = other.trim_start_matches("/commit ").trim();
            let result = std::process::Command::new("git")
                .args(["add", "-A"])
                .output()
                .and_then(|_| {
                    std::process::Command::new("git")
                        .args(["commit", "-m", msg])
                        .output()
                });
            match result {
                Ok(o) => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    let out = if !stdout.trim().is_empty() { stdout } else { stderr };
                    format!("git commit:\n{}", out.trim())
                }
                Err(e) => format!("git error: {e}"),
            }
        }

        other if other.starts_with("/cd ") => {
            let path_str = other.trim_start_matches("/cd ").trim();
            let path = std::path::PathBuf::from(path_str);
            if path.is_dir() {
                let abs = path.canonicalize().unwrap_or(path);
                *agent.cwd.lock().unwrap() = Some(abs.clone());
                format!("cwd: {}", abs.display())
            } else {
                format!("Not a directory: {path_str}")
            }
        }

        "/cd" => {
            *agent.cwd.lock().unwrap() = None;
            "Working directory reset.".to_string()
        }

        other if other.starts_with("/worktree ") => {
            let branch = other.trim_start_matches("/worktree ").trim();
            let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
            let wt_path = home.join(".code-iris").join("worktrees").join(branch);
            match std::process::Command::new("git")
                .args(["worktree", "add", &wt_path.to_string_lossy(), "-b", branch])
                .output()
            {
                Ok(o) if o.status.success() => {
                    *agent.cwd.lock().unwrap() = Some(wt_path.clone());
                    format!("Worktree created → {}", wt_path.display())
                }
                Ok(o) => {
                    let e = String::from_utf8_lossy(&o.stderr);
                    format!("git worktree add failed: {}", e.trim())
                }
                Err(e) => format!("git error: {e}"),
            }
        }

        other => format!("Unknown command: {other}  (try /help)"),
    })
}

async fn cmd_login() -> Result<()> {
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .build()
        .context("failed to build HTTP client")?;
    let tokens = login(&client).await?;
    println!("\nLogged in successfully.");
    println!("Access token expires at: {} (unix)", tokens.expires_at);
    println!("Run `iris chat` to start a session — no API key needed.");
    Ok(())
}

fn cmd_logout() -> Result<()> {
    clear_credentials()?;
    println!("Logged out. OAuth credentials removed.");
    Ok(())
}

// ── Utility ───────────────────────────────────────────────────────────────────

fn resolve_path(path: Option<PathBuf>) -> PathBuf {
    path.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}
