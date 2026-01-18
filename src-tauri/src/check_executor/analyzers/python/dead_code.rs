//! Dead Code Analyzer for Python
//!
//! Detects unused imports, variables, and functions in Python code using tree-sitter.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{CheckStructuredOutput, CheckSummary, IssueSeverity};

/// Information about a defined symbol
#[derive(Debug, Clone)]
struct SymbolInfo {
    name: String,
    file: String,
    line: u32,
    column: u32,
    kind: SymbolKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SymbolKind {
    Import,
    ImportFrom,
    Variable,
    Function,
    Class,
    Parameter,
}

/// Analyze a Python codebase for dead code
pub fn analyze(working_dir: &str) -> Result<ParsedOutput, String> {
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

    let mut all_definitions: Vec<SymbolInfo> = Vec::new();
    let mut all_references: HashSet<String> = HashSet::new();
    let mut cross_file_exports: HashSet<String> = HashSet::new();
    let mut files_analyzed = 0;

    // First pass: collect all definitions and references
    for file_path in &files {
        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let tree = match parser.parse(&content, None) {
            Some(t) => t,
            None => continue,
        };

        let _file_str = file_path.to_string_lossy().to_string();
        let relative_path = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        // Collect definitions and references from this file
        collect_definitions_and_references(
            &tree,
            &content,
            &relative_path,
            &mut all_definitions,
            &mut all_references,
            &mut cross_file_exports,
        );

        files_analyzed += 1;
    }

    // Identify unused symbols
    let mut issues = Vec::new();
    let mut files_with_issues = HashSet::new();

    for def in &all_definitions {
        // Skip if it's referenced anywhere
        if all_references.contains(&def.name) {
            continue;
        }

        // Skip dunder methods and variables (e.g., __init__, __name__)
        if def.name.starts_with("__") && def.name.ends_with("__") {
            continue;
        }

        // Skip single underscore prefix (conventionally private, but may be used externally)
        if def.name.starts_with('_') && !def.name.starts_with("__") {
            // Still report if it's a local variable starting with _
            if def.kind != SymbolKind::Variable && def.kind != SymbolKind::Parameter {
                continue;
            }
        }

        // Skip __all__ exports
        if cross_file_exports.contains(&def.name) {
            continue;
        }

        // Skip common patterns that are often intentionally unused
        if is_intentionally_unused(&def.name, &def.kind) {
            continue;
        }

        let (code, message) = match def.kind {
            SymbolKind::Import => ("DC001", format!("Unused import: '{}'", def.name)),
            SymbolKind::ImportFrom => (
                "DC002",
                format!("Unused import from module: '{}'", def.name),
            ),
            SymbolKind::Variable => ("DC003", format!("Unused variable: '{}'", def.name)),
            SymbolKind::Function => ("DC004", format!("Unused function: '{}'", def.name)),
            SymbolKind::Class => ("DC005", format!("Unused class: '{}'", def.name)),
            SymbolKind::Parameter => ("DC006", format!("Unused parameter: '{}'", def.name)),
        };

        files_with_issues.insert(def.file.clone());

        let severity = match def.kind {
            SymbolKind::Import | SymbolKind::ImportFrom => IssueSeverity::Warning,
            SymbolKind::Parameter => IssueSeverity::Info,
            _ => IssueSeverity::Warning,
        };

        issues.push(
            IssueBuilder::new(&def.file, message)
                .line(def.line)
                .column(def.column)
                .code(code)
                .severity(severity)
                .fixable(matches!(
                    def.kind,
                    SymbolKind::Import | SymbolKind::ImportFrom
                ))
                .build(),
        );
    }

    // Sort issues by file, then line
    issues.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.unwrap_or(0).cmp(&b.line.unwrap_or(0)))
    });

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
            tool_data: HashMap::new(),
        },
    })
}

fn collect_definitions_and_references(
    tree: &tree_sitter::Tree,
    source: &str,
    file_path: &str,
    definitions: &mut Vec<SymbolInfo>,
    references: &mut HashSet<String>,
    exports: &mut HashSet<String>,
) {
    let root = tree.root_node();
    let source_bytes = source.as_bytes();

    // Use a cursor to walk the tree
    let mut cursor = root.walk();
    collect_from_node(
        &mut cursor,
        source_bytes,
        file_path,
        definitions,
        references,
        exports,
        0,
    );
}

