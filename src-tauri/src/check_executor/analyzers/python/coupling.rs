//! Python Coupling Analyzer
//!
//! Analyzes Python code to detect coupling metrics between modules:
//! - Afferent coupling (Ca): number of modules that depend on this module
//! - Efferent coupling (Ce): number of modules this module depends on
//! - Instability (I): Ce / (Ca + Ce) - a value of 1.0 means highly unstable
//!
//! Uses tree-sitter-python for parsing imports.

use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{CheckStructuredOutput, CheckSummary, IssueSeverity};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

/// Default maximum coupling threshold
const DEFAULT_MAX_COUPLING: u32 = 10;

/// High instability threshold (modules with I > this are flagged)
const HIGH_INSTABILITY_THRESHOLD: f64 = 0.85;

/// Module coupling information
#[derive(Debug, Default)]
struct ModuleCoupling {
    /// File path
    file_path: String,
    /// Module name (derived from file path)
    module_name: String,
    /// Modules this module imports (outgoing dependencies)
    imports: HashSet<String>,
    /// Modules that import this module (incoming dependencies)
    imported_by: HashSet<String>,
}

impl ModuleCoupling {
    /// Efferent coupling: number of modules this module depends on
    fn efferent_coupling(&self) -> u32 {
        self.imports.len() as u32
    }

    /// Afferent coupling: number of modules that depend on this module
    fn afferent_coupling(&self) -> u32 {
        self.imported_by.len() as u32
    }

    /// Instability: Ce / (Ca + Ce)
    /// A value of 0 means stable (only incoming dependencies)
    /// A value of 1 means unstable (only outgoing dependencies)
    fn instability(&self) -> f64 {
        let ca = self.afferent_coupling() as f64;
        let ce = self.efferent_coupling() as f64;
        let total = ca + ce;
        if total == 0.0 {
            0.0
        } else {
            ce / total
        }
    }
}

