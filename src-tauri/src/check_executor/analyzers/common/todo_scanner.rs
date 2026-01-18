//! TODO/FIXME/HACK Comment Scanner
//!
//! Scans source files for TODO, FIXME, HACK, XXX, and other technical debt markers.
//! Works across multiple languages (Python, TypeScript, JavaScript, Rust, etc.).

use super::file_walker::{walk_files, WalkConfig};
use super::issue_builder::IssueBuilder;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{
    CheckIssue, CheckStructuredOutput, CheckSummary, IssueSeverity,
};
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::debug;

/// Types of comment markers we scan for
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkerType {
    /// TODO - something to be done later
    Todo,
    /// FIXME - broken code that needs fixing
    Fixme,
    /// HACK - temporary workaround
    Hack,
    /// XXX - warning about problematic code
    Xxx,
    /// NOTE - important information
    Note,
    /// OPTIMIZE - performance improvement needed
    Optimize,
    /// BUG - known bug that needs attention
    Bug,
    /// REVIEW - code that needs review
    Review,
}

impl MarkerType {
    fn as_str(&self) -> &'static str {
        match self {
            MarkerType::Todo => "TODO",
            MarkerType::Fixme => "FIXME",
            MarkerType::Hack => "HACK",
            MarkerType::Xxx => "XXX",
            MarkerType::Note => "NOTE",
            MarkerType::Optimize => "OPTIMIZE",
            MarkerType::Bug => "BUG",
            MarkerType::Review => "REVIEW",
        }
    }

    fn severity(&self) -> IssueSeverity {
        match self {
            MarkerType::Fixme | MarkerType::Bug => IssueSeverity::Warning,
            MarkerType::Hack | MarkerType::Xxx => IssueSeverity::Warning,
            MarkerType::Todo | MarkerType::Optimize | MarkerType::Review => IssueSeverity::Info,
            MarkerType::Note => IssueSeverity::Hint,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            MarkerType::Todo => "TODO001",
            MarkerType::Fixme => "FIXME001",
            MarkerType::Hack => "HACK001",
            MarkerType::Xxx => "XXX001",
            MarkerType::Note => "NOTE001",
            MarkerType::Optimize => "OPT001",
            MarkerType::Bug => "BUG001",
            MarkerType::Review => "REV001",
        }
    }
}

/// A found comment marker with its context
#[derive(Debug, Clone)]
struct FoundMarker {
    marker_type: MarkerType,
    line: u32,
    column: u32,
    text: String,
}

/// Compiled regex patterns for all marker types
struct CompiledPatterns {
    patterns: Vec<(MarkerType, Regex)>,
}

impl CompiledPatterns {
    fn new() -> Self {
        // Pattern matches: TODO, FIXME, etc. followed by optional colon and text
        // Handles common comment prefixes: //, #, /*, *, <!--
        let markers = [
            (MarkerType::Todo, r"(?i)\bTODO\b"),
            (MarkerType::Fixme, r"(?i)\bFIXME\b"),
            (MarkerType::Hack, r"(?i)\bHACK\b"),
            (MarkerType::Xxx, r"(?i)\bXXX\b"),
            (MarkerType::Note, r"(?i)\bNOTE\b"),
            (MarkerType::Optimize, r"(?i)\bOPTIMIZE\b"),
            (MarkerType::Bug, r"(?i)\bBUG\b"),
            (MarkerType::Review, r"(?i)\bREVIEW\b"),
        ];

        let patterns = markers
            .into_iter()
            .filter_map(|(marker_type, pattern)| {
                Regex::new(pattern).ok().map(|regex| (marker_type, regex))
            })
            .collect();

        Self { patterns }
    }

    fn find_markers(&self, line: &str, line_num: u32) -> Vec<FoundMarker> {
        let mut markers = Vec::new();

        // Check if this line looks like a comment
        let trimmed = line.trim();
        let is_comment = trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with('*')
            || trimmed.starts_with("/*")
            || trimmed.starts_with("<!--")
            || trimmed.contains("//")
            || trimmed.contains("/*");

        // Only scan comment-like lines to reduce false positives
        if !is_comment {
            return markers;
        }

        for (marker_type, regex) in &self.patterns {
            if let Some(mat) = regex.find(line) {
                // Extract the text after the marker
                let after_marker = &line[mat.end()..];
                let text = extract_comment_text(after_marker);

                markers.push(FoundMarker {
                    marker_type: *marker_type,
                    line: line_num,
                    column: (mat.start() + 1) as u32,
                    text,
                });
            }
        }

        markers
    }
}

/// Extract the meaningful text after a marker (removes : and leading whitespace)
fn extract_comment_text(text: &str) -> String {
    let text = text.trim_start();
    let text = text.strip_prefix(':').unwrap_or(text);
    let text = text.trim_start();

    // Take until end of line or end of comment
    let text = text
        .split("*/")
        .next()
        .unwrap_or(text)
        .split("-->")
        .next()
        .unwrap_or(text);

    text.trim().to_string()
}

