//! TypeScript Type Coverage Analyzer
//!
//! Analyzes TypeScript code for type coverage by detecting:
//! - `any` type usage
//! - Missing type annotations on function parameters
//! - Missing return type annotations on functions
//!
//! Uses tree-sitter-typescript for parsing.

use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{
    CheckIssue, CheckStructuredOutput, CheckSummary, IssueSeverity,
};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, warn};

/// Type coverage analysis result for a single file
#[derive(Debug, Default)]
struct FileCoverageResult {
    /// Total type annotations found
    total_annotations: u32,
    /// Annotations that are typed (not `any`)
    typed_annotations: u32,
    /// Issues found in this file
    issues: Vec<CheckIssue>,
}

/// Analyze TypeScript type coverage in a directory
///
/// # Arguments
/// * `working_dir` - The directory to analyze
/// * `min_coverage` - Minimum acceptable type coverage percentage (0.0 - 100.0)
///
/// # Returns
/// * `Result<ParsedOutput, String>` - Analysis results or error
pub fn analyze(working_dir: &str, min_coverage: f64) -> Result<ParsedOutput, String> {
    let path = Path::new(working_dir);
    if !path.exists() {
        return Err(format!("Directory does not exist: {}", working_dir));
    }

    // Configure file walking for TypeScript files
    let config = WalkConfig {
        extensions: vec!["ts".to_string(), "tsx".to_string()],
        exclude_patterns: vec![
            "node_modules".to_string(),
            ".git".to_string(),
            "dist".to_string(),
            "build".to_string(),
            ".next".to_string(),
            "coverage".to_string(),
            "__tests__".to_string(),
            "*.test.ts".to_string(),
            "*.spec.ts".to_string(),
            "*.d.ts".to_string(),
        ],
        max_depth: None,
    };

    let files = walk_files(path, &config);
    debug!("Found {} TypeScript files to analyze", files.len());

    let mut all_issues: Vec<CheckIssue> = Vec::new();
    let mut total_annotations: u32 = 0;
    let mut typed_annotations: u32 = 0;
    let mut files_with_issues = 0u32;
    let mut files_checked = 0u32;

    // Initialize tree-sitter parser
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
    parser
        .set_language(&language.into())
        .map_err(|e| format!("Failed to set TypeScript language: {}", e))?;

    for file_path in &files {
        files_checked += 1;

        // Read file content
        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to read file {:?}: {}", file_path, e);
                continue;
            }
        };

        // Use TSX parser for .tsx files
        let is_tsx = file_path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase() == "tsx")
            .unwrap_or(false);

        if is_tsx {
            let tsx_language = tree_sitter_typescript::LANGUAGE_TSX;
            if parser.set_language(&tsx_language.into()).is_err() {
                warn!("Failed to set TSX language for {:?}", file_path);
                continue;
            }
        } else if parser.set_language(&language.into()).is_err() {
            warn!("Failed to set TypeScript language for {:?}", file_path);
            continue;
        }

        // Parse the file
        let tree = match parser.parse(&content, None) {
            Some(t) => t,
            None => {
                warn!("Failed to parse file {:?}", file_path);
                continue;
            }
        };

        // Analyze the AST
        let relative_path = file_path
            .strip_prefix(path)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        let result = analyze_file_ast(&tree, &content, &relative_path);

        total_annotations += result.total_annotations;
        typed_annotations += result.typed_annotations;

        if !result.issues.is_empty() {
            files_with_issues += 1;
            all_issues.extend(result.issues);
        }
    }

    // Calculate coverage percentage
    let coverage = if total_annotations > 0 {
        (typed_annotations as f64 / total_annotations as f64) * 100.0
    } else {
        100.0 // No annotations means 100% coverage (nothing to type)
    };

    // Add coverage threshold issue if below minimum
    if coverage < min_coverage {
        all_issues.push(
            IssueBuilder::new(
                working_dir.to_string(),
                format!(
                    "Type coverage {:.1}% is below minimum threshold of {:.1}%",
                    coverage, min_coverage
                ),
            )
            .code("TS-COV-001")
            .error()
            .build(),
        );
    }

    let issues_found = all_issues.len() as u32;
    let error_count = all_issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Error)
        .count() as u32;
    let warning_count = all_issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Warning)
        .count() as u32;

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
                    ("error".to_string(), error_count),
                    ("warning".to_string(), warning_count),
                ]),
            }),
            tool_data: HashMap::from([
                (
                    "type_coverage".to_string(),
                    serde_json::json!(format!("{:.1}%", coverage)),
                ),
                (
                    "total_annotations".to_string(),
                    serde_json::json!(total_annotations),
                ),
                (
                    "typed_annotations".to_string(),
                    serde_json::json!(typed_annotations),
                ),
                ("min_coverage".to_string(), serde_json::json!(min_coverage)),
            ]),
        },
    })
}

