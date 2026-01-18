//! TypeScript Dead Code Analyzer
//!
//! Detects potentially dead code in TypeScript projects:
//! - Unused imports
//! - Unused variables
//! - Unused exports (exported but never imported elsewhere)
//!
//! Uses tree-sitter-typescript for parsing.

use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{CheckIssue, CheckStructuredOutput, CheckSummary, IssueSeverity};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tracing::{debug, warn};

/// Import information collected from a file
#[derive(Debug, Clone)]
struct ImportInfo {
    /// The imported identifier name
    name: String,
    /// File path where this import is declared
    file: String,
    /// Line number of the import
    line: u32,
    /// Column number
    column: u32,
    /// Whether this import is used in the file
    used: bool,
    /// Source module path
    source: String,
}

/// Export information collected from a file
#[derive(Debug, Clone)]
struct ExportInfo {
    /// The exported identifier name
    name: String,
    /// File path where this export is declared
    file: String,
    /// Line number of the export
    line: u32,
    /// Column number
    column: u32,
    /// The kind of export (function, class, variable, type)
    kind: String,
}

/// Variable information collected from a file
#[derive(Debug, Clone)]
struct VariableInfo {
    /// Variable name
    name: String,
    /// File path
    file: String,
    /// Line number
    line: u32,
    /// Column number
    column: u32,
    /// Whether the variable is used
    used: bool,
    /// Scope level (0 = module level)
    scope_level: u32,
}

/// Analyze TypeScript code for dead code
///
/// # Arguments
/// * `working_dir` - The directory to analyze
///
/// # Returns
/// * `Result<ParsedOutput, String>` - Analysis results or error
pub fn analyze(working_dir: &str) -> Result<ParsedOutput, String> {
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
            "*.d.ts".to_string(),
        ],
        max_depth: None,
    };

    let files = walk_files(path, &config);
    debug!("Found {} TypeScript files to analyze", files.len());

    // Initialize tree-sitter parser
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
    parser
        .set_language(&language.into())
        .map_err(|e| format!("Failed to set TypeScript language: {}", e))?;

    // First pass: collect all exports and imports across all files
    let mut all_exports: HashMap<String, Vec<ExportInfo>> = HashMap::new();
    let mut all_imports: Vec<ImportInfo> = Vec::new();
    let mut all_variables: Vec<VariableInfo> = Vec::new();
    let mut identifier_usage: HashMap<String, HashSet<String>> = HashMap::new(); // file -> used identifiers

    for file_path in &files {
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

        let relative_path = file_path
            .strip_prefix(path)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        // Collect exports, imports, variables, and usage
        let mut file_imports: Vec<ImportInfo> = Vec::new();
        let mut file_exports: Vec<ExportInfo> = Vec::new();
        let mut file_variables: Vec<VariableInfo> = Vec::new();
        let mut file_usage: HashSet<String> = HashSet::new();

        collect_declarations(
            &tree.root_node(),
            &content,
            &relative_path,
            &mut file_imports,
            &mut file_exports,
            &mut file_variables,
            &mut file_usage,
            0,
        );

        all_imports.extend(file_imports);

        for export in file_exports {
            all_exports
                .entry(export.name.clone())
                .or_default()
                .push(export);
        }

        all_variables.extend(file_variables);
        identifier_usage.insert(relative_path.clone(), file_usage);
    }

    // Second pass: check what's actually used
    let mut issues: Vec<CheckIssue> = Vec::new();
    let mut files_with_issues: HashSet<String> = HashSet::new();

    // Check for unused imports
    for import in &all_imports {
        let file_usage = identifier_usage.get(&import.file).unwrap_or(&HashSet::new());
        if !file_usage.contains(&import.name) && !is_likely_side_effect_import(&import.name) {
            files_with_issues.insert(import.file.clone());
            issues.push(
                IssueBuilder::new(
                    import.file.clone(),
                    format!("Unused import: '{}'", import.name),
                )
                .line(import.line)
                .column(import.column)
                .code("TS-DEAD-001")
                .warning()
                .fixable(true)
                .build(),
            );
        }
    }

    // Check for unused variables (module-level only to avoid false positives)
    for var in &all_variables {
        if var.scope_level == 0 {
            let file_usage = identifier_usage.get(&var.file).unwrap_or(&HashSet::new());
            // Module-level variables that aren't used and aren't exported
            let is_exported = all_exports
                .get(&var.name)
                .map(|exports| exports.iter().any(|e| e.file == var.file))
                .unwrap_or(false);

            if !file_usage.contains(&var.name) && !is_exported && !var.name.starts_with('_') {
                files_with_issues.insert(var.file.clone());
                issues.push(
                    IssueBuilder::new(
                        var.file.clone(),
                        format!("Unused variable: '{}'", var.name),
                    )
                    .line(var.line)
                    .column(var.column)
                    .code("TS-DEAD-002")
                    .warning()
                    .fixable(true)
                    .build(),
                );
            }
        }
    }

    // Check for unused exports (exported but never imported anywhere)
    // Build a set of all imported identifiers across all files
    let mut all_imported_names: HashSet<String> = HashSet::new();
    for import in &all_imports {
        all_imported_names.insert(import.name.clone());
    }

    for (export_name, exports) in &all_exports {
        // Skip common patterns that are often used externally
        if is_likely_entry_point(export_name) {
            continue;
        }

        // Check if this export is ever imported
        if !all_imported_names.contains(export_name) {
            for export in exports {
                // Don't report index files as they're often entry points
                if export.file.contains("index.ts") || export.file.contains("index.tsx") {
                    continue;
                }

                files_with_issues.insert(export.file.clone());
                issues.push(
                    IssueBuilder::new(
                        export.file.clone(),
                        format!(
                            "Unused export: '{}' ({}) is never imported",
                            export_name, export.kind
                        ),
                    )
                    .line(export.line)
                    .column(export.column)
                    .code("TS-DEAD-003")
                    .info()
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
    let info_count = issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Info)
        .count() as u32;

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
                    ("warning".to_string(), warning_count),
                    ("info".to_string(), info_count),
                ]),
            }),
            tool_data: HashMap::from([
                (
                    "unused_imports".to_string(),
                    serde_json::json!(issues
                        .iter()
                        .filter(|i| i.code.as_deref() == Some("TS-DEAD-001"))
                        .count()),
                ),
                (
                    "unused_variables".to_string(),
                    serde_json::json!(issues
                        .iter()
                        .filter(|i| i.code.as_deref() == Some("TS-DEAD-002"))
                        .count()),
                ),
                (
                    "unused_exports".to_string(),
                    serde_json::json!(issues
                        .iter()
                        .filter(|i| i.code.as_deref() == Some("TS-DEAD-003"))
                        .count()),
                ),
            ]),
        },
    })
}