fn collect_from_node(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    file_path: &str,
    definitions: &mut Vec<SymbolInfo>,
    references: &mut HashSet<String>,
    exports: &mut HashSet<String>,
    depth: usize,
) {
    let node = cursor.node();
    let kind = node.kind();

    match kind {
        // Import statements
        "import_statement" => {
            collect_import_names(node, source, file_path, definitions);
        }
        "import_from_statement" => {
            collect_import_from_names(node, source, file_path, definitions);
        }
        // Function definitions
        "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                definitions.push(SymbolInfo {
                    name,
                    file: file_path.to_string(),
                    line: name_node.start_position().row as u32 + 1,
                    column: name_node.start_position().column as u32 + 1,
                    kind: SymbolKind::Function,
                });
            }
            // Collect parameter names
            if let Some(params) = node.child_by_field_name("parameters") {
                collect_parameters(params, source, file_path, definitions);
            }
        }
        // Class definitions
        "class_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                definitions.push(SymbolInfo {
                    name,
                    file: file_path.to_string(),
                    line: name_node.start_position().row as u32 + 1,
                    column: name_node.start_position().column as u32 + 1,
                    kind: SymbolKind::Class,
                });
            }
        }
        // Assignments (variable definitions)
        "assignment" => {
            if depth <= 1 {
                // Only top-level or class-level assignments
                if let Some(left) = node.child_by_field_name("left") {
                    collect_assignment_targets(left, source, file_path, definitions, exports);
                }
            }
        }
        // Augmented assignments (+=, -= etc.)
        "augmented_assignment" => {
            if let Some(left) = node.child_by_field_name("left") {
                let name = node_text(left, source);
                references.insert(name);
            }
        }
        // Named expressions (walrus operator :=)
        "named_expression" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                definitions.push(SymbolInfo {
                    name,
                    file: file_path.to_string(),
                    line: name_node.start_position().row as u32 + 1,
                    column: name_node.start_position().column as u32 + 1,
                    kind: SymbolKind::Variable,
                });
            }
        }
        // For loop variables
        "for_statement" => {
            if let Some(left) = node.child_by_field_name("left") {
                collect_loop_targets(left, source, file_path, definitions);
            }
        }
        // With statement variables
        "with_clause" => {
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    if child.kind() == "with_item" {
                        if let Some(alias) = child.child_by_field_name("alias") {
                            let name = node_text(alias, source);
                            definitions.push(SymbolInfo {
                                name,
                                file: file_path.to_string(),
                                line: alias.start_position().row as u32 + 1,
                                column: alias.start_position().column as u32 + 1,
                                kind: SymbolKind::Variable,
                            });
                        }
                    }
                }
            }
        }
        // Exception handlers
        "except_clause" => {
            // Look for "as identifier" pattern
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    if child.kind() == "as_pattern" {
                        if let Some(alias) = child.child_by_field_name("alias") {
                            let name = node_text(alias, source);
                            definitions.push(SymbolInfo {
                                name,
                                file: file_path.to_string(),
                                line: alias.start_position().row as u32 + 1,
                                column: alias.start_position().column as u32 + 1,
                                kind: SymbolKind::Variable,
                            });
                        }
                    } else if child.kind() == "identifier" {
                        // This might be the exception type or the binding
                        // We need to check the previous sibling
                        if i > 0 {
                            if let Some(prev) = node.child(i - 1) {
                                if prev.kind() == "as" {
                                    let name = node_text(child, source);
                                    definitions.push(SymbolInfo {
                                        name,
                                        file: file_path.to_string(),
                                        line: child.start_position().row as u32 + 1,
                                        column: child.start_position().column as u32 + 1,
                                        kind: SymbolKind::Variable,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        // Comprehension iterators
        "list_comprehension"
        | "set_comprehension"
        | "dictionary_comprehension"
        | "generator_expression" => {
            // These create local scopes, we'd need more complex handling
            // For now, just collect references
        }
        // Identifiers (references)
        "identifier" => {
            let name = node_text(node, source);
            // Check if this is actually a reference (not a definition context)
            if let Some(parent) = node.parent() {
                let parent_kind = parent.kind();
                // Skip if this identifier is being defined
                if !is_definition_context(parent_kind, node, &parent) {
                    references.insert(name);
                }
            } else {
                references.insert(name);
            }
        }
        // Attribute access (track base references)
        "attribute" => {
            // Track the object being accessed
            if let Some(obj) = node.child_by_field_name("object") {
                if obj.kind() == "identifier" {
                    let name = node_text(obj, source);
                    references.insert(name);
                }
            }
        }
        // Call expressions
        "call" => {
            if let Some(func) = node.child_by_field_name("function") {
                if func.kind() == "identifier" {
                    let name = node_text(func, source);
                    references.insert(name);
                } else if func.kind() == "attribute" {
                    // Method call - track the object
                    if let Some(obj) = func.child_by_field_name("object") {
                        if obj.kind() == "identifier" {
                            let name = node_text(obj, source);
                            references.insert(name);
                        }
                    }
                }
            }
        }
        _ => {}
    }

    // Recurse into children
    if cursor.goto_first_child() {
        loop {
            collect_from_node(
                cursor,
                source,
                file_path,
                definitions,
                references,
                exports,
                depth + 1,
            );
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn collect_import_names(
    node: tree_sitter::Node,
    source: &[u8],
    file_path: &str,
    definitions: &mut Vec<SymbolInfo>,
) {
    // import foo, bar, baz
    // import foo as f
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "dotted_name" => {
                    // Use only the first part as the imported name
                    if let Some(first) = child.child(0) {
                        let name = node_text(first, source);
                        definitions.push(SymbolInfo {
                            name,
                            file: file_path.to_string(),
                            line: first.start_position().row as u32 + 1,
                            column: first.start_position().column as u32 + 1,
                            kind: SymbolKind::Import,
                        });
                    }
                }
                "aliased_import" => {
                    if let Some(alias) = child.child_by_field_name("alias") {
                        let name = node_text(alias, source);
                        definitions.push(SymbolInfo {
                            name,
                            file: file_path.to_string(),
                            line: alias.start_position().row as u32 + 1,
                            column: alias.start_position().column as u32 + 1,
                            kind: SymbolKind::Import,
                        });
                    }
                }
                _ => {}
            }
        }
    }
}

fn collect_import_from_names(
    node: tree_sitter::Node,
    source: &[u8],
    file_path: &str,
    definitions: &mut Vec<SymbolInfo>,
) {
    // from foo import bar, baz
    // from foo import bar as b
    // from foo import *  (skip this case - can't track)
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "dotted_name" | "identifier" => {
                    // This is the module name, not the imported names
                    // Skip if it's right after "from"
                    if i > 0 {
                        if let Some(prev) = node.child(i - 1) {
                            if prev.kind() == "import" {
                                let name = node_text(child, source);
                                definitions.push(SymbolInfo {
                                    name,
                                    file: file_path.to_string(),
                                    line: child.start_position().row as u32 + 1,
                                    column: child.start_position().column as u32 + 1,
                                    kind: SymbolKind::ImportFrom,
                                });
                            }
                        }
                    }
                }
                "aliased_import" => {
                    if let Some(alias) = child.child_by_field_name("alias") {
                        let name = node_text(alias, source);
                        definitions.push(SymbolInfo {
                            name,
                            file: file_path.to_string(),
                            line: alias.start_position().row as u32 + 1,
                            column: alias.start_position().column as u32 + 1,
                            kind: SymbolKind::ImportFrom,
                        });
                    } else if let Some(name_node) = child.child_by_field_name("name") {
                        let name = node_text(name_node, source);
                        definitions.push(SymbolInfo {
                            name,
                            file: file_path.to_string(),
                            line: name_node.start_position().row as u32 + 1,
                            column: name_node.start_position().column as u32 + 1,
                            kind: SymbolKind::ImportFrom,
                        });
                    }
                }
                "wildcard_import" => {
                    // from foo import * - skip, can't track
                }
                _ => {}
            }
        }
    }
}

fn collect_parameters(
    params_node: tree_sitter::Node,
    source: &[u8],
    file_path: &str,
    definitions: &mut Vec<SymbolInfo>,
) {
    for i in 0..params_node.child_count() {
        if let Some(child) = params_node.child(i) {
            match child.kind() {
                "identifier" => {
                    let name = node_text(child, source);
                    // Skip self and cls
                    if name != "self" && name != "cls" {
                        definitions.push(SymbolInfo {
                            name,
                            file: file_path.to_string(),
                            line: child.start_position().row as u32 + 1,
                            column: child.start_position().column as u32 + 1,
                            kind: SymbolKind::Parameter,
                        });
                    }
                }
                "default_parameter" | "typed_parameter" | "typed_default_parameter" => {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let name = node_text(name_node, source);
                        if name != "self" && name != "cls" {
                            definitions.push(SymbolInfo {
                                name,
                                file: file_path.to_string(),
                                line: name_node.start_position().row as u32 + 1,
                                column: name_node.start_position().column as u32 + 1,
                                kind: SymbolKind::Parameter,
                            });
                        }
                    }
                }
                "list_splat_pattern" | "dictionary_splat_pattern" => {
                    // *args, **kwargs
                    for j in 0..child.child_count() {
                        if let Some(name_child) = child.child(j) {
                            if name_child.kind() == "identifier" {
                                let name = node_text(name_child, source);
                                definitions.push(SymbolInfo {
                                    name,
                                    file: file_path.to_string(),
                                    line: name_child.start_position().row as u32 + 1,
                                    column: name_child.start_position().column as u32 + 1,
                                    kind: SymbolKind::Parameter,
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn collect_assignment_targets(
    target: tree_sitter::Node,
    source: &[u8],
    file_path: &str,
    definitions: &mut Vec<SymbolInfo>,
    exports: &mut HashSet<String>,
) {
    match target.kind() {
        "identifier" => {
            let name = node_text(target, source);
            // Check if this is __all__ definition
            if name == "__all__" {
                // Try to collect exported names from the right side
                if let Some(parent) = target.parent() {
                    if let Some(right) = parent.child_by_field_name("right") {
                        collect_all_exports(right, source, exports);
                    }
                }
            }
            definitions.push(SymbolInfo {
                name,
                file: file_path.to_string(),
                line: target.start_position().row as u32 + 1,
                column: target.start_position().column as u32 + 1,
                kind: SymbolKind::Variable,
            });
        }
        "pattern_list" | "tuple_pattern" => {
            // a, b = 1, 2
            for i in 0..target.child_count() {
                if let Some(child) = target.child(i) {
                    collect_assignment_targets(child, source, file_path, definitions, exports);
                }
            }
        }
        "subscript" | "attribute" => {
            // a[0] = x or a.b = x - these are mutations, not definitions
        }
        _ => {}
    }
}

fn collect_loop_targets(
    target: tree_sitter::Node,
    source: &[u8],
    file_path: &str,
    definitions: &mut Vec<SymbolInfo>,
) {
    match target.kind() {
        "identifier" => {
            let name = node_text(target, source);
            definitions.push(SymbolInfo {
                name,
                file: file_path.to_string(),
                line: target.start_position().row as u32 + 1,
                column: target.start_position().column as u32 + 1,
                kind: SymbolKind::Variable,
            });
        }
        "pattern_list" | "tuple_pattern" => {
            for i in 0..target.child_count() {
                if let Some(child) = target.child(i) {
                    collect_loop_targets(child, source, file_path, definitions);
                }
            }
        }
        _ => {}
    }
}

fn collect_all_exports(node: tree_sitter::Node, source: &[u8], exports: &mut HashSet<String>) {
    match node.kind() {
        "list" | "tuple" => {
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    if child.kind() == "string" {
                        let text = node_text(child, source);
                        // Remove quotes
                        let name = text.trim_matches(|c| c == '"' || c == '\'');
                        exports.insert(name.to_string());
                    }
                }
            }
        }
        _ => {}
    }
}

fn is_definition_context(
    parent_kind: &str,
    _node: tree_sitter::Node,
    parent: &tree_sitter::Node,
) -> bool {
    match parent_kind {
        "function_definition" | "class_definition" => {
            // Check if this is the name field
            if let Some(name_field) = parent.child_by_field_name("name") {
                return name_field.id() == _node.id();
            }
            false
        }
        "assignment" => {
            // Check if this is on the left side
            if let Some(left) = parent.child_by_field_name("left") {
                return contains_node(left, _node);
            }
            false
        }
        "augmented_assignment" => {
            if let Some(left) = parent.child_by_field_name("left") {
                return left.id() == _node.id();
            }
            false
        }
        "for_statement" => {
            if let Some(left) = parent.child_by_field_name("left") {
                return contains_node(left, _node);
            }
            false
        }
        "parameters" | "default_parameter" | "typed_parameter" | "typed_default_parameter" => true,
        "aliased_import" => {
            if let Some(alias) = parent.child_by_field_name("alias") {
                return alias.id() == _node.id();
            }
            false
        }
        "dotted_name" => {
            // First component of dotted name after import keyword is a definition
            if let Some(grandparent) = parent.parent() {
                if grandparent.kind() == "import_statement" {
                    return true;
                }
            }
            false
        }
        "named_expression" => {
            if let Some(name) = parent.child_by_field_name("name") {
                return name.id() == _node.id();
            }
            false
        }
        _ => false,
    }
}

fn contains_node(container: tree_sitter::Node, target: tree_sitter::Node) -> bool {
    if container.id() == target.id() {
        return true;
    }
    for i in 0..container.child_count() {
        if let Some(child) = container.child(i) {
            if contains_node(child, target) {
                return true;
            }
        }
    }
    false
}

fn node_text(node: tree_sitter::Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte();
    String::from_utf8_lossy(&source[start..end]).to_string()
}

fn is_intentionally_unused(name: &str, kind: &SymbolKind) -> bool {
    // Underscore prefix for intentionally unused variables
    if name == "_" {
        return true;
    }

    // Common pytest fixtures that look unused
    if matches!(kind, SymbolKind::Parameter) {
        let fixtures = [
            "request",
            "tmp_path",
            "tmpdir",
            "capsys",
            "capfd",
            "caplog",
            "monkeypatch",
            "pytestconfig",
            "cache",
            "fixture",
        ];
        if fixtures.contains(&name) {
            return true;
        }
    }

    // Common type checking imports
    if matches!(kind, SymbolKind::Import | SymbolKind::ImportFrom) {
        let type_imports = [
            "TYPE_CHECKING",
            "Any",
            "Optional",
            "Union",
            "List",
            "Dict",
            "Set",
            "Tuple",
            "Callable",
            "TypeVar",
            "Generic",
            "Protocol",
            "Literal",
            "Final",
            "ClassVar",
            "Annotated",
            "overload",
            "cast",
        ];
        if type_imports.contains(&name) {
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

    #[test]
    fn test_unused_import() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.py");
        fs::write(
            &file_path,
            r#"
import os
import sys

print(sys.version)
"#,
        )
        .unwrap();

        let result = analyze(dir.path().to_str().unwrap()).unwrap();
        assert!(result.issues_found >= 1);
        let issues = &result.structured_output.issues;
        assert!(issues.iter().any(|i| i.message.contains("os")));
    }

    #[test]
    fn test_unused_variable() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.py");
        fs::write(
            &file_path,
            r#"
x = 10
y = 20
print(y)
"#,
        )
        .unwrap();

        let result = analyze(dir.path().to_str().unwrap()).unwrap();
        assert!(result.issues_found >= 1);
        let issues = &result.structured_output.issues;
        assert!(issues.iter().any(|i| i.message.contains("x")));
    }

    #[test]
    fn test_unused_function() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.py");
        fs::write(
            &file_path,
            r#"
def unused_func():
    pass

def used_func():
    pass

used_func()
"#,
        )
        .unwrap();

        let result = analyze(dir.path().to_str().unwrap()).unwrap();
        assert!(result.issues_found >= 1);
        let issues = &result.structured_output.issues;
        assert!(issues.iter().any(|i| i.message.contains("unused_func")));
    }

    #[test]
    fn test_underscore_ignored() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.py");
        fs::write(
            &file_path,
            r#"
for _ in range(10):
    print("hello")
"#,
        )
        .unwrap();

        let result = analyze(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result.issues_found, 0);
    }

    #[test]
    fn test_dunder_methods_ignored() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.py");
        fs::write(
            &file_path,
            r#"
class MyClass:
    def __init__(self):
        pass

    def __str__(self):
        return "MyClass"
"#,
        )
        .unwrap();

        let result = analyze(dir.path().to_str().unwrap()).unwrap();
        // __init__ and __str__ should not be reported
        let issues = &result.structured_output.issues;
        assert!(!issues.iter().any(|i| i.message.contains("__init__")));
        assert!(!issues.iter().any(|i| i.message.contains("__str__")));
    }
}
