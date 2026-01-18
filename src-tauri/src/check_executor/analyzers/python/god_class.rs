//! God Class Detector for Python
//!
//! Detects Python classes that are too large (too many lines or methods),
//! indicating a potential design issue where a class has too many responsibilities.

use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{CheckStructuredOutput, CheckSummary};
use std::collections::HashMap;
use std::path::Path;
use tracing::debug;

/// Default thresholds for god class detection
const DEFAULT_MAX_LINES: u32 = 500;
const DEFAULT_MAX_METHODS: u32 = 30;

/// Information about a detected class
#[derive(Debug)]
struct ClassInfo {
    name: String,
    line: u32,
    end_line: u32,
    lines: u32,
    method_count: u32,
    attribute_count: u32,
}

/// Analyze Python files for god classes with default thresholds
pub fn analyze_with_defaults(working_dir: &str) -> Result<ParsedOutput, String> {
    analyze(working_dir, DEFAULT_MAX_LINES, DEFAULT_MAX_METHODS)
}

/// Analyze Python files for god classes with custom thresholds
pub fn analyze(
    working_dir: &str,
    max_lines: u32,
    max_methods: u32,
) -> Result<ParsedOutput, String> {
    let root = Path::new(working_dir);
    if !root.exists() {
        return Err(format!("Directory does not exist: {}", working_dir));
    }

    // Configure file walker for Python files
    let config = WalkConfig {
        extensions: vec!["py".to_string()],
        ..Default::default()
    };

    let files = walk_files(root, &config);
    debug!("Found {} Python files to analyze", files.len());

    // Initialize parser
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser
        .set_language(&language.into())
        .map_err(|e| format!("Failed to set Python language: {}", e))?;

    let mut issues = Vec::new();
    let mut files_with_issues = std::collections::HashSet::new();
    let mut total_classes = 0u32;

    for file_path in &files {
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

        let classes = extract_classes(&tree, &content);
        total_classes += classes.len() as u32;

        for class_info in classes {
            if class_info.lines > max_lines || class_info.method_count > max_methods {
                let file_str = file_path.to_string_lossy().to_string();
                files_with_issues.insert(file_str.clone());

                let message = format!(
                    "God class detected: {} has {} lines and {} methods (thresholds: {} lines, {} methods)",
                    class_info.name,
                    class_info.lines,
                    class_info.method_count,
                    max_lines,
                    max_methods
                );

                let issue = IssueBuilder::new(&file_str, message)
                    .line(class_info.line)
                    .end_line(class_info.end_line)
                    .code("GOD_CLASS")
                    .warning()
                    .build();

                issues.push(issue);
            }
        }
    }

    let issues_found = issues.len() as u32;
    let files_checked = files.len() as u32;

    Ok(ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked,
        structured_output: CheckStructuredOutput {
            issues,
            summary: Some(CheckSummary {
                total_files: files_checked,
                files_with_issues: files_with_issues.len() as u32,
                total_issues: issues_found,
                issues_by_severity: HashMap::from([
                    ("warning".to_string(), issues_found),
                    ("classes_analyzed".to_string(), total_classes),
                ]),
            }),
            tool_data: HashMap::from([
                (
                    "max_lines_threshold".to_string(),
                    serde_json::json!(max_lines),
                ),
                (
                    "max_methods_threshold".to_string(),
                    serde_json::json!(max_methods),
                ),
                (
                    "total_classes_analyzed".to_string(),
                    serde_json::json!(total_classes),
                ),
            ]),
        },
    })
}

/// Extract class information from a parsed Python AST
fn extract_classes(tree: &tree_sitter::Tree, source: &str) -> Vec<ClassInfo> {
    let mut classes = Vec::new();
    let root = tree.root_node();

    // Traverse the tree to find class definitions
    let mut cursor = root.walk();
    traverse_for_classes(&mut cursor, source, &mut classes, false);

    classes
}