/// Analyze with default settings (90% minimum coverage)
pub fn analyze_with_defaults(working_dir: &str) -> Result<ParsedOutput, String> {
    analyze(working_dir, 90.0)
}

/// Analyze a single file's AST for type coverage
fn analyze_file_ast(tree: &tree_sitter::Tree, source: &str, file_path: &str) -> FileCoverageResult {
    let mut result = FileCoverageResult::default();
    let root_node = tree.root_node();

    analyze_node(&root_node, source, file_path, &mut result);

    result
}

/// Recursively analyze AST nodes for type coverage
fn analyze_node(
    node: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    result: &mut FileCoverageResult,
) {
    match node.kind() {
        // Check function parameters
        "formal_parameters" | "parameter_list" => {
            analyze_parameters(node, source, file_path, result);
        }

        // Check function declarations for return types
        "function_declaration" | "method_definition" | "arrow_function" => {
            analyze_function(node, source, file_path, result);
        }

        // Check variable declarations
        "variable_declarator" => {
            analyze_variable_declarator(node, source, file_path, result);
        }

        // Check for `any` type annotations
        "type_annotation" => {
            analyze_type_annotation(node, source, file_path, result);
        }

        // Check for `any` in type aliases
        "type_alias_declaration" => {
            analyze_type_alias(node, source, file_path, result);
        }

        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        analyze_node(&child, source, file_path, result);
    }
}

/// Analyze function parameters for type annotations
fn analyze_parameters(
    node: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    result: &mut FileCoverageResult,
) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "required_parameter" | "optional_parameter" | "rest_parameter" => {
                result.total_annotations += 1;

                // Check if parameter has a type annotation
                let has_type = child.child_by_field_name("type").is_some()
                    || has_child_of_kind(&child, "type_annotation");

                if has_type {
                    // Check if the type is `any`
                    if let Some(type_node) = child.child_by_field_name("type") {
                        if is_any_type(&type_node, source) {
                            let line = child.start_position().row + 1;
                            let column = child.start_position().column + 1;
                            let param_text = get_node_text(&child, source);

                            result.issues.push(
                                IssueBuilder::new(
                                    file_path.to_string(),
                                    format!("Parameter uses 'any' type: {}", param_text),
                                )
                                .line(line as u32)
                                .column(column as u32)
                                .code("TS-ANY-001")
                                .warning()
                                .fixable(true)
                                .build(),
                            );
                        } else {
                            result.typed_annotations += 1;
                        }
                    } else {
                        result.typed_annotations += 1;
                    }
                } else {
                    // Missing type annotation
                    let line = child.start_position().row + 1;
                    let column = child.start_position().column + 1;
                    let param_text = get_node_text(&child, source);

                    result.issues.push(
                        IssueBuilder::new(
                            file_path.to_string(),
                            format!("Parameter missing type annotation: {}", param_text),
                        )
                        .line(line as u32)
                        .column(column as u32)
                        .code("TS-MISS-001")
                        .warning()
                        .fixable(true)
                        .build(),
                    );
                }
            }
            _ => {}
        }
    }
}

