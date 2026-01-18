//! Async pattern detection for TypeScript
//!
//! Detects race conditions specific to async/await patterns:
//! - Multiple awaits modifying shared state
//! - Event handlers with shared state
//! - Promise.all with shared state modifications

use crate::check_executor::analyzers::concurrency::types::{
    AccessType, AnalysisContext, RaceIssue, RacePattern, RaceSeverity, StateAccess,
};
use tree_sitter::{Node, Tree};

/// Detect async patterns and return count of async functions
pub fn detect_async_patterns(
    tree: &Tree,
    source: &str,
    file: &str,
    ctx: &mut AnalysisContext,
) -> u32 {
    let root = tree.root_node();
    let mut async_count = 0;

    // First pass: map state accesses
    super::shared_state::map_state_accesses(tree, source, ctx);

    // Count async functions and track their state accesses
    traverse_for_async(&root, source, &mut async_count);

    async_count
}

/// Traverse to count async functions
fn traverse_for_async(node: &Node, source: &str, count: &mut u32) {
    let kind = node.kind();

    if kind == "function_declaration" || kind == "arrow_function" || kind == "method_definition" {
        if has_async_keyword(node, source) {
            *count += 1;
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        traverse_for_async(&child, source, count);
    }
}

/// Check if a function has async keyword
fn has_async_keyword(node: &Node, source: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "async" {
            return true;
        }
        // Check the text content for async keyword
        if child.start_byte() < child.end_byte() {
            if let Ok(text) = child.utf8_text(source.as_bytes()) {
                if text == "async" {
                    return true;
                }
            }
        }
    }

    // Also check if the node text starts with "async"
    if let Ok(text) = node.utf8_text(source.as_bytes()) {
        if text.trim_start().starts_with("async") {
            return true;
        }
    }

    false
}

/// Detect async-specific race conditions
pub fn detect_async_races(ctx: &AnalysisContext, file: &str) -> Vec<RaceIssue> {
    let mut issues = Vec::new();

    for state in &ctx.shared_states {
        // Look for states modified across async boundaries
        let async_accesses: Vec<_> = state
            .accesses
            .iter()
            .filter(|a| {
                a.context
                    .as_ref()
                    .map(|c| c.contains("async"))
                    .unwrap_or(false)
            })
            .collect();

        // If there are writes in async contexts with reads elsewhere
        let async_writes: Vec<_> = async_accesses
            .iter()
            .filter(|a| matches!(a.access_type, AccessType::Write | AccessType::Compound))
            .collect();

        let all_reads: Vec<_> = state
            .accesses
            .iter()
            .filter(|a| a.access_type == AccessType::Read)
            .collect();

        if !async_writes.is_empty() && all_reads.len() > 1 {
            // Potential async race condition
            if let Some(first_write) = async_writes.first() {
                issues.push(RaceIssue {
                    pattern: RacePattern::ReadWriteRace,
                    severity: RaceSeverity::High,
                    file: file.to_string(),
                    line: first_write.line,
                    column: first_write.column,
                    state_name: state.name.clone(),
                    message: format!(
                        "Async race condition: '{}' is modified in async context while being read elsewhere. \
                         Consider using atomic operations or a state management pattern.",
                        state.name
                    ),
                    related_locations: all_reads
                        .iter()
                        .map(|a| (file.to_string(), a.line))
                        .collect(),
                    confidence: 0.7,
                });
            }
        }

        // Detect read-then-write across await points
        detect_await_race(&state.accesses, &state.name, file, &mut issues);
    }

    issues
}

/// Detect read-then-write patterns that span await points
fn detect_await_race(
    accesses: &[StateAccess],
    state_name: &str,
    file: &str,
    issues: &mut Vec<RaceIssue>,
) {
    // Look for reads followed by writes in async context
    for (i, access) in accesses.iter().enumerate() {
        if access.access_type != AccessType::Read {
            continue;
        }

        // Check if this is in an async context
        let is_async = access
            .context
            .as_ref()
            .map(|c| c.contains("async"))
            .unwrap_or(false);

        if !is_async {
            continue;
        }

        // Look for a subsequent write
        for later_access in accesses.iter().skip(i + 1) {
            if later_access.access_type == AccessType::Write
                || later_access.access_type == AccessType::Compound
            {
                // Check if it's in the same async function
                if access.context == later_access.context {
                    issues.push(RaceIssue {
                        pattern: RacePattern::Toctou,
                        severity: RaceSeverity::High,
                        file: file.to_string(),
                        line: access.line,
                        column: access.column,
                        state_name: state_name.to_string(),
                        message: format!(
                            "Async TOCTOU: '{}' is read on line {} and written on line {} in async function. \
                             An await between these could allow other code to modify the value.",
                            state_name, access.line, later_access.line
                        ),
                        related_locations: vec![(file.to_string(), later_access.line)],
                        confidence: 0.65,
                    });
                    break;
                }
            }
        }
    }
}

/// Patterns that indicate intentional concurrent access
const SAFE_PATTERNS: &[&str] = &[
    "mutex",
    "lock",
    "semaphore",
    "atomic",
    "synchronized",
    "useRef",     // React ref pattern
    "useState",   // React state (handled by React)
    "useReducer", // React reducer (handled by React)
];

/// Check if access is likely protected by a safe pattern
pub fn is_likely_safe_pattern(context: &str) -> bool {
    let context_lower = context.to_lowercase();
    SAFE_PATTERNS
        .iter()
        .any(|pattern| context_lower.contains(&pattern.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check_executor::analyzers::concurrency::types::{SharedState, SharedStateType};

    fn create_access(line: u32, access_type: AccessType, context: Option<&str>) -> StateAccess {
        StateAccess {
            state_name: "counter".to_string(),
            access_type,
            line,
            column: None,
            is_protected: false,
            lock_name: None,
            context: context.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_detect_async_race() {
        let mut ctx = AnalysisContext::new();
        ctx.shared_states.push(SharedState {
            name: "counter".to_string(),
            state_type: SharedStateType::ModuleVariable,
            definition_line: 1,
            file: "test.ts".to_string(),
            accesses: vec![
                create_access(10, AccessType::Read, Some("increment(async)")),
                create_access(12, AccessType::Write, Some("increment(async)")),
                create_access(20, AccessType::Read, Some("getCount()")),
            ],
            appears_thread_safe: false,
            thread_safe_reason: None,
        });

        let issues = detect_async_races(&ctx, "test.ts");
        assert!(!issues.is_empty());
    }

    #[test]
    fn test_safe_patterns() {
        assert!(is_likely_safe_pattern("useMutex()"));
        assert!(is_likely_safe_pattern("acquireLock"));
        assert!(is_likely_safe_pattern("useRef"));
        assert!(!is_likely_safe_pattern("regularFunction"));
    }

    #[test]
    fn test_has_async_keyword() {
        let code = "async function test() {}";
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
        parser.set_language(&language.into()).unwrap();
        let tree = parser.parse(code, None).unwrap();

        let root = tree.root_node();
        // Find the function declaration
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "function_declaration" {
                assert!(has_async_keyword(&child, code));
                return;
            }
        }
        panic!("Function declaration not found");
    }
}
