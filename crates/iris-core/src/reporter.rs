use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::models::{DependencyType, ScanResult};
use crate::scanner::Scanner;

pub struct Reporter {
    scan: ScanResult,
}

impl Reporter {
    pub fn from_path(path: &Path) -> Result<Self> {
        let scan = Scanner::new().scan(path)?;
        Ok(Self { scan })
    }

    pub fn from_scan(scan: ScanResult) -> Self {
        Self { scan }
    }

    pub fn scan(&self) -> &ScanResult {
        &self.scan
    }

    pub fn render_manifest(&self) -> String {
        let mut out = String::new();
        out.push_str("# Manifest\n\n");
        out.push_str(&format!("Root: `{}`\n\n", self.scan.root.display()));
        for module in &self.scan.modules {
            out.push_str(&format!(
                "- `{}` [{}] lines={} symbols={} imports={}\n",
                module.path.display(),
                module.language,
                module.line_count,
                module.symbols.len(),
                module.imports.len()
            ));
        }
        out
    }

    pub fn render_dependencies(&self) -> String {
        let mut grouped: BTreeMap<DependencyType, Vec<String>> = BTreeMap::new();
        for dependency in &self.scan.dependencies {
            grouped
                .entry(dependency.dependency_type)
                .or_default()
                .push(format!("`{}` -> `{}`", dependency.from.display(), dependency.to));
        }

        let mut out = String::new();
        out.push_str("# Dependencies\n\n");
        for dependency_type in [DependencyType::Internal, DependencyType::External] {
            let title = match dependency_type {
                DependencyType::Internal => "Internal",
                DependencyType::External => "External",
            };
            out.push_str(&format!("## {title}\n\n"));
            if let Some(items) = grouped.get_mut(&dependency_type) {
                items.sort();
                items.dedup();
                for item in items {
                    out.push_str(&format!("- {item}\n"));
                }
            } else {
                out.push_str("- none\n");
            }
            out.push('\n');
        }
        out
    }

    pub fn render_stats(&self) -> String {
        let mut out = String::new();
        out.push_str("# Stats\n\n");
        out.push_str(&format!("- files: {}\n", self.scan.stats.total_files));
        out.push_str(&format!("- lines: {}\n", self.scan.stats.total_lines));
        out.push_str(&format!("- symbols: {}\n", self.scan.stats.total_symbols));
        out.push_str(&format!(
            "- dependencies: {}\n\n",
            self.scan.dependencies.len()
        ));
        out.push_str("## Languages\n\n");
        for (language, files) in &self.scan.stats.files_by_language {
            let lines = self
                .scan
                .stats
                .lines_by_language
                .get(language)
                .copied()
                .unwrap_or_default();
            out.push_str(&format!("- {language}: {files} files, {lines} lines\n"));
        }
        out
    }

    pub fn render_full_report(&self) -> String {
        format!(
            "{}\n{}\n{}",
            self.render_manifest(),
            self.render_dependencies(),
            self.render_stats()
        )
    }
}