/// Analyze function declaration for return type
fn analyze_function(
    node: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    result: &mut FileCoverageResult,
) {
    // Skip if this is in a type context (type alias, interface, etc.)
    if let Some(parent) = node.parent() {
        if parent.kind() == "type_alias_declaration" || parent.kind() == "interface_declaration" {
            return;
        }
    }

    result.total_annotations += 1;

    // Check for return type annotation
    let has_return_type = node.child_by_field_name("return_type").is_some()
        || has_child_of_kind(node, "type_annotation");

    if has_return_type {
        // Check if return type is `any`
        if let Some(return_type) = node.child_by_field_name("return_type") {
            if is_any_type(&return_type, source) {
                let line = node.start_position().row + 1;
                let column = node.start_position().column + 1;
                let func_name = get_function_name(node, source);

                result.issues.push(
                    IssueBuilder::new(
                        file_path.to_string(),
                        format!("Function '{}' has 'any' return type", func_name),
                    )
                    .line(line as u32)
                    .column(column as u32)
                    .code("TS-ANY-002")
                    .warning()
                    .fixable(true)
                    .build(),
                );
            } else {
                result.typed_annotations += 1;
            }
        } else {
            result.typed_annotations += 1;
        }
    } else {
        // Arrow functions without explicit return types are often intentional
        // Only report for regular function declarations
        if node.kind() == "function_declaration" || node.kind() == "method_definition" {
            let line = node.start_position().row + 1;
            let column = node.start_position().column + 1;
            let func_name = get_function_name(node, source);

            result.issues.push(
                IssueBuilder::new(
                    file_path.to_string(),
                    format!("Function '{}' missing return type annotation", func_name),
                )
                .line(line as u32)
                .column(column as u32)
                .code("TS-MISS-002")
                .warning()
                .fixable(true)
                .build(),
            );
        }
    }
}

/// Analyze variable declarator for type annotation
fn analyze_variable_declarator(
    node: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    result: &mut FileCoverageResult,
) {
    // Only count if the variable has an initializer (otherwise it must have a type)
    if node.child_by_field_name("value").is_none() {
        return;
    }

    // Check if it has a type annotation
    if let Some(type_annotation) = node.child_by_field_name("type") {
        result.total_annotations += 1;

        if is_any_type(&type_annotation, source) {
            let line = node.start_position().row + 1;
            let column = node.start_position().column + 1;
            let var_name = node
                .child_by_field_name("name")
                .map(|n| get_node_text(&n, source))
                .unwrap_or_else(|| "unknown".to_string());

            result.issues.push(
                IssueBuilder::new(
                    file_path.to_string(),
                    format!("Variable '{}' has explicit 'any' type", var_name),
                )
                .line(line as u32)
                .column(column as u32)
                .code("TS-ANY-003")
                .warning()
                .fixable(true)
                .build(),
            );
        } else {
            result.typed_annotations += 1;
        }
    }
    // Variables with initializers often have inferred types, which is fine
}

/// Analyze type annotation node for `any` usage
fn analyze_type_annotation(
    node: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    result: &mut FileCoverageResult,
) {
    if is_any_type(node, source) {
        let line = node.start_position().row + 1;
        let column = node.start_position().column + 1;

        result.issues.push(
            IssueBuilder::new(
                file_path.to_string(),
                "Explicit 'any' type annotation".to_string(),
            )
            .line(line as u32)
            .column(column as u32)
            .code("TS-ANY-004")
            .warning()
            .fixable(true)
            .build(),
        );
    }
}

/// Analyze type alias for `any` usage
fn analyze_type_alias(
    node: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    result: &mut FileCoverageResult,
) {
    if let Some(value_node) = node.child_by_field_name("value") {
        if is_any_type(&value_node, source) {
            let line = node.start_position().row + 1;
            let column = node.start_position().column + 1;
            let alias_name = node
                .child_by_field_name("name")
                .map(|n| get_node_text(&n, source))
                .unwrap_or_else(|| "unknown".to_string());

            result.issues.push(
                IssueBuilder::new(
                    file_path.to_string(),
                    format!("Type alias '{}' is defined as 'any'", alias_name),
                )
                .line(line as u32)
                .column(column as u32)
                .code("TS-ANY-005")
                .error()
                .fixable(true)
                .build(),
            );
        }
    }
}

