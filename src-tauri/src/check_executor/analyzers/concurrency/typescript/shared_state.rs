//! Shared state detection for TypeScript
//!
//! Detects:
//! - Module-level let/var declarations (mutable globals)
//! - Class properties that could be shared
//! - Closure captures in event handlers

use crate::check_executor::analyzers::concurrency::types::{
    AccessType, AnalysisContext, SharedState, SharedStateType, StateAccess,
};
use tree_sitter::{Node, Tree};

/// Detect shared state in a TypeScript file
pub fn detect_shared_state(tree: &Tree, source: &str, file: &str, ctx: &mut AnalysisContext) {
    let root = tree.root_node();

    // Detect module-level variables
    detect_module_variables(&root, source, file, ctx);

    // Detect class properties
    detect_class_properties(&root, source, file, ctx);
}

/// Detect module-level let/var declarations
fn detect_module_variables(root: &Node, source: &str, file: &str, ctx: &mut AnalysisContext) {
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        // Look for variable declarations at module level
        if child.kind() == "lexical_declaration" || child.kind() == "variable_declaration" {
            let is_const = get_declaration_kind(&child, source) == "const";

            // Get all declared variables
            let mut decl_cursor = child.walk();
            for decl_child in child.children(&mut decl_cursor) {
                if decl_child.kind() == "variable_declarator" {
                    if let Some(name_node) = decl_child.child_by_field_name("name") {
                        if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                            // Skip const declarations (immutable)
                            if is_const {
                                continue;
                            }

                            ctx.shared_states.push(SharedState {
                                name: name.to_string(),
                                state_type: SharedStateType::ModuleVariable,
                                definition_line: child.start_position().row as u32 + 1,
                                file: file.to_string(),
                                accesses: Vec::new(),
                                appears_thread_safe: is_likely_immutable(&decl_child, source),
                                thread_safe_reason: None,
                            });
                        }
                    }
                }
            }
        }

        // Also check for export statements with variable declarations
        if child.kind() == "export_statement" {
            let mut export_cursor = child.walk();
            for export_child in child.children(&mut export_cursor) {
                if export_child.kind() == "lexical_declaration"
                    || export_child.kind() == "variable_declaration"
                {
                    let is_const = get_declaration_kind(&export_child, source) == "const";
                    if is_const {
                        continue;
                    }

                    let mut var_cursor = export_child.walk();
                    for var_child in export_child.children(&mut var_cursor) {
                        if var_child.kind() == "variable_declarator" {
                            if let Some(name_node) = var_child.child_by_field_name("name") {
                                if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                                    ctx.shared_states.push(SharedState {
                                        name: name.to_string(),
                                        state_type: SharedStateType::ModuleVariable,
                                        definition_line: export_child.start_position().row as u32
                                            + 1,
                                        file: file.to_string(),
                                        accesses: Vec::new(),
                                        appears_thread_safe: false,
                                        thread_safe_reason: None,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Get the declaration kind (let, const, var)
fn get_declaration_kind<'a>(node: &'a Node<'a>, source: &'a str) -> &'a str {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "let" || child.kind() == "const" || child.kind() == "var" {
            return child.utf8_text(source.as_bytes()).unwrap_or("let");
        }
    }
    "let"
}

/// Detect class properties that could be shared
fn detect_class_properties(root: &Node, source: &str, file: &str, ctx: &mut AnalysisContext) {
    traverse_for_classes(root, source, file, ctx);
}

/// Recursively find class definitions
fn traverse_for_classes(node: &Node, source: &str, file: &str, ctx: &mut AnalysisContext) {
    if node.kind() == "class_declaration" || node.kind() == "class" {
        analyze_class(node, source, file, ctx);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        traverse_for_classes(&child, source, file, ctx);
    }
}

/// Analyze a class for shared properties
fn analyze_class(class_node: &Node, source: &str, file: &str, ctx: &mut AnalysisContext) {
    let class_name = class_node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .unwrap_or("AnonymousClass");

    // Find the class body
    if let Some(body) = class_node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            // Look for property definitions
            if child.kind() == "public_field_definition"
                || child.kind() == "field_definition"
                || child.kind() == "property_definition"
            {
                // Check if it's readonly
                let is_readonly = has_modifier(&child, source, "readonly");
                let is_static = has_modifier(&child, source, "static");

                if let Some(name_node) = child.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                        // Static properties are more likely to be shared
                        if is_static && !is_readonly {
                            ctx.shared_states.push(SharedState {
                                name: format!("{}.{}", class_name, name),
                                state_type: SharedStateType::ClassProperty,
                                definition_line: child.start_position().row as u32 + 1,
                                file: file.to_string(),
                                accesses: Vec::new(),
                                appears_thread_safe: is_readonly,
                                thread_safe_reason: if is_readonly {
                                    Some("readonly property".to_string())
                                } else {
                                    None
                                },
                            });
                        }
                    }
                }
            }
        }
    }
}

/// Check if a node has a specific modifier
fn has_modifier(node: &Node, source: &str, modifier: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "accessibility_modifier" || child.kind() == modifier {
            if let Ok(text) = child.utf8_text(source.as_bytes()) {
                if text == modifier {
                    return true;
                }
            }
        }
    }
    false
}

/// Check if a variable appears to be effectively immutable
fn is_likely_immutable(declarator: &Node, _source: &str) -> bool {
    // Check the initializer - if it's a primitive literal, it might be effectively constant
    if let Some(value) = declarator.child_by_field_name("value") {
        match value.kind() {
            "string" | "number" | "true" | "false" | "null" | "undefined" => return true,
            _ => {}
        }
    }
    false
}

/// Map accesses to shared state throughout a file
pub fn map_state_accesses(tree: &Tree, source: &str, ctx: &mut AnalysisContext) {
    let root = tree.root_node();
    traverse_for_accesses(&root, source, ctx, None, false);
}

