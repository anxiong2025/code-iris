use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::models::{Dependency, DependencyType, Module, ProjectStats, ScanResult};

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".venv",
    "__pycache__",
    "build",
    "dist",
    "node_modules",
    "target",
    "vendor",
];

pub struct Scanner;

impl Scanner {
    pub fn new() -> Self {
        Self
    }

    pub fn scan(&self, root: &Path) -> Result<ScanResult> {
        let root = root
            .canonicalize()
            .with_context(|| format!("failed to resolve {}", root.display()))?;
        let mut paths = Vec::new();
        self.walk(&root, &mut paths)?;
        paths.sort();

        let mut modules = Vec::new();
        let mut dependencies = Vec::new();
        let mut stats = ProjectStats::default();

        for path in paths {
            let relative = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_path_buf();
            let source = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let language = detect_language(&path).unwrap_or_else(|| "Unknown".to_string());
            let line_count = source.lines().count();
            let imports = extract_imports(&source, &language);
            let symbols = extract_symbols(&source, &language);

            for import in &imports {
                dependencies.push(Dependency {
                    from: relative.clone(),
                    to: import.clone(),
                    dependency_type: dependency_type(import),
                });
            }

            stats.total_files += 1;
            stats.total_lines += line_count;
            stats.total_symbols += symbols.len();
            *stats
                .files_by_language
                .entry(language.clone())
                .or_insert(0) += 1;
            *stats
                .lines_by_language
                .entry(language.clone())
                .or_insert(0) += line_count;

            modules.push(Module {
                path: relative,
                language,
                line_count,
                symbols,
                imports,
            });
        }

        Ok(ScanResult {
            root,
            modules,
            dependencies,
            stats,
        })
    }

    fn walk(&self, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(dir)
            .with_context(|| format!("failed to read directory {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();

            if path.is_dir() {
                if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                    continue;
                }
                self.walk(&path, out)?;
                continue;
            }

            if detect_language(&path).is_some() {
                out.push(path);
            }
        }

        Ok(())
    }
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

fn detect_language(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    let language = match ext {
        "rs" => "Rust",
        "py" => "Python",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "go" => "Go",
        _ => return None,
    };
    Some(language.to_string())
}

fn extract_imports(source: &str, language: &str) -> Vec<String> {
    let mut imports = Vec::new();

    for line in source.lines() {
        let trimmed = line.trim();
        match language {
            "Rust" => {
                if let Some(rest) = trimmed.strip_prefix("use ") {
                    if let Some(target) = rest.split(';').next() {
                        let name = target
                            .trim()
                            .trim_start_matches('{')
                            .split("::")
                            .next()
                            .unwrap_or("")
                            .trim();
                        if !name.is_empty() {
                            imports.push(name.to_string());
                        }
                    }
                }
            }
            "Python" => {
                if let Some(rest) = trimmed.strip_prefix("import ") {
                    let name = rest
                        .split(',')
                        .next()
                        .unwrap_or("")
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .split('.')
                        .next()
                        .unwrap_or("");
                    if !name.is_empty() {
                        imports.push(name.to_string());
                    }
                } else if let Some(rest) = trimmed.strip_prefix("from ") {
                    let name = rest
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .split('.')
                        .next()
                        .unwrap_or("");
                    if !name.is_empty() {
                        imports.push(name.to_string());
                    }
                }
            }
            "TypeScript" | "JavaScript" => {
                if trimmed.starts_with("import ") && trimmed.contains(" from ") {
                    if let Some(spec) = parse_js_specifier(trimmed.rsplit(" from ").next().unwrap_or("")) {
                        imports.push(spec);
                    }
                } else if let Some(start) = trimmed.find("require(") {
                    let rest = &trimmed[start + "require(".len()..];
                    if let Some(spec) = parse_js_specifier(rest) {
                        imports.push(spec);
                    }
                }
            }
            "Go" => {
                if trimmed.starts_with("import ") {
                    if let Some(spec) = parse_go_import(trimmed.trim_start_matches("import ").trim()) {
                        imports.push(spec);
                    }
                } else if trimmed.starts_with('"') {
                    if let Some(spec) = parse_go_import(trimmed) {
                        imports.push(spec);
                    }
                }
            }
            _ => {}
        }
    }

    imports.sort();
    imports.dedup();
    imports
}

fn extract_symbols(source: &str, language: &str) -> Vec<String> {
    let mut symbols = Vec::new();
    let prefixes: &[&str] = match language {
        "Rust" => &["pub fn ", "fn ", "pub struct ", "struct ", "pub enum ", "enum ", "pub trait ", "trait ", "impl "],
        "Python" => &["def ", "async def ", "class "],
        "TypeScript" | "JavaScript" => &["function ", "async function ", "export function ", "export async function ", "class ", "export class ", "const ", "let "],
        "Go" => &["func ", "type "],
        _ => &[],
    };

    for line in source.lines() {
        let trimmed = line.trim();
        for prefix in prefixes {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let symbol = rest
                    .chars()
                    .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
                    .collect::<String>();
                if !symbol.is_empty() {
                    symbols.push(symbol);
                }
                break;
            }
        }
    }

    symbols.sort();
    symbols.dedup();
    symbols
}

fn parse_js_specifier(raw: &str) -> Option<String> {
    let spec = raw
        .trim()
        .trim_end_matches(';')
        .trim_matches(|ch| ch == '"' || ch == '\'' || ch == ')');
    if spec.is_empty() {
        None
    } else {
        Some(spec.to_string())
    }
}

fn parse_go_import(raw: &str) -> Option<String> {
    let spec = raw.trim().trim_matches('"');
    if spec.is_empty() || spec == "(" || spec == ")" {
        None
    } else {
        Some(spec.to_string())
    }
}

fn dependency_type(import: &str) -> DependencyType {
    if import.starts_with("./")
        || import.starts_with("../")
        || import.starts_with('/')
        || import.starts_with("crate::")
        || import.starts_with("self::")
        || import.starts_with("super::")
    {
        DependencyType::Internal
    } else {
        DependencyType::External
    }
}
