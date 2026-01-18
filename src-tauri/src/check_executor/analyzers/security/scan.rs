//! Security vulnerability scanner
//!
//! Scans source files for common security vulnerabilities using regex patterns.

use super::patterns::{SecurityPattern, ALL_PATTERNS};
use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{
    CheckIssue, CheckStructuredOutput, CheckSummary, IssueSeverity,
};
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::{debug, warn};

/// File extension to language mapping
fn extension_to_language(ext: &str) -> Option<&'static str> {
    match ext.to_lowercase().as_str() {
        "py" => Some("python"),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some("typescript"),
        "rs" => Some("rust"),
        _ => None,
    }
}

/// Get IssueSeverity from pattern severity string
fn severity_to_issue_severity(severity: &str) -> IssueSeverity {
    match severity {
        "critical" => IssueSeverity::Error,
        "high" => IssueSeverity::Error,
        "medium" => IssueSeverity::Warning,
        "low" => IssueSeverity::Info,
        _ => IssueSeverity::Warning,
    }
}

/// Result of scanning a single file
struct FileScanResult {
    issues: Vec<CheckIssue>,
    patterns_matched: HashMap<String, u32>,
}

/// Scan a single file with the given patterns
fn scan_file(
    file_path: &Path,
    content: &str,
    patterns: &[&SecurityPattern],
    compiled_patterns: &HashMap<&str, Regex>,
) -> FileScanResult {
    let mut issues = Vec::new();
    let mut patterns_matched: HashMap<String, u32> = HashMap::new();
    let file_str = file_path.to_string_lossy().to_string();

    for (line_num, line) in content.lines().enumerate() {
        let line_number = (line_num + 1) as u32;

        for pattern in patterns {
            if let Some(regex) = compiled_patterns.get(pattern.id) {
                if let Some(mat) = regex.find(line) {
                    // Track pattern matches
                    *patterns_matched.entry(pattern.id.to_string()).or_insert(0) += 1;

                    // Create issue
                    let issue = IssueBuilder::new(
                        &file_str,
                        format!("{}: {}", pattern.name, pattern.description),
                    )
                    .line(line_number)
                    .column((mat.start() + 1) as u32)
                    .end_column((mat.end() + 1) as u32)
                    .code(pattern.id)
                    .severity(severity_to_issue_severity(pattern.severity))
                    .build();

                    issues.push(issue);
                }
            }
        }
    }

    FileScanResult {
        issues,
        patterns_matched,
    }
}