/// Scan a single file for comment markers
fn scan_file(file_path: &Path, content: &str, patterns: &CompiledPatterns) -> Vec<CheckIssue> {
    let mut issues = Vec::new();
    let file_str = file_path.to_string_lossy().to_string();

    for (line_num, line) in content.lines().enumerate() {
        let line_number = (line_num + 1) as u32;
        let markers = patterns.find_markers(line, line_number);

        for marker in markers {
            let message = if marker.text.is_empty() {
                format!("{} marker found", marker.marker_type.as_str())
            } else {
                format!("{}: {}", marker.marker_type.as_str(), marker.text)
            };

            let issue = IssueBuilder::new(&file_str, message)
                .line(marker.line)
                .column(marker.column)
                .code(marker.marker_type.code())
                .severity(marker.marker_type.severity())
                .build();

            issues.push(issue);
        }
    }

    issues
}

/// Analyze a directory for TODO/FIXME/HACK markers
pub fn analyze(working_dir: &str) -> Result<ParsedOutput, String> {
    let root = Path::new(working_dir);
    if !root.exists() {
        return Err(format!("Directory does not exist: {}", working_dir));
    }

    // Configure file walking - scan common source file types
    let config = WalkConfig {
        extensions: vec![
            // Rust
            "rs".to_string(),
            // Python
            "py".to_string(),
            // TypeScript/JavaScript
            "ts".to_string(),
            "tsx".to_string(),
            "js".to_string(),
            "jsx".to_string(),
            "mjs".to_string(),
            "cjs".to_string(),
            // Web
            "html".to_string(),
            "css".to_string(),
            "scss".to_string(),
            "vue".to_string(),
            "svelte".to_string(),
            // Config
            "yaml".to_string(),
            "yml".to_string(),
            "toml".to_string(),
            "json".to_string(),
            // Other
            "go".to_string(),
            "java".to_string(),
            "c".to_string(),
            "cpp".to_string(),
            "h".to_string(),
            "hpp".to_string(),
            "cs".to_string(),
            "rb".to_string(),
            "php".to_string(),
            "swift".to_string(),
            "kt".to_string(),
            "scala".to_string(),
            "sh".to_string(),
            "bash".to_string(),
            "ps1".to_string(),
            "sql".to_string(),
            "md".to_string(),
        ],
        ..Default::default()
    };

    let files = walk_files(root, &config);
    debug!("Found {} source files to scan for TODOs", files.len());

    let patterns = CompiledPatterns::new();
    let mut all_issues: Vec<CheckIssue> = Vec::new();
    let mut files_checked = 0u32;
    let mut files_with_issues = 0u32;
    let mut marker_counts: HashMap<String, u32> = HashMap::new();

    for file_path in files {
        files_checked += 1;

        let content = match fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(e) => {
                debug!("Failed to read file {:?}: {}", file_path, e);
                continue;
            }
        };

        let issues = scan_file(&file_path, &content, &patterns);

        if !issues.is_empty() {
            files_with_issues += 1;

            // Count markers by type
            for issue in &issues {
                if let Some(code) = &issue.code {
                    *marker_counts.entry(code.clone()).or_insert(0) += 1;
                }
            }

            all_issues.extend(issues);
        }
    }

    let issues_found = all_issues.len() as u32;

    // Count by severity
    let warning_count = all_issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Warning)
        .count() as u32;
    let info_count = all_issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Info)
        .count() as u32;
    let hint_count = all_issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Hint)
        .count() as u32;

    // Build tool_data with marker breakdown
    let mut tool_data: HashMap<String, serde_json::Value> = HashMap::new();
    tool_data.insert(
        "todo_count".to_string(),
        serde_json::json!(marker_counts.get("TODO001").copied().unwrap_or(0)),
    );
    tool_data.insert(
        "fixme_count".to_string(),
        serde_json::json!(marker_counts.get("FIXME001").copied().unwrap_or(0)),
    );
    tool_data.insert(
        "hack_count".to_string(),
        serde_json::json!(marker_counts.get("HACK001").copied().unwrap_or(0)),
    );
    tool_data.insert(
        "xxx_count".to_string(),
        serde_json::json!(marker_counts.get("XXX001").copied().unwrap_or(0)),
    );
    tool_data.insert(
        "note_count".to_string(),
        serde_json::json!(marker_counts.get("NOTE001").copied().unwrap_or(0)),
    );
    tool_data.insert(
        "optimize_count".to_string(),
        serde_json::json!(marker_counts.get("OPT001").copied().unwrap_or(0)),
    );
    tool_data.insert(
        "bug_count".to_string(),
        serde_json::json!(marker_counts.get("BUG001").copied().unwrap_or(0)),
    );
    tool_data.insert(
        "review_count".to_string(),
        serde_json::json!(marker_counts.get("REV001").copied().unwrap_or(0)),
    );
    tool_data.insert(
        "marker_counts".to_string(),
        serde_json::json!(marker_counts),
    );

    Ok(ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked,
        structured_output: CheckStructuredOutput {
            issues: all_issues,
            summary: Some(CheckSummary {
                total_files: files_checked,
                files_with_issues,
                total_issues: issues_found,
                issues_by_severity: HashMap::from([
                    ("warning".to_string(), warning_count),
                    ("info".to_string(), info_count),
                    ("hint".to_string(), hint_count),
                ]),
            }),
            tool_data,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, name: &str, content: &str) {
        let file_path = dir.join(name);
        let mut file = File::create(file_path).unwrap();
        write!(file, "{}", content).unwrap();
    }

    #[test]
    fn test_extract_comment_text() {
        assert_eq!(extract_comment_text(": fix this"), "fix this");
        assert_eq!(extract_comment_text(" fix this"), "fix this");
        assert_eq!(extract_comment_text("fix this */"), "fix this");
        assert_eq!(extract_comment_text(": fix this -->"), "fix this");
    }

    #[test]
    fn test_scan_rust_todo() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
fn main() {
    // TODO: implement this function
    let x = 1;
}
"#;
        create_test_file(temp_dir.path(), "main.rs", content);

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();

        assert_eq!(result.issues_found, 1);
        assert!(result
            .structured_output
            .issues
            .iter()
            .any(|i| i.code == Some("TODO001".to_string())));
    }

    #[test]
    fn test_scan_python_fixme() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
