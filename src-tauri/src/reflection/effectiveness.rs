//! Effectiveness evaluation engine for reflection fixes.
//!
//! Uses timestamp-based comparison to determine whether a fix actually resolved
//! the issue it targeted. Compares finding signature hashes before and after
//! the fix was applied across subsequent workflow runs.

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
fn get_verification_metrics(task_run_id: &str) -> Result<Option<VerificationMetrics>, String> {
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
pub fn evaluate_fix(fix: &ReflectionFix) -> Result<EvaluationResult, String> {
    Err("SQLite removed".to_string())
}

/// Evaluate a fix by checking if its source finding's signature recurs.
fn evaluate_by_finding_signature(
    fix: &ReflectionFix,
    finding_id: &str,
    subsequent_run_ids: &[String],
) -> Result<EvaluationResult, String> {
    Err("SQLite removed".to_string())
}

/// When a fix is evaluated as Effective, link it to any matching error events
/// and record a fix application for accumulation monotonicity tracking.
fn link_effective_fix_to_error(fix: &ReflectionFix, signature_hash: &str) {
    // SQLite removed - no-op
}

/// When a fix causes a regression, create a causal link recording the relationship.
fn link_regression_fix(fix: &ReflectionFix) {
    // SQLite removed - no-op
}

/// Check if any new findings appeared in subsequent runs that weren't present before the fix.
fn check_for_regression(
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
pub fn evaluate_pending_fixes(workflow_name: &str) -> Result<Vec<EvaluationResult>, String> {
    Err("SQLite removed".to_string())
}

/// Auto-promote fixes that are effective across 2+ different project_paths.
///
/// When the same fix (by content_hash) independently proves effective in
/// multiple projects, it's a strong signal that the pattern is universal.
pub fn check_cross_project_promotion() -> Result<Vec<String>, String> {
    Err("SQLite removed".to_string())
}

