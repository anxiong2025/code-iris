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

pub fn configure_interactive() -> Result<()> {
    println!("Code Iris Configuration");
    println!();
    println!("API keys are read from environment variables or a local .env file.");
    println!("Current status:");

    for (label, env_key) in [
        ("Anthropic", "ANTHROPIC_API_KEY"),
        ("OpenAI", "OPENAI_API_KEY"),
        ("Google", "GOOGLE_API_KEY"),
        ("DeepSeek", "DEEPSEEK_API_KEY"),
        ("Groq", "GROQ_API_KEY"),
        ("OpenRouter", "OPENROUTER_API_KEY"),
    ] {
        let status = match std::env::var(env_key) {
            Ok(value) if !value.trim().is_empty() => "set",
            _ => "not set",
        };
        println!("  {label:<12} {status} ({env_key})");
    }

    let mut config = load_config().unwrap_or_default();

    print!(
        "Default provider [{}]: ",
        config.default_provider.as_deref().unwrap_or("anthropic")
    );
    io::stdout().flush().context("failed to flush stdout")?;
    let mut provider = String::new();
    io::stdin()
        .read_line(&mut provider)
        .context("failed to read provider")?;
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
    io::stdout().flush().context("failed to flush stdout")?;
    let mut model = String::new();
    io::stdin()
        .read_line(&mut model)
        .context("failed to read model")?;
    let model = model.trim();
    if !model.is_empty() {
        config.default_model = Some(model.to_string());
    }

    config.save()?;

    println!();
    println!(
        "Saved configuration to {}",
        config_dir()?.join("config.toml").display()
    );
    println!("Set API keys with environment variables such as `ANTHROPIC_API_KEY=...`.");
    Ok(())
}
