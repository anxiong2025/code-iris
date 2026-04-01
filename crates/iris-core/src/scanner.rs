use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tree_sitter::{Language, Node, Parser};

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

// ── Tree-sitter extraction ────────────────────────────────────────────────────

/// Extract (symbols, imports) from source using tree-sitter node traversal.
///
/// Returns `None` if the grammar is unavailable or parsing fails; the caller
/// should fall back to text-based extraction.
fn ts_extract(language: Language, source: &str) -> Option<(Vec<String>, Vec<String>)> {
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(source, None)?;
    let src = source.as_bytes();

    let mut symbols: Vec<String> = Vec::new();
    let mut imports: Vec<String> = Vec::new();

    walk_tree(tree.root_node(), src, &mut symbols, &mut imports);

    symbols.sort();
    symbols.dedup();
    imports.sort();
    imports.dedup();

    Some((symbols, imports))
}

/// Recursively walk the parse tree and collect symbol names and import paths.
fn walk_tree(node: Node<'_>, src: &[u8], symbols: &mut Vec<String>, imports: &mut Vec<String>) {
    let kind = node.kind();

    match kind {
        // ── Rust ─────────────────────────────────────────────────────────────
        "function_item" | "struct_item" | "enum_item" | "trait_item" => {
            if let Some(name) = named_child_text(node, "name", src) {
                symbols.push(name);
            }
        }
        "impl_item" => {
            if let Some(name) = named_child_text(node, "type", src) {
                symbols.push(name);
            }
        }
        "use_declaration" => {
            // Grab the first identifier in the use path (the crate/module root).
            if let Some(arg) = node.child_by_field_name("argument") {
                collect_use_root(arg, src, imports);
            }
        }

        // ── Python ───────────────────────────────────────────────────────────
        "function_definition" | "class_definition" => {
            if let Some(name) = named_child_text(node, "name", src) {
                symbols.push(name);
            }
        }
        "import_statement" => {
            // JavaScript/TypeScript: `import ... from "spec"` — source field
            if let Some(src_node) = node.child_by_field_name("source") {
                if let Ok(raw) = src_node.utf8_text(src) {
                    let spec = raw.trim().trim_matches(|c| c == '"' || c == '\'');
                    if !spec.is_empty() {
                        imports.push(spec.to_string());
                    }
                }
            } else {
                // Python: `import a.b.c` → first dotted_name identifier
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    if child.kind() == "dotted_name" {
                        if let Some(ident) = first_identifier(child, src) {
                            imports.push(ident);
                        }
                        break;
                    }
                }
            }
        }
        "import_from_statement" => {
            // Python: `from a.b import c` → module_name field
            if let Some(mod_node) = node.child_by_field_name("module_name") {
                if let Some(ident) = first_identifier(mod_node, src) {
                    imports.push(ident);
                }
            }
        }

        // ── JavaScript / TypeScript ───────────────────────────────────────────
        "function_declaration" | "class_declaration" | "interface_declaration"
        | "type_alias_declaration" => {
            if let Some(name) = named_child_text(node, "name", src) {
                symbols.push(name);
            }
        }
        "call_expression" => {
            // `require("spec")`
            if let Some(fn_node) = node.child_by_field_name("function") {
                if fn_node.utf8_text(src).ok() == Some("require") {
                    if let Some(args) = node.child_by_field_name("arguments") {
                        let mut c = args.walk();
                        for arg in args.children(&mut c) {
                            if arg.kind() == "string" {
                                if let Ok(raw) = arg.utf8_text(src) {
                                    let spec = raw.trim().trim_matches(|c| c == '"' || c == '\'');
                                    if !spec.is_empty() {
                                        imports.push(spec.to_string());
                                    }
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }

        _ => {}
    }

    // Recurse — skip nodes that are purely syntactic filler.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree(child, src, symbols, imports);
    }
}

/// Return the UTF-8 text of the named field child of `node`.
fn named_child_text(node: Node<'_>, field: &str, src: &[u8]) -> Option<String> {
    node.child_by_field_name(field)
        .and_then(|n| n.utf8_text(src).ok())
        .map(|s| s.to_string())
}

/// Return the text of the first `identifier` descendent of `node`.
fn first_identifier(node: Node<'_>, src: &[u8]) -> Option<String> {
    if node.kind() == "identifier" {
        return node.utf8_text(src).ok().map(|s| s.to_string());
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(s) = first_identifier(child, src) {
            return Some(s);
        }
    }
    None
}

/// Collect the root crate/module name from a Rust `use` path node.
fn collect_use_root(node: Node<'_>, src: &[u8], imports: &mut Vec<String>) {
    match node.kind() {
        "identifier" => {
            if let Ok(text) = node.utf8_text(src) {
                imports.push(text.to_string());
            }
        }
        "scoped_identifier" => {
            // Recurse into the `path` field (the left-hand side).
            if let Some(path) = node.child_by_field_name("path") {
                collect_use_root(path, src, imports);
            }
        }
        _ => {}
    }
}

// ── Scanner ───────────────────────────────────────────────────────────────────

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

            let (symbols, imports) = extract_symbols_and_imports(&source, &language);

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
            *stats.files_by_language.entry(language.clone()).or_insert(0) += 1;
            *stats.lines_by_language.entry(language.clone()).or_insert(0) += line_count;

            modules.push(Module {
                path: relative,
                language,
                line_count,
                symbols,
                imports,
            });
        }

        Ok(ScanResult { root, modules, dependencies, stats })
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

// ── Language detection ────────────────────────────────────────────────────────

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

// ── Extraction dispatcher ─────────────────────────────────────────────────────

/// Returns (symbols, imports) — tries tree-sitter first, falls back to text parsing.
fn extract_symbols_and_imports(source: &str, language: &str) -> (Vec<String>, Vec<String>) {
    let ts_lang: Option<Language> = match language {
        "Rust" => Some(Language::new(tree_sitter_rust::LANGUAGE)),
        "Python" => Some(Language::new(tree_sitter_python::LANGUAGE)),
        "JavaScript" => Some(Language::new(tree_sitter_javascript::LANGUAGE)),
        "TypeScript" => Some(Language::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT)),
        _ => None,
    };

    if let Some(lang) = ts_lang {
        if let Some(result) = ts_extract(lang, source) {
            return result;
        }
    }

    // Fallback: text-based extraction (covers Go and any parse failure).
    let symbols = extract_symbols_text(source, language);
    let imports = extract_imports_text(source, language);
    (symbols, imports)
}

// ── Text-based fallback ───────────────────────────────────────────────────────

fn extract_imports_text(source: &str, language: &str) -> Vec<String> {
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
                    let name = rest.split(',').next().unwrap_or("")
                        .split_whitespace().next().unwrap_or("")
                        .split('.').next().unwrap_or("");
                    if !name.is_empty() { imports.push(name.to_string()); }
                } else if let Some(rest) = trimmed.strip_prefix("from ") {
                    let name = rest.split_whitespace().next().unwrap_or("")
                        .split('.').next().unwrap_or("");
                    if !name.is_empty() { imports.push(name.to_string()); }
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

fn extract_symbols_text(source: &str, language: &str) -> Vec<String> {
    let mut symbols = Vec::new();
    let prefixes: &[&str] = match language {
        "Rust" => &["pub fn ", "fn ", "pub struct ", "struct ", "pub enum ", "enum ",
                    "pub trait ", "trait ", "impl "],
        "Python" => &["def ", "async def ", "class "],
        "TypeScript" | "JavaScript" => &["function ", "async function ", "export function ",
                    "export async function ", "class ", "export class ", "const ", "let "],
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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_js_specifier(raw: &str) -> Option<String> {
    let spec = raw.trim().trim_end_matches(';')
        .trim_matches(|ch| ch == '"' || ch == '\'' || ch == ')');
    if spec.is_empty() { None } else { Some(spec.to_string()) }
}

fn parse_go_import(raw: &str) -> Option<String> {
    let spec = raw.trim().trim_matches('"');
    if spec.is_empty() || spec == "(" || spec == ")" { None } else { Some(spec.to_string()) }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn detect_rust_language() {
        assert_eq!(detect_language(Path::new("foo.rs")), Some("Rust".to_string()));
    }

    #[test]
    fn detect_python_language() {
        assert_eq!(detect_language(Path::new("bar.py")), Some("Python".to_string()));
    }

    #[test]
    fn detect_typescript_language() {
        assert_eq!(detect_language(Path::new("app.ts")), Some("TypeScript".to_string()));
        assert_eq!(detect_language(Path::new("comp.tsx")), Some("TypeScript".to_string()));
    }

    #[test]
    fn detect_javascript_language() {
        assert_eq!(detect_language(Path::new("main.js")), Some("JavaScript".to_string()));
        assert_eq!(detect_language(Path::new("util.mjs")), Some("JavaScript".to_string()));
    }

    #[test]
    fn detect_go_language() {
        assert_eq!(detect_language(Path::new("server.go")), Some("Go".to_string()));
    }

    #[test]
    fn detect_unknown_returns_none() {
        assert_eq!(detect_language(Path::new("data.json")), None);
        assert_eq!(detect_language(Path::new("README.md")), None);
        assert_eq!(detect_language(Path::new("no_extension")), None);
    }

    #[test]
    fn extract_rust_symbols_via_treesitter() {
        let src = r#"
pub fn hello() {}
fn world() {}
pub struct Foo {}
enum Bar { A, B }
pub trait Baz {}
"#;
        let (symbols, _) = extract_symbols_and_imports(src, "Rust");
        assert!(symbols.contains(&"hello".to_string()), "should find hello: {symbols:?}");
        assert!(symbols.contains(&"Foo".to_string()), "should find Foo: {symbols:?}");
        assert!(symbols.contains(&"Bar".to_string()), "should find Bar: {symbols:?}");
    }

    #[test]
    fn extract_rust_imports_via_treesitter() {
        let src = "use std::collections::HashMap;\nuse tokio::sync::mpsc;\n";
        let (_, imports) = extract_symbols_and_imports(src, "Rust");
        assert!(imports.contains(&"std".to_string()), "should find std: {imports:?}");
        assert!(imports.contains(&"tokio".to_string()), "should find tokio: {imports:?}");
    }

    #[test]
    fn extract_python_symbols_via_treesitter() {
        let src = "def foo():\n    pass\n\nclass Bar:\n    pass\n";
        let (symbols, _) = extract_symbols_and_imports(src, "Python");
        assert!(symbols.contains(&"foo".to_string()), "{symbols:?}");
        assert!(symbols.contains(&"Bar".to_string()), "{symbols:?}");
    }

    #[test]
    fn extract_python_imports_via_treesitter() {
        let src = "import os\nimport sys\nfrom pathlib import Path\n";
        let (_, imports) = extract_symbols_and_imports(src, "Python");
        assert!(imports.contains(&"os".to_string()), "{imports:?}");
        assert!(imports.contains(&"pathlib".to_string()), "{imports:?}");
    }

    #[test]
    fn extract_javascript_imports_via_treesitter() {
        let src = r#"import React from 'react';
import { useState } from 'react';
const fs = require('fs');
"#;
        let (_, imports) = extract_symbols_and_imports(src, "JavaScript");
        assert!(imports.contains(&"react".to_string()), "{imports:?}");
        assert!(imports.contains(&"fs".to_string()), "{imports:?}");
    }

    #[test]
    fn go_fallback_text_parsing() {
        let src = "package main\n\nimport \"fmt\"\n\nfunc main() {}\n";
        let (symbols, imports) = extract_symbols_and_imports(src, "Go");
        assert!(symbols.contains(&"main".to_string()), "{symbols:?}");
        assert!(imports.contains(&"fmt".to_string()), "{imports:?}");
    }

    #[test]
    fn dependency_type_internal() {
        assert_eq!(dependency_type("./utils"), DependencyType::Internal);
        assert_eq!(dependency_type("../lib"), DependencyType::Internal);
        assert_eq!(dependency_type("crate::tools"), DependencyType::Internal);
    }

    #[test]
    fn dependency_type_external() {
        assert_eq!(dependency_type("serde"), DependencyType::External);
        assert_eq!(dependency_type("tokio"), DependencyType::External);
    }
}
