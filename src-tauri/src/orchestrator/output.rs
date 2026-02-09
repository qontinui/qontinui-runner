//! AI Output for the task orchestrator agents.
//!
//! This module provides functions to emit AI output from the orchestrator
//! so users can see what each agent is doing in real-time.
//!
//! Output is streamed to the frontend via the `ai-output` event.

#![allow(dead_code)]

use tracing::debug;

use super::types::{
    CriterionType, Finding, IterationVerificationResults, VerificationPlan, VerificationResult,
};
use crate::execution_context::AiSessionContext;
use crate::mcp::shared::emit_ai_output;

// ============================================================================
// Output Source Constants
// ============================================================================

/// Source identifier for Planning Agent output
pub const SOURCE_PLANNING: &str = "planning_agent";

/// Source identifier for Verification Agent output
pub const SOURCE_VERIFICATION: &str = "verification_agent";

/// Source identifier for Knowledge Base output
pub const SOURCE_KNOWLEDGE: &str = "knowledge_base";

/// Source identifier for Orchestrator system output
pub const SOURCE_ORCHESTRATOR: &str = "orchestrator";

// ============================================================================
// Planning Agent Output
// ============================================================================

/// Emit output when the Planning Agent starts analyzing a task.
pub fn emit_planning_start(
    app_handle: &tauri::AppHandle,
    goal: &str,
    session_ctx: Option<&AiSessionContext>,
) {
    emit_ai_output(
        app_handle,
        &format!("[Planning Agent] Analyzing task: {}", truncate(goal, 80)),
        SOURCE_PLANNING,
        None,
        session_ctx,
    );
    debug!("Planning Agent started for goal: {}", goal);
}

/// Emit output when the Planning Agent creates a verification plan.
pub fn emit_plan_created(
    app_handle: &tauri::AppHandle,
    plan: &VerificationPlan,
    session_ctx: Option<&AiSessionContext>,
) {
    let deterministic_count = plan.deterministic_criteria().len();
    let ai_count = plan.ai_criteria().len();

    emit_ai_output(
        app_handle,
        &format!(
            "[Planning Agent] Created verification plan with {} criteria ({} deterministic, {} AI-evaluated)",
            plan.success_criteria.len(),
            deterministic_count,
            ai_count
        ),
        SOURCE_PLANNING,
        None,
        session_ctx,
    );

    // Emit each criterion
    for criterion in &plan.success_criteria {
        let type_marker = match criterion.criterion_type {
            CriterionType::Deterministic => "DETERMINISTIC",
            CriterionType::AiEvaluated => "AI",
        };
        let critical_marker = if criterion.is_critical {
            "[CRITICAL]"
        } else {
            ""
        };

        emit_ai_output(
            app_handle,
            &format!(
                "  [{}] {} {}: {}",
                type_marker,
                critical_marker,
                criterion.id,
                truncate(&criterion.description, 60)
            ),
            SOURCE_PLANNING,
            None,
            session_ctx,
        );
    }

    debug!(
        "Plan created: {} total criteria ({} det, {} ai)",
        plan.success_criteria.len(),
        deterministic_count,
        ai_count
    );
}

/// Emit output when the Planning Agent is replanning.
pub fn emit_replanning(
    app_handle: &tauri::AppHandle,
    reason: &str,
    session_ctx: Option<&AiSessionContext>,
) {
    emit_ai_output(
        app_handle,
        &format!(
            "[Planning Agent] Replanning due to: {}",
            truncate(reason, 100)
        ),
        SOURCE_PLANNING,
        None,
        session_ctx,
    );
    debug!("Replanning triggered: {}", reason);
}

/// Emit output when the Planning Agent completes plan creation.
pub fn emit_planning_complete(
    app_handle: &tauri::AppHandle,
    plan_version: u32,
    session_ctx: Option<&AiSessionContext>,
) {
    emit_ai_output(
        app_handle,
        &format!(
            "[Planning Agent] Plan v{} ready for execution",
            plan_version
        ),
        SOURCE_PLANNING,
        None,
        session_ctx,
    );
}

// ============================================================================
// Verification Agent Output
// ============================================================================

/// Emit output when the Verification Agent starts running checks.
pub fn emit_verification_start(
    app_handle: &tauri::AppHandle,
    iteration: u32,
    deterministic_count: usize,
    ai_count: usize,
    session_ctx: Option<&AiSessionContext>,
) {
    emit_ai_output(
        app_handle,
        &format!(
            "[Verification Agent] Running checks (iteration {}, {} deterministic, {} AI)",
            iteration, deterministic_count, ai_count
        ),
        SOURCE_VERIFICATION,
        None,
        session_ctx,
    );
    debug!(
        "Verification started: iteration={}, det={}, ai={}",
        iteration, deterministic_count, ai_count
    );
}