/// Collect imports, exports, variables, and identifier usage from a node
fn collect_declarations(
    node: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    imports: &mut Vec<ImportInfo>,
    exports: &mut Vec<ExportInfo>,
    variables: &mut Vec<VariableInfo>,
    usage: &mut HashSet<String>,
    scope_level: u32,
) {
    match node.kind() {
        // Import declarations
        "import_statement" => {
            collect_imports(node, source, file_path, imports);
        }

        // Export declarations
        "export_statement" => {
            collect_exports(node, source, file_path, exports);
        }

        // Variable declarations
        "lexical_declaration" | "variable_declaration" => {
            collect_variables(node, source, file_path, variables, scope_level);
        }

        // Function/class declarations (also count as variables at module level)
        "function_declaration" => {
            if scope_level == 0 {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = get_node_text(&name_node, source);
                    if !name.is_empty() {
                        variables.push(VariableInfo {
                            name,
                            file: file_path.to_string(),
                            line: name_node.start_position().row as u32 + 1,
                            column: name_node.start_position().column as u32 + 1,
                            used: false,
                            scope_level,
                        });
                    }
                }
            }
        }

        "class_declaration" => {
            if scope_level == 0 {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = get_node_text(&name_node, source);
                    if !name.is_empty() {
                        variables.push(VariableInfo {
                            name,
                            file: file_path.to_string(),
                            line: name_node.start_position().row as u32 + 1,
                            column: name_node.start_position().column as u32 + 1,
                            used: false,
                            scope_level,
                        });
                    }
                }
            }
        }

        // Identifier usage
        "identifier" | "property_identifier" | "shorthand_property_identifier" => {
            let text = get_node_text(node, source);
            if !text.is_empty() {
                // Don't count identifiers in declarations as "usage"
                if let Some(parent) = node.parent() {
                    let parent_kind = parent.kind();
                    if parent_kind != "import_specifier"
                        && parent_kind != "import_clause"
                        && parent_kind != "export_specifier"
                        && parent_kind != "variable_declarator"
                        && parent_kind != "function_declaration"
                        && parent_kind != "class_declaration"
                        && parent_kind != "formal_parameters"
                        && parent_kind != "required_parameter"
                        && parent_kind != "optional_parameter"
                    {
                        usage.insert(text);
                    }
                }
            }
        }

        // JSX identifiers
        "jsx_element" | "jsx_self_closing_element" => {
            // Collect JSX component usage
            if let Some(opening) = node.child_by_field_name("open_tag") {
                if let Some(name) = opening.child_by_field_name("name") {
                    let text = get_node_text(&name, source);
                    if !text.is_empty() && text.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                        usage.insert(text);
                    }
                }
            }
            // For self-closing elements
            if let Some(name) = node.child_by_field_name("name") {
                let text = get_node_text(&name, source);
                if !text.is_empty() && text.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                    usage.insert(text);
                }
            }
        }

        _ => {}
    }

    // Determine new scope level for children
    let new_scope_level = if matches!(
        node.kind(),
        "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition"
            | "class_declaration"
            | "class_expression"
            | "for_statement"
            | "for_in_statement"
            | "for_of_statement"
            | "while_statement"
            | "do_statement"
            | "if_statement"
            | "switch_statement"
            | "try_statement"
            | "catch_clause"
            | "block"
    ) {
        scope_level + 1
    } else {
        scope_level
    };

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_declarations(
            &child,
            source,
            file_path,
            imports,
            exports,
            variables,
            usage,
            new_scope_level,
        );
    }
}

