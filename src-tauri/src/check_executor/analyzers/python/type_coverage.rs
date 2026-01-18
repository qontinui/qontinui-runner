//! Python Type Coverage Analyzer
//!
//! Analyzes Python code for type hint coverage using tree-sitter-python.
//! Reports functions and methods missing type annotations.

use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{
    CheckIssue, CheckStructuredOutput, CheckSummary, IssueSeverity,
};
use std::collections::HashMap;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

/// Default minimum type coverage percentage
const DEFAULT_MIN_COVERAGE: f64 = 85.0;

/// Statistics for a single function/method
#[derive(Debug, Clone)]
struct FunctionTypeInfo {
    name: String,
    file: String,
    line: u32,
    column: u32,
    total_params: usize,
    typed_params: usize,
    untyped_params: Vec<String>,
    has_return_type: bool,
}

/// Analyze Python type coverage with specified minimum coverage threshold
pub fn analyze(working_dir: &str, min_coverage: f64) -> Result<ParsedOutput, String> {
    let root = Path::new(working_dir);
    if !root.exists() {
        return Err(format!("Directory does not exist: {}", working_dir));
    }

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
            structured_output: CheckStructuredOutput {
                issues: vec![],
                summary: Some(CheckSummary {
                    total_files: 0,
                    files_with_issues: 0,
                    total_issues: 0,
                    issues_by_severity: HashMap::new(),
                }),
                tool_data: HashMap::from([
                    ("type_coverage".to_string(), serde_json::json!(100.0)),
                    ("min_coverage".to_string(), serde_json::json!(min_coverage)),
                    ("total_functions".to_string(), serde_json::json!(0)),
                    ("fully_typed_functions".to_string(), serde_json::json!(0)),
                ]),
            },
        });
    }

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .map_err(|e| format!("Failed to set Python language: {}", e))?;

    let mut all_functions: Vec<FunctionTypeInfo> = Vec::new();
    let mut files_with_issues = std::collections::HashSet::new();
    let mut issues: Vec<CheckIssue> = Vec::new();

    for file_path in &files {
        let source = match std::fs::read_to_string(file_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let tree = match parser.parse(&source, None) {
            Some(t) => t,
            None => continue,
        };

        let file_str = file_path.to_string_lossy().to_string();
        let functions = extract_function_type_info(&source, &tree, &file_str);
        all_functions.extend(functions);
    }

    // Calculate coverage
    let total_functions = all_functions.len();
    let mut fully_typed_count = 0;
    let mut total_type_slots = 0; // params + return type
    let mut typed_slots = 0;

    for func in &all_functions {
        // Each function has: params + return type
        let slots = func.total_params + 1; // +1 for return type
        let typed = func.typed_params + if func.has_return_type { 1 } else { 0 };

        total_type_slots += slots;
        typed_slots += typed;

        let is_fully_typed = func.untyped_params.is_empty() && func.has_return_type;
        if is_fully_typed {
            fully_typed_count += 1;
        }
    }

    let type_coverage = if total_type_slots > 0 {
        (typed_slots as f64 / total_type_slots as f64) * 100.0
    } else {
        100.0
    };

    // Generate issues for functions below threshold (if coverage is below min)
    if type_coverage < min_coverage {
        for func in &all_functions {
            let mut issue_messages = Vec::new();

            // Report missing parameter types
            if !func.untyped_params.is_empty() {
                issue_messages.push(format!(
                    "Function '{}' missing type hints for parameters: {}",
                    func.name,
                    func.untyped_params.join(", ")
                ));
            }

            // Report missing return type
            if !func.has_return_type {
                issue_messages.push(format!(
                    "Function '{}' missing return type annotation",
                    func.name
                ));
            }

            // Create issues
            for message in issue_messages {
                files_with_issues.insert(func.file.clone());
                issues.push(
                    IssueBuilder::new(&func.file, message)
                        .line(func.line)
                        .column(func.column)
                        .code("TC001")
                        .warning()
                        .fixable(true)
                        .build(),
                );
            }
        }
    }

    let issues_found = issues.len() as u32;
    let warning_count = issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Warning)
        .count() as u32;
    let error_count = issues_found - warning_count;

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
                ]),
            }),
            tool_data: HashMap::from([
                (
                    "type_coverage".to_string(),
                    serde_json::json!(type_coverage),
                ),
                ("min_coverage".to_string(), serde_json::json!(min_coverage)),
                (
                    "total_functions".to_string(),
                    serde_json::json!(total_functions),
                ),
                (
                    "fully_typed_functions".to_string(),
                    serde_json::json!(fully_typed_count),
                ),
                (
                    "total_type_slots".to_string(),
                    serde_json::json!(total_type_slots),
                ),
                ("typed_slots".to_string(), serde_json::json!(typed_slots)),
            ]),
        },
    })
}

/// Analyze with default minimum coverage of 85%
pub fn analyze_with_defaults(working_dir: &str) -> Result<ParsedOutput, String> {
    analyze(working_dir, DEFAULT_MIN_COVERAGE)
}

