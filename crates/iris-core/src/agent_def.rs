//! Agent definitions — built-in agent types and custom TOML-defined agents.
//!
//! Built-in agents mirror the Codex `explorer / worker / reviewer` taxonomy:
//!
//! | Name       | Sandbox     | Model       | Purpose                          |
//! |------------|-------------|-------------|----------------------------------|
//! | `explorer` | read-only   | haiku/fast  | Code reading, search, analysis   |
//! | `worker`   | full        | main model  | Implementation, file writes      |
//! | `reviewer` | read-only   | main model  | Code review, risk, correctness   |
//!
//! Custom agents are loaded from TOML files:
//! - Project-level: `.iris/agents/<name>.toml`
//! - User-level:    `~/.code-iris/agents/<name>.toml`
//!
//! Custom definitions override built-ins when names collide.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::permissions::PermissionMode;

// ── SandboxMode ───────────────────────────────────────────────────────────────

/// Whether the agent may write files / execute commands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    /// Agent may only read files and run non-destructive tools (PermissionMode::Plan).
    ReadOnly,
    /// Agent has full tool access including writes and bash (PermissionMode::Auto).
    #[default]
    Full,
}

impl SandboxMode {
    pub fn to_permission_mode(&self) -> PermissionMode {
        match self {
            SandboxMode::ReadOnly => PermissionMode::Plan,
            SandboxMode::Full => PermissionMode::Auto,
        }
    }
}

// ── AgentDefinition ───────────────────────────────────────────────────────────

/// A named agent configuration that can be referenced in pipelines and coordinators.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    /// Unique identifier used to reference this agent (e.g. `"explorer"`).
    pub name: String,
    /// Human-readable description of what this agent does.
    pub description: String,
    /// System prompt / developer instructions injected at the start of each run.
    #[serde(default)]
    pub instructions: String,
    /// Model override. `None` means inherit from the coordinator/caller.
    #[serde(default)]
    pub model: Option<String>,
    /// Tool access level.
    #[serde(default)]
    pub sandbox_mode: SandboxMode,
}

impl AgentDefinition {
    pub fn permission_mode(&self) -> PermissionMode {
        self.sandbox_mode.to_permission_mode()
    }
}

// ── Built-in agents ───────────────────────────────────────────────────────────

/// Read-only explorer — fast codebase navigation.
pub fn builtin_explorer() -> AgentDefinition {
    AgentDefinition {
        name: "explorer".to_string(),
        description: "Read-only codebase explorer for gathering evidence before changes are proposed.".to_string(),
        instructions: "\
You are a read-only code exploration agent.
Your job is to thoroughly understand the codebase: trace execution paths, identify relevant \
files, symbols, and data flows. Cite exact file paths and line numbers in your findings.
Never propose code changes or write files — only read and analyse.
Prefer targeted reads over broad scans. Be concise and structured in your output.".to_string(),
        model: Some("claude-haiku-4-5-20251001".to_string()),
        sandbox_mode: SandboxMode::ReadOnly,
    }
}

/// Full-permission worker — implementation and fixes.
pub fn builtin_worker() -> AgentDefinition {
    AgentDefinition {
        name: "worker".to_string(),
        description: "Execution-focused agent for implementation, file edits, and fixes.".to_string(),
        instructions: "\
You are an implementation agent.
Your job is to carry out concrete code changes efficiently and correctly.
Make the smallest defensible change to accomplish the task.
Keep unrelated files untouched. After making changes, verify correctness.".to_string(),
        model: None, // inherit from coordinator
        sandbox_mode: SandboxMode::Full,
    }
}

/// Read-only reviewer — correctness, security, risk analysis.
pub fn builtin_reviewer() -> AgentDefinition {
    AgentDefinition {
        name: "reviewer".to_string(),
        description: "Code reviewer focused on correctness, security, and missing tests.".to_string(),
        instructions: "\
You are a senior code reviewer.
Prioritise correctness, security vulnerabilities, behaviour regressions, and missing test coverage.
Lead with concrete findings and include reproduction steps where possible.
Avoid style-only comments unless they hide a real bug.
Never make code changes — only report findings.".to_string(),
        model: None, // inherit from coordinator
        sandbox_mode: SandboxMode::ReadOnly,
    }
}

