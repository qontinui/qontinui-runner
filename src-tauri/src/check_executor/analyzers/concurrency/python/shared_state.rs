//! Shared state detection for Python
//!
//! Detects class variables, module globals, and closure captures that could be
//! shared across threads.

use crate::check_executor::analyzers::concurrency::types::{
    AccessType, AnalysisContext, SharedState, SharedStateType, StateAccess,
};
use tree_sitter::{Node, Tree};

/// Detect shared state in a Python file
pub fn detect_shared_state(tree: &Tree, source: &str, file: &str, ctx: &mut AnalysisContext) {
    let root = tree.root_node();
    let _cursor = root.walk();

    // Detect module-level globals
    detect_module_globals(&root, source, file, ctx);

    // Detect class variables
    detect_class_variables(&root, source, file, ctx);
}

/// Detect module-level global variables
fn detect_module_globals(root: &Node, source: &str, file: &str, ctx: &mut AnalysisContext) {
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        // Look for top-level assignments
        if child.kind() == "expression_statement" {
            if let Some(expr) = child.child(0) {
                if expr.kind() == "assignment" {
                    if let Some(target) = expr.child_by_field_name("left") {
                        if let Some(name) = extract_identifier_name(&target, source) {
                            // Skip private/dunder names that are less likely to be shared
                            if !name.starts_with("__") || name.ends_with("__") {
                                ctx.shared_states.push(SharedState {
                                    name: name.clone(),
                                    state_type: SharedStateType::ModuleGlobal,
                                    definition_line: child.start_position().row as u32 + 1,
                                    file: file.to_string(),
                                    accesses: Vec::new(),
                                    appears_thread_safe: is_likely_thread_safe(
                                        &name, &expr, source,
                                    ),
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

/// Detect class-level (shared) variables
fn detect_class_variables(root: &Node, source: &str, file: &str, ctx: &mut AnalysisContext) {
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if child.kind() == "class_definition" {
            let class_name = child
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .unwrap_or("unknown");

            // Find the class body
            if let Some(body) = child.child_by_field_name("body") {
                let mut body_cursor = body.walk();
                for body_child in body.children(&mut body_cursor) {
                    // Look for assignments that are NOT inside methods
                    if body_child.kind() == "expression_statement" {
                        if let Some(expr) = body_child.child(0) {
                            if expr.kind() == "assignment" || expr.kind() == "annotated_assignment"
                            {
                                if let Some(target) = expr.child_by_field_name("left") {
                                    if let Some(name) = extract_identifier_name(&target, source) {
                                        let full_name = format!("{}.{}", class_name, name);
                                        ctx.shared_states.push(SharedState {
                                            name: full_name,
                                            state_type: SharedStateType::ClassVariable,
                                            definition_line: body_child.start_position().row as u32
                                                + 1,
                                            file: file.to_string(),
                                            accesses: Vec::new(),
                                            appears_thread_safe: is_likely_thread_safe(
                                                &name, &expr, source,
                                            ),
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
}

/// Extract identifier name from a node
fn extract_identifier_name(node: &Node, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" => node
            .utf8_text(source.as_bytes())
            .ok()
            .map(|s| s.to_string()),
        "pattern_list" | "tuple_pattern" => {
            // For tuple unpacking, get the first element
            node.child(0)
                .and_then(|c| extract_identifier_name(&c, source))
        }
        _ => None,
    }
}

/// Check if a variable appears to be thread-safe based on its initialization
fn is_likely_thread_safe(name: &str, expr: &Node, source: &str) -> bool {
    // Check for Queue, deque, and other thread-safe types
    let thread_safe_types = [
        "queue.Queue",
        "Queue",
        "queue.SimpleQueue",
        "SimpleQueue",
        "queue.LifoQueue",
        "queue.PriorityQueue",
        "collections.deque",
        "deque",
        "threading.local",
        "local",
    ];

    if let Some(right) = expr.child_by_field_name("right") {
        if let Ok(text) = right.utf8_text(source.as_bytes()) {
            for safe_type in thread_safe_types {
                if text.contains(safe_type) {
                    return true;
                }
            }
        }
    }

    // Check if name suggests thread-local
    let name_lower = name.to_lowercase();
    if name_lower.contains("local") || name_lower.contains("thread_local") {
        return true;
    }

    false
}

/// Map all accesses to shared state throughout the file
pub fn map_state_accesses(tree: &Tree, source: &str, ctx: &mut AnalysisContext) {
    let root = tree.root_node();
    traverse_for_accesses(&root, source, ctx, false, None);
}

/// Recursively traverse AST to find state accesses
fn traverse_for_accesses(
    node: &Node,
    source: &str,
    ctx: &mut AnalysisContext,
    in_lock_context: bool,
    current_function: Option<&str>,
) {
    let kind = node.kind();

    // Track with-statement for lock context
    let new_in_lock = if kind == "with_statement" {
        // Check if this is a lock context
        is_lock_with_statement(node, source, ctx)
    } else {
        in_lock_context
    };

    // Track function context
    let func_name = if kind == "function_definition" {
        node.child_by_field_name("name")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
    } else {
        current_function
    };

    // Check for identifier access
    if kind == "identifier" {
        if let Ok(name) = node.utf8_text(source.as_bytes()) {
            // Check if this is a known shared state
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
                    is_protected: new_in_lock,
                    lock_name: if new_in_lock {
                        ctx.current_lock.clone()
                    } else {
                        None
                    },
                    context: func_name.map(|s| s.to_string()),
                };

                ctx.add_access(&state_name, access);
            }
        }
    }

    // Check for attribute access (self.x, cls.x)
    if kind == "attribute" {
        if let Some(value) = node.child_by_field_name("object") {
            if let Some(attr) = node.child_by_field_name("attribute") {
                if let (Ok(obj), Ok(attr_name)) = (
                    value.utf8_text(source.as_bytes()),
                    attr.utf8_text(source.as_bytes()),
                ) {
                    // Check for class variable access (ClassName.var or self.__class__.var)
                    let full_name = format!("{}.{}", obj, attr_name);
                    if ctx.shared_states.iter().any(|s| s.name == full_name) {
                        let access_type = determine_access_type(node, source);
                        let access = StateAccess {
                            state_name: full_name.clone(),
                            access_type,
                            line: node.start_position().row as u32 + 1,
                            column: Some(node.start_position().column as u32 + 1),
                            is_protected: new_in_lock,
                            lock_name: if new_in_lock {
                                ctx.current_lock.clone()
                            } else {
                                None
                            },
                            context: func_name.map(|s| s.to_string()),
                        };
                        ctx.add_access(&full_name, access);
                    }
                }
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        traverse_for_accesses(&child, source, ctx, new_in_lock, func_name);
    }
}

/// Determine the type of access based on context
fn determine_access_type(node: &Node, source: &str) -> AccessType {
    // Check parent context
    if let Some(parent) = node.parent() {
        match parent.kind() {
            // Assignment target = Write
            "assignment" => {
                if let Some(left) = parent.child_by_field_name("left") {
                    if node_contains(left, node) {
                        return AccessType::Write;
                    }
                }
            }
            // Augmented assignment (+=, -=, etc.) = Compound
            "augmented_assignment" => {
                if let Some(left) = parent.child_by_field_name("left") {
                    if node_contains(left, node) {
                        return AccessType::Compound;
                    }
                }
            }
            // Delete statement = Write
            "delete_statement" => return AccessType::Write,
            // Function call with .append(), .extend(), etc. = Write
            "call" => {
                if let Some(func) = parent.child_by_field_name("function") {
                    if func.kind() == "attribute" {
                        if let Some(attr) = func.child_by_field_name("attribute") {
                            if let Ok(method_name) = attr.utf8_text(source.as_bytes()) {
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

/// Check if a method name indicates mutation
fn is_mutating_method(method: &str) -> bool {
    matches!(
        method,
        "append"
            | "extend"
            | "insert"
            | "remove"
            | "pop"
            | "clear"
            | "update"
            | "add"
            | "discard"
            | "setdefault"
            | "sort"
            | "reverse"
    )
}

/// Check if a with-statement is a lock context
fn is_lock_with_statement(node: &Node, source: &str, ctx: &mut AnalysisContext) -> bool {
    // Look for the with clause
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "with_clause" {
            let mut clause_cursor = child.walk();
            for clause_child in child.children(&mut clause_cursor) {
                if clause_child.kind() == "with_item" {
                    if let Some(value) = clause_child.child_by_field_name("value") {
                        if let Ok(text) = value.utf8_text(source.as_bytes()) {
                            // Check if it's a known lock
                            if ctx.locks.iter().any(|l| text.contains(&l.name)) {
                                return true;
                            }
                            // Check for lock-like patterns
                            if text.contains("lock")
                                || text.contains("Lock")
                                || text.contains("mutex")
                                || text.contains("Mutex")
                            {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

/// Check if a node contains another node
fn node_contains(container: Node, target: &Node) -> bool {
    container.start_byte() <= target.start_byte() && container.end_byte() >= target.end_byte()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_python(code: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_python::LANGUAGE;
        parser.set_language(&language.into()).unwrap();
        parser.parse(code, None).unwrap()
    }

    #[test]
    fn test_detect_module_global() {
        let code = r#"
counter = 0
cache = {}
"#;
        let tree = parse_python(code);
        let mut ctx = AnalysisContext::new();
        detect_shared_state(&tree, code, "test.py", &mut ctx);

        assert_eq!(ctx.shared_states.len(), 2);
        assert!(ctx.shared_states.iter().any(|s| s.name == "counter"));
        assert!(ctx.shared_states.iter().any(|s| s.name == "cache"));
    }

    #[test]
    fn test_detect_class_variable() {
        let code = r#"
class MyClass:
    shared_list = []
    counter = 0

    def __init__(self):
        self.instance_var = 1
"#;
        let tree = parse_python(code);
        let mut ctx = AnalysisContext::new();
        detect_shared_state(&tree, code, "test.py", &mut ctx);

        assert!(ctx
            .shared_states
            .iter()
            .any(|s| s.name == "MyClass.shared_list"));
        assert!(ctx
            .shared_states
            .iter()
            .any(|s| s.name == "MyClass.counter"));
    }

    #[test]
    fn test_thread_safe_detection() {
        let code = r#"
from queue import Queue
task_queue = Queue()
local_data = threading.local()
"#;
        let tree = parse_python(code);
        let mut ctx = AnalysisContext::new();
        detect_shared_state(&tree, code, "test.py", &mut ctx);

        // Thread-safe types should be marked
        for state in &ctx.shared_states {
            if state.name == "task_queue" || state.name == "local_data" {
                assert!(state.appears_thread_safe);
            }
        }
    }
}
