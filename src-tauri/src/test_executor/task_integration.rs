//! Task Integration for Verification Tests
//!
//! Formerly provided SQLite-backed helpers for executing verification tests
//! at trigger points and writing results/findings. The persistence path was
//! not ported to PG, so the public entry points here are now no-op shims
//! that return empty results — the callers (tiered_info event handler)
//! swallow the empty summaries silently.

use crate::database::TriggerPoint;
use crate::test_executor::TestExecutionResult;

/// Result of executing tests for a trigger point
#[derive(Debug, Clone, Default)]
pub struct TriggerTestsResult {
    /// Number of tests executed
    pub total: usize,
    /// Number of tests passed
    pub passed: usize,
    /// Number of tests failed
    pub failed: usize,
    /// Whether any critical test failed
    pub critical_failure: bool,
    /// Formatted output for AI context
    pub ai_context: String,
    /// Individual test results
    pub results: Vec<TestExecutionResult>,
}

/// Execute verification tests for a specific trigger point.
///
/// No-op shim: the SQLite-backed test association lookup was removed and
/// has no PG replacement yet. Returns an empty result.
pub fn execute_tests_for_trigger(
    _config_id: &str,
    _trigger: &TriggerPoint,
    _task_run_id: Option<&str>,
) -> TriggerTestsResult {
    TriggerTestsResult::default()
}

/// Create findings for failed critical tests.
///
/// No-op shim: paired with `execute_tests_for_trigger`, this never receives
/// any results to record.
pub fn create_findings_for_failures(
    _task_run_id: &str,
    _session_num: i32,
    _tests_result: &TriggerTestsResult,
    _config_id: &str,
) {
}

/// Format test results as AI context string
pub fn format_results_for_ai(results: &TriggerTestsResult) -> String {
    if results.total == 0 {
        return String::new();
    }

    results.ai_context.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_result() {
        let result = TriggerTestsResult::default();
        assert_eq!(result.total, 0);
        assert_eq!(result.passed, 0);
        assert_eq!(result.failed, 0);
        assert!(!result.critical_failure);
    }
}
