//! SRP (Single Responsibility Principle) Analyzer for Python
//!
//! Analyzes class cohesion using LCOM (Lack of Cohesion of Methods) metric.
//! Classes with low cohesion (high LCOM) likely violate SRP.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{CheckStructuredOutput, CheckSummary, IssueSeverity};

/// Default LCOM threshold (above this indicates SRP violation)
const DEFAULT_MAX_LCOM: f64 = 0.8;

/// Minimum number of methods required for LCOM calculation to be meaningful
const MIN_METHODS_FOR_LCOM: usize = 2;

/// Information about a class
#[derive(Debug)]
struct ClassInfo {
    name: String,
    file: String,
    line: u32,
    column: u32,
    /// Instance attributes accessed by each method
    method_attributes: HashMap<String, HashSet<String>>,
    /// All instance attributes in the class
    all_attributes: HashSet<String>,
}

/// LCOM calculation result
#[derive(Debug)]
struct LcomResult {
    /// LCOM value (0.0 = highly cohesive, 1.0 = no cohesion)
    value: f64,
    /// Number of methods analyzed
    method_count: usize,
    /// Number of instance attributes
    attribute_count: usize,
    /// Methods that share no attributes with others
    isolated_methods: Vec<String>,
    /// Suggested groupings based on attribute usage
    suggested_groups: Vec<Vec<String>>,
}

/// Analyze a Python codebase for SRP violations
pub fn analyze(working_dir: &str, max_lcom: f64) -> Result<ParsedOutput, String> {
    let root = Path::new(working_dir);
    if !root.exists() {
        return Err(format!("Working directory does not exist: {}", working_dir));
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
            structured_output: CheckStructuredOutput::default(),
        });
    }

    // Initialize tree-sitter parser
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser
        .set_language(&language.into())
        .map_err(|e| format!("Failed to set Python language: {}", e))?;

    let mut all_classes: Vec<ClassInfo> = Vec::new();
    let mut files_analyzed = 0;

    // Collect class information from all files
    for file_path in &files {
        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let tree = match parser.parse(&content, None) {
            Some(t) => t,
            None => continue,
        };

        let relative_path = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        collect_classes(&tree, &content, &relative_path, &mut all_classes);
        files_analyzed += 1;
    }

    // Calculate LCOM for each class and generate issues
    let mut issues = Vec::new();
    let mut files_with_issues = HashSet::new();
    let mut tool_data = HashMap::new();
    let mut class_metrics = Vec::new();

    for class_info in &all_classes {
        if class_info.method_attributes.len() < MIN_METHODS_FOR_LCOM {
            continue;
        }

        let lcom_result = calculate_lcom(class_info);

        // Store metrics for tool_data
        class_metrics.push(serde_json::json!({
            "class": class_info.name,
            "file": class_info.file,
            "line": class_info.line,
            "lcom": lcom_result.value,
            "method_count": lcom_result.method_count,
            "attribute_count": lcom_result.attribute_count,
            "isolated_methods": lcom_result.isolated_methods,
        }));

        if lcom_result.value > max_lcom {
            files_with_issues.insert(class_info.file.clone());

            let severity = if lcom_result.value > 0.95 {
                IssueSeverity::Error
            } else if lcom_result.value > 0.9 {
                IssueSeverity::Warning
            } else {
                IssueSeverity::Warning
            };

            let message = format!(
                "Class '{}' has low cohesion (LCOM={:.2}). {} methods operate on {} attributes. {}",
                class_info.name,
                lcom_result.value,
                lcom_result.method_count,
                lcom_result.attribute_count,
                suggest_refactoring(&lcom_result)
            );

            issues.push(
                IssueBuilder::new(&class_info.file, message)
                    .line(class_info.line)
                    .column(class_info.column)
                    .code("SRP001")
                    .severity(severity)
                    .fixable(false)
                    .build(),
            );

            // Add info issues for isolated methods
            for method in &lcom_result.isolated_methods {
                issues.push(
                    IssueBuilder::new(
                        &class_info.file,
                        format!(
                            "Method '{}' in class '{}' shares no attributes with other methods",
                            method, class_info.name
                        ),
                    )
                    .line(class_info.line)
                    .code("SRP002")
                    .info()
                    .build(),
                );
            }
        }
    }

    // Sort issues by severity, then file, then line
    issues.sort_by(|a, b| {
        let severity_order = |s: &IssueSeverity| match s {
            IssueSeverity::Error => 0,
            IssueSeverity::Warning => 1,
            IssueSeverity::Info => 2,
            IssueSeverity::Hint => 3,
        };
        severity_order(&a.severity)
            .cmp(&severity_order(&b.severity))
            .then(a.file.cmp(&b.file))
            .then(a.line.unwrap_or(0).cmp(&b.line.unwrap_or(0)))
    });

    tool_data.insert(
        "class_metrics".to_string(),
        serde_json::Value::Array(class_metrics),
    );
    tool_data.insert(
        "max_lcom_threshold".to_string(),
        serde_json::json!(max_lcom),
    );
    tool_data.insert(
        "total_classes_analyzed".to_string(),
        serde_json::json!(all_classes.len()),
    );

    let issues_found = issues.len() as u32;
    let error_count = issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Error)
        .count() as u32;
    let warning_count = issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Warning)
        .count() as u32;

    Ok(ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked: files_analyzed,
        structured_output: CheckStructuredOutput {
            issues,
            summary: Some(CheckSummary {
                total_files: files_analyzed,
                files_with_issues: files_with_issues.len() as u32,
                total_issues: issues_found,
                issues_by_severity: HashMap::from([
                    ("error".to_string(), error_count),
                    ("warning".to_string(), warning_count),
                ]),
            }),
            tool_data,
        },
    })
}