/// Extract function/method type information from a parsed Python file
fn extract_function_type_info(
    source: &str,
    tree: &tree_sitter::Tree,
    file_path: &str,
) -> Vec<FunctionTypeInfo> {
    let mut functions = Vec::new();

    // Query for function definitions (both regular and async)
    // Note: This captures the function name, parameters, and return type annotation
    let query_str = r#"
        (function_definition
            name: (identifier) @func_name
            parameters: (parameters) @params
            return_type: (type)? @return_type
        ) @function

        (async_function_definition
            name: (identifier) @func_name
            parameters: (parameters) @params
            return_type: (type)? @return_type
        ) @function
    "#;

    let query = match Query::new(&tree_sitter_python::LANGUAGE.into(), query_str) {
        Ok(q) => q,
        Err(_) => return functions,
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());

    let func_name_idx = query.capture_index_for_name("func_name").unwrap_or(0);
    let params_idx = query.capture_index_for_name("params").unwrap_or(1);
    let return_type_idx = query.capture_index_for_name("return_type");

    while let Some(m) = matches.next() {
        let mut func_name = String::new();
        let mut params_node = None;
        let mut has_return_type = false;
        let mut line = 1u32;
        let mut column = 1u32;

        for capture in m.captures {
            if capture.index == func_name_idx {
                func_name = source[capture.node.byte_range()].to_string();
                line = capture.node.start_position().row as u32 + 1;
                column = capture.node.start_position().column as u32 + 1;
            } else if capture.index == params_idx {
                params_node = Some(capture.node);
            } else if return_type_idx.is_some() && capture.index == return_type_idx.unwrap() {
                has_return_type = true;
            }
        }

        // Skip dunder methods and private methods starting with underscore
        // (except __init__ which should have typed parameters)
        if func_name.starts_with("__") && func_name.ends_with("__") && func_name != "__init__" {
            continue;
        }

        // Analyze parameters
        let (total_params, typed_params, untyped_params) = if let Some(params) = params_node {
            analyze_parameters(source, params)
        } else {
            (0, 0, vec![])
        };

        // Skip functions with no parameters and no return type requirement
        // (like simple pass-through functions)
        if total_params == 0 && func_name.starts_with("_") && func_name != "__init__" {
            continue;
        }

        functions.push(FunctionTypeInfo {
            name: func_name,
            file: file_path.to_string(),
            line,
            column,
            total_params,
            typed_params,
            untyped_params,
            has_return_type,
        });
    }

    functions
}

/// Analyze function parameters for type annotations
fn analyze_parameters(source: &str, params_node: tree_sitter::Node) -> (usize, usize, Vec<String>) {
    let mut total_params = 0;
    let mut typed_params = 0;
    let mut untyped_params = Vec::new();

    let mut cursor = params_node.walk();

    // Iterate through parameter children
    for child in params_node.children(&mut cursor) {
        match child.kind() {
            // Regular parameter: name or name: type
            "identifier" => {
                let param_name = source[child.byte_range()].to_string();
                // Skip 'self' and 'cls' parameters
                if param_name != "self" && param_name != "cls" {
                    total_params += 1;
                    untyped_params.push(param_name);
                }
            }
            // Typed parameter: name: type
            "typed_parameter" => {
                total_params += 1;
                typed_params += 1;
            }
            // Default parameter: name = value or name: type = value
            "default_parameter" => {
                let param_name = get_default_param_name(source, child);
                // Skip 'self' and 'cls'
                if param_name != "self" && param_name != "cls" {
                    total_params += 1;
                    // Check if it has a type annotation
                    if has_type_annotation(child) {
                        typed_params += 1;
                    } else {
                        untyped_params.push(param_name);
                    }
                }
            }
            // Typed default parameter: name: type = value
            "typed_default_parameter" => {
                total_params += 1;
                typed_params += 1;
            }
            // *args
            "list_splat_pattern" => {
                let param_name = get_splat_param_name(source, child);
                if !param_name.is_empty() {
                    total_params += 1;
                    // Check if typed
                    if has_type_annotation(child) {
                        typed_params += 1;
                    } else {
                        untyped_params.push(format!("*{}", param_name));
                    }
                }
            }
            // **kwargs
            "dictionary_splat_pattern" => {
                let param_name = get_splat_param_name(source, child);
                if !param_name.is_empty() {
                    total_params += 1;
                    // Check if typed
                    if has_type_annotation(child) {
                        typed_params += 1;
                    } else {
                        untyped_params.push(format!("**{}", param_name));
                    }
                }
            }
            _ => {}
        }
    }

    (total_params, typed_params, untyped_params)
}

/// Get the parameter name from a default_parameter node
fn get_default_param_name(source: &str, node: tree_sitter::Node) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return source[child.byte_range()].to_string();
        }
    }
    String::new()
}

/// Get the parameter name from a splat pattern node
fn get_splat_param_name(source: &str, node: tree_sitter::Node) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return source[child.byte_range()].to_string();
        }
    }
    String::new()
}