/// Analyze a directory for security vulnerabilities
pub fn analyze(working_dir: &str, patterns: &[SecurityPattern]) -> Result<ParsedOutput, String> {
    let root = Path::new(working_dir);
    if !root.exists() {
        return Err(format!("Directory does not exist: {}", working_dir));
    }

    // Configure file walking
    let config = WalkConfig {
        extensions: vec![
            "py".to_string(),
            "ts".to_string(),
            "tsx".to_string(),
            "js".to_string(),
            "jsx".to_string(),
            "rs".to_string(),
        ],
        ..Default::default()
    };

    // Walk directory and get files
    let files = walk_files(root, &config);
    debug!("Found {} source files to scan", files.len());

    // Compile all regex patterns once
    let mut compiled_patterns: HashMap<&str, Regex> = HashMap::new();
    for pattern in patterns {
        match Regex::new(pattern.pattern) {
            Ok(regex) => {
                compiled_patterns.insert(pattern.id, regex);
            }
            Err(e) => {
                warn!("Failed to compile pattern {}: {}", pattern.id, e);
            }
        }
    }

    // Group patterns by language for efficient lookup
    let all_language_patterns: Vec<&SecurityPattern> = patterns
        .iter()
        .filter(|p| p.languages.contains(&"all"))
        .collect();

    let python_patterns: Vec<&SecurityPattern> = patterns
        .iter()
        .filter(|p| p.languages.contains(&"python"))
        .collect();

    let typescript_patterns: Vec<&SecurityPattern> = patterns
        .iter()
        .filter(|p| p.languages.contains(&"typescript"))
        .collect();

    let rust_patterns: Vec<&SecurityPattern> = patterns
        .iter()
        .filter(|p| p.languages.contains(&"rust"))
        .collect();

    // Scan all files
    let mut all_issues: Vec<CheckIssue> = Vec::new();
    let mut files_with_issues = 0u32;
    let mut total_patterns_matched: HashMap<String, u32> = HashMap::new();
    let mut severity_counts: HashMap<String, u32> = HashMap::new();

    for file_path in &files {
        // Determine language from extension
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language = extension_to_language(ext);

        // Get applicable patterns for this file
        let mut applicable_patterns: Vec<&SecurityPattern> = all_language_patterns.clone();
        if let Some(lang) = language {
            match lang {
                "python" => applicable_patterns.extend(python_patterns.iter().cloned()),
                "typescript" => applicable_patterns.extend(typescript_patterns.iter().cloned()),
                "rust" => applicable_patterns.extend(rust_patterns.iter().cloned()),
                _ => {}
            }
        }

        // Skip if no patterns apply
        if applicable_patterns.is_empty() {
            continue;
        }

        // Read file content
        let content = match fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(e) => {
                debug!("Failed to read file {:?}: {}", file_path, e);
                continue;
            }
        };

        // Scan the file
        let result = scan_file(
            file_path,
            &content,
            &applicable_patterns,
            &compiled_patterns,
        );

        if !result.issues.is_empty() {
            files_with_issues += 1;

            // Count severities
            for issue in &result.issues {
                let severity_key = match issue.severity {
                    IssueSeverity::Error => "critical",
                    IssueSeverity::Warning => "medium",
                    IssueSeverity::Info => "low",
                    IssueSeverity::Hint => "info",
                };
                *severity_counts.entry(severity_key.to_string()).or_insert(0) += 1;
            }

            all_issues.extend(result.issues);
        }

        // Merge pattern match counts
        for (pattern_id, count) in result.patterns_matched {
            *total_patterns_matched.entry(pattern_id).or_insert(0) += count;
        }
    }

    let issues_found = all_issues.len() as u32;
    let files_checked = files.len() as u32;

    // Build tool_data with pattern match summary
    let mut tool_data: HashMap<String, serde_json::Value> = HashMap::new();
    tool_data.insert(
        "patterns_checked".to_string(),
        serde_json::json!(patterns.len()),
    );
    tool_data.insert(
        "patterns_matched".to_string(),
        serde_json::json!(total_patterns_matched),
    );

    // Add severity breakdown
    let critical_count = severity_counts.get("critical").copied().unwrap_or(0);
    let medium_count = severity_counts.get("medium").copied().unwrap_or(0);
    let low_count = severity_counts.get("low").copied().unwrap_or(0);

    tool_data.insert(
        "critical_issues".to_string(),
        serde_json::json!(critical_count),
    );
    tool_data.insert("medium_issues".to_string(), serde_json::json!(medium_count));
    tool_data.insert("low_issues".to_string(), serde_json::json!(low_count));

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
                    ("critical".to_string(), critical_count),
                    (
                        "high".to_string(),
                        severity_counts.get("high").copied().unwrap_or(0),
                    ),
                    ("medium".to_string(), medium_count),
                    ("low".to_string(), low_count),
                ]),
            }),
            tool_data,
        },
    })
}

/// Analyze with all default patterns
pub fn analyze_with_defaults(working_dir: &str) -> Result<ParsedOutput, String> {
    analyze(working_dir, ALL_PATTERNS)
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
    fn test_extension_to_language() {
        assert_eq!(extension_to_language("py"), Some("python"));
        assert_eq!(extension_to_language("ts"), Some("typescript"));
        assert_eq!(extension_to_language("tsx"), Some("typescript"));
        assert_eq!(extension_to_language("rs"), Some("rust"));
        assert_eq!(extension_to_language("txt"), None);
    }

    #[test]
    fn test_severity_conversion() {
        assert_eq!(severity_to_issue_severity("critical"), IssueSeverity::Error);
        assert_eq!(severity_to_issue_severity("high"), IssueSeverity::Error);
        assert_eq!(severity_to_issue_severity("medium"), IssueSeverity::Warning);
        assert_eq!(severity_to_issue_severity("low"), IssueSeverity::Info);
    }

    #[test]
    fn test_scan_hardcoded_secret() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
api_key = "sk_live_1234567890abcdefghij"
password = "mysecretpassword123"
"#;
        create_test_file(temp_dir.path(), "config.py", content);

        let result = analyze_with_defaults(temp_dir.path().to_str().unwrap()).unwrap();

        // Should find the hardcoded secret
        assert!(result.issues_found > 0);
    }

    #[test]
    fn test_scan_aws_key() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