/// Analyze Python coupling with custom max coupling threshold
pub fn analyze(working_dir: &str, max_coupling: u32) -> Result<ParsedOutput, String> {
    let root = Path::new(working_dir);
    if !root.exists() {
        return Err(format!("Directory does not exist: {}", working_dir));
    }

    // Walk Python files
    let config = WalkConfig {
        extensions: vec!["py".to_string()],
        ..Default::default()
    };
    let files = walk_files(root, &config);

    if files.is_empty() {
        return Ok(ParsedOutput {
            issues_found: 0,
            issues_fixed: 0,
            files_checked: 0,
            structured_output: CheckStructuredOutput::default(),
        });
    }

    // Initialize parser with Python language
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser
        .set_language(&language.into())
        .map_err(|e| format!("Failed to set Python language: {}", e))?;

    // Build module coupling map
    let mut modules: HashMap<String, ModuleCoupling> = HashMap::new();

    // First pass: parse all files and extract imports
    for file_path in &files {
        let file_path_str = file_path.to_string_lossy().to_string();
        let module_name = path_to_module_name(file_path, root);

        let source = std::fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read {}: {}", file_path_str, e))?;

        let tree = parser
            .parse(&source, None)
            .ok_or_else(|| format!("Failed to parse {}", file_path_str))?;

        let imports = extract_imports(&source, &tree, &language.into())?;

        modules.insert(
            module_name.clone(),
            ModuleCoupling {
                file_path: file_path_str,
                module_name,
                imports,
                imported_by: HashSet::new(),
            },
        );
    }

    // Second pass: build afferent coupling (imported_by)
    let module_names: Vec<String> = modules.keys().cloned().collect();
    for module_name in &module_names {
        let imports = modules.get(module_name).map(|m| m.imports.clone()).unwrap_or_default();
        for imported in imports {
            // Normalize the import to match module names
            let normalized = normalize_import(&imported, &module_names);
            if let Some(target_module) = modules.get_mut(&normalized) {
                target_module.imported_by.insert(module_name.clone());
            }
        }
    }

    // Generate issues
    let mut issues = Vec::new();
    let mut files_with_issues = HashSet::new();
    let mut high_coupling_count = 0u32;
    let mut high_instability_count = 0u32;

    for module in modules.values() {
        let ce = module.efferent_coupling();
        let instability = module.instability();

        // Check for high efferent coupling (too many outgoing dependencies)
        if ce > max_coupling {
            high_coupling_count += 1;
            files_with_issues.insert(module.file_path.clone());
            issues.push(
                IssueBuilder::new(
                    &module.file_path,
                    format!(
                        "High coupling in module '{}': {} dependencies (threshold: {})",
                        module.module_name, ce, max_coupling
                    ),
                )
                .code("COUPLING001")
                .warning()
                .build(),
            );
        }

        // Check for high instability
        if instability > HIGH_INSTABILITY_THRESHOLD && ce > 3 {
            // Only flag if there are significant dependencies
            high_instability_count += 1;
            files_with_issues.insert(module.file_path.clone());
            issues.push(
                IssueBuilder::new(
                    &module.file_path,
                    format!(
                        "Module '{}' has high instability ({:.2}) - consider reducing outgoing dependencies",
                        module.module_name, instability
                    ),
                )
                .code("COUPLING002")
                .info()
                .build(),
            );
        }
    }

    let issues_found = issues.len() as u32;
    let error_count = issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Error)
        .count() as u32;
    let warning_count = issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Warning)
        .count() as u32;
    let info_count = issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Info)
        .count() as u32;

    Ok(ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked: files.len() as u32,
        structured_output: CheckStructuredOutput {
            issues,
            summary: Some(CheckSummary {
                total_files: files.len() as u32,
                files_with_issues: files_with_issues.len() as u32,
                total_issues: issues_found,
                issues_by_severity: HashMap::from([
                    ("error".to_string(), error_count),
                    ("warning".to_string(), warning_count),
                    ("info".to_string(), info_count),
                ]),
            }),
            tool_data: HashMap::from([
                ("max_coupling".to_string(), serde_json::json!(max_coupling)),
                (
                    "high_coupling_modules".to_string(),
                    serde_json::json!(high_coupling_count),
                ),
                (
                    "high_instability_modules".to_string(),
                    serde_json::json!(high_instability_count),
                ),
                ("total_modules".to_string(), serde_json::json!(modules.len())),
            ]),
        },
    })
}

/// Analyze Python coupling with default threshold (10)
pub fn analyze_with_defaults(working_dir: &str) -> Result<ParsedOutput, String> {
    analyze(working_dir, DEFAULT_MAX_COUPLING)
}

/// Convert a file path to a Python module name
fn path_to_module_name(file_path: &Path, root: &Path) -> String {
    let relative = file_path
        .strip_prefix(root)
        .unwrap_or(file_path);

    let mut parts: Vec<&str> = relative
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();

    // Remove .py extension from last part
    if let Some(last) = parts.last_mut() {
        if let Some(stripped) = last.strip_suffix(".py") {
            *last = stripped;
        }
    }

    // Remove __init__ if it's the last part
    if parts.last() == Some(&"__init__") {
        parts.pop();
    }

    parts.join(".")
}

/// Extract imports from a Python file using tree-sitter
fn extract_imports(
    source: &str,
    tree: &tree_sitter::Tree,
    language: &tree_sitter::Language,
) -> Result<HashSet<String>, String> {
    let mut imports = HashSet::new();

    // Query for import statements
    // import_statement: import module.name
    // import_from_statement: from module import something
    let query_str = r#"
        (import_statement
            name: (dotted_name) @import_name)
        (import_statement
            name: (aliased_import
                name: (dotted_name) @import_name))
        (import_from_statement
            module_name: (dotted_name) @from_module)
        (import_from_statement
            module_name: (relative_import
                (dotted_name) @relative_import))
    "#;

    let query = Query::new(language, query_str)
        .map_err(|e| format!("Failed to create query: {}", e))?;

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());

    while let Some(m) = matches.next() {
        for capture in m.captures {
            let node = capture.node;
            let text = node
                .utf8_text(source.as_bytes())
                .unwrap_or("")
                .to_string();

            if !text.is_empty() {
                // Get the top-level module name (first part before dot)
                let module = text.split('.').next().unwrap_or(&text).to_string();
                imports.insert(module);
            }
        }
    }

    Ok(imports)
}

