//! Heuristics for reducing false positives in race condition detection
//!
//! Contains rules for identifying cases that look like races but are likely safe.

use super::types::{RaceIssue, RaceSeverity, SharedState, SharedStateType};
use std::collections::HashSet;

/// Names that indicate thread-local storage
const THREAD_LOCAL_INDICATORS: &[&str] = &[
    "thread_local",
    "threadlocal",
    "local_",
    "_local",
    "tls_",
    "_tls",
    "per_thread",
];

/// Names that indicate constants (typically uppercase)
fn is_constant_name(name: &str) -> bool {
    // All uppercase with underscores
    name.chars()
        .all(|c| c.is_uppercase() || c == '_' || c.is_numeric())
        && name.chars().any(|c| c.is_alphabetic())
}

/// Names that indicate immutable/read-only state
const IMMUTABLE_INDICATORS: &[&str] = &[
    "readonly",
    "read_only",
    "immutable",
    "frozen",
    "const_",
    "_const",
];

/// Thread-safe type names
const THREAD_SAFE_TYPES: &[&str] = &[
    // Python
    "queue",
    "Queue",
    "SimpleQueue",
    "PriorityQueue",
    "LifoQueue",
    "deque", // technically not thread-safe but often used safely
    "AtomicInteger",
    "Counter", // collections.Counter is not thread-safe but often used read-only
    // Rust
    "Atomic",
    "AtomicBool",
    "AtomicI8",
    "AtomicI16",
    "AtomicI32",
    "AtomicI64",
    "AtomicU8",
    "AtomicU16",
    "AtomicU32",
    "AtomicU64",
    "AtomicUsize",
    "AtomicIsize",
    "AtomicPtr",
    "Sender",
    "Receiver",
    "channel",
    // TypeScript
    "Atomics",
    "SharedArrayBuffer",
];

/// Check if a state name indicates thread-local storage
pub fn is_thread_local(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    THREAD_LOCAL_INDICATORS
        .iter()
        .any(|indicator| name_lower.contains(indicator))
}

/// Check if a state appears to be thread-safe based on type
pub fn is_thread_safe_type(type_hint: &str) -> bool {
    THREAD_SAFE_TYPES
        .iter()
        .any(|safe_type| type_hint.contains(safe_type))
}

/// Check if a state appears to be a constant
pub fn is_constant(state: &SharedState) -> bool {
    is_constant_name(&state.name)
        || IMMUTABLE_INDICATORS
            .iter()
            .any(|ind| state.name.to_lowercase().contains(ind))
}

/// Check if a state is only accessed in initialization context
pub fn is_init_only(state: &SharedState) -> bool {
    // Check if all accesses are in __init__, constructor, or similar
    let init_contexts = ["__init__", "new", "init", "setup", "configure"];

    state.accesses.iter().all(|access| {
        access.context.as_ref().map_or(false, |ctx| {
            init_contexts.iter().any(|init| ctx.contains(init))
        })
    })
}

/// Check if the file appears to be a test file
pub fn is_test_file(file_path: &str) -> bool {
    let path_lower = file_path.to_lowercase();
    path_lower.contains("test")
        || path_lower.contains("spec")
        || path_lower.ends_with("_test.py")
        || path_lower.ends_with("_test.rs")
        || path_lower.ends_with(".test.ts")
        || path_lower.ends_with(".spec.ts")
        || path_lower.ends_with("_test.go")
}

/// Check if a state has an associated lock based on naming convention
pub fn has_matching_lock(state_name: &str, lock_names: &HashSet<String>) -> bool {
    let patterns = [
        format!("{}_lock", state_name),
        format!("{}_mutex", state_name),
        format!("{}Lock", state_name),
        format!("{}Mutex", state_name),
        format!("lock_{}", state_name),
        format!("mutex_{}", state_name),
    ];

    patterns.iter().any(|p| lock_names.contains(p))
}

/// Calculate adjusted severity based on heuristics
pub fn adjust_severity(issue: &mut RaceIssue, state: &SharedState, is_test: bool) {
    let mut confidence_adjustment = 0.0;

    // Lower confidence for test files
    if is_test {
        confidence_adjustment -= 0.3;
    }

    // Lower confidence for states that appear to be constants
    if is_constant(state) {
        confidence_adjustment -= 0.4;
    }

    // Lower confidence for thread-local indicators
    if is_thread_local(&state.name) {
        confidence_adjustment -= 0.5;
    }

    // Lower confidence for init-only accesses
    if is_init_only(state) {
        confidence_adjustment -= 0.4;
    }

    // Apply adjustment
    issue.confidence = (issue.confidence + confidence_adjustment).clamp(0.0, 1.0);

    // Downgrade severity for low confidence issues
    if issue.confidence < 0.3 {
        issue.severity = RaceSeverity::Low;
    } else if issue.confidence < 0.5 && issue.severity == RaceSeverity::Critical {
        issue.severity = RaceSeverity::High;
    }
}

/// Filter out likely false positives from a list of issues
pub fn filter_false_positives(
    issues: Vec<RaceIssue>,
    states: &[SharedState],
    lock_names: &HashSet<String>,
) -> Vec<RaceIssue> {
    issues
        .into_iter()
        .filter(|issue| {
            // Find the corresponding state
            let state = states.iter().find(|s| s.name == issue.state_name);

            // Always keep if we can't find the state (shouldn't happen)
            let Some(state) = state else {
                return true;
            };

            // Filter out thread-local storage
            if is_thread_local(&state.name) {
                return false;
            }

            // Filter out constants
            if is_constant(state) {
                return false;
            }

            // Filter out states with matching locks (likely properly protected)
            if has_matching_lock(&state.name, lock_names) {
                return false;
            }

            // Filter out states marked as thread-safe
            if state.appears_thread_safe {
                return false;
            }

            // Keep the issue
            true
        })
        .collect()
}

/// Determine if a state type is inherently thread-safe
pub fn is_inherently_thread_safe(state_type: &SharedStateType) -> bool {
    matches!(
        state_type,
        SharedStateType::OnceCell | SharedStateType::LazyStatic
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_constant_name() {
        assert!(is_constant_name("MAX_SIZE"));
        assert!(is_constant_name("DEFAULT_VALUE"));
        assert!(is_constant_name("API_KEY"));
        assert!(!is_constant_name("myVariable"));
        assert!(!is_constant_name("MyClass"));
        assert!(!is_constant_name("some_function"));
    }

    #[test]
    fn test_is_thread_local() {
        assert!(is_thread_local("thread_local_cache"));
        assert!(is_thread_local("_local_data"));
        assert!(is_thread_local("tls_context"));
        assert!(!is_thread_local("global_cache"));
        assert!(!is_thread_local("shared_data"));
    }

    #[test]
    fn test_is_test_file() {
        assert!(is_test_file("test_something.py"));
        assert!(is_test_file("something_test.py"));
        assert!(is_test_file("something.test.ts"));
        assert!(is_test_file("something.spec.ts"));
        assert!(is_test_file("/tests/test_module.py"));
        assert!(!is_test_file("main.py"));
        assert!(!is_test_file("server.ts"));
    }

    #[test]
    fn test_has_matching_lock() {
        let mut locks = HashSet::new();
        locks.insert("data_lock".to_string());
        locks.insert("cacheMutex".to_string());

        assert!(has_matching_lock("data", &locks));
        assert!(has_matching_lock("cache", &locks));
        assert!(!has_matching_lock("other", &locks));
    }
}
