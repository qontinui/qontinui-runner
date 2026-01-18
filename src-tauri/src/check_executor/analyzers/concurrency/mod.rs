//! Concurrency Analysis Module
//!
//! Static analysis for detecting potential race conditions in Python, Rust, and TypeScript code.
//! Uses tree-sitter AST parsing for language-specific analysis.

pub mod heuristics;
pub mod patterns;
pub mod python;
pub mod rust;
pub mod typescript;
pub mod types;

use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{CheckStructuredOutput, CheckSummary, IssueSeverity};
use std::collections::HashMap;
use std::path::Path;
use types::RaceSeverity;

/// Analyze a directory for race conditions across all supported languages
pub fn analyze_all(working_dir: &str) -> Result<ParsedOutput, String> {
    let root = Path::new(working_dir);
    if !root.exists() {
        return Err(format!("Directory does not exist: {}", working_dir));
    }

    let mut all_issues = Vec::new();
    let mut total_files = 0u32;
    let mut files_with_issues = std::collections::HashSet::new();
    let mut tool_data: HashMap<String, serde_json::Value> = HashMap::new();

    // Analyze Python files
    let py_result = python::analyze(working_dir);
    if let Ok(py_output) = py_result {
        total_files += py_output.files_checked;
        for issue in &py_output.structured_output.issues {
            if !issue.file.is_empty() {
                files_with_issues.insert(issue.file.clone());
            }
        }
        all_issues.extend(py_output.structured_output.issues);
        tool_data.insert(
            "python_files".to_string(),
            serde_json::json!(py_output.files_checked),
        );
        tool_data.insert(
            "python_issues".to_string(),
            serde_json::json!(py_output.issues_found),
        );
    }

    // Analyze Rust files
    let rust_result = rust::analyze(working_dir);
    if let Ok(rust_output) = rust_result {
        total_files += rust_output.files_checked;
        for issue in &rust_output.structured_output.issues {
            if !issue.file.is_empty() {
                files_with_issues.insert(issue.file.clone());
            }
        }
        all_issues.extend(rust_output.structured_output.issues);
        tool_data.insert(
            "rust_files".to_string(),
            serde_json::json!(rust_output.files_checked),
        );
        tool_data.insert(
            "rust_issues".to_string(),
            serde_json::json!(rust_output.issues_found),
        );
    }

    // Analyze TypeScript files
    let ts_result = typescript::analyze(working_dir);
    if let Ok(ts_output) = ts_result {
        total_files += ts_output.files_checked;
        for issue in &ts_output.structured_output.issues {
            if !issue.file.is_empty() {
                files_with_issues.insert(issue.file.clone());
            }
        }
        all_issues.extend(ts_output.structured_output.issues);
        tool_data.insert(
            "typescript_files".to_string(),
            serde_json::json!(ts_output.files_checked),
        );
        tool_data.insert(
            "typescript_issues".to_string(),
            serde_json::json!(ts_output.issues_found),
        );
    }

    let issues_found = all_issues.len() as u32;

    // Count by severity
    let critical_count = all_issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Error && i.code.as_deref() == Some("RACE001"))
        .count() as u32;
    let high_count = all_issues
        .iter()
        .filter(|i| {
            i.severity == IssueSeverity::Error
                && matches!(i.code.as_deref(), Some("RACE002") | Some("RACE003") | Some("RACE004"))
        })
        .count() as u32;
    let medium_count = all_issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Warning)
        .count() as u32;
    let low_count = all_issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Info)
        .count() as u32;

    Ok(ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked: total_files,
        structured_output: CheckStructuredOutput {
            issues: all_issues,
            summary: Some(CheckSummary {
                total_files,
                files_with_issues: files_with_issues.len() as u32,
                total_issues: issues_found,
                issues_by_severity: HashMap::from([
                    ("critical".to_string(), critical_count),
                    ("high".to_string(), high_count),
                    ("medium".to_string(), medium_count),
                    ("low".to_string(), low_count),
                ]),
            }),
            tool_data,
        },
    })
}

/// Convert RaceSeverity to IssueSeverity
pub fn race_severity_to_issue_severity(severity: RaceSeverity) -> IssueSeverity {
    match severity {
        RaceSeverity::Critical | RaceSeverity::High => IssueSeverity::Error,
        RaceSeverity::Medium => IssueSeverity::Warning,
        RaceSeverity::Low => IssueSeverity::Info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, name: &str, content: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn test_analyze_all_empty_dir() {
        let temp_dir = TempDir::new().unwrap();
        let result = analyze_all(temp_dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result.issues_found, 0);
    }

    #[test]
    fn test_analyze_nonexistent_dir() {
        let result = analyze_all("/nonexistent/path/that/does/not/exist");
        assert!(result.is_err());
    }

    #[test]
    #[ignore] // Run with: cargo test test_analyze_qontinui_codebase -- --ignored --nocapture
    fn test_analyze_qontinui_codebase() {
        // Analyze the qontinui Python library
        let qontinui_path = "D:/qontinui_parent_directory/qontinui";

        println!("\n=== Analyzing qontinui codebase for race conditions ===\n");

        let result = analyze_all(qontinui_path);
        match result {
            Ok(output) => {
                println!("Files checked: {}", output.files_checked);
                println!("Issues found: {}", output.issues_found);

                if let Some(summary) = &output.structured_output.summary {
                    println!("\nSummary:");
                    println!("  Total files: {}", summary.total_files);
                    println!("  Files with issues: {}", summary.files_with_issues);
                    println!("  Issues by severity: {:?}", summary.issues_by_severity);
                }

                println!("\nTool data:");
                for (key, value) in &output.structured_output.tool_data {
                    println!("  {}: {}", key, value);
                }

                if !output.structured_output.issues.is_empty() {
                    println!("\n=== Issues Found ===\n");
                    for issue in &output.structured_output.issues {
                        println!("[{}] {} (line {})",
                            issue.code.as_deref().unwrap_or("???"),
                            issue.file,
                            issue.line.unwrap_or(0)
                        );
                        println!("  Severity: {:?}", issue.severity);
                        println!("  Message: {}", issue.message);
                        println!();
                    }
                } else {
                    println!("\nNo issues found!");
                }
            }
            Err(e) => {
                println!("Error analyzing: {}", e);
            }
        }
    }
}