/// Check if a type node is or contains `any`
fn is_any_type(node: &tree_sitter::Node, source: &str) -> bool {
    let text = get_node_text(node, source);

    // Direct `any` type
    if text.trim() == "any" || text.trim() == ": any" {
        return true;
    }

    // Check children for predefined_type "any"
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "predefined_type" {
            let child_text = get_node_text(&child, source);
            if child_text.trim() == "any" {
                return true;
            }
        }
        // Recurse
        if is_any_type(&child, source) {
            return true;
        }
    }

    false
}

/// Check if node has a child of a specific kind
fn has_child_of_kind(node: &tree_sitter::Node, kind: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            return true;
        }
    }
    false
}

/// Get the text content of a node
fn get_node_text(node: &tree_sitter::Node, source: &str) -> String {
    let start = node.start_byte();
    let end = node.end_byte();
    if start < source.len() && end <= source.len() {
        source[start..end].to_string()
    } else {
        String::new()
    }
}

/// Get function name from a function node
fn get_function_name(node: &tree_sitter::Node, source: &str) -> String {
    if let Some(name_node) = node.child_by_field_name("name") {
        return get_node_text(&name_node, source);
    }

    // For method definitions, the name might be in a property_identifier
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "property_identifier" {
            return get_node_text(&child, source);
        }
    }

    "anonymous".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_file(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_detect_any_parameter() {
        let dir = TempDir::new().unwrap();
        create_test_file(
            &dir,
            "test.ts",
            r#"
function test(x: any) {
    return x;
}
"#,
        );

        let result = analyze(dir.path().to_str().unwrap(), 50.0).unwrap();
        assert!(result.issues_found > 0);
        assert!(result
            .structured_output
            .issues
            .iter()
            .any(|i| i.code.as_deref() == Some("TS-ANY-001")));
    }

    #[test]
    fn test_detect_missing_return_type() {
        let dir = TempDir::new().unwrap();
        create_test_file(
            &dir,
            "test.ts",
            r#"
function test(x: string) {
    return x.length;
}
"#,
        );

        let result = analyze(dir.path().to_str().unwrap(), 50.0).unwrap();
        assert!(result
            .structured_output
            .issues
            .iter()
            .any(|i| i.code.as_deref() == Some("TS-MISS-002")));
    }

    #[test]
    fn test_properly_typed_function() {
        let dir = TempDir::new().unwrap();
        create_test_file(
            &dir,
            "test.ts",
            r#"
function test(x: string): number {
    return x.length;
}
"#,
        );

        let result = analyze(dir.path().to_str().unwrap(), 50.0).unwrap();
        // Should not have any TS-ANY or TS-MISS issues for the function itself
        let any_or_miss_issues: Vec<_> = result
            .structured_output
            .issues
            .iter()
            .filter(|i| {
                i.code
                    .as_ref()
                    .map(|c| c.starts_with("TS-ANY") || c.starts_with("TS-MISS"))
                    .unwrap_or(false)
            })
            .collect();
        assert!(any_or_miss_issues.is_empty());
    }

    #[test]
    fn test_coverage_threshold() {
        let dir = TempDir::new().unwrap();
        // Create a file with all `any` types
        create_test_file(
            &dir,
            "test.ts",
            r#"
function test(x: any): any {
    return x;
}
"#,
        );

        // With 90% threshold, this should fail
        let result = analyze(dir.path().to_str().unwrap(), 90.0).unwrap();
        assert!(result
            .structured_output
            .issues
            .iter()
            .any(|i| i.code.as_deref() == Some("TS-COV-001")));
    }
}