/// Recursively traverse the AST looking for class definitions
fn traverse_for_classes(
    cursor: &mut tree_sitter::TreeCursor,
    source: &str,
    classes: &mut Vec<ClassInfo>,
    inside_class: bool,
) {
    loop {
        let node = cursor.node();

        if node.kind() == "class_definition" && !inside_class {
            if let Some(class_info) = analyze_class(&node, source) {
                classes.push(class_info);
            }
        }

        // Recurse into children (but not into class bodies, as we handle them separately)
        if node.kind() != "class_definition" && cursor.goto_first_child() {
            traverse_for_classes(cursor, source, classes, inside_class);
            cursor.goto_parent();
        }

        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

/// Analyze a class definition node
fn analyze_class(class_node: &tree_sitter::Node, source: &str) -> Option<ClassInfo> {
    // Find the class name
    let name = class_node
        .child_by_field_name("name")
        .map(|n| {
            n.utf8_text(source.as_bytes())
                .unwrap_or("unknown")
                .to_string()
        })
        .unwrap_or_else(|| "unknown".to_string());

    let start_line = class_node.start_position().row as u32 + 1; // 1-based
    let end_line = class_node.end_position().row as u32 + 1;
    let lines = end_line - start_line + 1;

    // Find the class body to count methods and attributes
    let body = class_node.child_by_field_name("body")?;

    let mut method_count = 0u32;
    let mut attribute_count = 0u32;

    // Iterate through direct children of the class body
    let mut cursor = body.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();

            match child.kind() {
                "function_definition" => {
                    method_count += 1;
                }
                "expression_statement" => {
                    // Check if it's an assignment (class attribute)
                    if let Some(expr) = child.child(0) {
                        if expr.kind() == "assignment" || expr.kind() == "annotated_assignment" {
                            attribute_count += 1;
                        }
                    }
                }
                _ => {}
            }

            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    Some(ClassInfo {
        name,
        line: start_line,
        end_line,
        lines,
        method_count,
        attribute_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check_executor::types::IssueSeverity;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, name: &str, content: &str) {
        let path = dir.join(name);
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn test_analyze_small_class() {
        let temp_dir = TempDir::new().unwrap();
        let code = r#"
class SmallClass:
    def __init__(self):
        self.value = 0

    def get_value(self):
        return self.value

    def set_value(self, v):
        self.value = v
"#;
        create_test_file(temp_dir.path(), "small.py", code);

        let result = analyze(temp_dir.path().to_str().unwrap(), 500, 30).unwrap();

        // Small class should not be flagged
        assert_eq!(result.issues_found, 0);
        assert_eq!(result.files_checked, 1);
    }

    #[test]
    fn test_analyze_god_class_by_methods() {
        let temp_dir = TempDir::new().unwrap();

        // Generate a class with many methods
        let mut code = String::from("class ManyMethods:\n");
        for i in 0..35 {
            code.push_str(&format!("    def method_{}(self):\n        pass\n", i));
        }

        create_test_file(temp_dir.path(), "many_methods.py", &code);

        let result = analyze(temp_dir.path().to_str().unwrap(), 500, 30).unwrap();

        // Should be flagged for too many methods
        assert_eq!(result.issues_found, 1);
        assert!(result.structured_output.issues[0]
            .message
            .contains("ManyMethods"));
        assert!(result.structured_output.issues[0]
            .message
            .contains("35 methods"));
    }

    #[test]
    fn test_analyze_god_class_by_lines() {
        let temp_dir = TempDir::new().unwrap();

        // Generate a large class with many lines but few methods
        let mut code = String::from("class LargeClass:\n");
        code.push_str("    def large_method(self):\n");
        // Add enough lines to exceed 100 lines (using lower threshold for testing)
        for i in 0..120 {
            code.push_str(&format!("        x{} = {}\n", i, i));
        }

        create_test_file(temp_dir.path(), "large.py", &code);

        // Use lower threshold for testing
        let result = analyze(temp_dir.path().to_str().unwrap(), 100, 30).unwrap();

        // Should be flagged for too many lines
        assert_eq!(result.issues_found, 1);
        assert!(result.structured_output.issues[0]
            .message
            .contains("LargeClass"));
    }

    #[test]
    fn test_analyze_with_defaults() {
        let temp_dir = TempDir::new().unwrap();
        let code = r#"
class NormalClass:
    def method(self):
        pass
"#;
        create_test_file(temp_dir.path(), "normal.py", code);

        let result = analyze_with_defaults(temp_dir.path().to_str().unwrap()).unwrap();

        assert_eq!(result.issues_found, 0);
    }

    #[test]
    fn test_analyze_multiple_classes() {
        let temp_dir = TempDir::new().unwrap();

        let code = r#"
class SmallClass:
    def method(self):
        pass

class AnotherSmall:
    def foo(self):
        pass

    def bar(self):
        pass
"#;
        create_test_file(temp_dir.path(), "multi.py", code);

        let result = analyze(temp_dir.path().to_str().unwrap(), 500, 30).unwrap();

        assert_eq!(result.issues_found, 0);
        // Check that we analyzed multiple classes
        let tool_data = &result.structured_output.tool_data;
        let total_classes = tool_data
            .get("total_classes_analyzed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert_eq!(total_classes, 2);
    }

    #[test]
    fn test_analyze_nonexistent_directory() {
        let result = analyze("/nonexistent/path/that/does/not/exist", 500, 30);
        assert!(result.is_err());
    }

    #[test]
    fn test_issue_message_format() {
        let temp_dir = TempDir::new().unwrap();

        // Generate a class that exceeds thresholds
        let mut code = String::from("class GodClass:\n");
        for i in 0..5 {
            code.push_str(&format!("    def method_{}(self):\n        pass\n", i));
        }

        create_test_file(temp_dir.path(), "god.py", &code);

        // Use very low thresholds to trigger
        let result = analyze(temp_dir.path().to_str().unwrap(), 5, 3).unwrap();

        assert_eq!(result.issues_found, 1);
        let issue = &result.structured_output.issues[0];

        // Check message format
        assert!(issue.message.contains("God class detected:"));
        assert!(issue.message.contains("GodClass"));
        assert!(issue.message.contains("5 methods"));
        assert!(issue.message.contains("(thresholds: 5 lines, 3 methods)"));

        // Check issue metadata
        assert_eq!(issue.code, Some("GOD_CLASS".to_string()));
        assert_eq!(issue.severity, IssueSeverity::Warning);
        assert!(issue.line.is_some());
    }

    #[test]
    fn test_ignores_excluded_directories() {
        let temp_dir = TempDir::new().unwrap();

        // Create a file in an excluded directory
        let venv_dir = temp_dir.path().join(".venv");
        std::fs::create_dir_all(&venv_dir).unwrap();

        let mut code = String::from("class HugeClass:\n");
        for i in 0..50 {
            code.push_str(&format!("    def method_{}(self):\n        pass\n", i));
        }

        // This should be ignored due to .venv exclusion
        create_test_file(&venv_dir, "huge.py", &code);

        let result = analyze(temp_dir.path().to_str().unwrap(), 500, 30).unwrap();

        // Should find no files (excluded directory)
        assert_eq!(result.issues_found, 0);
        assert_eq!(result.files_checked, 0);
    }
}