/// Emit output for a deterministic verification result.
pub fn emit_deterministic_result(
    app_handle: &tauri::AppHandle,
    result: &VerificationResult,
    session_ctx: Option<&AiSessionContext>,
) {
    let status = if result.passed { "PASS" } else { "FAIL" };
    let icon = if result.passed { "[PASS]" } else { "[FAIL]" };

    let details = if result.passed {
        result
            .observations
            .first()
            .map(|o| truncate(o, 50))
            .unwrap_or_else(|| "OK".to_string())
    } else {
        result
            .issues
            .first()
            .map(|i| truncate(i, 50))
            .unwrap_or_else(|| "Failed".to_string())
    };

    emit_ai_output(
        app_handle,
        &format!("  {} {} - {}", icon, result.criterion_id, details),
        SOURCE_VERIFICATION,
        None,
        session_ctx,
    );
    debug!(
        "Deterministic verification {}: {} - {}",
        result.criterion_id, status, details
    );
}

/// Emit output when starting AI verification for a criterion.
pub fn emit_ai_verification_start(
    app_handle: &tauri::AppHandle,
    criterion_id: &str,
    evaluation_prompt: Option<&str>,
    session_ctx: Option<&AiSessionContext>,
) {
    let prompt_preview = evaluation_prompt
        .map(|p| format!(": {}", truncate(p, 50)))
        .unwrap_or_default();

    emit_ai_output(
        app_handle,
        &format!("  [AI] Verifying: {}{}", criterion_id, prompt_preview),
        SOURCE_VERIFICATION,
        None,
        session_ctx,
    );
    debug!("AI verification started for: {}", criterion_id);
}

/// Emit output for an AI verification result.
pub fn emit_ai_verification_result(
    app_handle: &tauri::AppHandle,
    result: &VerificationResult,
    session_ctx: Option<&AiSessionContext>,
) {
    let status = if result.passed { "PASS" } else { "FAIL" };
    let icon = if result.passed { "[PASS]" } else { "[FAIL]" };

    let confidence = result
        .confidence
        .map(|c| format!(" (confidence: {:?})", c))
        .unwrap_or_default();

    let details = if result.passed {
        result
            .observations
            .first()
            .map(|o| truncate(o, 50))
            .unwrap_or_else(|| "Verified".to_string())
    } else {
        result
            .issues
            .first()
            .map(|i| truncate(i, 50))
            .unwrap_or_else(|| "Failed".to_string())
    };

    emit_ai_output(
        app_handle,
        &format!(
            "  {} {} - {}{}",
            icon, result.criterion_id, details, confidence
        ),
        SOURCE_VERIFICATION,
        None,
        session_ctx,
    );
    debug!(
        "AI verification {}: {} - {}",
        result.criterion_id, status, details
    );
}

/// Emit output for the final verification summary.
pub fn emit_verification_complete(
    app_handle: &tauri::AppHandle,
    results: &IterationVerificationResults,
    session_ctx: Option<&AiSessionContext>,
) {
    let total_det = results.deterministic_results.len();
    let passed_det = results
        .deterministic_results
        .iter()
        .filter(|r| r.passed)
        .count();
    let total_ai = results.ai_results.len();
    let passed_ai = results.ai_results.iter().filter(|r| r.passed).count();

    if results.all_passed {
        emit_ai_output(
            app_handle,
            &format!(
                "[Verification Agent] All checks passed ({}/{} deterministic, {}/{} AI)",
                passed_det, total_det, passed_ai, total_ai
            ),
            SOURCE_VERIFICATION,
            None,
            session_ctx,
        );
    } else {
        let failed_det = total_det - passed_det;
        let failed_ai = total_ai - passed_ai;
        let total_failed = failed_det + failed_ai;

        emit_ai_output(
            app_handle,
            &format!(
                "[Verification Agent] {} check(s) failed ({} deterministic, {} AI)",
                total_failed, failed_det, failed_ai
            ),
            SOURCE_VERIFICATION,
            None,
            session_ctx,
        );

        // Show summary of failures
        if let Some(summary) = &results.failure_summary {
            for line in summary.lines().take(5) {
                if !line.trim().is_empty() {
                    emit_ai_output(
                        app_handle,
                        &format!("  {}", line.trim()),
                        SOURCE_VERIFICATION,
                        None,
                        session_ctx,
                    );
                }
            }
        }
    }

    debug!(
        "Verification complete: all_passed={}, det={}/{}, ai={}/{}",
        results.all_passed, passed_det, total_det, passed_ai, total_ai
    );
}

// ============================================================================
// Knowledge Base Output
// ============================================================================

