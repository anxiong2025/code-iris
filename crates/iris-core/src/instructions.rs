//! Layered instructions loader — equivalent to Claude Code's CLAUDE.md.
//!
//! Loads instructions from three levels (in order, all optional):
//!
//! 1. `~/.code-iris/instructions.md` — **global**, applied to every project
//! 2. `<project_root>/.iris/instructions.md` — **project-level**
//! 3. `<cwd>/.iris/instructions_local.md` — **directory-level** (for monorepos)
//!
//! All existing files are concatenated with separators and prepended to
//! the agent's system prompt.

use std::path::Path;

/// Load and merge layered instructions.
///
/// - `project_root` is typically the git root or cwd.
/// - `cwd` is the current working directory (may be a subdirectory of the project).
///
/// Returns `None` when no instruction files exist or all are empty.
pub fn load(project_root: Option<&Path>) -> Option<String> {
    load_with_cwd(project_root, None)
}

/// Load instructions with an explicit `cwd` for directory-level instructions.
pub fn load_with_cwd(project_root: Option<&Path>, cwd: Option<&Path>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    // 1. Global — ~/.code-iris/instructions.md
    if let Some(home) = dirs::home_dir() {
        collect(home.join(".code-iris").join("instructions.md"), &mut parts);
    }
    // 2. Project-level — <root>/.iris/instructions.md
    if let Some(root) = project_root {
        collect(root.join(".iris").join("instructions.md"), &mut parts);
    }
    // 3. Directory-level — <cwd>/.iris/instructions_local.md
    // Only if cwd differs from project_root (avoids double-loading).
    let effective_cwd = cwd.or(project_root);
    if let Some(dir) = effective_cwd {
        let local_path = dir.join(".iris").join("instructions_local.md");
        if local_path.exists() {
            // Avoid duplicating project-level instructions.
            let is_project_root = project_root.map_or(false, |r| r == dir);
            if !is_project_root || local_path != dir.join(".iris").join("instructions.md") {
                collect(local_path, &mut parts);
            }
        }
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