/// All built-in agent definitions.
pub fn builtin_agents() -> Vec<AgentDefinition> {
    vec![builtin_explorer(), builtin_worker(), builtin_reviewer()]
}

// ── TOML loading ──────────────────────────────────────────────────────────────

/// Search paths for agent definition files (highest priority first):
/// 1. `.iris/agents/` relative to `project_root`
/// 2. `~/.code-iris/agents/`
fn agent_search_paths(project_root: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(root) = project_root {
        paths.push(root.join(".iris").join("agents"));
    }

    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".code-iris").join("agents"));
    }

    paths
}

fn load_toml_agent(path: &Path) -> Option<AgentDefinition> {
    let content = std::fs::read_to_string(path).ok()?;
    toml::from_str(&content)
        .map_err(|e| tracing::warn!(path = %path.display(), "invalid agent TOML: {e}"))
        .ok()
}

/// Load all custom agent definitions visible from `project_root`.
///
/// Files in higher-priority directories shadow files with the same name
/// in lower-priority directories.
pub fn load_custom_agents(project_root: Option<&Path>) -> Vec<AgentDefinition> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut agents = Vec::new();

    for dir in agent_search_paths(project_root) {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            if let Some(def) = load_toml_agent(&path) {
                if seen.insert(def.name.clone()) {
                    agents.push(def);
                }
            }
        }
    }

    agents
}

/// Find an agent by name.
///
/// Lookup order:
/// 1. Custom agents (project-level `.iris/agents/` → user-level `~/.code-iris/agents/`)
/// 2. Built-in agents (`explorer`, `worker`, `reviewer`)
pub fn find_agent(name: &str, project_root: Option<&Path>) -> Option<AgentDefinition> {
    // Custom first (allows overriding built-ins).
    for def in load_custom_agents(project_root) {
        if def.name == name {
            return Some(def);
        }
    }
    // Fall back to built-ins.
    builtin_agents().into_iter().find(|a| a.name == name)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_have_correct_sandbox() {
        assert_eq!(builtin_explorer().sandbox_mode, SandboxMode::ReadOnly);
        assert_eq!(builtin_worker().sandbox_mode, SandboxMode::Full);
        assert_eq!(builtin_reviewer().sandbox_mode, SandboxMode::ReadOnly);
    }

    #[test]
    fn permission_mode_mapping() {
        assert!(matches!(builtin_explorer().permission_mode(), PermissionMode::Plan));
        assert!(matches!(builtin_worker().permission_mode(), PermissionMode::Auto));
        assert!(matches!(builtin_reviewer().permission_mode(), PermissionMode::Plan));
    }

    #[test]
    fn find_builtin_by_name() {
        let def = find_agent("explorer", None).unwrap();
        assert_eq!(def.name, "explorer");
        assert_eq!(def.sandbox_mode, SandboxMode::ReadOnly);
    }

    #[test]
    fn unknown_agent_returns_none() {
        assert!(find_agent("nonexistent-xyz", None).is_none());
    }

    #[test]
    fn all_builtins_have_names_and_instructions() {
        for def in builtin_agents() {
            assert!(!def.name.is_empty(), "name empty for {:?}", def.name);
            assert!(!def.instructions.is_empty(), "instructions empty for {}", def.name);
        }
    }

    #[test]
    fn toml_round_trip() {
        let def = builtin_worker();
        let serialized = toml::to_string(&def).unwrap();
        let parsed: AgentDefinition = toml::from_str(&serialized).unwrap();
        assert_eq!(parsed.name, def.name);
        assert_eq!(parsed.sandbox_mode, def.sandbox_mode);
    }
}
