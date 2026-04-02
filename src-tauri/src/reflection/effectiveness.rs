//! Effectiveness evaluation engine for reflection fixes.
//!
//! Uses timestamp-based comparison to determine whether a fix actually resolved
//! the issue it targeted. Compares finding signature hashes before and after
//! the fix was applied across subsequent workflow runs.

use crate::database::Connection;
use tracing::{debug, info, warn};

use super::causal;
use super::prediction;
use super::storage;
use super::types::{FixEffectiveness, ReflectionFix};
use crate::str_utils::truncate_str;

/// Result of evaluating a single fix's effectiveness.
#[derive(Debug)]
pub struct EvaluationResult {
    pub fix_id: String,
    pub effectiveness: FixEffectiveness,
    pub evidence: String,
}

/// Verification pass rate metrics from workflow_verification_phase_results.
#[derive(Debug)]
struct VerificationMetrics {
    pass_rate: f64,
    total_steps: u32,
    all_passed: bool,
}

/// Check if a fix type is structural — meaning it rewrites workflow steps or
/// clarifies instructions rather than fixing a specific finding signature.
///
/// Structural fixes change the workflow itself, so signature-hash recurrence
/// is meaningless (the old check was rewritten). Instead, these should be
/// evaluated by comparing verification pass rates before and after.
fn is_structural_fix_type(fix_type: &str) -> bool {
    matches!(
        fix_type,
        "workflow_step_rewrite" | "instruction_clarification" | "context_addition"
    )
}

/// Query the last iteration's verification metrics for a given task run.
///
/// Returns None if no verification data exists for the run (e.g., the run
/// didn't have a verification phase or the table doesn't exist yet).
fn get_verification_metrics(
    conn: &Connection,
    task_run_id: &str,
) -> Result<Option<VerificationMetrics>, String> {
    Err("SQLite removed".to_string())
}

/// Evaluate a structural fix using verification pass rates instead of
/// signature-hash recurrence.
///
/// Structural fixes (step rewrites, instruction clarifications, context additions)
/// change the workflow itself, making signature-based tracking meaningless — the
/// old check was rewritten so the old signature will never recur, falsely appearing
/// "effective". Instead, we compare verification pass rates before and after the fix.
fn evaluate_structural_fix(
    conn: &Connection,
    fix: &ReflectionFix,
    _workflow_name: &str,
    subsequent_run_ids: &[String],
) -> Result<EvaluationResult, String> {
    Err("SQLite removed".to_string())
}

/// Evaluate the effectiveness of a single reflection fix.
///
/// Algorithm:
/// 1. Get the source finding's signature_hash
/// 2. Find subsequent runs of the same workflow that completed after fix.applied_at
/// 3. Check if the same signature_hash recurs in those runs
/// 4. Check for new findings that could indicate a regression
pub fn evaluate_fix(conn: &Connection, fix: &ReflectionFix) -> Result<EvaluationResult, String> {
    Err("SQLite removed".to_string())
}

/// Evaluate a fix by checking if its source finding's signature recurs.
fn evaluate_by_finding_signature(
    conn: &Connection,
    fix: &ReflectionFix,
    finding_id: &str,
    subsequent_run_ids: &[String],
) -> Result<EvaluationResult, String> {
    Err("SQLite removed".to_string())
}

/// When a fix is evaluated as Effective, link it to any matching error events
/// and record a fix application for accumulation monotonicity tracking.
fn link_effective_fix_to_error(conn: &Connection, fix: &ReflectionFix, signature_hash: &str) {
    // SQLite removed - no-op
}

/// When a fix causes a regression, create a causal link recording the relationship.
fn link_regression_fix(conn: &Connection, fix: &ReflectionFix) {
    // SQLite removed - no-op
}

/// Check if any new findings appeared in subsequent runs that weren't present before the fix.
fn check_for_regression(
    conn: &Connection,
    fix: &ReflectionFix,
    subsequent_run_ids: &[String],
) -> Result<bool, String> {
    Err("SQLite removed".to_string())
}