/// Analyze with default LCOM threshold
pub fn analyze_with_defaults(working_dir: &str) -> Result<ParsedOutput, String> {
    analyze(working_dir, DEFAULT_MAX_LCOM)
}

fn collect_classes(
    tree: &tree_sitter::Tree,
    source: &str,
    file_path: &str,
    classes: &mut Vec<ClassInfo>,
) {
    let root = tree.root_node();
    let source_bytes = source.as_bytes();

    let mut cursor = root.walk();
    collect_classes_from_node(&mut cursor, source_bytes, file_path, classes);
}

fn collect_classes_from_node(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    file_path: &str,
    classes: &mut Vec<ClassInfo>,
) {
    let node = cursor.node();

    if node.kind() == "class_definition" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let class_name = node_text(name_node, source);

            let mut class_info = ClassInfo {
                name: class_name,
                file: file_path.to_string(),
                line: name_node.start_position().row as u32 + 1,
                column: name_node.start_position().column as u32 + 1,
                method_attributes: HashMap::new(),
                all_attributes: HashSet::new(),
            };

            // Find the class body and analyze methods
            if let Some(body) = node.child_by_field_name("body") {
                analyze_class_body(body, source, &mut class_info);
            }

            // Only add classes with methods
            if !class_info.method_attributes.is_empty() {
                classes.push(class_info);
            }
        }
    }

    // Continue traversing (but don't go into nested classes for now)
    if cursor.goto_first_child() {
        loop {
            let child_node = cursor.node();
            // Don't recurse into nested class definitions
            if child_node.kind() != "class_definition" || node.kind() != "class_definition" {
                collect_classes_from_node(cursor, source, file_path, classes);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn analyze_class_body(body: tree_sitter::Node, source: &[u8], class_info: &mut ClassInfo) {
    for i in 0..body.child_count() {
        if let Some(child) = body.child(i) {
            if child.kind() == "function_definition" {
                analyze_method(child, source, class_info);
            }
        }
    }
}

fn analyze_method(method_node: tree_sitter::Node, source: &[u8], class_info: &mut ClassInfo) {
    let method_name = if let Some(name_node) = method_node.child_by_field_name("name") {
        node_text(name_node, source)
    } else {
        return;
    };

    // Skip dunder methods as they often don't reflect true cohesion
    if method_name.starts_with("__") && method_name.ends_with("__") {
        // But still collect attribute assignments from __init__
        if method_name == "__init__" {
            if let Some(body) = method_node.child_by_field_name("body") {
                collect_init_attributes(body, source, class_info);
            }
        }
        return;
    }

    // Skip private methods starting with underscore (but include them for attribute collection)
    let include_in_lcom = !method_name.starts_with('_');

    // Collect attributes accessed by this method
    let mut method_attrs = HashSet::new();
    if let Some(body) = method_node.child_by_field_name("body") {
        collect_accessed_attributes(body, source, &mut method_attrs);
    }

    // Also check parameters for attributes
    if let Some(params) = method_node.child_by_field_name("parameters") {
        collect_accessed_attributes(params, source, &mut method_attrs);
    }

    // Add all attributes to the class's attribute set
    for attr in &method_attrs {
        class_info.all_attributes.insert(attr.clone());
    }

    // Only add to method_attributes if we're including in LCOM calculation
    if include_in_lcom && !method_attrs.is_empty() {
        class_info
            .method_attributes
            .insert(method_name, method_attrs);
    }
}

fn collect_init_attributes(body: tree_sitter::Node, source: &[u8], class_info: &mut ClassInfo) {
    // Look for self.attr = ... assignments in __init__
    let mut cursor = body.walk();
    collect_init_attrs_recursive(&mut cursor, source, class_info);
}

fn collect_init_attrs_recursive(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    class_info: &mut ClassInfo,
) {
    let node = cursor.node();

    if node.kind() == "assignment" {
        if let Some(left) = node.child_by_field_name("left") {
            if left.kind() == "attribute" {
                if let Some(obj) = left.child_by_field_name("object") {
                    if obj.kind() == "identifier" && node_text(obj, source) == "self" {
                        if let Some(attr) = left.child_by_field_name("attribute") {
                            let attr_name = node_text(attr, source);
                            class_info.all_attributes.insert(attr_name);
                        }
                    }
                }
            }
        }
    }

    if cursor.goto_first_child() {
        loop {
            collect_init_attrs_recursive(cursor, source, class_info);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn collect_accessed_attributes(
    node: tree_sitter::Node,
    source: &[u8],
    attrs: &mut HashSet<String>,
) {
    let mut cursor = node.walk();
    collect_attrs_recursive(&mut cursor, source, attrs);
}

fn collect_attrs_recursive(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    attrs: &mut HashSet<String>,
) {
    let node = cursor.node();

    // Look for self.attribute patterns
    if node.kind() == "attribute" {
        if let Some(obj) = node.child_by_field_name("object") {
            if obj.kind() == "identifier" && node_text(obj, source) == "self" {
                if let Some(attr) = node.child_by_field_name("attribute") {
                    let attr_name = node_text(attr, source);
                    // Exclude method calls (self.method())
                    if let Some(parent) = node.parent() {
                        if parent.kind() != "call"
                            || parent.child_by_field_name("function").map(|f| f.id())
                                != Some(node.id())
                        {
                            attrs.insert(attr_name);
                        }
                    } else {
                        attrs.insert(attr_name);
                    }
                }
            }
        }
    }

    if cursor.goto_first_child() {
        loop {
            collect_attrs_recursive(cursor, source, attrs);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// Calculate LCOM (Lack of Cohesion of Methods) using LCOM4 variant
///
/// LCOM4 measures cohesion based on method-attribute relationships:
/// - Build a graph where methods are connected if they share attributes
/// - LCOM4 = number of connected components
/// - We normalize it to 0-1 range: (components - 1) / (methods - 1)
fn calculate_lcom(class_info: &ClassInfo) -> LcomResult {
    let methods: Vec<&String> = class_info.method_attributes.keys().collect();
    let method_count = methods.len();

    if method_count < MIN_METHODS_FOR_LCOM {
        return LcomResult {
            value: 0.0,
            method_count,
            attribute_count: class_info.all_attributes.len(),
            isolated_methods: vec![],
            suggested_groups: vec![],
        };
    }

    // Build adjacency list for methods that share attributes
    let mut adjacency: HashMap<&String, HashSet<&String>> = HashMap::new();
    for method in &methods {
        adjacency.insert(method, HashSet::new());
    }

    for (i, method1) in methods.iter().enumerate() {
        let attrs1 = &class_info.method_attributes[*method1];
        for method2 in methods.iter().skip(i + 1) {
            let attrs2 = &class_info.method_attributes[*method2];
            // Check if methods share any attributes
            if attrs1.intersection(attrs2).next().is_some() {
                adjacency.get_mut(method1).unwrap().insert(method2);
                adjacency.get_mut(method2).unwrap().insert(method1);
            }
        }
    }

    // Find connected components using BFS
    let mut visited: HashSet<&String> = HashSet::new();
    let mut components: Vec<Vec<String>> = Vec::new();

    for method in &methods {
        if visited.contains(method) {
            continue;
        }

        let mut component = Vec::new();
        let mut queue = vec![*method];

        while let Some(current) = queue.pop() {
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current);
            component.push(current.clone());

            for neighbor in &adjacency[&current] {
                if !visited.contains(neighbor) {
                    queue.push(neighbor);
                }
            }
        }

        components.push(component);
    }

    let num_components = components.len();

    // Find isolated methods (methods in singleton components)
    let isolated_methods: Vec<String> = components
        .iter()
        .filter(|c| c.len() == 1)
        .map(|c| c[0].clone())
        .collect();

    // Calculate normalized LCOM4
    // Value of 0 means all methods are connected (high cohesion)
    // Value of 1 means all methods are isolated (no cohesion)
    let lcom_value = if method_count <= 1 {
        0.0
    } else {
        (num_components as f64 - 1.0) / (method_count as f64 - 1.0)
    };

    LcomResult {
        value: lcom_value,
        method_count,
        attribute_count: class_info.all_attributes.len(),
        isolated_methods,
        suggested_groups: components,
    }
}

fn suggest_refactoring(lcom_result: &LcomResult) -> String {
    if lcom_result.suggested_groups.len() <= 1 {
        return String::new();
    }

    let group_count = lcom_result.suggested_groups.len();
    let mut suggestion = format!(
        "Consider splitting into {} classes based on method groupings",
        group_count
    );

    // Only show groupings if there are a reasonable number
    if group_count <= 4 {
        let groups: Vec<String> = lcom_result
            .suggested_groups
            .iter()
            .filter(|g| !g.is_empty())
            .map(|g| {
                let methods: Vec<&str> = g.iter().map(|s| s.as_str()).collect();
                format!("[{}]", methods.join(", "))
            })
            .collect();
        suggestion.push_str(&format!(": {}", groups.join(", ")));
    }

    suggestion
}

fn node_text(node: tree_sitter::Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte();
    String::from_utf8_lossy(&source[start..end]).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_high_cohesion_class() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.py");
        fs::write(
            &file_path,
            r#"
class Rectangle:
    def __init__(self, width, height):
        self.width = width
        self.height = height

    def area(self):
        return self.width * self.height

    def perimeter(self):
        return 2 * (self.width + self.height)

    def scale(self, factor):
        self.width *= factor
        self.height *= factor
"#,
        )
        .unwrap();

        let result = analyze(dir.path().to_str().unwrap(), 0.8).unwrap();
        // All methods use the same attributes, so should be highly cohesive
        assert_eq!(result.issues_found, 0);
    }

    #[test]
    fn test_low_cohesion_class() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.py");
        fs::write(
            &file_path,
            r#"
class GodClass:
    def __init__(self):
        self.user_name = ""
        self.user_email = ""
        self.db_connection = None
        self.cache = {}
        self.logger = None

    def get_user_name(self):
        return self.user_name

    def set_user_email(self, email):
        self.user_email = email

    def connect_db(self):
        self.db_connection = "connected"

    def cache_data(self, key, value):
        self.cache[key] = value

    def log_message(self, msg):
        self.logger = msg
"#,
        )
        .unwrap();

        let result = analyze(dir.path().to_str().unwrap(), 0.5).unwrap();
        // Methods operate on completely different attributes
        assert!(result.issues_found >= 1);
        let issues = &result.structured_output.issues;
        assert!(issues.iter().any(|i| i.message.contains("GodClass")));
    }

    #[test]
    fn test_small_class_ignored() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.py");
        fs::write(
            &file_path,
            r#"
class SmallClass:
    def __init__(self):
        self.value = 0

    def get_value(self):
        return self.value
"#,
        )
        .unwrap();

        // Only one non-dunder method, should be ignored
        let result = analyze(dir.path().to_str().unwrap(), 0.8).unwrap();
        assert_eq!(result.issues_found, 0);
    }

    #[test]
    fn test_lcom_calculation() {
        let class_info = ClassInfo {
            name: "TestClass".to_string(),
            file: "test.py".to_string(),
            line: 1,
            column: 1,
            method_attributes: HashMap::from([
                (
                    "method1".to_string(),
                    HashSet::from(["a".to_string(), "b".to_string()]),
                ),
                (
                    "method2".to_string(),
                    HashSet::from(["b".to_string(), "c".to_string()]),
                ),
                ("method3".to_string(), HashSet::from(["d".to_string()])),
            ]),
            all_attributes: HashSet::from([
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
            ]),
        };

        let result = calculate_lcom(&class_info);
        // method1 and method2 share 'b', method3 is isolated
        // So we have 2 components out of 3 methods
        // LCOM = (2 - 1) / (3 - 1) = 0.5
        assert!((result.value - 0.5).abs() < 0.01);
        assert_eq!(result.isolated_methods.len(), 1);
        assert!(result.isolated_methods.contains(&"method3".to_string()));
    }

    #[test]
    fn test_analyze_with_defaults() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.py");
        fs::write(
            &file_path,
            r#"
class SimpleClass:
    def __init__(self):
        self.x = 0

    def get_x(self):
        return self.x

    def set_x(self, value):
        self.x = value
"#,
        )
        .unwrap();

        let result = analyze_with_defaults(dir.path().to_str().unwrap()).unwrap();
        assert!(result
            .structured_output
            .tool_data
            .contains_key("max_lcom_threshold"));
    }
}