/// Normalize an import name to match module names in the codebase
fn normalize_import(import: &str, known_modules: &[String]) -> String {
    // Direct match
    if known_modules.contains(&import.to_string()) {
        return import.to_string();
    }

    // Try to find a module that starts with this import
    for module in known_modules {
        if module.starts_with(import) || module == import {
            return module.clone();
        }
    }

    // Return as-is (external dependency)
    import.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, name: &str, content: &str) {
        let file_path = dir.join(name);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(file_path, content).unwrap();
    }

    #[test]
    fn test_path_to_module_name() {
        let root = Path::new("/project");

        assert_eq!(
            path_to_module_name(Path::new("/project/main.py"), root),
            "main"
        );
        assert_eq!(
            path_to_module_name(Path::new("/project/utils/helpers.py"), root),
            "utils.helpers"
        );
        assert_eq!(
            path_to_module_name(Path::new("/project/package/__init__.py"), root),
            "package"
        );
    }

    #[test]
    fn test_analyze_simple_coupling() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create a simple module structure
        create_test_file(
            root,
            "main.py",
            r#"
import os
import sys
import utils
from helpers import something
"#,
        );

        create_test_file(
            root,
            "utils.py",
            r#"
import json
"#,
        );

        create_test_file(
            root,
            "helpers.py",
            r#"
import utils
"#,
        );

        let result = analyze(root.to_str().unwrap(), 10).unwrap();
        assert_eq!(result.files_checked, 3);

        // No high coupling (COUPLING001) issues expected with threshold of 10
        // Note: High instability (COUPLING002) info messages may be present for small codebases
        let coupling_issues: Vec<_> = result
            .structured_output
            .issues
            .iter()
            .filter(|i| i.code == Some("COUPLING001".to_string()))
            .collect();
        assert_eq!(coupling_issues.len(), 0);
    }

    #[test]
    fn test_analyze_high_coupling() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create a module with many imports (high coupling)
        create_test_file(
            root,
            "high_coupling.py",
            r#"
import os
import sys
import json
import re
import logging
import datetime
import collections
import itertools
import functools
import operator
import math
import random
"#,
        );

        // Set threshold to 5 to trigger the warning
        let result = analyze(root.to_str().unwrap(), 5).unwrap();
        assert_eq!(result.files_checked, 1);
        assert!(result.issues_found > 0);

        // Check that we got a high coupling warning
        let has_coupling_issue = result
            .structured_output
            .issues
            .iter()
            .any(|i| i.code == Some("COUPLING001".to_string()));
        assert!(has_coupling_issue);
    }

    #[test]
    fn test_analyze_with_defaults() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(
            root,
            "simple.py",
            r#"
import os
"#,
        );

        let result = analyze_with_defaults(root.to_str().unwrap()).unwrap();
        assert_eq!(result.files_checked, 1);
        assert_eq!(result.issues_found, 0);
    }

    #[test]
    fn test_analyze_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        let result = analyze(temp_dir.path().to_str().unwrap(), 10).unwrap();
        assert_eq!(result.files_checked, 0);
        assert_eq!(result.issues_found, 0);
    }

    #[test]
    fn test_analyze_nonexistent_directory() {
        let result = analyze("/nonexistent/path/to/nowhere", 10);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_various_import_styles() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(
            root,
            "imports.py",
            r#"
import os
import sys as system
from collections import defaultdict
from typing import List, Dict
from pathlib import Path
import json, re
"#,
        );

        let result = analyze(root.to_str().unwrap(), 100).unwrap();
        assert_eq!(result.files_checked, 1);
    }
}