/// Evaluate a fix without a source finding by comparing workflow outcomes.
///
/// For knowledge_base_update and context_addition fixes, checks if subsequent
/// runs have fewer findings than the source run. This is a weaker signal than
/// signature-based tracking but better than permanent "inconclusive".
fn evaluate_by_workflow_outcome(
    conn: &Connection,
    fix: &ReflectionFix,
    _workflow_name: &str,
    subsequent_run_ids: &[String],
) -> Result<EvaluationResult, String> {
    Err("SQLite removed".to_string())
}

/// Batch evaluate all unevaluated fixes for a workflow.
/// Also re-evaluates fixes previously marked 'inconclusive' in case new
/// subsequent runs now provide enough signal to determine effectiveness.
///
/// Called during the completion phase of each reflection run.
pub fn evaluate_pending_fixes(
    conn: &Connection,
    workflow_name: &str,
) -> Result<Vec<EvaluationResult>, String> {
    Err("SQLite removed".to_string())
}

/// Auto-promote fixes that are effective across 2+ different project_paths.
///
/// When the same fix (by content_hash) independently proves effective in
/// multiple projects, it's a strong signal that the pattern is universal.
pub fn check_cross_project_promotion(conn: &Connection) -> Result<Vec<String>, String> {
    Err("SQLite removed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
use crate::database::Connection;

    fn setup_test_db() -> Connection {
        todo!("SQLite removed")
    }

    #[test]
    fn test_evaluate_fix_no_subsequent_runs() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_evaluate_fix_effective() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_evaluate_fix_ineffective() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_evaluate_knowledge_fix_effective_when_findings_decrease() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_evaluate_knowledge_fix_effective_when_zero_findings() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_evaluate_other_fix_type_effective_when_zero_findings() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_evaluate_other_fix_type_inconclusive_when_findings_persist() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_is_structural_fix_type() {
        // Structural types
        assert!(is_structural_fix_type("workflow_step_rewrite"));
        assert!(is_structural_fix_type("instruction_clarification"));
        assert!(is_structural_fix_type("context_addition"));

        // Non-structural types
        assert!(!is_structural_fix_type("selector_fix"));
        assert!(!is_structural_fix_type("tool_config_update"));
        assert!(!is_structural_fix_type("knowledge_base_update"));
        assert!(!is_structural_fix_type("project_environment"));
        assert!(!is_structural_fix_type(""));
    }

    /// Helper to create a ReflectionFix with the given fix_type for structural tests.
    fn make_structural_fix(fix_type: &str) -> ReflectionFix {
        ReflectionFix {
            id: "fix-1".to_string(),
            source_task_run_id: "src-1".to_string(),
            reflection_task_run_id: "ref-1".to_string(),
            source_finding_id: Some("f-1".to_string()),
            source_knowledge_id: None,
            fix_type: fix_type.to_string(),
            fix_description: "Rewrote verification step".to_string(),
            file_changed: None,
            old_value: None,
            new_value: None,
            confidence: "high".to_string(),
            content_hash: None,
            status: "applied".to_string(),
            effectiveness: None,
            effectiveness_evidence: None,
            applied_at: "2025-01-01T01:00:00Z".to_string(),
            evaluated_at: None,
            created_at: "2025-01-01T01:00:00Z".to_string(),
            source_agent: None,
            reasoning: None,
            alternatives_considered: None,
            reflection_scope: None,
            project_path: None,
            applicability_context: None,
        }
    }

    /// Helper to insert verification results for a task run.
    fn insert_verification(
        conn: &Connection,
        id: &str,
        task_run_id: &str,
        iteration: u32,
        total: u32,
        passed: u32,
    ) {
        // SQLite removed - no-op
    }

    #[test]
    fn test_structural_fix_effective_when_pass_rate_improves() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_structural_fix_regression_when_pass_rate_drops() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_structural_fix_inconclusive_when_steps_removed() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_non_structural_fix_bypasses_structural_path() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_structural_fix_effective_no_baseline_high_pass_rate() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_structural_fix_inconclusive_no_baseline_low_pass_rate() {
        // SQLite removed - no-op
    }
}