# AWS credentials
AWS_ACCESS_KEY_ID = "AKIAIOSFODNN7EXAMPLE"
"#;
        create_test_file(temp_dir.path(), "config.py", content);

        let result = analyze_with_defaults(temp_dir.path().to_str().unwrap()).unwrap();

        // Should detect AWS key
        assert!(result.issues_found > 0);
        assert!(result
            .structured_output
            .issues
            .iter()
            .any(|i| i.code == Some("SEC001".to_string())));
    }

    #[test]
    fn test_scan_sql_injection() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
def get_user(user_id):
    query = f"SELECT * FROM users WHERE id = {user_id}"
    cursor.execute(query)
"#;
        create_test_file(temp_dir.path(), "db.py", content);

        let result = analyze_with_defaults(temp_dir.path().to_str().unwrap()).unwrap();

        // Should detect SQL injection pattern
        assert!(result.issues_found > 0);
    }

    #[test]
    fn test_scan_debug_code() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
function processUser(user) {
    debugger;
    console.log("Processing password:", user.password);
}
"#;
        create_test_file(temp_dir.path(), "user.ts", content);

        let result = analyze_with_defaults(temp_dir.path().to_str().unwrap()).unwrap();

        // Should detect debugger statement
        assert!(result.issues_found > 0);
    }

    #[test]
    fn test_scan_unsafe_rust() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
fn dangerous_operation(ptr: *const u8) {
    unsafe {
        let val = *ptr;
    }
}
"#;
        create_test_file(temp_dir.path(), "lib.rs", content);

        let result = analyze_with_defaults(temp_dir.path().to_str().unwrap()).unwrap();

        // Should detect unsafe block
        assert!(result.issues_found > 0);
        assert!(result
            .structured_output
            .issues
            .iter()
            .any(|i| i.code == Some("SEC080".to_string())));
    }

    #[test]
    fn test_scan_clean_code() {
        let temp_dir = TempDir::new().unwrap();
        let content = r#"
def safe_function():
    # This is safe code
    result = calculate_value()
    return result
"#;
        create_test_file(temp_dir.path(), "safe.py", content);

        let result = analyze_with_defaults(temp_dir.path().to_str().unwrap()).unwrap();

        // Should find no issues
        assert_eq!(result.issues_found, 0);
    }

    #[test]
    fn test_scan_empty_directory() {
        let temp_dir = TempDir::new().unwrap();

        let result = analyze_with_defaults(temp_dir.path().to_str().unwrap()).unwrap();

        assert_eq!(result.issues_found, 0);
        assert_eq!(result.files_checked, 0);
    }

    #[test]
    fn test_scan_nonexistent_directory() {
        let result = analyze_with_defaults("/nonexistent/path/that/does/not/exist");

        assert!(result.is_err());
    }

    #[test]
    fn test_summary_generation() {
        let temp_dir = TempDir::new().unwrap();

        // Create files with various issues
        create_test_file(
            temp_dir.path(),
            "secrets.py",
            r#"api_key = "secret_key_123456789012""#,
        );
        create_test_file(temp_dir.path(), "debug.ts", r#"debugger;"#);
        create_test_file(temp_dir.path(), "safe.py", r#"x = 1"#);

        let result = analyze_with_defaults(temp_dir.path().to_str().unwrap()).unwrap();

        assert!(result.files_checked >= 2);
        assert!(result.structured_output.summary.is_some());

        let summary = result.structured_output.summary.unwrap();
        assert!(summary.total_issues > 0);
    }
}