/// Emit output when a finding is recorded in the knowledge base.
pub fn emit_finding_recorded(
    app_handle: &tauri::AppHandle,
    finding: &Finding,
    session_ctx: Option<&AiSessionContext>,
) {
    let confidence_str = format!("{:?}", finding.confidence).to_lowercase();

    emit_ai_output(
        app_handle,
        &format!(
            "[Knowledge Base] Finding recorded [{}] ({}): {}",
            finding.finding_type.to_uppercase(),
            confidence_str,
            truncate(&finding.description, 60)
        ),
        SOURCE_KNOWLEDGE,
        None,
        session_ctx,
    );

    // Show related files if any
    if !finding.related_files.is_empty() {
        let files_preview = if finding.related_files.len() <= 3 {
            finding.related_files.join(", ")
        } else {
            format!(
                "{}, ... (+{} more)",
                finding.related_files[..2].join(", "),
                finding.related_files.len() - 2
            )
        };
        emit_ai_output(
            app_handle,
            &format!("  Related files: {}", files_preview),
            SOURCE_KNOWLEDGE,
            None,
            session_ctx,
        );
    }

    debug!(
        "Finding recorded: type={}, confidence={:?}",
        finding.finding_type, finding.confidence
    );
}

/// Emit output when verification feedback is recorded.
pub fn emit_verification_feedback_recorded(
    app_handle: &tauri::AppHandle,
    iteration: u32,
    failed_criteria_count: usize,
    session_ctx: Option<&AiSessionContext>,
) {
    emit_ai_output(
        app_handle,
        &format!(
            "[Knowledge Base] Verification feedback recorded for iteration {} ({} failed criteria)",
            iteration, failed_criteria_count
        ),
        SOURCE_KNOWLEDGE,
        None,
        session_ctx,
    );
}

/// Emit output when injecting context from previous iterations.
pub fn emit_context_injection(
    app_handle: &tauri::AppHandle,
    findings_count: usize,
    feedback_count: usize,
    observations_count: usize,
    session_ctx: Option<&AiSessionContext>,
) {
    let total = findings_count + feedback_count + observations_count;
    if total == 0 {
        return;
    }

    emit_ai_output(
        app_handle,
        &format!(
            "[Knowledge Base] Injecting context from previous iterations ({} findings, {} feedback, {} observations)",
            findings_count,
            feedback_count,
            observations_count
        ),
        SOURCE_KNOWLEDGE,
        None,
        session_ctx,
    );
    debug!(
        "Context injected: {} findings, {} feedback, {} observations",
        findings_count, feedback_count, observations_count
    );
}

/// Emit output when a finding is resolved.
pub fn emit_finding_resolved(
    app_handle: &tauri::AppHandle,
    finding_id: &str,
    notes: Option<&str>,
    session_ctx: Option<&AiSessionContext>,
) {
    let notes_str = notes
        .map(|n| format!(": {}", truncate(n, 50)))
        .unwrap_or_default();

    emit_ai_output(
        app_handle,
        &format!(
            "[Knowledge Base] Finding resolved: {}{}",
            finding_id, notes_str
        ),
        SOURCE_KNOWLEDGE,
        None,
        session_ctx,
    );
}

// ============================================================================
// Orchestrator System Output
// ============================================================================

/// Emit output when the orchestrator starts a task.
pub fn emit_orchestrator_task_start(
    app_handle: &tauri::AppHandle,
    task_run_id: &str,
    goal: &str,
    session_ctx: Option<&AiSessionContext>,
) {
    emit_ai_output(
        app_handle,
        &format!("[Orchestrator] Starting task: {}", truncate(goal, 80)),
        SOURCE_ORCHESTRATOR,
        None,
        session_ctx,
    );
    emit_ai_output(
        app_handle,
        &format!("[Orchestrator] Task ID: {}", task_run_id),
        SOURCE_ORCHESTRATOR,
        None,
        session_ctx,
    );
}

/// Emit output when starting a new iteration.
pub fn emit_iteration_start(
    app_handle: &tauri::AppHandle,
    iteration: u32,
    max_iterations: u32,
    session_ctx: Option<&AiSessionContext>,
) {
    emit_ai_output(
        app_handle,
        &format!(
            "[Orchestrator] Starting iteration {}/{}",
            iteration, max_iterations
        ),
        SOURCE_ORCHESTRATOR,
        None,
        session_ctx,
    );
}