/// Recursively traverse to find state accesses
fn traverse_for_accesses(
    node: &Node,
    source: &str,
    ctx: &mut AnalysisContext,
    current_function: Option<&str>,
    in_async: bool,
) {
    let kind = node.kind();

    // Track async context
    let is_async_func =
        kind == "arrow_function" || kind == "function_declaration" || kind == "method_definition";
    let new_in_async = if is_async_func {
        has_async_modifier(node, source)
    } else {
        in_async
    };

    // Track function name
    let func_name = if kind == "function_declaration" || kind == "method_definition" {
        node.child_by_field_name("name")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
    } else {
        current_function
    };

    // Check for identifier access
    if kind == "identifier" {
        if let Ok(name) = node.utf8_text(source.as_bytes()) {
            if ctx
                .shared_states
                .iter()
                .any(|s| s.name == name || s.name.ends_with(&format!(".{}", name)))
            {
                let access_type = determine_access_type(node, source);
                let state_name = ctx
                    .shared_states
                    .iter()
                    .find(|s| s.name == name || s.name.ends_with(&format!(".{}", name)))
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| name.to_string());

                let access = StateAccess {
                    state_name: state_name.clone(),
                    access_type,
                    line: node.start_position().row as u32 + 1,
                    column: Some(node.start_position().column as u32 + 1),
                    is_protected: false, // TypeScript doesn't have built-in locks
                    lock_name: None,
                    context: func_name
                        .map(|s| format!("{}({})", s, if new_in_async { "async" } else { "" })),
                };

                ctx.add_access(&state_name, access);
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        traverse_for_accesses(&child, source, ctx, func_name, new_in_async);
    }
}

/// Check if a function has async modifier
fn has_async_modifier(node: &Node, source: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "async" {
            return true;
        }
        if let Ok(text) = child.utf8_text(source.as_bytes()) {
            if text == "async" {
                return true;
            }
        }
    }
    false
}

/// Determine access type from context
fn determine_access_type(node: &Node, source: &str) -> AccessType {
    if let Some(parent) = node.parent() {
        match parent.kind() {
            // Assignment expressions
            "assignment_expression" => {
                if let Some(left) = parent.child_by_field_name("left") {
                    if contains_node(&left, node) {
                        return AccessType::Write;
                    }
                }
            }
            // Augmented assignments (+=, -=, etc.)
            "augmented_assignment_expression" => {
                if let Some(left) = parent.child_by_field_name("left") {
                    if contains_node(&left, node) {
                        return AccessType::Compound;
                    }
                }
            }
            // Update expressions (++, --)
            "update_expression" => {
                return AccessType::Compound;
            }
            // Delete expression
            "unary_expression" => {
                if let Some(operator) = parent.child_by_field_name("operator") {
                    if operator.utf8_text(source.as_bytes()).ok() == Some("delete") {
                        return AccessType::Write;
                    }
                }
            }
            // Method calls that might mutate
            "call_expression" => {
                if let Some(func) = parent.child_by_field_name("function") {
                    if func.kind() == "member_expression" {
                        if let Some(prop) = func.child_by_field_name("property") {
                            if let Ok(method_name) = prop.utf8_text(source.as_bytes()) {
                                if is_mutating_method(method_name) {
                                    return AccessType::Write;
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    AccessType::Read
}

/// Check if a method is known to mutate
fn is_mutating_method(method: &str) -> bool {
    matches!(
        method,
        "push"
            | "pop"
            | "shift"
            | "unshift"
            | "splice"
            | "sort"
            | "reverse"
            | "set"
            | "delete"
            | "clear"
            | "add"
    )
}

/// Check if container node contains target node
fn contains_node(container: &Node, target: &Node) -> bool {
    container.start_byte() <= target.start_byte() && container.end_byte() >= target.end_byte()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ts(code: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
        parser.set_language(&language.into()).unwrap();
        parser.parse(code, None).unwrap()
    }

    #[test]
    fn test_detect_module_variable() {
        let code = r#"
let counter = 0;
const MAX_SIZE = 100;
var oldStyle = "value";
"#;
        let tree = parse_ts(code);
        let mut ctx = AnalysisContext::new();
        detect_shared_state(&tree, code, "test.ts", &mut ctx);

        // Should detect let and var, but not const
        assert!(ctx.shared_states.iter().any(|s| s.name == "counter"));
        assert!(ctx.shared_states.iter().any(|s| s.name == "oldStyle"));
        assert!(!ctx.shared_states.iter().any(|s| s.name == "MAX_SIZE"));
    }

    #[test]
    fn test_detect_class_static_property() {
        let code = r#"
class Cache {
    static items: Map<string, any> = new Map();
    static readonly MAX_SIZE = 100;
}
"#;
        let tree = parse_ts(code);
        let mut ctx = AnalysisContext::new();
        detect_shared_state(&tree, code, "test.ts", &mut ctx);

        // Should detect mutable static property
        assert!(ctx.shared_states.iter().any(|s| s.name == "Cache.items"));
    }

    #[test]
    fn test_access_type_detection() {
        let code = "counter = 5";
        let tree = parse_ts(code);
        let root = tree.root_node();

        // Find the identifier
        let mut cursor = root.walk();
        fn find_identifier(node: &Node, source: &str) -> Option<AccessType> {
            if node.kind() == "identifier" {
                if node.utf8_text(source.as_bytes()).ok() == Some("counter") {
                    return Some(determine_access_type(node, source));
                }
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(result) = find_identifier(&child, source) {
                    return Some(result);
                }
            }
            None
        }

        let access_type = find_identifier(&root, code);
        assert_eq!(access_type, Some(AccessType::Write));
    }
}