/// Check if a parameter node has a type annotation
fn has_type_annotation(node: tree_sitter::Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "type" {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn create_test_file(dir: &Path, name: &str, content: &str) {
        let file_path = dir.join(name);
        fs::write(file_path, content).unwrap();
    }

    #[test]
    fn test_fully_typed_function() {
        let dir = tempdir().unwrap();
        let code = r#"
def greet(name: str, age: int) -> str:
    return f"Hello {name}, you are {age} years old"
"#;
        create_test_file(dir.path(), "test.py", code);

        let result = analyze(dir.path().to_str().unwrap(), 80.0).unwrap();
        assert_eq!(result.issues_found, 0);

        let coverage = result
            .structured_output
            .tool_data
            .get("type_coverage")
            .unwrap()
            .as_f64()
            .unwrap();
        assert!((coverage - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_untyped_function() {
        let dir = tempdir().unwrap();
        let code = r#"
def process(x, y):
    return x + y
"#;
        create_test_file(dir.path(), "test.py", code);

        let result = analyze(dir.path().to_str().unwrap(), 80.0).unwrap();
        assert!(result.issues_found > 0);

        // Should have issues for missing param types and return type
        let has_param_issue = result
            .structured_output
            .issues
            .iter()
            .any(|i| i.message.contains("missing type hints for parameters"));
        let has_return_issue = result
            .structured_output
            .issues
            .iter()
            .any(|i| i.message.contains("missing return type"));

        assert!(has_param_issue);
        assert!(has_return_issue);
    }

    #[test]
    fn test_partial_typing() {
        let dir = tempdir().unwrap();
        let code = r#"
def calculate(x: int, y) -> int:
    return x + y
"#;
        create_test_file(dir.path(), "test.py", code);

        let result = analyze(dir.path().to_str().unwrap(), 80.0).unwrap();

        // Should report only 'y' as untyped
        let param_issue = result
            .structured_output
            .issues
            .iter()
            .find(|i| i.message.contains("missing type hints"));

        if let Some(issue) = param_issue {
            assert!(issue.message.contains("y"));
            assert!(!issue.message.contains("x"));
        }
    }

    #[test]
    fn test_self_and_cls_ignored() {
        let dir = tempdir().unwrap();
        let code = r#"
class MyClass:
    def method(self, x: int) -> int:
        return x

    @classmethod
    def class_method(cls, y: str) -> str:
        return y
"#;
        create_test_file(dir.path(), "test.py", code);

        let result = analyze(dir.path().to_str().unwrap(), 80.0).unwrap();

        // self and cls should not be counted as missing type hints
        assert_eq!(result.issues_found, 0);
    }

    #[test]
    fn test_async_function() {
        let dir = tempdir().unwrap();
        let code = r#"
async def fetch_data(url: str) -> dict:
    return {}

async def untyped_fetch(url):
    return {}
"#;
        create_test_file(dir.path(), "test.py", code);

        let result = analyze(dir.path().to_str().unwrap(), 80.0).unwrap();

        // Should have issues only for untyped_fetch
        let untyped_issues: Vec<_> = result
            .structured_output
            .issues
            .iter()
            .filter(|i| i.message.contains("untyped_fetch"))
            .collect();

        assert!(!untyped_issues.is_empty());
    }

    #[test]
    fn test_coverage_calculation() {
        let dir = tempdir().unwrap();
        let code = r#"
def typed(x: int) -> int:
    return x

def untyped(y):
    return y
"#;
        create_test_file(dir.path(), "test.py", code);

        let result = analyze(dir.path().to_str().unwrap(), 50.0).unwrap();

        // typed: 2/2 slots (param + return)
        // untyped: 0/2 slots
        // Total: 2/4 = 50%
        let coverage = result
            .structured_output
            .tool_data
            .get("type_coverage")
            .unwrap()
            .as_f64()
            .unwrap();

        assert!((coverage - 50.0).abs() < 0.01);
    }

    #[test]
    fn test_default_parameters() {
        let dir = tempdir().unwrap();
        let code = r#"
def with_defaults(x: int = 0, y = 1) -> int:
    return x + y
"#;
        create_test_file(dir.path(), "test.py", code);

        let result = analyze(dir.path().to_str().unwrap(), 80.0).unwrap();

        // x is typed, y is not
        let param_issue = result
            .structured_output
            .issues
            .iter()
            .find(|i| i.message.contains("missing type hints"));

        if let Some(issue) = param_issue {
            assert!(issue.message.contains("y"));
            assert!(!issue.message.contains("x"));
        }
    }

    #[test]
    fn test_empty_directory() {
        let dir = tempdir().unwrap();

        let result = analyze(dir.path().to_str().unwrap(), 80.0).unwrap();
        assert_eq!(result.issues_found, 0);
        assert_eq!(result.files_checked, 0);
    }

    #[test]
    fn test_nonexistent_directory() {
        let result = analyze("/nonexistent/path", 80.0);
        assert!(result.is_err());
    }
}
