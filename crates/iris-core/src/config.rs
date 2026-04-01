use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub use iris_llm::McpServerConfig;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IrisConfig {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    /// MCP servers to connect on startup.
    ///
    /// Example in ~/.code-iris/config.toml:
    /// ```toml
    /// [[mcp_servers]]
    /// name = "filesystem"
    /// command = "npx"
    /// args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
    /// ```
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

impl IrisConfig {
    pub fn save(&self) -> Result<()> {
        let dir = config_dir()?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create config directory {}", dir.display()))?;

        let path = dir.join("config.toml");
        let content = toml::to_string_pretty(self).context("failed to serialize config")?;
        std::fs::write(&path, content)
            .with_context(|| format!("failed to write config to {}", path.display()))?;
        Ok(())
    }
}

pub fn config_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("unable to determine home directory")?;
    Ok(home.join(".code-iris"))
}

pub fn load_config() -> Result<IrisConfig> {
    let path = config_dir()?.join("config.toml");
    if !path.exists() {
        return Ok(IrisConfig::default());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read config from {}", path.display()))?;
    let config = toml::from_str(&content)
        .with_context(|| format!("failed to parse config from {}", path.display()))?;
    Ok(config)
}

/// Path to the per-user `.env` file managed by `iris configure`.
pub fn user_env_path() -> Result<PathBuf> {
    Ok(config_dir()?.join(".env"))
}

pub fn configure_interactive() -> Result<()> {
    use iris_llm::PROVIDERS;

    let dir = config_dir()?;
    let env_path = dir.join(".env");

    println!("iris configure\n");

    // ── Show provider status ──────────────────────────────────────────────────
    println!("{:<4} {:<20} {:<32} Status", "#", "Provider", "Env var");
    println!("{}", "-".repeat(70));
    for (i, p) in PROVIDERS.iter().enumerate() {
        let status = if std::env::var(p.env_key).map(|v| !v.trim().is_empty()).unwrap_or(false) {
            "\x1b[32m✓ set\x1b[0m"
        } else {
            "\x1b[90m✗ not set\x1b[0m"
        };
        println!("{:<4} {:<20} {:<32} {}", i + 1, p.name, p.env_key, status);
    }

    println!();
    if env_path.exists() {
        println!("Existing keys file: {}", env_path.display());
    }
    println!("Keys can also be set as environment variables or in a shell .env file.");
    println!();

    // ── Optionally save an API key ────────────────────────────────────────────
    print!("Save an API key to {} ? [y/N]: ", env_path.display());
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if answer.trim().to_lowercase() == "y" {
        print!("Provider number (1-{}): ", PROVIDERS.len());
        io::stdout().flush()?;
        let mut num_str = String::new();
        io::stdin().read_line(&mut num_str)?;
        if let Ok(n) = num_str.trim().parse::<usize>() {
            if n >= 1 && n <= PROVIDERS.len() {
                let p = &PROVIDERS[n - 1];
                print!("{} (will be saved to file, not echoed): ", p.env_key);
                io::stdout().flush()?;
                let mut key = String::new();
                io::stdin().read_line(&mut key)?;
                let key = key.trim();
                if !key.is_empty() {
                    append_env_file(&env_path, p.env_key, key)?;
                    // Apply immediately for the rest of this configure run.
                    // SAFETY: configure_interactive is single-threaded by design.
                    unsafe { std::env::set_var(p.env_key, key); }
                    println!("Saved to {}", env_path.display());
                } else {
                    println!("Empty — skipped.");
                }
            } else {
                println!("Invalid number — skipped.");
            }
        }
        println!();
    }

    // ── Default provider / model ──────────────────────────────────────────────
    let mut config = load_config().unwrap_or_default();

    print!(
        "Default provider [{}]: ",
        config.default_provider.as_deref().unwrap_or("anthropic")
    );
    io::stdout().flush()?;
    let mut provider = String::new();
    io::stdin().read_line(&mut provider)?;
    let provider = provider.trim();
    if !provider.is_empty() {
        config.default_provider = Some(provider.to_string());
    } else if config.default_provider.is_none() {
        config.default_provider = Some("anthropic".to_string());
    }

    print!(
        "Default model [{}]: ",
        config.default_model.as_deref().unwrap_or("provider default")
    );
    io::stdout().flush()?;
    let mut model = String::new();
    io::stdin().read_line(&mut model)?;
    let model = model.trim();
    if !model.is_empty() {
        config.default_model = Some(model.to_string());
    }

    config.save()?;

    println!();
    println!("Configuration saved to {}",  dir.join("config.toml").display());
    println!(
        "Run `iris chat` to start a session.\n\
         Tip: add `source {}` to your shell profile to auto-load keys.",
        env_path.display()
    );
    Ok(())
}

/// Append or overwrite a single `KEY=value` line in the .env file.
fn append_env_file(path: &std::path::Path, key: &str, value: &str) -> Result<()> {
    // Read existing lines, replace if key already present.
    let existing = if path.exists() {
        std::fs::read_to_string(path).unwrap_or_default()
    } else {
        String::new()
    };

    let prefix = format!("{key}=");
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| !l.starts_with(&prefix))
        .map(|l| l.to_string())
        .collect();
    lines.push(format!("{key}={value}"));

    std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")))?;
    std::fs::write(path, lines.join("\n") + "\n")
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}
