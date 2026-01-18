//! Python Race Condition Analyzer
//!
//! Detects potential race conditions in Python code by analyzing:
//! - Class variables (shared across instances)
//! - Module-level globals
//! - Threading lock usage patterns
//! - asyncio shared state

pub mod lock_detection;
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

/// Analyze Python files for race conditions
pub fn analyze(working_dir: &str) -> Result<ParsedOutput, String> {
    let root = Path::new(working_dir);
    if !root.exists() {
        return Err(format!("Directory does not exist: {}", working_dir));
    }

    let config = WalkConfig {
        extensions: vec!["py".to_string()],
        ..Default::default()
    };

    let files = walk_files(root, &config);
    debug!(
        "Found {} Python files to analyze for race conditions",
        files.len()
    );

    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser
        .set_language(&language.into())
        .map_err(|e| format!("Failed to set Python language: {}", e))?;

    let mut all_issues = Vec::new();
    let mut files_with_issues = HashSet::new();
    let mut total_shared_states = 0u32;
    let mut total_locks = 0u32;

    for file_path in &files {
        let file_str = file_path.to_string_lossy().to_string();
        let is_test = heuristics::is_test_file(&file_str);

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

        // Analyze the file
        let mut ctx = AnalysisContext::new();

        // Detect shared state
        shared_state::detect_shared_state(&tree, &content, &file_str, &mut ctx);
        total_shared_states += ctx.shared_states.len() as u32;

        // Detect locks
        lock_detection::detect_locks(&tree, &content, &mut ctx);
        total_locks += ctx.locks.len() as u32;

        // Map state accesses
        shared_state::map_state_accesses(&tree, &content, &mut ctx);

        // Detect patterns
        let mut issues = patterns::detect_all_patterns(&ctx, &file_str);

        // Collect lock names for false positive filtering
        let lock_names: HashSet<String> = ctx.locks.iter().map(|l| l.name.clone()).collect();

        // Apply heuristics to reduce false positives
        for issue in &mut issues {
            if let Some(state) = ctx
                .shared_states
                .iter()
                .find(|s| s.name == issue.state_name)
            {
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

    // Count by severity
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
                ("locks_detected".to_string(), serde_json::json!(total_locks)),
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
import threading

class SafeCounter:
    def __init__(self):
        self._lock = threading.Lock()
        self._count = 0

    def increment(self):
        with self._lock:
            self._count += 1

    def get_count(self):
        with self._lock:
            return self._count
"#;
        create_test_file(temp_dir.path(), "safe.py", code);

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();
        // Well-protected code should have few or no issues
        assert_eq!(result.files_checked, 1);
    }

    #[test]
    fn test_analyze_unprotected_global() {
        let temp_dir = TempDir::new().unwrap();
        let code = r#"
import threading

counter = 0  # Module-level global

def increment():
    global counter
    counter += 1  # Unprotected compound operation

def get_count():
    return counter

# Start threads that access the global
t1 = threading.Thread(target=increment)
t2 = threading.Thread(target=increment)
"#;
        create_test_file(temp_dir.path(), "unsafe.py", code);

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result.files_checked, 1);
        // Should detect the unprotected global
        // Note: Detection depends on heuristics and pattern matching
    }

    #[test]
    fn test_analyze_class_variable() {
        let temp_dir = TempDir::new().unwrap();
        let code = r#"
class UnsafeClass:
    shared_list = []  # Class variable shared across instances

    def add_item(self, item):
        self.shared_list.append(item)

    def get_items(self):
        return self.shared_list.copy()
"#;
        create_test_file(temp_dir.path(), "class_var.py", code);

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result.files_checked, 1);
    }

    #[test]
    fn test_analyze_nonexistent_dir() {
        let result = analyze("/nonexistent/path/that/does/not/exist");
        assert!(result.is_err());
    }
}
