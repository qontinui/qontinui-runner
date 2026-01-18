//! Rust Race Condition Analyzer
//!
//! Detects potential race conditions in Rust code by analyzing:
//! - `static mut` variables (inherently unsafe)
//! - `lazy_static!` and `OnceCell` usage
//! - Arc<Mutex<T>> and Arc<RwLock<T>> patterns
//! - Unsafe blocks accessing shared state

pub mod shared_state;
pub mod sync_detection;

use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::analyzers::concurrency::heuristics;
use crate::check_executor::analyzers::concurrency::patterns;
use crate::check_executor::analyzers::concurrency::race_severity_to_issue_severity;
use crate::check_executor::analyzers::concurrency::types::AnalysisContext;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{CheckStructuredOutput, CheckSummary, IssueSeverity};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use syn::Item;
use tracing::debug;

/// Analyze Rust files for race conditions
pub fn analyze(working_dir: &str) -> Result<ParsedOutput, String> {
    let root = Path::new(working_dir);
    if !root.exists() {
        return Err(format!("Directory does not exist: {}", working_dir));
    }

    let config = WalkConfig {
        extensions: vec!["rs".to_string()],
        ..Default::default()
    };

    let files = walk_files(root, &config);
    debug!(
        "Found {} Rust files to analyze for race conditions",
        files.len()
    );

    let mut all_issues = Vec::new();
    let mut files_with_issues = HashSet::new();
    let mut total_shared_states = 0u32;
    let mut total_syncs = 0u32;

    for file_path in &files {
        let file_str = file_path.to_string_lossy().to_string();
        let is_test = heuristics::is_test_file(&file_str);

        let content = match fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(e) => {
                debug!("Failed to read file {:?}: {}", file_path, e);
                continue;
            }
        };

        let syntax = match syn::parse_file(&content) {
            Ok(s) => s,
            Err(e) => {
                debug!("Failed to parse file {:?}: {}", file_path, e);
                continue;
            }
        };

        let mut ctx = AnalysisContext::new();

        // Detect shared state (static mut, lazy_static, etc.)
        shared_state::detect_shared_state(&syntax, &file_str, &mut ctx);
        total_shared_states += ctx.shared_states.len() as u32;

        // Detect synchronization primitives
        sync_detection::detect_sync_primitives(&syntax, &mut ctx);
        total_syncs += ctx.locks.len() as u32;

        // Analyze function bodies for access patterns
        for item in &syntax.items {
            analyze_item(item, &file_str, &mut ctx);
        }

        // Detect patterns
        let mut issues = patterns::detect_all_patterns(&ctx, &file_str);

        // Collect lock names for false positive filtering
        let lock_names: HashSet<String> = ctx.locks.iter().map(|l| l.name.clone()).collect();

        // Apply heuristics
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
                    "sync_primitives_detected".to_string(),
                    serde_json::json!(total_syncs),
                ),
            ]),
        },
    })
}

/// Analyze a single item (function, impl, etc.)
fn analyze_item(item: &Item, file: &str, ctx: &mut AnalysisContext) {
    match item {
        Item::Fn(func) => {
            shared_state::analyze_function_body(&func.block, file, ctx);
        }
        Item::Impl(impl_block) => {
            for impl_item in &impl_block.items {
                if let syn::ImplItem::Fn(method) = impl_item {
                    shared_state::analyze_function_body(&method.block, file, ctx);
                }
            }
        }
        Item::Mod(module) => {
            if let Some((_, items)) = &module.content {
                for item in items {
                    analyze_item(item, file, ctx);
                }
            }
        }
        _ => {}
    }
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
use std::sync::{Arc, Mutex};

struct SafeCounter {
    count: Arc<Mutex<i32>>,
}

impl SafeCounter {
    fn new() -> Self {
        Self {
            count: Arc::new(Mutex::new(0)),
        }
    }

    fn increment(&self) {
        let mut guard = self.count.lock().unwrap();
        *guard += 1;
    }
}
"#;
        create_test_file(temp_dir.path(), "safe.rs", code);

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result.files_checked, 1);
    }

    #[test]
    fn test_analyze_static_mut() {
        let temp_dir = TempDir::new().unwrap();
        let code = r#"
static mut COUNTER: i32 = 0;

unsafe fn increment() {
    COUNTER += 1;
}

fn get_count() -> i32 {
    unsafe { COUNTER }
}
"#;
        create_test_file(temp_dir.path(), "unsafe_static.rs", code);

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result.files_checked, 1);
        // Should detect the static mut as potential race condition
    }

    #[test]
    fn test_analyze_nonexistent_dir() {
        let result = analyze("/nonexistent/path/that/does/not/exist");
        assert!(result.is_err());
    }
}