/// Collect imports from an import statement
fn collect_imports(
    node: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    imports: &mut Vec<ImportInfo>,
) {
    // Get the source module
    let source_module = node
        .child_by_field_name("source")
        .map(|n| get_node_text(&n, source))
        .unwrap_or_default();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "import_clause" => {
                // Handle default imports and named imports
                let mut clause_cursor = child.walk();
                for clause_child in child.children(&mut clause_cursor) {
                    match clause_child.kind() {
                        "identifier" => {
                            // Default import
                            let name = get_node_text(&clause_child, source);
                            if !name.is_empty() {
                                imports.push(ImportInfo {
                                    name,
                                    file: file_path.to_string(),
                                    line: clause_child.start_position().row as u32 + 1,
                                    column: clause_child.start_position().column as u32 + 1,
                                    used: false,
                                    source: source_module.clone(),
                                });
                            }
                        }
                        "named_imports" => {
                            collect_named_imports(
                                &clause_child,
                                source,
                                file_path,
                                imports,
                                &source_module,
                            );
                        }
                        "namespace_import" => {
                            // import * as name from '...'
                            if let Some(name_node) = clause_child.child_by_field_name("name") {
                                let name = get_node_text(&name_node, source);
                                if !name.is_empty() {
                                    imports.push(ImportInfo {
                                        name,
                                        file: file_path.to_string(),
                                        line: name_node.start_position().row as u32 + 1,
                                        column: name_node.start_position().column as u32 + 1,
                                        used: false,
                                        source: source_module.clone(),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// Collect named imports from a named_imports node
fn collect_named_imports(
    node: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    imports: &mut Vec<ImportInfo>,
    source_module: &str,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "import_specifier" {
            // Get the local name (alias or original name)
            let name = child
                .child_by_field_name("alias")
                .or_else(|| child.child_by_field_name("name"))
                .map(|n| get_node_text(&n, source))
                .unwrap_or_default();

            if !name.is_empty() {
                imports.push(ImportInfo {
                    name,
                    file: file_path.to_string(),
                    line: child.start_position().row as u32 + 1,
                    column: child.start_position().column as u32 + 1,
                    used: false,
                    source: source_module.to_string(),
                });
            }
        }
    }
}

/// Collect exports from an export statement
fn collect_exports(
    node: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    exports: &mut Vec<ExportInfo>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = get_node_text(&name_node, source);
                    if !name.is_empty() {
                        exports.push(ExportInfo {
                            name,
                            file: file_path.to_string(),
                            line: name_node.start_position().row as u32 + 1,
                            column: name_node.start_position().column as u32 + 1,
                            kind: "function".to_string(),
                        });
                    }
                }
            }
            "class_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = get_node_text(&name_node, source);
                    if !name.is_empty() {
                        exports.push(ExportInfo {
                            name,
                            file: file_path.to_string(),
                            line: name_node.start_position().row as u32 + 1,
                            column: name_node.start_position().column as u32 + 1,
                            kind: "class".to_string(),
                        });
                    }
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                // export const/let/var
                let mut decl_cursor = child.walk();
                for decl_child in child.children(&mut decl_cursor) {
                    if decl_child.kind() == "variable_declarator" {
                        if let Some(name_node) = decl_child.child_by_field_name("name") {
                            let name = get_node_text(&name_node, source);
                            if !name.is_empty() {
                                exports.push(ExportInfo {
                                    name,
                                    file: file_path.to_string(),
                                    line: name_node.start_position().row as u32 + 1,
                                    column: name_node.start_position().column as u32 + 1,
                                    kind: "variable".to_string(),
                                });
                            }
                        }
                    }
                }
            }
            "export_clause" => {
                // export { ... }
                let mut clause_cursor = child.walk();
                for clause_child in child.children(&mut clause_cursor) {
                    if clause_child.kind() == "export_specifier" {
                        let name = clause_child
                            .child_by_field_name("alias")
                            .or_else(|| clause_child.child_by_field_name("name"))
                            .map(|n| get_node_text(&n, source))
                            .unwrap_or_default();

                        if !name.is_empty() {
                            exports.push(ExportInfo {
                                name,
                                file: file_path.to_string(),
                                line: clause_child.start_position().row as u32 + 1,
                                column: clause_child.start_position().column as u32 + 1,
                                kind: "re-export".to_string(),
                            });
                        }
                    }
                }
            }
            "type_alias_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = get_node_text(&name_node, source);
                    if !name.is_empty() {
                        exports.push(ExportInfo {
                            name,
                            file: file_path.to_string(),
                            line: name_node.start_position().row as u32 + 1,
                            column: name_node.start_position().column as u32 + 1,
                            kind: "type".to_string(),
                        });
                    }
                }
            }
            "interface_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = get_node_text(&name_node, source);
                    if !name.is_empty() {
                        exports.push(ExportInfo {
                            name,
                            file: file_path.to_string(),
                            line: name_node.start_position().row as u32 + 1,
                            column: name_node.start_position().column as u32 + 1,
                            kind: "interface".to_string(),
                        });
                    }
                }
            }
            "enum_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = get_node_text(&name_node, source);
                    if !name.is_empty() {
                        exports.push(ExportInfo {
                            name,
                            file: file_path.to_string(),
                            line: name_node.start_position().row as u32 + 1,
                            column: name_node.start_position().column as u32 + 1,
                            kind: "enum".to_string(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

/// Collect variable declarations
fn collect_variables(
    node: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    variables: &mut Vec<VariableInfo>,
    scope_level: u32,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_declarator" {
            if let Some(name_node) = child.child_by_field_name("name") {
                // Handle both simple identifiers and patterns
                match name_node.kind() {
                    "identifier" => {
                        let name = get_node_text(&name_node, source);
                        if !name.is_empty() {
                            variables.push(VariableInfo {
                                name,
                                file: file_path.to_string(),
                                line: name_node.start_position().row as u32 + 1,
                                column: name_node.start_position().column as u32 + 1,
                                used: false,
                                scope_level,
                            });
                        }
                    }
                    "array_pattern" | "object_pattern" => {
                        // Handle destructuring - extract identifiers
                        collect_pattern_identifiers(&name_node, source, file_path, variables, scope_level);
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Collect identifiers from destructuring patterns
fn collect_pattern_identifiers(
    node: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    variables: &mut Vec<VariableInfo>,
    scope_level: u32,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => {
                let name = get_node_text(&child, source);
                if !name.is_empty() {
                    variables.push(VariableInfo {
                        name,
                        file: file_path.to_string(),
                        line: child.start_position().row as u32 + 1,
                        column: child.start_position().column as u32 + 1,
                        used: false,
                        scope_level,
                    });
                }
            }
            "pair_pattern" => {
                // { key: value } - get the value part
                if let Some(value) = child.child_by_field_name("value") {
                    collect_pattern_identifiers(&value, source, file_path, variables, scope_level);
                }
            }
            "assignment_pattern" => {
                // { key = default } - get the left part
                if let Some(left) = child.child_by_field_name("left") {
                    collect_pattern_identifiers(&left, source, file_path, variables, scope_level);
                }
            }
            "array_pattern" | "object_pattern" => {
                // Nested patterns
                collect_pattern_identifiers(&child, source, file_path, variables, scope_level);
            }
            "rest_pattern" => {
                // ...rest
                if let Some(name) = child.child_by_field_name("name") {
                    let name_text = get_node_text(&name, source);
                    if !name_text.is_empty() {
                        variables.push(VariableInfo {
                            name: name_text,
                            file: file_path.to_string(),
                            line: name.start_position().row as u32 + 1,
                            column: name.start_position().column as u32 + 1,
                            used: false,
                            scope_level,
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

/// Get the text content of a node
fn get_node_text(node: &tree_sitter::Node, source: &str) -> String {
    let start = node.start_byte();
    let end = node.end_byte();
    if start < source.len() && end <= source.len() {
        source[start..end].trim_matches('"').trim_matches('\'').to_string()
    } else {
        String::new()
    }
}

/// Check if an import is likely a side-effect import (CSS, polyfills, etc.)
fn is_likely_side_effect_import(name: &str) -> bool {
    // Empty name means it's a side-effect import like `import './styles.css'`
    name.is_empty()
}

/// Check if an export name is likely an entry point that's used externally
fn is_likely_entry_point(name: &str) -> bool {
    // Common patterns for entry points
    let entry_patterns = [
        "default",
        "App",
        "main",
        "index",
        "init",
        "setup",
        "configure",
        "config",
        "store",
        "router",
        "routes",
    ];

    entry_patterns.iter().any(|p| name.eq_ignore_ascii_case(p))
        || name.ends_with("Provider")
        || name.ends_with("Context")
        || name.ends_with("Hook")
        || name.starts_with("use")  // React hooks
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
    fn test_unused_import() {
        let dir = TempDir::new().unwrap();
        create_test_file(
            &dir,
            "test.ts",
            r#"
import { unused } from './module';

const x = 1;
console.log(x);
"#,
        );

        let result = analyze(dir.path().to_str().unwrap()).unwrap();
        assert!(result.issues_found > 0);
        assert!(result
            .structured_output
            .issues
            .iter()
            .any(|i| i.code.as_deref() == Some("TS-DEAD-001")));
    }

    #[test]
    fn test_used_import() {
        let dir = TempDir::new().unwrap();
        create_test_file(
            &dir,
            "test.ts",
            r#"
import { used } from './module';

console.log(used);
"#,
        );

        let result = analyze(dir.path().to_str().unwrap()).unwrap();
        let unused_import_issues: Vec<_> = result
            .structured_output
            .issues
            .iter()
            .filter(|i| i.code.as_deref() == Some("TS-DEAD-001") && i.message.contains("used"))
            .collect();
        assert!(unused_import_issues.is_empty());
    }

    #[test]
    fn test_unused_variable() {
        let dir = TempDir::new().unwrap();
        create_test_file(
            &dir,
            "test.ts",
            r#"
const unusedVar = 42;
const usedVar = 1;
console.log(usedVar);
"#,
        );

        let result = analyze(dir.path().to_str().unwrap()).unwrap();
        assert!(result
            .structured_output
            .issues
            .iter()
            .any(|i| i.code.as_deref() == Some("TS-DEAD-002")
                && i.message.contains("unusedVar")));
    }

    #[test]
    fn test_underscore_prefix_ignored() {
        let dir = TempDir::new().unwrap();
        create_test_file(
            &dir,
            "test.ts",
            r#"
const _intentionallyUnused = 42;
"#,
        );

        let result = analyze(dir.path().to_str().unwrap()).unwrap();
        // Variables starting with _ should not be reported
        assert!(!result
            .structured_output
            .issues
            .iter()
            .any(|i| i.message.contains("_intentionallyUnused")));
    }
}
