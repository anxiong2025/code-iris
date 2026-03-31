use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Module {
    pub path: PathBuf,
    pub language: String,
    pub line_count: usize,
    pub symbols: Vec<String>,
    pub imports: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DependencyType {
    Internal,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Dependency {
    pub from: PathBuf,
    pub to: String,
    pub dependency_type: DependencyType,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectStats {
    pub total_files: usize,
    pub total_lines: usize,
    pub total_symbols: usize,
    pub files_by_language: BTreeMap<String, usize>,
    pub lines_by_language: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanResult {
    pub root: PathBuf,
    pub modules: Vec<Module>,
    pub dependencies: Vec<Dependency>,
    pub stats: ProjectStats,
}
