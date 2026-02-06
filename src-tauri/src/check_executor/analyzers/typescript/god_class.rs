//! God Class Detector for TypeScript
//!
//! Detects TypeScript classes that are too large (too many lines or methods),
//! indicating a potential design issue where a class has too many responsibilities.

use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{CheckStructuredOutput, CheckSummary};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, warn};

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
    property_count: u32,
}

/// Analyze TypeScript files for god classes with default thresholds
pub fn analyze_with_defaults(working_dir: &str) -> Result<ParsedOutput, String> {
    analyze(working_dir, DEFAULT_MAX_LINES, DEFAULT_MAX_METHODS)
}

/// Analyze TypeScript files for god classes with custom thresholds
pub fn analyze(
    working_dir: &str,
    max_lines: u32,
    max_methods: u32,
) -> Result<ParsedOutput, String> {
    let root = Path::new(working_dir);
    if !root.exists() {
        return Err(format!("Directory does not exist: {}", working_dir));
    }

    // Configure file walker for TypeScript files
    let config = WalkConfig {
        extensions: vec!["ts".to_string(), "tsx".to_string()],
        exclude_patterns: vec![
            "node_modules".to_string(),
            ".git".to_string(),
            "dist".to_string(),
            "build".to_string(),
            ".next".to_string(),
            "coverage".to_string(),
            "*.test.ts".to_string(),
            "*.spec.ts".to_string(),
            "*.d.ts".to_string(),
        ],
        max_depth: None,
    };

    let files = walk_files(root, &config);
    debug!("Found {} TypeScript files to analyze", files.len());

    // Initialize parsers
    let mut ts_parser = tree_sitter::Parser::new();
    let ts_language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
    ts_parser
        .set_language(&ts_language.into())
        .map_err(|e| format!("Failed to set TypeScript language: {}", e))?;

    let mut tsx_parser = tree_sitter::Parser::new();
    let tsx_language = tree_sitter_typescript::LANGUAGE_TSX;
    tsx_parser
        .set_language(&tsx_language.into())
        .map_err(|e| format!("Failed to set TSX language: {}", e))?;

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

        // Use TSX parser for .tsx files
        let is_tsx = file_path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase() == "tsx")
            .unwrap_or(false);

        let parser = if is_tsx {
            &mut tsx_parser
        } else {
            &mut ts_parser
        };

        let tree = match parser.parse(&content, None) {
            Some(t) => t,
            None => {
                warn!("Failed to parse file {:?}", file_path);
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
                    .code("GOD_CLASS_TS")
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

/// Extract class information from a parsed TypeScript AST
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

        // TypeScript class_declaration
        if node.kind() == "class_declaration" && !inside_class {
            if let Some(class_info) = analyze_class(&node, source) {
                classes.push(class_info);
            }
        }

        // Recurse into children (but not into class bodies, as we handle them separately)
        if node.kind() != "class_declaration" && cursor.goto_first_child() {
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

    // Find the class body to count methods and properties
    let body = class_node.child_by_field_name("body")?;

    let mut method_count = 0u32;
    let mut property_count = 0u32;

    // Iterate through direct children of the class body
    let mut cursor = body.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();

            match child.kind() {
                // TypeScript method definitions (includes constructor, regular methods, getters, setters)
                "method_definition" => {
                    method_count += 1;
                }
                // Public field definitions can be either properties or arrow function methods
                "public_field_definition" => {
                    if has_arrow_function_value(&child) {
                        method_count += 1;
                    } else {
                        property_count += 1;
                    }
                }
                // Property signatures and field definitions
                "property_signature" | "field_definition" => {
                    property_count += 1;
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
        property_count,
    })
}

/// Check if a field definition has an arrow function value
fn has_arrow_function_value(node: &tree_sitter::Node) -> bool {
    if let Some(value) = node.child_by_field_name("value") {
        return value.kind() == "arrow_function";
    }
    false
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
    fn test_analyze_small_class() {
        let temp_dir = TempDir::new().unwrap();
        let code = r#"
class SmallClass {
    private value: number = 0;

    getValue(): number {
        return this.value;
    }

    setValue(v: number): void {
        this.value = v;
    }
}
"#;
        create_test_file(temp_dir.path(), "small.ts", code);

        let result = analyze(temp_dir.path().to_str().unwrap(), 500, 30).unwrap();

        // Small class should not be flagged
        assert_eq!(result.issues_found, 0);
        assert_eq!(result.files_checked, 1);
    }

    #[test]
    fn test_analyze_god_class_by_methods() {
        let temp_dir = TempDir::new().unwrap();

        // Generate a class with many methods
        let mut code = String::from("class ManyMethods {\n");
        for i in 0..35 {
            code.push_str(&format!("    method{}(): void {{}}\n", i));
        }
        code.push_str("}\n");

        create_test_file(temp_dir.path(), "many_methods.ts", &code);

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
        let mut code = String::from("class LargeClass {\n");
        code.push_str("    largeMethod(): void {\n");
        // Add enough lines to exceed 100 lines (using lower threshold for testing)
        for i in 0..120 {
            code.push_str(&format!("        const x{} = {};\n", i, i));
        }
        code.push_str("    }\n");
        code.push_str("}\n");

        create_test_file(temp_dir.path(), "large.ts", &code);

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
class NormalClass {
    method(): void {}
}
"#;
        create_test_file(temp_dir.path(), "normal.ts", code);

        let result = analyze_with_defaults(temp_dir.path().to_str().unwrap()).unwrap();

        assert_eq!(result.issues_found, 0);
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
        let mut code = String::from("class GodClass {\n");
        for i in 0..5 {
            code.push_str(&format!("    method{}(): void {{}}\n", i));
        }
        code.push_str("}\n");

        create_test_file(temp_dir.path(), "god.ts", &code);

        // Use very low thresholds to trigger
        let result = analyze(temp_dir.path().to_str().unwrap(), 5, 3).unwrap();

        assert_eq!(result.issues_found, 1);
        let issue = &result.structured_output.issues[0];

        // Check message format
        assert!(issue.message.contains("God class detected:"));
        assert!(issue.message.contains("GodClass"));
        assert!(issue.message.contains("(thresholds: 5 lines, 3 methods)"));

        // Check issue metadata
        assert_eq!(issue.code, Some("GOD_CLASS_TS".to_string()));
        assert!(issue.line.is_some());
    }

    #[test]
    fn test_ignores_node_modules() {
        let temp_dir = TempDir::new().unwrap();

        // Create a file in node_modules (should be ignored)
        let node_modules_dir = temp_dir.path().join("node_modules");
        std::fs::create_dir_all(&node_modules_dir).unwrap();

        let mut code = String::from("class HugeClass {\n");
        for i in 0..50 {
            code.push_str(&format!("    method{}(): void {{}}\n", i));
        }
        code.push_str("}\n");

        // This should be ignored due to node_modules exclusion
        create_test_file(&node_modules_dir, "huge.ts", &code);

        let result = analyze(temp_dir.path().to_str().unwrap(), 500, 30).unwrap();

        // Should find no files (excluded directory)
        assert_eq!(result.issues_found, 0);
        assert_eq!(result.files_checked, 0);
    }
}
