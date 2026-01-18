//! TypeScript Race Condition Analyzer
//!
//! Detects potential race conditions in TypeScript/JavaScript code by analyzing:
//! - Module-level mutable variables
//! - Class properties modified across async boundaries
//! - Multiple await expressions modifying shared state
//! - Event handler patterns with shared state

pub mod async_detection;
pub mod shared_state;

use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::analyzers::concurrency::heuristics;
use crate::check_executor::analyzers::concurrency::patterns;
use crate::check_executor::analyzers::concurrency::race_severity_to_issue_severity;
use crate::check_executor::analyzers::concurrency::types::AnalysisContext;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{CheckStructuredOutput, CheckSummary, IssueSeverity};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tracing::debug;

/// Analyze TypeScript files for race conditions
pub fn analyze(working_dir: &str) -> Result<ParsedOutput, String> {
    let root = Path::new(working_dir);
    if !root.exists() {
        return Err(format!("Directory does not exist: {}", working_dir));
    }

    let config = WalkConfig {
        extensions: vec!["ts".to_string(), "tsx".to_string(), "js".to_string(), "jsx".to_string()],
        ..Default::default()
    };

    let files = walk_files(root, &config);
    debug!(
        "Found {} TypeScript/JavaScript files to analyze for race conditions",
        files.len()
    );

    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
    parser
        .set_language(&language.into())
        .map_err(|e| format!("Failed to set TypeScript language: {}", e))?;

    let mut all_issues = Vec::new();
    let mut files_with_issues = HashSet::new();
    let mut total_shared_states = 0u32;
    let mut total_async_funcs = 0u32;

    for file_path in &files {
        let file_str = file_path.to_string_lossy().to_string();
        let is_test = heuristics::is_test_file(&file_str);

        // Use TSX parser for .tsx files
        let is_tsx = file_str.ends_with(".tsx") || file_str.ends_with(".jsx");
        if is_tsx {
            let tsx_language = tree_sitter_typescript::LANGUAGE_TSX;
            if parser.set_language(&tsx_language.into()).is_err() {
                debug!("Failed to set TSX language for {:?}", file_path);
                continue;
            }
        } else {
            let ts_language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
            if parser.set_language(&ts_language.into()).is_err() {
                debug!("Failed to set TypeScript language for {:?}", file_path);
                continue;
            }
        }

        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(e) => {
                debug!("Failed to read file {:?}: {}", file_path, e);
                continue;
            }
        };

        let tree = match parser.parse(&content, None) {
            Some(t) => t,
            None => {
                debug!("Failed to parse file {:?}", file_path);
                continue;
            }
        };

        let mut ctx = AnalysisContext::new();

        // Detect shared state (module variables, class properties)
        shared_state::detect_shared_state(&tree, &content, &file_str, &mut ctx);
        total_shared_states += ctx.shared_states.len() as u32;

        // Detect async patterns and map accesses
        let async_count = async_detection::detect_async_patterns(&tree, &content, &file_str, &mut ctx);
        total_async_funcs += async_count;

        // Detect patterns
        let mut issues = patterns::detect_all_patterns(&ctx, &file_str);

        // Add async-specific issues
        let async_issues = async_detection::detect_async_races(&ctx, &file_str);
        issues.extend(async_issues);

        // Collect lock names (TypeScript doesn't have built-in locks, but we track custom ones)
        let lock_names: HashSet<String> = ctx.locks.iter().map(|l| l.name.clone()).collect();

        // Apply heuristics
        for issue in &mut issues {
            if let Some(state) = ctx.shared_states.iter().find(|s| s.name == issue.state_name) {
                heuristics::adjust_severity(issue, state, is_test);
            }
        }

        // Filter false positives
        let issues = heuristics::filter_false_positives(issues, &ctx.shared_states, &lock_names);

        // Convert to CheckIssues
        for issue in issues {
            if !file_str.is_empty() {
                files_with_issues.insert(file_str.clone());
            }

            all_issues.push(
                IssueBuilder::new(&file_str, &issue.message)
                    .line(issue.line)
                    .code(issue.pattern.code())
                    .severity(race_severity_to_issue_severity(issue.severity))
                    .build(),
            );
        }
    }

    let issues_found = all_issues.len() as u32;
    let files_checked = files.len() as u32;

    let error_count = all_issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Error)
        .count() as u32;
    let warning_count = all_issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Warning)
        .count() as u32;
    let info_count = all_issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Info)
        .count() as u32;

    Ok(ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked,
        structured_output: CheckStructuredOutput {
            issues: all_issues,
            summary: Some(CheckSummary {
                total_files: files_checked,
                files_with_issues: files_with_issues.len() as u32,
                total_issues: issues_found,
                issues_by_severity: HashMap::from([
                    ("error".to_string(), error_count),
                    ("warning".to_string(), warning_count),
                    ("info".to_string(), info_count),
                ]),
            }),
            tool_data: HashMap::from([
                (
                    "shared_states_detected".to_string(),
                    serde_json::json!(total_shared_states),
                ),
                (
                    "async_functions_detected".to_string(),
                    serde_json::json!(total_async_funcs),
                ),
            ]),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, name: &str, content: &str) {
        let path = dir.join(name);
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn test_analyze_safe_code() {
        let temp_dir = TempDir::new().unwrap();
        let code = r#"
// Using local state only
async function fetchData() {
    const response = await fetch('/api/data');
    const data = await response.json();
    return data;
}
"#;
        create_test_file(temp_dir.path(), "safe.ts", code);

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result.files_checked, 1);
    }

    #[test]
    fn test_analyze_shared_state() {
        let temp_dir = TempDir::new().unwrap();
        let code = r#"
let globalCounter = 0;

async function incrementCounter() {
    const current = globalCounter;
    await someAsyncOperation();
    globalCounter = current + 1;  // Race condition!
}
"#;
        create_test_file(temp_dir.path(), "unsafe.ts", code);

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result.files_checked, 1);
        // Should detect shared state and potential race
    }

    #[test]
    fn test_analyze_nonexistent_dir() {
        let result = analyze("/nonexistent/path/that/does/not/exist");
        assert!(result.is_err());
    }
}