/// Emit output when the orchestrator completes a task.
pub fn emit_orchestrator_task_complete(
    app_handle: &tauri::AppHandle,
    success: bool,
    iterations: u32,
    reason: Option<&str>,
    session_ctx: Option<&AiSessionContext>,
) {
    if success {
        emit_ai_output(
            app_handle,
            &format!(
                "[Orchestrator] Task completed successfully after {} iteration(s)",
                iterations
            ),
            SOURCE_ORCHESTRATOR,
            None,
            session_ctx,
        );
    } else {
        let reason_str = reason.unwrap_or("Unknown reason");
        emit_ai_output(
            app_handle,
            &format!(
                "[Orchestrator] Task failed after {} iteration(s): {}",
                iterations,
                truncate(reason_str, 80)
            ),
            SOURCE_ORCHESTRATOR,
            None,
            session_ctx,
        );
    }
}

/// Emit output for a worker signal.
pub fn emit_worker_signal(
    app_handle: &tauri::AppHandle,
    signal_type: &str,
    reason: Option<&str>,
    session_ctx: Option<&AiSessionContext>,
) {
    let reason_str = reason
        .map(|r| format!(": {}", truncate(r, 60)))
        .unwrap_or_default();

    emit_ai_output(
        app_handle,
        &format!(
            "[Orchestrator] Worker signal: {}{}",
            signal_type, reason_str
        ),
        SOURCE_ORCHESTRATOR,
        None,
        session_ctx,
    );
}

// ============================================================================
// Criterion Override Output
// ============================================================================

/// Emit output when a criterion override is detected from worker output.
pub fn emit_override_detected(
    app_handle: &tauri::AppHandle,
    criterion_id: &str,
    item: &str,
    session_ctx: Option<&AiSessionContext>,
) {
    emit_ai_output(
        app_handle,
        &format!(
            "[Knowledge Base] Override detected for criterion '{}': {}",
            criterion_id,
            truncate(item, 50)
        ),
        SOURCE_KNOWLEDGE,
        None,
        session_ctx,
    );
    debug!(
        "Override detected: criterion={}, item={}",
        criterion_id, item
    );
}

/// Emit output when an override is applied during verification.
pub fn emit_override_applied(
    app_handle: &tauri::AppHandle,
    criterion_id: &str,
    item: &str,
    justification: &str,
    session_ctx: Option<&AiSessionContext>,
) {
    emit_ai_output(
        app_handle,
        &format!(
            "[Verification Agent] Override applied for '{}': {} - {}",
            criterion_id,
            truncate(item, 30),
            truncate(justification, 50)
        ),
        SOURCE_VERIFICATION,
        None,
        session_ctx,
    );
    debug!(
        "Override applied: criterion={}, item={}, justification={}",
        criterion_id, item, justification
    );
}

/// Emit summary of overrides applied in an iteration.
pub fn emit_overrides_summary(
    app_handle: &tauri::AppHandle,
    override_count: usize,
    criteria_ids: &[String],
    session_ctx: Option<&AiSessionContext>,
) {
    if override_count == 0 {
        return;
    }

    let criteria_list = if criteria_ids.len() <= 3 {
        criteria_ids.join(", ")
    } else {
        format!(
            "{}, ... (+{} more)",
            criteria_ids[..2].join(", "),
            criteria_ids.len() - 2
        )
    };

    emit_ai_output(
        app_handle,
        &format!(
            "[Verification Agent] {} criterion override(s) applied: {}",
            override_count, criteria_list
        ),
        SOURCE_VERIFICATION,
        None,
        session_ctx,
    );
}

// ============================================================================
// Phase Transition Events
// ============================================================================

/// Emit a phase transition event (setup -> verification -> completion).
///
/// This is emitted when the orchestrator transitions between phases:
/// - "setup" -> "verification": Setup steps complete, entering verification loop
/// - "verification" -> "completion": Verification loop done, running completion steps
/// - "completion" -> "complete": All phases done
pub fn emit_phase_transition(
    app_handle: &tauri::AppHandle,
    from_phase: &str,
    to_phase: &str,
    iteration: u32,
    session_ctx: Option<&AiSessionContext>,
) {
    let message = match (from_phase, to_phase) {
        ("setup", "verification") => {
            "Setup phase complete. Transitioning to verification phase.".to_string()
        }
        ("verification", "completion") => {
            format!(
                "Verification loop finished at iteration {}. Running completion steps.",
                iteration
            )
        }
        ("completion", "complete") => "Completion phase done. Task finished.".to_string(),
        _ => format!(
            "Phase transition: {} -> {} (iteration {})",
            from_phase, to_phase, iteration
        ),
    };

    emit_ai_output(
        app_handle,
        &format!("[Orchestrator] {}", message),
        SOURCE_ORCHESTRATOR,
        None,
        session_ctx,
    );
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Truncate a string to a maximum length, adding "..." if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long() {
        assert_eq!(truncate("hello world foo bar", 10), "hello w...");
    }

    #[test]
    fn test_truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }
}
