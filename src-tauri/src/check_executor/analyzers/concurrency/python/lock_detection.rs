//! Lock detection for Python
//!
//! Detects threading.Lock, RLock, Semaphore, and asyncio.Lock usage.

use crate::check_executor::analyzers::concurrency::types::{AnalysisContext, LockInfo, LockType};
use tree_sitter::{Node, Tree};

/// Lock type patterns to detect (ordered by specificity - more specific first)
const LOCK_PATTERNS: &[(&str, LockType)] = &[
    // RLock patterns must come before Lock to match correctly
    ("threading.RLock", LockType::ThreadingRLock),
    ("RLock", LockType::ThreadingRLock),
    ("threading.Lock", LockType::ThreadingLock),
    ("Lock", LockType::ThreadingLock),
    ("threading.Semaphore", LockType::ThreadingSemaphore),
    ("Semaphore", LockType::ThreadingSemaphore),
    ("asyncio.Lock", LockType::AsyncioLock),
];

/// Detect lock definitions in a Python file
pub fn detect_locks(tree: &Tree, source: &str, ctx: &mut AnalysisContext) {
    let root = tree.root_node();
    traverse_for_locks(&root, source, ctx);
}

/// Recursively traverse AST to find lock definitions
fn traverse_for_locks(node: &Node, source: &str, ctx: &mut AnalysisContext) {
    let kind = node.kind();

    // Look for assignments that create locks
    if kind == "assignment" || kind == "annotated_assignment" {
        if let Some(name) = get_assignment_target_name(node, source) {
            if let Some(value) = node
                .child_by_field_name("right")
                .or_else(|| node.child_by_field_name("value"))
            {
                if let Some(lock_type) = detect_lock_type(&value, source) {
                    ctx.locks.push(LockInfo {
                        name: name.clone(),
                        lock_type,
                        definition_line: node.start_position().row as u32 + 1,
                        protects: infer_protected_states(&name, ctx),
                    });
                }
            }
        }
    }

    // Look for instance attribute locks in __init__
    if kind == "function_definition" {
        if let Some(name_node) = node.child_by_field_name("name") {
            if let Ok("__init__") = name_node.utf8_text(source.as_bytes()) {
                if let Some(body) = node.child_by_field_name("body") {
                    detect_init_locks(&body, source, ctx);
                }
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        traverse_for_locks(&child, source, ctx);
    }
}

/// Detect locks defined in __init__
fn detect_init_locks(body: &Node, source: &str, ctx: &mut AnalysisContext) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == "expression_statement" {
            if let Some(expr) = child.child(0) {
                if expr.kind() == "assignment" {
                    // Check for self._lock = threading.Lock() pattern
                    if let Some(left) = expr.child_by_field_name("left") {
                        if left.kind() == "attribute" {
                            if let Some(obj) = left.child_by_field_name("object") {
                                if let Ok("self") = obj.utf8_text(source.as_bytes()) {
                                    if let Some(attr) = left.child_by_field_name("attribute") {
                                        if let Ok(attr_name) = attr.utf8_text(source.as_bytes()) {
                                            if let Some(right) = expr.child_by_field_name("right") {
                                                if let Some(lock_type) =
                                                    detect_lock_type(&right, source)
                                                {
                                                    let lock_name = format!("self.{}", attr_name);
                                                    ctx.locks.push(LockInfo {
                                                        name: lock_name.clone(),
                                                        lock_type,
                                                        definition_line: child.start_position().row
                                                            as u32
                                                            + 1,
                                                        protects: infer_protected_states(
                                                            &lock_name, ctx,
                                                        ),
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
        }
    }
}

/// Get the target name from an assignment
fn get_assignment_target_name(node: &Node, source: &str) -> Option<String> {
    node.child_by_field_name("left")
        .and_then(|left| match left.kind() {
            "identifier" => left
                .utf8_text(source.as_bytes())
                .ok()
                .map(|s| s.to_string()),
            "attribute" => {
                // Handle self.lock or cls.lock
                let obj = left.child_by_field_name("object")?;
                let attr = left.child_by_field_name("attribute")?;
                let obj_text = obj.utf8_text(source.as_bytes()).ok()?;
                let attr_text = attr.utf8_text(source.as_bytes()).ok()?;
                Some(format!("{}.{}", obj_text, attr_text))
            }
            _ => None,
        })
}

/// Detect if a value expression creates a lock
fn detect_lock_type(node: &Node, source: &str) -> Option<LockType> {
    if node.kind() == "call" {
        if let Some(func) = node.child_by_field_name("function") {
            let func_text = func.utf8_text(source.as_bytes()).ok()?;

            for (pattern, lock_type) in LOCK_PATTERNS {
                if func_text == *pattern || func_text.ends_with(pattern) {
                    return Some(lock_type.clone());
                }
            }

            // Check for lock in the function name
            let func_lower = func_text.to_lowercase();
            if func_lower.contains("lock") || func_lower.contains("mutex") {
                return Some(LockType::Other(func_text.to_string()));
            }
        }
    }
    None
}

/// Infer what states a lock might protect based on naming conventions
fn infer_protected_states(lock_name: &str, ctx: &AnalysisContext) -> Vec<String> {
    let mut protected = Vec::new();

    // Extract the base name from the lock
    let base_name = lock_name
        .strip_prefix("self.")
        .unwrap_or(lock_name)
        .strip_suffix("_lock")
        .or_else(|| lock_name.strip_suffix("Lock"))
        .or_else(|| lock_name.strip_suffix("_mutex"))
        .unwrap_or(lock_name);

    // Look for states with matching names
    for state in &ctx.shared_states {
        let state_base = state.name.strip_prefix("self.").unwrap_or(&state.name);

        if state_base == base_name
            || state_base.starts_with(&format!("{}_", base_name))
            || state_base.ends_with(&format!("_{}", base_name))
        {
            protected.push(state.name.clone());
        }
    }

    protected
}

/// Check if a name appears to be a lock based on naming convention
pub fn is_lock_name(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    name_lower.contains("lock")
        || name_lower.contains("mutex")
        || name_lower.contains("semaphore")
        || name_lower.ends_with("_lk")
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
    fn test_detect_threading_lock() {
        let code = r#"
import threading
lock = threading.Lock()
"#;
        let tree = parse_python(code);
        let mut ctx = AnalysisContext::new();
        detect_locks(&tree, code, &mut ctx);

        assert_eq!(ctx.locks.len(), 1);
        assert_eq!(ctx.locks[0].name, "lock");
        assert_eq!(ctx.locks[0].lock_type, LockType::ThreadingLock);
    }

    #[test]
    fn test_detect_rlock() {
        let code = r#"
from threading import RLock
my_rlock = RLock()
"#;
        let tree = parse_python(code);
        let mut ctx = AnalysisContext::new();
        detect_locks(&tree, code, &mut ctx);

        assert_eq!(ctx.locks.len(), 1);
        assert_eq!(ctx.locks[0].lock_type, LockType::ThreadingRLock);
    }

    #[test]
    fn test_detect_init_lock() {
        let code = r#"
import threading

class SafeClass:
    def __init__(self):
        self._lock = threading.Lock()
        self._data = []
"#;
        let tree = parse_python(code);
        let mut ctx = AnalysisContext::new();
        detect_locks(&tree, code, &mut ctx);

        // Should find the lock in __init__
        assert!(!ctx.locks.is_empty());
        assert!(ctx.locks.iter().any(|l| l.name == "self._lock"));
    }

    #[test]
    fn test_is_lock_name() {
        assert!(is_lock_name("data_lock"));
        assert!(is_lock_name("cacheMutex"));
        assert!(is_lock_name("_lock"));
        assert!(is_lock_name("semaphore_pool"));
        assert!(!is_lock_name("data"));
        assert!(!is_lock_name("counter"));
    }
}
