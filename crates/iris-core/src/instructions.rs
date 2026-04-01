//! Layered instructions loader — equivalent to Claude Code's CLAUDE.md.
//!
//! Loads `.iris/instructions.md` files from two locations (in order):
//!
//! 1. `~/.code-iris/instructions.md` — user-level, applied to every project
//! 2. `<project_root>/.iris/instructions.md` — project-level
//!
//! Both files are optional. When both exist they are concatenated with a
//! separator and prepended to the agent's system prompt.
//!
//! # Example `.iris/instructions.md`
//!
//! ```markdown
//! You are working inside the `code-iris` Rust workspace.
//!
//! - Always run `cargo check` after editing `.rs` files.
//! - Never commit directly to `main`; create a branch first.
//! - Prefer `anyhow::Result` for error handling throughout.
//! ```

use std::path::Path;

/// Load and merge layered instructions.
///
/// Returns `None` when neither file exists or both are empty.
pub fn load(project_root: Option<&Path>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    // User-level
    if let Some(home) = dirs::home_dir() {
        collect(home.join(".code-iris").join("instructions.md"), &mut parts);
    }
    // Project-level
    if let Some(root) = project_root {
        collect(root.join(".iris").join("instructions.md"), &mut parts);
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n---\n\n"))
    }
}

fn collect(path: impl AsRef<Path>, out: &mut Vec<String>) {
    if let Ok(text) = std::fs::read_to_string(path) {
        let t = text.trim().to_string();
        if !t.is_empty() {
            out.push(t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn returns_none_when_no_files_exist() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(Some(dir.path())).is_none());
    }

    #[test]
    fn loads_project_instructions() {
        let dir = tempfile::tempdir().unwrap();
        let iris_dir = dir.path().join(".iris");
        fs::create_dir_all(&iris_dir).unwrap();
        fs::write(iris_dir.join("instructions.md"), "Use anyhow for errors.").unwrap();

        let result = load(Some(dir.path())).unwrap();
        assert!(result.contains("Use anyhow for errors."), "{result}");
    }

    #[test]
    fn empty_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let iris_dir = dir.path().join(".iris");
        fs::create_dir_all(&iris_dir).unwrap();
        fs::write(iris_dir.join("instructions.md"), "   \n  ").unwrap();

        assert!(load(Some(dir.path())).is_none());
    }
}