def broken_function():
    # FIXME: this is broken
    return None
"#;
        create_test_file(temp_dir.path(), "module.py", content);

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();

        assert_eq!(result.issues_found, 1);
        assert!(result
            .structured_output
            .issues
            .iter()
            .any(|i| i.code == Some("FIXME001".to_string())));
    }

    #[test]
    fn test_scan_typescript_hack() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
function workaround() {
    // HACK: temporary workaround for bug #123
    return 42;
}
"#;
        create_test_file(temp_dir.path(), "utils.ts", content);

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();

        assert_eq!(result.issues_found, 1);
        assert!(result
            .structured_output
            .issues
            .iter()
            .any(|i| i.code == Some("HACK001".to_string())));
    }

    #[test]
    fn test_scan_multiple_markers() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
// TODO: first task
// FIXME: broken code
// HACK: workaround
// XXX: warning
// NOTE: important info
"#;
        create_test_file(temp_dir.path(), "multi.rs", content);

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();

        assert_eq!(result.issues_found, 5);
    }

    #[test]
    fn test_scan_block_comments() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
/*
 * TODO: implement feature
 * FIXME: fix this bug
 */
fn placeholder() {}
"#;
        create_test_file(temp_dir.path(), "block.rs", content);

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();

        assert_eq!(result.issues_found, 2);
    }

    #[test]
    fn test_scan_no_markers() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
fn clean_function() {
    // This is a regular comment
    let x = 1;
}
"#;
        create_test_file(temp_dir.path(), "clean.rs", content);

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();

        assert_eq!(result.issues_found, 0);
    }

    #[test]
    fn test_case_insensitive() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
// todo: lowercase
// Todo: mixed case
// TODO: uppercase
"#;
        create_test_file(temp_dir.path(), "case.rs", content);

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();

        assert_eq!(result.issues_found, 3);
    }

    #[test]
    fn test_scan_empty_directory() {
        let temp_dir = TempDir::new().unwrap();

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();

        assert_eq!(result.issues_found, 0);
        assert_eq!(result.files_checked, 0);
    }

    #[test]
    fn test_scan_nonexistent_directory() {
        let result = analyze("/nonexistent/path/that/does/not/exist");

        assert!(result.is_err());
    }

    #[test]
    fn test_marker_severity() {
        assert_eq!(MarkerType::Fixme.severity(), IssueSeverity::Warning);
        assert_eq!(MarkerType::Bug.severity(), IssueSeverity::Warning);
        assert_eq!(MarkerType::Hack.severity(), IssueSeverity::Warning);
        assert_eq!(MarkerType::Todo.severity(), IssueSeverity::Info);
        assert_eq!(MarkerType::Note.severity(), IssueSeverity::Hint);
    }

    #[test]
    fn test_tool_data_counts() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
// TODO: task 1
// TODO: task 2
// FIXME: bug
"#;
        create_test_file(temp_dir.path(), "counts.rs", content);

        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();

        assert_eq!(
            result.structured_output.tool_data.get("todo_count"),
            Some(&serde_json::json!(2))
        );
        assert_eq!(
            result.structured_output.tool_data.get("fixme_count"),
            Some(&serde_json::json!(1))
        );
    }
}
