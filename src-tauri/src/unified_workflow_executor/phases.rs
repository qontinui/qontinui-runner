//! Phase executors for the unified workflow.
//!
//! Each phase has a dedicated executor that handles a single responsibility:
//! - SetupExecutor: Runs one-time setup steps
//! - VerificationExecutor: Runs verification/test steps and reports results
//! - AgenticExecutor: Runs the AI with failure context
//! - CompletionExecutor: Runs completion steps (only if verification passed)
//!
//! All step event logging is done through the StepEventLogger facade, which
//! ensures consistent event format and prevents duplicate logging.
//!
//! AI session execution is delegated to the UnifiedAiSessionExecutor, which
//! consolidates the common logic for context building, prompt transformation,
//! and session management.
//!
//! ## Executor Trait
//!
//! Each phase executor implements the `Executor` trait from `crate::executor::traits`,
//! providing a uniform interface for execution with typed configuration and results.
//! This enables:
//! - Consistent error handling through `ExecutorError`
//! - Typed configuration via `SetupConfig`, `VerificationConfig`, etc.
//! - Factory construction via `FromContext`

#![allow(dead_code)]

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, info, instrument, warn};

use crate::config_storage::ConfigStorage;
use crate::database::{CheckpointDb, CreateTaskRunEventInput};
use crate::executor::{
    prompt_builder, timeout_helper, ExecutionOutcome, Executor, ExecutorContext, ExecutorError,
    FromContext, IntoOutcome,
};
use crate::step_executor::{
    ExecutionStepConfig, StepExecutionResult, StepExecutor, VerificationPhaseResult,
};
use crate::step_metadata::{StepDetails, StepMetadata};
use crate::step_registry::{StepEventKind, StepEventLogger};
use crate::step_types::StepType;
use crate::unified_ai_session::{AiSessionConfig, UnifiedAiSessionExecutor};
use crate::workflow_state::{CheckpointManager, StepCheckpoint};
use crate::AppState;

use super::phase_configs::{
    AgenticConfig, CompletionConfig, CompletionResult, SetupConfig, SetupResult,
    VerificationConfig, VerificationResult,
};
use super::types::{get_parent_task_id, AgenticOutcome, LoopConfig};

// Token tracking, UI Bridge, environment readiness, response mode, and token
// estimation extracted to phase_helpers module.
pub(super) use super::phase_helpers::{
    check_environment_readiness, clear_console_errors, compute_embedding_sync, estimate_tokens,
    execute_prompt_response_mode, fetch_browser_events_from_ui_bridge,
    fetch_console_errors_from_ui_bridge, fetch_health_from_ui_bridge,
    fetch_network_failures_from_ui_bridge,
    record_phase_token_usage, try_auto_connect_sdk_for_ui_workflow,
    REFLECTION_MODE_PREAMBLE,
};

// Execution Timing Context
// =============================================================================

/// Build a timing context string from execution spans for the current execution.
///
/// Returns None if no spans exist or the query fails.
fn build_execution_timing_context(
    checkpoint_db: &CheckpointDb,
    execution_id: &str,
) -> Option<String> {
    let spans = checkpoint_db
        .get_execution_spans(Some(execution_id), None, None, Some(100))
        .ok()?;

    if spans.is_empty() {
        return None;
    }

    let mut sections = Vec::new();

    // Phase timings
    let phase_spans: Vec<_> = spans
        .iter()
        .filter(|s| s.name.starts_with("workflow.phase."))
        .collect();
    if !phase_spans.is_empty() {
        let mut phase_lines = vec!["**Phase Timings:**".to_string()];
        for span in &phase_spans {
            let phase_name = span
                .name
                .strip_prefix("workflow.phase.")
                .unwrap_or(&span.name);
            let duration = span
                .duration_ms
                .map(format_duration_ms)
                .unwrap_or_else(|| "in progress".to_string());
            let status = if !span.success { " (failed)" } else { "" };
            phase_lines.push(format!("- {}: {}{}", phase_name, duration, status));
        }
        sections.push(phase_lines.join("\n"));
    }

    // AI session stats
    let ai_spans: Vec<_> = spans.iter().filter(|s| s.name == "ai.session").collect();
    if !ai_spans.is_empty() {
        let total_ms: i64 = ai_spans.iter().filter_map(|s| s.duration_ms).sum();
        let count = ai_spans.len();
        let avg_ms = if count > 0 {
            total_ms / count as i64
        } else {
            0
        };
        let failed = ai_spans.iter().filter(|s| !s.success).count();

        let mut ai_lines = vec!["**AI Sessions:**".to_string()];
        ai_lines.push(format!(
            "- Total: {} sessions, {} total",
            count,
            format_duration_ms(total_ms)
        ));
        ai_lines.push(format!(
            "- Average: {} per session",
            format_duration_ms(avg_ms)
        ));
        if failed > 0 {
            ai_lines.push(format!("- Failed: {} sessions", failed));
        }
        sections.push(ai_lines.join("\n"));
    }

    // Slow operations (>5s)
    let slow_spans: Vec<_> = spans
        .iter()
        .filter(|s| s.duration_ms.unwrap_or(0) > 5000)
        .collect();
    if !slow_spans.is_empty() {
        let mut slow_lines = vec!["**Slow Operations (>5s):**".to_string()];
        for span in &slow_spans {
            let duration = span.duration_ms.map(format_duration_ms).unwrap_or_default();
            let error_suffix = if let Some(ref err) = span.error {
                format!(" - FAILED: {}", err)
            } else if !span.success {
                " - FAILED".to_string()
            } else {
                String::new()
            };
            slow_lines.push(format!("- {}: {}{}", span.name, duration, error_suffix));
        }
        sections.push(slow_lines.join("\n"));
    }

    if sections.is_empty() {
        return None;
    }

    Some(format!(
        "---\n\n### Execution Timing\n\n{}",
        sections.join("\n\n")
    ))
}

/// Format milliseconds into a human-readable duration string.
fn format_duration_ms(ms: i64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let minutes = ms / 60_000;
        let seconds = (ms % 60_000) / 1000;
        format!("{}m {}s", minutes, seconds)
    }
}

// =============================================================================
// Prompt Response Mode Helper
// =============================================================================

/// Execute a single prompt step in "response" mode.
///
/// This runs a simple prompt->response AI call instead of a full Claude CLI session.
/// Used for meta-workflows and other cases where a full session is overkill.
///
/// Build compressed iteration history with tiered compression.
///
/// To prevent prompt bloat on iterations 4+, applies tiered compression:
/// - Recent iteration (N-1): Full context
/// - Old iterations (1..N-2): ~400 chars each
fn build_compressed_iteration_history(
    checkpoint_db: &CheckpointDb,
    execution_id: &str,
    current_iteration: u32,
    process_status_summary: Option<&str>,
    error_monitor_summary: Option<&str>,
    workflow_name: Option<&str>,
    max_context_tokens: usize,
    cross_workflow_learning: bool,
    project_path: Option<&str>,
) -> Option<String> {
    let mut sections = Vec::new();
    let mut budget_remaining = max_context_tokens;
    let mut sections_included: usize = 0;
    let mut sections_truncated: usize = 0;
    let mut sections_skipped: usize = 0;

    // Compression budget: recent iteration gets full fidelity, old ones get summarized
    let full_fidelity_iterations: u32 = 1; // N-1 gets full detail
    let max_per_old_iteration_chars: usize = 400;

    // === CRITICAL PRIORITY: Always included ===

    // 1. Latest verification feedback from knowledge base (most actionable -- show first)
    // Priority: CRITICAL -- always included regardless of budget
    if let Ok(feedback) =
        checkpoint_db.list_task_knowledge(execution_id, Some("verification_feedback"), false)
    {
        if let Some(latest) = feedback.last() {
            let mut lines = vec!["### Last Verification Feedback".to_string()];
            lines.push(String::new());
            lines.push(latest.content.clone());
            let section = lines.join("\n");
            let tokens = estimate_tokens(&section);
            budget_remaining = budget_remaining.saturating_sub(tokens);
            sections.push(section);
            sections_included += 1;
        }
    }

    // === HIGH PRIORITY: Include if budget allows ===

    // 2. Collect findings from the findings database (task_run_findings table)
    // Priority: HIGH
    if let Ok(findings) = checkpoint_db.get_findings_for_task(execution_id) {
        if !findings.is_empty() {
            let mut findings_lines = vec!["### Findings from Previous Iterations".to_string()];
            for finding in &findings {
                let status_str = finding.status.as_str();
                let category_str = finding.category.as_str();
                findings_lines.push(format!(
                    "- [{}:{}] {}",
                    category_str, status_str, finding.title
                ));
                if !finding.description.is_empty() {
                    let desc = truncate_str(&finding.description, 200);
                    findings_lines.push(format!("  {}", desc));
                }
            }
            let section = findings_lines.join("\n");
            let tokens = estimate_tokens(&section);
            if tokens <= budget_remaining {
                budget_remaining -= tokens;
                sections.push(section);
                sections_included += 1;
            } else if budget_remaining > 100 {
                // Truncate to fit within 80% of remaining budget
                let max_chars = (budget_remaining * 4 * 80) / 100;
                sections.push(truncate_str(&section, max_chars));
                budget_remaining = budget_remaining.saturating_sub(max_chars / 4);
                sections_included += 1;
                sections_truncated += 1;
            } else {
                sections_skipped += 1;
            }
        }
    }

    // 3. Tiered iteration history -- compressed for old, full for recent
    //
    // Old iterations (1..N-1-full_fidelity) get compressed to ~400 chars each.
    // Recent iterations (N-1) get full verification feedback + findings + diff.
    let recent_cutoff = current_iteration.saturating_sub(full_fidelity_iterations);

    // Load all observations once for efficient per-iteration lookup
    let all_observations = checkpoint_db
        .list_task_knowledge(execution_id, Some("observation"), false)
        .unwrap_or_default();

    // Load all solutions for fix descriptions
    let all_solutions = checkpoint_db
        .list_task_knowledge(execution_id, Some("solution"), false)
        .unwrap_or_default();

    // --- 3a. Recent iteration (full fidelity) ---
    // Priority: HIGH -- recent iteration details are critical for the AI
    if recent_cutoff >= 1 && current_iteration > 1 {
        let recent_iter = current_iteration - 1;
        let mut recent_lines = vec![format!(
            "### Last Iteration Details (Iteration {})",
            recent_iter
        )];
        recent_lines.push(String::new());

        // Full verification results for recent iteration
        if let Ok(Some(result)) =
            checkpoint_db.get_verification_phase_result(execution_id, recent_iter)
        {
            let passed = result
                .get("passed_steps")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let total = result
                .get("total_steps")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let failed = result
                .get("failed_steps")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let all_passed = result
                .get("all_passed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let status = if all_passed {
                "ALL PASSED".to_string()
            } else {
                format!("{}/{} passed, {} failed", passed, total, failed)
            };
            recent_lines.push(format!("**Verification:** {}", status));

            // List all step results with pass/fail
            if let Some(step_results) = result.get("step_results").and_then(|v| v.as_array()) {
                for step in step_results {
                    let name = step
                        .get("step_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let success = step
                        .get("success")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let icon = if success { "PASS" } else { "FAIL" };
                    recent_lines.push(format!("- [{}] {}", icon, name));
                }
            }
            recent_lines.push(String::new());
        }

        // Full observations (including git diff) for recent iteration
        for obs in all_observations
            .iter()
            .filter(|o| o.iteration == recent_iter)
        {
            recent_lines.push(truncate_str(&obs.content, 4000));
        }

        if recent_lines.len() > 2 {
            let section = recent_lines.join("\n");
            let tokens = estimate_tokens(&section);
            if tokens <= budget_remaining {
                budget_remaining -= tokens;
                sections.push(section);
                sections_included += 1;
            } else if budget_remaining > 200 {
                let max_chars = (budget_remaining * 4 * 80) / 100;
                sections.push(truncate_str(&section, max_chars));
                budget_remaining = budget_remaining.saturating_sub(max_chars / 4);
                sections_included += 1;
                sections_truncated += 1;
            } else {
                sections_skipped += 1;
            }
        }
    }

    // --- 3b. Old iterations (compressed) ---
    // Priority: HIGH (but compressed, so smaller token footprint)
    if recent_cutoff > 1 && budget_remaining > 200 {
        let mut compressed_lines = vec!["### Iteration History (Compressed)".to_string()];
        compressed_lines.push(String::new());

        // Track previous iteration's failed steps for regression detection
        let mut prev_failed_names: Vec<String> = Vec::new();

        for iter in 1..recent_cutoff {
            let mut iter_parts: Vec<String> = Vec::new();
            let mut current_failed_names: Vec<String> = Vec::new();

            // Verification pass/fail summary
            if let Ok(Some(result)) =
                checkpoint_db.get_verification_phase_result(execution_id, iter)
            {
                let passed = result
                    .get("passed_steps")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let total = result
                    .get("total_steps")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let failed = result
                    .get("failed_steps")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let all_passed = result
                    .get("all_passed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let status = if all_passed {
                    "ALL PASSED".to_string()
                } else {
                    format!("{}/{} passed, {} failed", passed, total, failed)
                };
                iter_parts.push(format!("**Iteration {}:** {}", iter, status));

                // Extract failed step names
                if let Some(step_results) = result.get("step_results").and_then(|v| v.as_array()) {
                    current_failed_names = step_results
                        .iter()
                        .filter(|s| s.get("success").and_then(|v| v.as_bool()) == Some(false))
                        .filter_map(|s| {
                            s.get("step_name")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .collect();

                    if !current_failed_names.is_empty() {
                        iter_parts.push(format!("  Failed: {}", current_failed_names.join(", ")));
                    }
                }

                // Detect regressions (newly failing steps) and fixes
                if iter > 1 && !prev_failed_names.is_empty() {
                    let newly_passing: Vec<_> = prev_failed_names
                        .iter()
                        .filter(|name| !current_failed_names.contains(name))
                        .collect();
                    if !newly_passing.is_empty() {
                        iter_parts.push(format!(
                            "  Fixed: {}",
                            newly_passing
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                    let regressions: Vec<_> = current_failed_names
                        .iter()
                        .filter(|name| !prev_failed_names.contains(name))
                        .collect();
                    if !regressions.is_empty() {
                        iter_parts.push(format!(
                            "  REGRESSION: {} (was passing)",
                            regressions
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                }
            }

            // Extract fix description from solutions for this iteration
            if let Some(solution) = all_solutions.iter().find(|s| s.iteration == iter) {
                iter_parts.push(format!("  Tried: {}", truncate_str(&solution.content, 120)));
            }

            // Extract git diff stat from observations for this iteration
            for obs in all_observations.iter().filter(|o| o.iteration == iter) {
                if let Some(diff_stat) = extract_diff_stat_from_observation(&obs.content) {
                    iter_parts.push(format!("  Changes: {}", diff_stat));
                    break;
                }
            }

            if !iter_parts.is_empty() {
                // Enforce per-iteration char budget
                let joined = iter_parts.join("\n");
                if joined.len() > max_per_old_iteration_chars {
                    compressed_lines.push(truncate_str(&joined, max_per_old_iteration_chars));
                } else {
                    compressed_lines.push(joined);
                }
                compressed_lines.push(String::new());
            }

            prev_failed_names = current_failed_names;
        }

        if compressed_lines.len() > 2 {
            let section = compressed_lines.join("\n");
            let tokens = estimate_tokens(&section);
            if tokens <= budget_remaining {
                budget_remaining -= tokens;
                sections.push(section);
                sections_included += 1;
            } else {
                sections_skipped += 1;
            }
        }
    }

    // 3c. Managed process status (pre-built by caller)
    // Priority: HIGH -- directly relevant to current execution
    if let Some(summary) = process_status_summary {
        let tokens = estimate_tokens(summary);
        if tokens <= budget_remaining {
            budget_remaining -= tokens;
            sections.push(summary.to_string());
            sections_included += 1;
        } else {
            sections_skipped += 1;
        }
    }

    // 3d. Error monitor summary (pre-built by caller)
    // Priority: HIGH -- errors are actionable
    if let Some(summary) = error_monitor_summary {
        let tokens = estimate_tokens(summary);
        if tokens <= budget_remaining {
            budget_remaining -= tokens;
            sections.push(summary.to_string());
            sections_included += 1;
        } else {
            sections_skipped += 1;
        }
    }

    // === MEDIUM PRIORITY: Include if budget allows ===

    // 4. Accumulated knowledge (unresolved only -- deduped against iteration history above)
    // Priority: MEDIUM
    if budget_remaining > 200 {
        if let Ok(all_knowledge) = checkpoint_db.list_task_knowledge(execution_id, None, false) {
            if !all_knowledge.is_empty() {
                let unresolved: Vec<_> = all_knowledge
                    .iter()
                    .filter(|k| {
                        !k.is_resolved
                            && k.category != "verification_feedback"
                            && k.category != "observation"
                    })
                    .collect();

                let solutions: Vec<_> = all_knowledge
                    .iter()
                    .filter(|k| k.category == "solution" && !k.is_resolved)
                    .collect();

                // Show unresolved findings/root causes
                if !unresolved.is_empty() {
                    let mut lines = vec!["### Accumulated Knowledge (Unresolved)".to_string()];
                    for entry in unresolved.iter().take(10) {
                        lines.push(format!(
                            "- **[{}]** (iter {}, {}): {}",
                            entry.category.to_uppercase(),
                            entry.iteration,
                            entry.confidence,
                            truncate_str(&entry.content, 300),
                        ));
                        if let Some(ref evidence) = entry.evidence {
                            lines.push(format!("  Evidence: {}", truncate_str(evidence, 150)));
                        }
                    }
                    let section = lines.join("\n");
                    let tokens = estimate_tokens(&section);
                    if tokens <= budget_remaining {
                        budget_remaining -= tokens;
                        sections.push(section);
                        sections_included += 1;
                    } else {
                        sections_skipped += 1;
                    }
                }

                // Show unresolved solution attempts (resolved ones are already proven working)
                if !solutions.is_empty() && budget_remaining > 100 {
                    let mut lines = vec!["### Previous Solution Attempts (Unresolved)".to_string()];
                    for entry in solutions.iter().take(5) {
                        lines.push(format!(
                            "- [iter {}] {}",
                            entry.iteration,
                            truncate_str(&entry.content, 300),
                        ));
                    }
                    let section = lines.join("\n");
                    let tokens = estimate_tokens(&section);
                    if tokens <= budget_remaining {
                        budget_remaining -= tokens;
                        sections.push(section);
                        sections_included += 1;
                    } else {
                        sections_skipped += 1;
                    }
                }
            }
        }
    }

    // 4b. Cross-run historical knowledge (from previous runs of the same workflow)
    // Priority: MEDIUM — scored by relevance instead of chronological order
    if budget_remaining > 200 {
        if let Some(wf_name) = workflow_name {
            // Try relevance-scored knowledge first (v99 cognitive system model)
            let scored_knowledge = checkpoint_db.with_conn(|conn| {
                crate::reflection::prediction::score_knowledge_relevance(conn, wf_name, 10)
            });

            if let Ok(scored) = scored_knowledge {
                if !scored.is_empty() {
                    let mut lines = vec!["### Historical Knowledge (relevance-ranked)".to_string()];
                    lines.push(
                        "These patterns were identified by reflection, ordered by relevance:"
                            .to_string(),
                    );
                    for entry in &scored {
                        lines.push(format!(
                            "- **[{}, relevance: {:.2}]** {}",
                            entry.category.to_uppercase(),
                            entry.relevance_score,
                            truncate_str(&entry.content, 300),
                        ));
                    }
                    let section = lines.join("\n");
                    let tokens = estimate_tokens(&section);
                    if tokens <= budget_remaining {
                        budget_remaining -= tokens;
                        sections.push(section);
                        sections_included += 1;
                    } else {
                        sections_skipped += 1;
                    }
                }
            } else {
                // Fallback: chronological ordering (pre-v99 databases)
                if let Ok(historical) = checkpoint_db.list_workflow_knowledge(
                    wf_name,
                    execution_id,
                    &["recurring_pattern", "context"],
                    10,
                ) {
                    if !historical.is_empty() {
                        let mut lines =
                            vec!["### Historical Knowledge (from previous runs)".to_string()];
                        lines.push(
                            "These patterns were identified by reflection across previous runs of this workflow:".to_string(),
                        );
                        for entry in &historical {
                            lines.push(format!(
                                "- **[{}]** ({}): {}",
                                entry.category.to_uppercase(),
                                entry.confidence,
                                truncate_str(&entry.content, 300),
                            ));
                        }
                        let section = lines.join("\n");
                        let tokens = estimate_tokens(&section);
                        if tokens <= budget_remaining {
                            budget_remaining -= tokens;
                            sections.push(section);
                            sections_included += 1;
                        } else {
                            sections_skipped += 1;
                        }
                    }
                }
            }
        }
    }

    // 4c. Cross-workflow learning (insights from similar but different workflows)
    // Priority: MEDIUM -- only on first iteration when cross_workflow_learning is enabled
    if cross_workflow_learning && current_iteration <= 1 && budget_remaining > 200 {
        if let Some(wf_name) = workflow_name {
            if let Ok(insights) =
                checkpoint_db.get_cross_workflow_knowledge(wf_name, execution_id, 3)
            {
                if !insights.is_empty() {
                    let mut lines = vec!["## Insights from similar workflows".to_string()];
                    lines.push(String::new());
                    for (source_wf_name, knowledge_text) in &insights {
                        lines.push(format!(
                            "From '{}': {}",
                            source_wf_name,
                            truncate_str(knowledge_text, 400),
                        ));
                        lines.push(String::new());
                    }
                    let section = lines.join("\n");
                    let tokens = estimate_tokens(&section);
                    if tokens <= budget_remaining {
                        budget_remaining -= tokens;
                        sections.push(section);
                        sections_included += 1;
                    } else {
                        sections_skipped += 1;
                    }
                }
            }
        }
    }

    // 4d. Project knowledge (from project reflection — knowledge about the user's project)
    // Priority: MEDIUM -- only on first iteration when project_path is available
    if current_iteration <= 1 && budget_remaining > 200 {
        if let Some(pp) = project_path {
            if let Ok(knowledge) = checkpoint_db.list_project_knowledge(pp, execution_id, 10) {
                if !knowledge.is_empty() {
                    let mut lines = vec!["## Project Knowledge".to_string()];
                    lines.push(
                        "(Learned from previous workflows targeting this project)".to_string(),
                    );
                    lines.push(String::new());
                    for (category, content) in &knowledge {
                        let label = match category.as_str() {
                            "project_environment" => "Environment",
                            "project_architecture" => "Architecture",
                            "project_test_pattern" => "Test Pattern",
                            "project_recurring_issue" => "Known Issue",
                            _ => category.as_str(),
                        };
                        lines.push(format!("**[{}]** {}", label, truncate_str(content, 400),));
                        lines.push(String::new());
                    }
                    let section = lines.join("\n");
                    let tokens = estimate_tokens(&section);
                    if tokens <= budget_remaining {
                        budget_remaining -= tokens;
                        sections.push(section);
                        sections_included += 1;
                    } else {
                        sections_skipped += 1;
                    }
                }
            }
        }
    }

    // 4e. Fix predictions (suggest known fixes for unresolved findings)
    // Priority: MEDIUM -- only on iteration 1, max 3 predictions
    if current_iteration <= 1 && budget_remaining > 200 {
        if let Ok(findings) = checkpoint_db.get_findings_for_task(execution_id) {
            let unresolved_findings: Vec<_> = findings
                .iter()
                .filter(|f| !f.status.is_terminal())
                .take(5)
                .collect();

            if !unresolved_findings.is_empty() {
                let mut prediction_lines = Vec::new();
                let mut prediction_count = 0;

                for finding in &unresolved_findings {
                    if prediction_count >= 3 {
                        break;
                    }
                    // Use the finding's actual signature_hash for fix prediction lookup
                    let sig = &finding.signature_hash;
                    if let Ok(Some(predicted)) = checkpoint_db.with_conn(|conn| {
                        crate::reflection::prediction::predict_fix_for_error(conn, sig)
                    }) {
                        if predicted.confidence >= 0.3 {
                            prediction_lines.push(format!(
                                "- **[Predicted Fix, confidence: {:.0}%]** Based on {} previous successful application(s): {}",
                                predicted.confidence * 100.0,
                                predicted.reuse_count,
                                truncate_str(&predicted.fix_description, 300),
                            ));
                            prediction_count += 1;

                            // Record that this fix was shown to the AI (outcome="shown" won't increment reuse_count)
                            let _ = checkpoint_db.with_conn(|conn| {
                                crate::reflection::prediction::record_fix_application(
                                    conn,
                                    &predicted.fix_id,
                                    execution_id,
                                    Some(sig),
                                    "shown",
                                )
                            });
                        }
                    }
                }

                if !prediction_lines.is_empty() {
                    let mut lines = vec!["### Predicted Fixes (from historical data)".to_string()];
                    lines.extend(prediction_lines);
                    let section = lines.join("\n");
                    let tokens = estimate_tokens(&section);
                    if tokens <= budget_remaining {
                        budget_remaining -= tokens;
                        sections.push(section);
                        sections_included += 1;
                    } else {
                        sections_skipped += 1;
                    }
                }
            }
        }
    }

    // 4f. Causal history (known cause→effect patterns from previous runs)
    // Priority: LOW -- only when causal data exists, max 500 tokens
    if budget_remaining > 200 {
        if let Some(wf_name) = workflow_name {
            let causal_events = checkpoint_db.with_conn(|conn| {
                crate::reflection::causal::get_causal_events_for_workflow(conn, wf_name, 10)
            });

            if let Ok(events) = causal_events {
                if !events.is_empty() {
                    let mut lines =
                        vec!["### Causal History (known cause→effect patterns)".to_string()];
                    // Group by relationship type for compact display
                    let mut by_relationship: std::collections::HashMap<String, Vec<String>> =
                        std::collections::HashMap::new();
                    for event in &events {
                        let desc = event.description.as_deref().unwrap_or("(no description)");
                        let entry = format!(
                            "{} → {} ({})",
                            event.cause_event_type, event.effect_event_type, desc
                        );
                        by_relationship
                            .entry(event.relationship.clone())
                            .or_default()
                            .push(entry);
                    }
                    for (rel, entries) in &by_relationship {
                        lines.push(format!("**{}:** {} occurrence(s)", rel, entries.len()));
                        for entry in entries.iter().take(3) {
                            lines.push(format!(
                                "  - {}",
                                crate::str_utils::truncate_str(entry, 200)
                            ));
                        }
                    }
                    let section = lines.join("\n");
                    let tokens = estimate_tokens(&section);
                    if tokens <= budget_remaining && tokens <= 500 {
                        budget_remaining -= tokens;
                        sections.push(section);
                        sections_included += 1;
                    } else {
                        sections_skipped += 1;
                    }
                }
            }
        }
    }

    // 4g. Universal patterns (cross-project knowledge via hybrid semantic search)
    // Priority: LOW -- only on first iteration, after all project-specific knowledge
    if current_iteration <= 1 && budget_remaining > 200 {
        // Build query text from workflow name for semantic matching
        let query_text = workflow_name.unwrap_or("").to_string();

        // Try hybrid search (semantic), fall back to SQL-only
        // Note: compute_embedding_sync runs async reqwest on a dedicated
        // thread with its own isolated runtime to avoid nested-runtime panics.
        let universal_fixes = if !query_text.trim().is_empty() {
            let query_embedding = compute_embedding_sync(&query_text);

            match query_embedding {
                Ok(emb) => {
                    let search_config = crate::database::hybrid_search::HybridSearchConfig {
                        sql_weight: 0.3,
                        vector_weight: 0.7,
                        limit: 5,
                        min_similarity: 0.3,
                    };
                    checkpoint_db
                        .with_conn(|conn| {
                            crate::database::hybrid_search::hybrid_search_universal_fixes(
                                conn,
                                &emb,
                                &search_config,
                            )
                        })
                        .ok()
                }
                Err(_) => None,
            }
        } else {
            None
        };

        // Fall back to SQL-only retrieval if hybrid search failed
        let fixes_to_inject: Vec<(String, String, Option<String>)> = match universal_fixes {
            Some(results) if !results.is_empty() => results
                .iter()
                .map(|r| {
                    (
                        r.item.fix_type.clone(),
                        r.item.fix_description.clone(),
                        r.item.applicability_context.clone(),
                    )
                })
                .collect(),
            _ => {
                // SQL-only fallback: get universal fixes by reuse_count
                checkpoint_db
                    .with_conn(|conn| crate::reflection::storage::get_universal_fixes(conn, 5))
                    .ok()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|f| (f.fix_type, f.fix_description, f.applicability_context))
                    .collect()
            }
        };

        if !fixes_to_inject.is_empty() {
            let mut lines = vec!["## Universal Patterns".to_string()];
            lines.push(
                "(Proven effective across multiple projects — retrieved by semantic relevance)"
                    .to_string(),
            );
            lines.push(String::new());
            for (fix_type, description, ctx) in &fixes_to_inject {
                let ctx_str = ctx.as_deref().unwrap_or("general");
                lines.push(format!(
                    "- **[{}]** (applies to: {}) {}",
                    fix_type,
                    ctx_str,
                    truncate_str(description, 300),
                ));
                lines.push(String::new());
            }
            let section = lines.join("\n");
            let tokens = estimate_tokens(&section);
            if tokens <= budget_remaining {
                budget_remaining -= tokens;
                sections.push(section);
                sections_included += 1;
            } else {
                sections_skipped += 1;
            }
        }
    }

    // 4h. Component impact analysis (iteration 2+ only)
    // Priority: LOW -- provides architectural awareness of changed files
    if current_iteration > 1 && budget_remaining > 200 {
        let changed: Vec<String> = all_observations
            .iter()
            .filter(|o| o.iteration == current_iteration - 1)
            .flat_map(|o| extract_changed_files_from_observation(&o.content))
            .collect();
        if let Some(wf_name) = workflow_name {
            if let Some(section) = build_impact_context(checkpoint_db, wf_name, &changed) {
                let tokens = estimate_tokens(&section);
                let capped_tokens = tokens.min(600);
                if capped_tokens <= budget_remaining {
                    let capped_section = if tokens > 600 {
                        let max_chars = 600 * 4;
                        truncate_str(&section, max_chars)
                    } else {
                        section
                    };
                    budget_remaining -= estimate_tokens(&capped_section);
                    sections.push(capped_section);
                    sections_included += 1;
                } else {
                    sections_skipped += 1;
                }
            }
        }
    }

    // === LOW PRIORITY: Include only if plenty of budget remains ===

    // 5. Available data APIs (so AI can drill deeper when needed)
    // Priority: LOW -- static reference content
    if budget_remaining > 500 {
        let mut api_lines = vec!["### Available Data APIs".to_string()];
        api_lines.push(String::new());
        api_lines.push(
            "The runner database contains detailed execution data. Access via HTTP:".to_string(),
        );
        api_lines.push(String::new());
        api_lines.push("**Verification & Testing:**".to_string());
        api_lines.push(format!(
            "- `curl http://localhost:9876/task-runs/{}/verification-results` - Full test results",
            execution_id
        ));
        api_lines.push(format!(
            "- `curl http://localhost:9876/task-runs/{}/verification-results?failed_only=true` - Only failed checks",
            execution_id
        ));
        api_lines.push(format!(
            "- `curl http://localhost:9876/task-runs/{}/playwright-results` - Playwright test results",
            execution_id
        ));
        api_lines.push(String::new());
        api_lines.push("**Knowledge & Findings:**".to_string());
        api_lines.push(format!(
            "- `curl http://localhost:9876/task-runs/{}/knowledge` - All findings, observations, solutions",
            execution_id
        ));
        api_lines.push(format!(
            "- `curl http://localhost:9876/task-runs/{}/knowledge?unresolved_only=true` - Unresolved issues",
            execution_id
        ));
        api_lines.push(String::new());
        api_lines.push("**Execution History:**".to_string());
        api_lines.push(format!(
            "- `curl http://localhost:9876/task-runs/{}/events` - All execution events",
            execution_id
        ));
        api_lines.push(format!(
            "- `curl http://localhost:9876/task-runs/{}/checkpoints` - Step completion checkpoints",
            execution_id
        ));
        api_lines.push(format!(
            "- `curl http://localhost:9876/task-runs/{}/mcp-calls` - MCP tool calls",
            execution_id
        ));
        api_lines.push(String::new());
        api_lines.push("**Managed Processes:**".to_string());
        api_lines.push(
            "- `GET /processes/status` \u{2014} Current status of all managed processes"
                .to_string(),
        );
        api_lines.push(
            "- `GET /processes/{id}/output?tail=100` \u{2014} Recent output lines from a managed process".to_string(),
        );
        api_lines.push(String::new());
        api_lines.push(
            "Use these APIs when you need more detail than provided in this context.".to_string(),
        );
        let section = api_lines.join("\n");
        let tokens = estimate_tokens(&section);
        if tokens <= budget_remaining {
            budget_remaining -= tokens;
            sections.push(section);
            sections_included += 1;
        } else {
            sections_skipped += 1;
        }
    } else {
        sections_skipped += 1;
    }

    // 6. Tool priority guidance
    // Priority: LOW -- static reference content
    if budget_remaining > 200 {
        let mut tool_lines = vec!["### Verification Tool Priority".to_string()];
        tool_lines.push(String::new());
        tool_lines.push(
            "**Prefer UI Bridge over Playwright** when the target app has the UI Bridge SDK integrated.".to_string(),
        );
        tool_lines.push(String::new());
        tool_lines.push(
            "- **UI Bridge SDK** (`/ui-bridge/sdk/*`): For SDK-integrated apps. Connect via `POST /ui-bridge/sdk/connect`, then query via `GET /ui-bridge/sdk/elements` or `GET /ui-bridge/sdk/snapshot`.".to_string(),
        );
        tool_lines.push(
            "- **UI Bridge Control** (`/ui-bridge/control/*`): For the runner's own UI. Always available.".to_string(),
        );
        tool_lines.push(
            "- **Playwright**: Only for non-SDK web apps or when you need real browser behavior. Fallback, not default.".to_string(),
        );
        tool_lines.push(String::new());
        tool_lines.push(
            "Check `GET /ui-bridge/sdk/status` to see if an SDK app is already connected."
                .to_string(),
        );
        let section = tool_lines.join("\n");
        let tokens = estimate_tokens(&section);
        if tokens <= budget_remaining {
            budget_remaining = budget_remaining.saturating_sub(tokens);
            sections.push(section);
            sections_included += 1;
        } else {
            sections_skipped += 1;
        }
    } else {
        sections_skipped += 1;
    }

    // 7. UI Bridge error monitoring APIs
    // Priority: LOW -- static reference content
    if budget_remaining > 600 {
        let mut em_lines = vec!["### UI Bridge Error Monitoring APIs".to_string()];
        em_lines.push(String::new());
        em_lines.push(
            "When the target app is connected via UI Bridge SDK, these endpoints provide runtime error monitoring. \
             Use them to detect console errors, network failures, and regressions introduced by code changes."
                .to_string(),
        );
        em_lines.push(String::new());
        em_lines.push("**Quick health check:**".to_string());
        em_lines.push(
            "- `curl http://localhost:9876/ui-bridge/sdk/console/health` — Returns a health score (0-100), status (healthy/degraded/broken), error breakdown by severity, and top issue. **Start here** to see if the app has problems.".to_string(),
        );
        em_lines.push(String::new());
        em_lines.push("**Browser events & timeline:**".to_string());
        em_lines.push(
            "- `curl http://localhost:9876/ui-bridge/sdk/console/browser-events` — Recent browser console events. Query params: `severity` (crash|error|warning|noise), `deduplicate` (bool), `since` (timestamp), `limit` (number).".to_string(),
        );
        em_lines.push(
            "- `curl http://localhost:9876/ui-bridge/sdk/console/timeline` — Interleaved action+error timeline. Query params: `since`, `limit`, `minSeverity`.".to_string(),
        );
        em_lines.push(
            "- `curl http://localhost:9876/ui-bridge/sdk/console/network-chains` — Network request chains with error correlation. Query params: `failuresOnly` (bool), `limit`, `url` (pattern).".to_string(),
        );
        em_lines.push(String::new());
        em_lines.push("**Error sessions (track errors around an action):**".to_string());
        em_lines.push(
            "- `curl -X POST http://localhost:9876/ui-bridge/sdk/console/error-sessions/start -d '{\"label\":\"my-action\"}'` — Start tracking. Call before performing an action.".to_string(),
        );
        em_lines.push(
            "- `curl -X POST http://localhost:9876/ui-bridge/sdk/console/error-sessions/end` — End session and get a summary of errors captured during the session.".to_string(),
        );
        em_lines.push(
            "- `curl http://localhost:9876/ui-bridge/sdk/console/error-sessions` — List all session summaries.".to_string(),
        );
        em_lines.push(String::new());
        em_lines.push("**Error baselines (detect regressions):**".to_string());
        em_lines.push(
            "- `curl -X POST http://localhost:9876/ui-bridge/sdk/console/error-baselines/capture -d '{\"label\":\"before-fix\"}'` — Snapshot current errors as a baseline.".to_string(),
        );
        em_lines.push(
            "- `curl -X POST http://localhost:9876/ui-bridge/sdk/console/error-baselines/compare -d '{\"label\":\"before-fix\"}'` — Compare current state against the baseline. Returns new errors (regressions) and fixed errors.".to_string(),
        );
        em_lines.push(String::new());
        em_lines.push(
            "**Workflow pattern:** Capture a baseline before making changes, then compare after to confirm you haven't introduced new errors.".to_string(),
        );
        let section = em_lines.join("\n");
        let tokens = estimate_tokens(&section);
        if tokens <= budget_remaining {
            budget_remaining = budget_remaining.saturating_sub(tokens);
            sections.push(section);
            sections_included += 1;
        } else {
            sections_skipped += 1;
        }
    } else {
        sections_skipped += 1;
    }

    // Log budget summary
    let tokens_used = max_context_tokens.saturating_sub(budget_remaining);
    info!(
        "CONTEXT-BUDGET: {}/{} tokens used ({} sections included, {} truncated, {} skipped)",
        tokens_used, max_context_tokens, sections_included, sections_truncated, sections_skipped
    );

    if sections.is_empty() {
        return None;
    }

    Some(format!(
        "---\n\n## Previous Iteration Context\n\n{}\n\n---\n\nUse this context to avoid repeating mistakes and build on previous progress.",
        sections.join("\n\n")
    ))
}

/// Extract the git diff --stat summary line from an observation that follows the format:
/// "Git changes after iteration N:\n<stat lines>\n\n<full diff>"
///
/// Returns the last stat line (the summary like "3 files changed, +12, -5").
fn extract_diff_stat_from_observation(content: &str) -> Option<String> {
    if !content.starts_with("Git changes after iteration") {
        return None;
    }
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() < 2 {
        return None;
    }
    // Collect stat lines until the first empty line or diff header
    let mut stat_lines = Vec::new();
    for line in &lines[1..] {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        stat_lines.push(trimmed);
    }
    if stat_lines.is_empty() {
        return None;
    }
    // The last stat line is the summary (e.g., "3 files changed, 12 insertions(+), 5 deletions(-)")
    Some(stat_lines.last().unwrap().to_string())
}

/// Extract changed file paths from a git diff stat observation.
///
/// Parses lines like ` src/foo/bar.rs | 12 +++---` into `["src/foo/bar.rs"]`.
fn extract_changed_files_from_observation(content: &str) -> Vec<String> {
    if !content.starts_with("Git changes after iteration") {
        return Vec::new();
    }
    let mut files = Vec::new();
    for line in content.lines().skip(1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        // diff stat lines have the form: " path/to/file | N +++---"
        if let Some(pipe_pos) = trimmed.find(" | ") {
            let path = trimmed[..pipe_pos].trim();
            if !path.is_empty() {
                files.push(path.to_string());
            }
        }
    }
    files
}

/// Build a component impact analysis context section from changed files.
///
/// For each changed file, looks up the architecture model for impact data.
/// Returns None if no architecture data exists or no impacts found.
fn build_impact_context(
    checkpoint_db: &crate::database::CheckpointDb,
    workflow_name: &str,
    changed_files: &[String],
) -> Option<String> {
    if changed_files.is_empty() {
        return None;
    }

    let mut impact_lines = Vec::new();

    for file_path in changed_files.iter().take(5) {
        let impact = checkpoint_db
            .with_conn(|conn| {
                crate::reflection::architecture::get_impact_analysis(conn, workflow_name, file_path)
            })
            .ok();

        if let Some(analysis) = impact {
            if analysis.total_impact_radius == 0 {
                continue;
            }
            let mut line = format!(
                "- **{}** (impact radius: {})",
                file_path, analysis.total_impact_radius
            );
            if !analysis.direct_impacts.is_empty() {
                let direct: Vec<String> = analysis
                    .direct_impacts
                    .iter()
                    .take(3)
                    .map(|e| format!("{} [{}]", e.component_path, e.relationship_type))
                    .collect();
                line.push_str(&format!("\n  Direct: {}", direct.join(", ")));
            }
            if !analysis.transitive_impacts.is_empty() {
                let count = analysis.transitive_impacts.len();
                line.push_str(&format!(
                    "\n  Transitive: {} additional component(s)",
                    count
                ));
            }
            impact_lines.push(line);
        }
    }

    if impact_lines.is_empty() {
        return None;
    }

    let mut section = String::from("## Component Impact Analysis\n");
    section.push_str(
        "The following components are affected by your changes from the previous iteration:\n\n",
    );
    section.push_str(&impact_lines.join("\n\n"));
    Some(section)
}

// =============================================================================
// Agentic Phase Enrichment: Pre-read Files from Failure Context
// =============================================================================

/// Regex for extracting file paths from failure context text.
///
/// Matches patterns like:
/// - `src/Foo.tsx(42,5)` (TypeScript-style errors)
/// - `at src/bar.rs:123` (Rust/stack trace style)
/// - `File: path/to/file.ext`
/// - `src/foo/bar.ts:42:5` (colon-separated line:col)
/// - `path/to/file.ext` (bare paths with common extensions)
fn failure_file_path_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r#"(?:File:\s*|at\s+)?(?:^|\s|`|'|")((?:[a-zA-Z]:[/\\])?(?:[\w.@-]+[/\\])*[\w.@-]+\.(?:rs|ts|tsx|js|jsx|py|json|toml|yaml|yml|css|scss|html|vue|svelte|go|java|kt|rb|php|cs|cpp|c|h|hpp))\b"#
        ).unwrap()
    })
}

/// Extract file paths from failure context and pre-read their contents.
///
/// Parses verification failure output (typecheck errors, test failures, stack traces)
/// to find referenced files, then reads their current contents so the AI has them
/// immediately without needing tool calls.
fn extract_and_preread_failure_files(
    failure_context: &str,
    project_path: Option<&str>,
    max_files: usize,
    max_lines_per_file: usize,
    max_total_bytes: usize,
) -> String {
    use std::collections::HashSet;
    use std::path::Path;

    if failure_context.is_empty() {
        return String::new();
    }

    let re = failure_file_path_regex();
    let mut seen = HashSet::new();
    let mut valid_paths = Vec::new();

    for cap in re.captures_iter(failure_context) {
        let raw_path = cap.get(1).unwrap().as_str().to_string();
        // Strip trailing line/col info like ":123:5" or "(42,5)"
        let clean_path = raw_path.split(['(', ':']).next().unwrap_or(&raw_path);

        if seen.contains(clean_path) {
            continue;
        }
        seen.insert(clean_path.to_string());

        // Try to resolve the path
        if let Some(root) = project_path {
            let candidate = Path::new(root).join(clean_path);
            if candidate.is_file() {
                valid_paths.push(candidate);
                continue;
            }
        }
        let abs = Path::new(clean_path);
        if abs.is_absolute() && abs.is_file() {
            valid_paths.push(abs.to_path_buf());
        }
    }

    if valid_paths.is_empty() {
        return String::new();
    }

    info!(
        "AGENTIC-ENRICHMENT: Found {} referenced files in failure context, will pre-read up to {}",
        valid_paths.len(),
        max_files
    );

    let mut result = String::new();
    let mut total_bytes = 0usize;

    for (i, path) in valid_paths.iter().enumerate() {
        if i >= max_files || total_bytes >= max_total_bytes {
            break;
        }

        match std::fs::read_to_string(path) {
            Ok(content) => {
                let truncated: String = content
                    .lines()
                    .take(max_lines_per_file)
                    .collect::<Vec<_>>()
                    .join("\n");

                let remaining = max_total_bytes.saturating_sub(total_bytes);
                let portion = if truncated.len() > remaining {
                    &truncated[..remaining]
                } else {
                    &truncated
                };

                result.push_str(&format!(
                    "--- File: {} ---\n{}\n\n",
                    path.display(),
                    portion
                ));
                total_bytes += portion.len();

                let was_truncated =
                    truncated.len() < content.len() || portion.len() < truncated.len();
                if was_truncated {
                    result.push_str("(truncated)\n\n");
                }
            }
            Err(e) => {
                debug!(
                    "AGENTIC-ENRICHMENT: Could not read {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }

    if !result.is_empty() {
        info!(
            "AGENTIC-ENRICHMENT: Pre-read {} bytes from {} files",
            total_bytes,
            valid_paths.len().min(max_files)
        );
    }

    result
}

// =============================================================================
// Agentic Phase Enrichment: Pre-read Previously Edited Files
// =============================================================================

/// Pre-read files that were edited in previous iterations.
///
/// On iteration 2+, extracts file paths from observations (git diff output) of
/// prior iterations, then reads the current state of those files. This gives the
/// AI immediate access to files it previously modified without needing tool calls.
fn preread_previously_edited_files(
    checkpoint_db: &CheckpointDb,
    execution_id: &str,
    current_iteration: u32,
    project_path: Option<&str>,
    max_files: usize,
    max_lines_per_file: usize,
    max_total_bytes: usize,
) -> String {
    use std::collections::HashSet;
    use std::path::Path;

    if current_iteration <= 1 {
        return String::new();
    }

    // Load observations from all previous iterations
    let all_observations = checkpoint_db
        .list_task_knowledge(execution_id, Some("observation"), false)
        .unwrap_or_default();

    // Extract changed files from observation content (git diff stat lines)
    let mut seen = HashSet::new();
    let mut valid_paths = Vec::new();

    for obs in all_observations
        .iter()
        .filter(|o| o.iteration < current_iteration)
    {
        for file_str in extract_changed_files_from_observation(&obs.content) {
            if seen.contains(&file_str) {
                continue;
            }
            seen.insert(file_str.clone());

            // Resolve relative to project_path
            if let Some(root) = project_path {
                let candidate = Path::new(root).join(&file_str);
                if candidate.is_file() {
                    valid_paths.push(candidate);
                    continue;
                }
            }
            let abs = Path::new(&file_str);
            if abs.is_absolute() && abs.is_file() {
                valid_paths.push(abs.to_path_buf());
            }
        }
    }

    if valid_paths.is_empty() {
        return String::new();
    }

    info!(
        "AGENTIC-ENRICHMENT: Found {} previously edited files, will pre-read up to {}",
        valid_paths.len(),
        max_files
    );

    let mut result = String::new();
    let mut total_bytes = 0usize;

    for (i, path) in valid_paths.iter().enumerate() {
        if i >= max_files || total_bytes >= max_total_bytes {
            break;
        }

        match std::fs::read_to_string(path) {
            Ok(content) => {
                let truncated: String = content
                    .lines()
                    .take(max_lines_per_file)
                    .collect::<Vec<_>>()
                    .join("\n");

                let remaining = max_total_bytes.saturating_sub(total_bytes);
                let portion = if truncated.len() > remaining {
                    &truncated[..remaining]
                } else {
                    &truncated
                };

                result.push_str(&format!(
                    "--- File: {} (previously edited) ---\n{}\n\n",
                    path.display(),
                    portion
                ));
                total_bytes += portion.len();

                let was_truncated =
                    truncated.len() < content.len() || portion.len() < truncated.len();
                if was_truncated {
                    result.push_str("(truncated)\n\n");
                }
            }
            Err(e) => {
                debug!(
                    "AGENTIC-ENRICHMENT: Could not read previously edited file {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }

    if !result.is_empty() {
        info!(
            "AGENTIC-ENRICHMENT: Pre-read {} bytes from {} previously edited files",
            total_bytes,
            valid_paths.len().min(max_files)
        );
    }

    result
}

/// Truncate a string to max_len characters, appending "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        // Find the last char boundary at or before max_len to avoid
        // panicking on multi-byte UTF-8 characters.
        let end = s
            .char_indices()
            .take_while(|(i, _)| *i <= max_len)
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);
        format!("{}...", &s[..end])
    } else {
        s.to_string()
    }
}

// =============================================================================
// Setup Phase Executor
// =============================================================================

/// Executes the setup phase (runs once at the start).
///
/// Handles both automation steps (shell commands, workflows) and prompt steps (AI tasks).
/// AI session execution is delegated to the UnifiedAiSessionExecutor.
pub struct SetupExecutor {
    app_state: Arc<AppState>,
    executor: StepExecutor,
    ai_executor: UnifiedAiSessionExecutor,
    checkpoint_db: Arc<CheckpointDb>,
}

impl SetupExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<TokioMutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        let checkpoint_db = app_state.checkpoint_db.clone();
        Self {
            app_state: app_state.clone(),
            executor: StepExecutor::with_app_handle(
                app_state.clone(),
                config_storage,
                app_handle.clone(),
            ),
            ai_executor: UnifiedAiSessionExecutor::new(app_state, app_handle, pid_tracker),
            checkpoint_db,
        }
    }

    /// Enable interactive sessions via the session manager.
    pub fn set_session_manager(&mut self, sm: Arc<crate::claude_session::SessionManager>) {
        self.ai_executor.session_manager = Some(sm);
    }

    /// Set the task run ID on the inner step executor for database logging.
    pub fn set_task_run_id(&mut self, task_run_id: String) {
        self.executor.set_task_run_id(task_run_id);
    }

    /// Get the shared variable store from the inner step executor.
    ///
    /// After setup phase completes, this contains all variables set by API steps
    /// (e.g., `source_findings`, `source_knowledge`). These need to be substituted
    /// into the agentic prompt before the agentic phase runs.
    pub fn shared_variables(
        &self,
    ) -> &crate::orchestrator::context_propagation::SharedVariableStore {
        self.executor.shared_variables()
    }

    /// Run setup steps. Returns true if successful.
    ///
    /// Executes automation steps first (shell commands, etc.), then prompt steps (AI tasks).
    /// The logger is required for consistent step event logging.
    ///
    /// Step checkpointing is integrated for resume capability.
    #[instrument(
        name = "qontinui.workflow.phase.setup",
        skip(self, automation_steps, prompt_steps, logger),
        fields(
            execution_id = %execution_id,
            workflow_name = %workflow_name,
            automation_step_count = automation_steps.len(),
            prompt_step_count = prompt_steps.len()
        )
    )]
    pub async fn run_setup(
        &self,
        automation_steps: &[ExecutionStepConfig],
        prompt_steps: &[ExecutionStepConfig],
        execution_id: &str,
        workflow_name: &str,
        logger: &StepEventLogger,
        stage_index: Option<u32>,
        model_override: Option<String>,
        provider_override: Option<String>,
    ) -> (bool, Vec<StepExecutionResult>) {
        let mut all_results = Vec::new();
        let mut overall_success = true;

        // Filter out dev_mode_only steps when not in dev mode
        let automation_steps: Vec<ExecutionStepConfig> = automation_steps
            .iter()
            .filter(|step| {
                if step.dev_mode_only.unwrap_or(false) && !cfg!(debug_assertions) {
                    info!(
                        "SETUP-PHASE: Skipping dev-mode-only automation step: {:?}",
                        step.name
                    );
                    false
                } else {
                    true
                }
            })
            .cloned()
            .collect();
        let automation_steps = automation_steps.as_slice();

        // Create checkpoint manager for step-level checkpointing
        let checkpoint_mgr = CheckpointManager::new(self.checkpoint_db.clone(), "unified");

        // Run automation setup steps first
        if !automation_steps.is_empty() {
            info!(
                "SETUP-PHASE: Running {} automation steps",
                automation_steps.len()
            );

            // Checkpoint each automation step
            for (idx, step) in automation_steps.iter().enumerate() {
                let step_type =
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Command);
                let step_name = step.name.as_deref().unwrap_or(&step.step_type);

                // Use Some(0) instead of None for iteration to ensure SQLite's
                // UNIQUE constraint works correctly (NULL != NULL in SQLite)
                let mut checkpoint = StepCheckpoint::new(
                    execution_id,
                    "unified",
                    "setup",
                    Some(0),
                    idx,
                    step_type.as_str(),
                )
                .with_step_name(step_name)
                .with_stage_index(stage_index);
                checkpoint.mark_started();
                if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                    warn!("Failed to save setup step checkpoint: {}", e);
                }
            }

            let (result, _has_gui) = self
                .executor
                .execute_setup_phase(automation_steps, execution_id, &[])
                .await;

            // Checkpoint completion for each step
            for (idx, step_result) in result.steps.iter().enumerate() {
                let step = &automation_steps[idx];
                let step_type =
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Command);
                let step_name = step.name.as_deref().unwrap_or(&step.step_type);

                // Use Some(0) instead of None for iteration to ensure SQLite's
                // UNIQUE constraint works correctly (NULL != NULL in SQLite)
                let mut checkpoint = StepCheckpoint::new(
                    execution_id,
                    "unified",
                    "setup",
                    Some(0),
                    idx,
                    step_type.as_str(),
                )
                .with_step_name(step_name)
                .with_stage_index(stage_index);

                let duration_ms = step_result.duration_ms as i64;
                if step_result.success {
                    checkpoint.mark_success(serde_json::to_string(step_result).ok(), duration_ms);
                } else {
                    checkpoint.mark_failed(
                        step_result.error.as_deref().unwrap_or("Unknown error"),
                        duration_ms,
                    );
                }

                if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                    warn!("Failed to save setup step completion checkpoint: {}", e);
                }
            }

            overall_success = overall_success && result.success;
            all_results.extend(result.steps);

            if !result.success {
                warn!("SETUP-PHASE: Automation steps failed");
                return (false, all_results);
            }
        }

        // Run prompt setup steps (AI tasks)
        if !prompt_steps.is_empty() {
            info!(
                "SETUP-PHASE: Running {} prompt steps (AI tasks)",
                prompt_steps.len()
            );

            // Separate response-mode steps from session-mode steps
            let mut session_prompt_steps = Vec::new();
            let mut response_step_count = 0usize;
            for step in prompt_steps {
                // Skip dev_mode_only steps when not in dev mode
                if step.dev_mode_only.unwrap_or(false) && !cfg!(debug_assertions) {
                    info!("Skipping dev-mode-only step: {:?}", step.name);
                    continue;
                }

                if step.prompt_mode.as_deref() == Some("response") {
                    let step_name = step.name.as_deref().unwrap_or("Response Prompt");
                    info!(
                        "SETUP-PHASE: Executing response-mode prompt step: {}",
                        step_name
                    );

                    // Checkpoint the response-mode prompt step as "running"
                    let step_idx = automation_steps.len() + response_step_count;
                    let mut resp_checkpoint = StepCheckpoint::new(
                        execution_id,
                        "unified",
                        "setup",
                        Some(0),
                        step_idx,
                        "prompt",
                    )
                    .with_step_name(step_name)
                    .with_stage_index(stage_index);
                    resp_checkpoint.mark_started();
                    if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                        warn!("Failed to save setup response-mode step checkpoint: {}", e);
                    }

                    // Log start event for Active Dashboard visibility
                    let metadata =
                        StepMetadata::setup(execution_id, StepType::Prompt, step_name, step_idx);
                    if let Err(e) = logger.log_start(
                        StepEventKind::SetupAiStart,
                        metadata,
                        StepDetails::default(),
                    ) {
                        warn!("Failed to log setup AI step start event: {}", e);
                    }

                    // Step-level overrides take precedence over phase-level
                    let step_model = step.model.clone().or_else(|| model_override.clone());
                    let step_provider = step.provider.clone().or_else(|| provider_override.clone());

                    // Retry loop for interruption resilience.
                    // Uses the step's retry_count if configured, otherwise falls back to default.
                    let mut retry_count = 0u32;
                    let max_retries = step.retry_count.unwrap_or(2);
                    let retry_delay_ms = step.retry_delay_ms.unwrap_or(10_000);
                    let overall_start = std::time::Instant::now();
                    let resp_result = loop {
                        let doctor_handle = self.app_state.doctor_handle.lock().await.clone();
                        let start = std::time::Instant::now();
                        match execute_prompt_response_mode(
                            step,
                            &self.checkpoint_db,
                            Some(execution_id),
                            doctor_handle,
                            step_model.clone(),
                            step_provider.clone(),
                            None,
                            None,
                            None,
                            None,
                        )
                        .await
                        {
                            Ok(resp) => break Ok((resp, start)),
                            Err(e) => {
                                let duration_ms = start.elapsed().as_millis() as u64;
                                if duration_ms < 5000 && retry_count < max_retries {
                                    retry_count += 1;
                                    let delay_secs = retry_delay_ms as f64 / 1000.0;
                                    warn!(
                                        "SETUP-PHASE: Step '{}' appears interrupted ({}ms < 5s), retry {}/{} after {}s delay",
                                        step_name, duration_ms, retry_count, max_retries, delay_secs
                                    );
                                    tokio::time::sleep(std::time::Duration::from_millis(
                                        retry_delay_ms,
                                    ))
                                    .await;
                                    continue;
                                }
                                break Err(e);
                            }
                        }
                    };

                    match resp_result {
                        Ok((resp, start)) => {
                            let duration_ms = start.elapsed().as_millis() as u64;
                            record_phase_token_usage(
                                &self.checkpoint_db,
                                execution_id,
                                "setup",
                                stage_index,
                                Some(0),
                                step_model.as_deref(),
                                step_provider.as_deref(),
                                resp.input_tokens,
                                resp.output_tokens,
                                Some(duration_ms),
                            );
                            let output = resp.output;
                            info!(
                                "SETUP-PHASE: Response-mode step '{}' completed successfully ({} bytes)",
                                step_name,
                                output.len()
                            );
                            // Persist AI output to chunks for the /output endpoint
                            if !output.is_empty() {
                                let formatted = format!(
                                    "\n--- AI Setup Output ({}) ---\n{}\n",
                                    step_name, output
                                );
                                if let Err(e) = self.checkpoint_db.append_task_output_ex(
                                    execution_id,
                                    &formatted,
                                    false,
                                    false,
                                ) {
                                    warn!("Failed to persist setup response-mode AI output to chunks: {}", e);
                                }
                            }
                            // Save completion checkpoint
                            let mut resp_checkpoint = StepCheckpoint::new(
                                execution_id,
                                "unified",
                                "setup",
                                Some(0),
                                step_idx,
                                "prompt",
                            )
                            .with_step_name(step_name)
                            .with_stage_index(stage_index);
                            resp_checkpoint.mark_success(Some(output.clone()), duration_ms as i64);
                            if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                                warn!("Failed to save setup response-mode step completion checkpoint: {}", e);
                            }

                            // Log complete event for Active Dashboard visibility
                            let metadata = StepMetadata::setup(
                                execution_id,
                                StepType::Prompt,
                                step_name,
                                step_idx,
                            );
                            if let Err(e) = logger.log_complete(
                                StepEventKind::SetupAiComplete,
                                metadata,
                                StepDetails::default(),
                                duration_ms as i64,
                            ) {
                                warn!("Failed to log setup AI step complete event: {}", e);
                            }
                            response_step_count += 1;
                            all_results.push(StepExecutionResult {
                                step_index: all_results.len(),
                                step_type: "prompt".to_string(),
                                step_name: step_name.to_string(),
                                step_id: step.id.clone(),
                                success: true,
                                error: None,
                                screenshot_path: None,
                                started_at: None,
                                ended_at: None,
                                duration_ms,
                                config: crate::step_executor::StepExecutionConfig::default(),
                                verification_details: None,
                                output_data: Some(serde_json::json!({ "output": output })),
                                required: step.required,
                                resolved_inputs: None,
                                extracted_values: None,
                                failure_category: None,
                                interrupted: None,
                            });
                        }
                        Err(e) => {
                            let duration_ms = overall_start.elapsed().as_millis() as u64;
                            response_step_count += 1; // Increment to avoid step_index collisions with subsequent steps
                            warn!(
                                "SETUP-PHASE: Response-mode step '{}' failed: {}",
                                step_name, e
                            );
                            // Save failure checkpoint
                            let mut resp_checkpoint = StepCheckpoint::new(
                                execution_id,
                                "unified",
                                "setup",
                                Some(0),
                                step_idx,
                                "prompt",
                            )
                            .with_step_name(step_name)
                            .with_stage_index(stage_index);
                            resp_checkpoint.mark_failed(&e, duration_ms as i64);
                            if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                                warn!("Failed to save setup response-mode step failure checkpoint: {}", e);
                            }

                            // Log error event for Active Dashboard visibility
                            let metadata = StepMetadata::setup(
                                execution_id,
                                StepType::Prompt,
                                step_name,
                                step_idx,
                            );
                            if let Err(log_err) = logger.log_error(
                                StepEventKind::SetupAiError,
                                metadata,
                                StepDetails::default(),
                                duration_ms as i64,
                                Some(&e),
                            ) {
                                warn!("Failed to log setup AI step error event: {}", log_err);
                            }

                            let is_required = step.required.unwrap_or(true);
                            all_results.push(StepExecutionResult {
                                step_index: all_results.len(),
                                step_type: "prompt".to_string(),
                                step_name: step_name.to_string(),
                                step_id: step.id.clone(),
                                success: false,
                                error: Some(e),
                                screenshot_path: None,
                                started_at: None,
                                ended_at: None,
                                duration_ms,
                                config: crate::step_executor::StepExecutionConfig::default(),
                                verification_details: None,
                                output_data: None,
                                required: step.required,
                                resolved_inputs: None,
                                extracted_values: None,
                                failure_category: None,
                                interrupted: Some(true),
                            });
                            if is_required {
                                return (false, all_results);
                            } else {
                                warn!(
                                    "SETUP-PHASE: Non-required response-mode step '{}' failed, continuing",
                                    step_name
                                );
                                // Non-required step failure doesn't affect overall_success
                            }
                        }
                    }
                } else {
                    session_prompt_steps.push(step.clone());
                }
            }

            // Run remaining session-mode prompt steps via consolidated AI session
            if !session_prompt_steps.is_empty() {
                // Checkpoint the AI step as a single step (after any response-mode steps)
                let ai_step_idx = automation_steps.len() + response_step_count;
                let step_name = prompt_builder::consolidate_step_names_with_default(
                    &session_prompt_steps,
                    "Setup AI Task",
                );

                // Use Some(0) instead of None for iteration to ensure SQLite's
                // UNIQUE constraint works correctly (NULL != NULL in SQLite)
                let mut ai_checkpoint = StepCheckpoint::new(
                    execution_id,
                    "unified",
                    "setup",
                    Some(0),
                    ai_step_idx,
                    "ai_session",
                )
                .with_step_name(&step_name)
                .with_stage_index(stage_index);
                ai_checkpoint.mark_started();
                if let Err(e) = checkpoint_mgr.save_step(&ai_checkpoint) {
                    warn!("Failed to save setup AI step checkpoint: {}", e);
                }

                // Log start event for Active Dashboard visibility
                {
                    let metadata = StepMetadata::setup(
                        execution_id,
                        StepType::Prompt,
                        &step_name,
                        ai_step_idx,
                    );
                    if let Err(e) = logger.log_start(
                        StepEventKind::SetupAiStart,
                        metadata,
                        StepDetails::default(),
                    ) {
                        warn!("Failed to log setup AI session start event: {}", e);
                    }
                }

                // Use structured prompts for granular sub-step tracking
                let (setup_prompt, sub_step_metadata) =
                    prompt_builder::consolidate_prompts_structured(&session_prompt_steps, "setup");

                if !setup_prompt.is_empty() {
                    // Use the unified AI session executor with sub-step metadata
                    let config = AiSessionConfig::setup(execution_id, workflow_name, &step_name)
                        .with_checkpoint_id(&ai_checkpoint.id)
                        .with_sub_step_metadata(sub_step_metadata)
                        .with_model_override(model_override.clone());

                    let (result, duration_ms) = timeout_helper::timed_result_async(
                        self.ai_executor.execute(&config, &setup_prompt, logger),
                    )
                    .await;
                    let duration_ms = duration_ms as i64;
                    // Only fail overall setup if at least one session-mode step is required.
                    // Non-required steps failing should not block the setup phase.
                    let any_required = session_prompt_steps
                        .iter()
                        .any(|s| s.required.unwrap_or(true));
                    if any_required {
                        overall_success = overall_success && result.success;
                    }
                    // Use Some(0) instead of None for iteration to ensure SQLite's
                    // UNIQUE constraint works correctly (NULL != NULL in SQLite)
                    let mut ai_checkpoint = StepCheckpoint::new(
                        execution_id,
                        "unified",
                        "setup",
                        Some(0),
                        ai_step_idx,
                        "ai_session",
                    )
                    .with_step_name(&step_name)
                    .with_stage_index(stage_index);

                    if result.success {
                        ai_checkpoint.mark_success(Some(result.output.clone()), duration_ms);
                    } else {
                        ai_checkpoint.mark_failed("AI session failed", duration_ms);
                    }

                    if let Err(e) = checkpoint_mgr.save_step(&ai_checkpoint) {
                        warn!("Failed to save setup AI step completion checkpoint: {}", e);
                    }

                    // Log complete/error event for Active Dashboard visibility
                    {
                        let metadata = StepMetadata::setup(
                            execution_id,
                            StepType::Prompt,
                            &step_name,
                            ai_step_idx,
                        );
                        if result.success {
                            if let Err(e) = logger.log_complete(
                                StepEventKind::SetupAiComplete,
                                metadata,
                                StepDetails::default(),
                                duration_ms,
                            ) {
                                warn!("Failed to log setup AI session complete event: {}", e);
                            }
                        } else if let Err(e) = logger.log_error(
                            StepEventKind::SetupAiError,
                            metadata,
                            StepDetails::default(),
                            duration_ms,
                            Some("AI session failed"),
                        ) {
                            warn!("Failed to log setup AI session error event: {}", e);
                        }
                    }

                    if !result.success {
                        warn!("SETUP-PHASE: AI prompt steps failed");
                    }
                }
            }
        }

        if automation_steps.is_empty() && prompt_steps.is_empty() {
            info!("SETUP-PHASE: No setup steps to execute");
        } else {
            info!("SETUP-PHASE: Completed with success={}", overall_success);
        }

        (overall_success, all_results)
    }

    /// Run setup and return a unified ExecutionOutcome.
    ///
    /// This uses the IntoOutcome trait to convert the SetupResult into a
    /// standardized ExecutionOutcome, which is useful for consistent result handling.
    ///
    /// # Arguments
    /// * `config` - The setup configuration
    /// * `logger` - Logger for step events
    ///
    /// # Returns
    /// An `ExecutionOutcome` summarizing the setup phase execution.
    pub async fn run_setup_to_outcome(
        &self,
        config: &SetupConfig,
        logger: &StepEventLogger,
    ) -> ExecutionOutcome {
        let start = std::time::Instant::now();

        let (success, step_results) = self
            .run_setup(
                &config.automation_steps,
                &config.prompt_steps,
                &config.execution_id,
                &config.workflow_name,
                logger,
                None,
                config.model_override.clone(),
                config.provider_override.clone(),
            )
            .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        // Use the IntoOutcome trait for consistent conversion
        let result = SetupResult {
            success,
            step_results,
        };
        result.into_outcome(duration_ms)
    }
}

// =============================================================================
// Verification Phase Executor
// =============================================================================

/// Executes verification steps and determines if they all pass.
pub struct VerificationExecutor {
    executor: StepExecutor,
    checkpoint_db: Arc<CheckpointDb>,
}

impl VerificationExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
    ) -> Self {
        let checkpoint_db = app_state.checkpoint_db.clone();
        Self {
            executor: StepExecutor::with_app_handle(app_state.clone(), config_storage, app_handle),
            checkpoint_db,
        }
    }

    /// Set the task run ID on the inner step executor for database logging.
    pub fn set_task_run_id(&mut self, task_run_id: String) {
        self.executor.set_task_run_id(task_run_id);
    }

    /// Run verification steps.
    ///
    /// Returns (verification_result, step_results)
    /// The logger is required for consistent step event logging.
    ///
    /// Step checkpointing is now integrated to enable resume after crashes:
    /// - Each step is checkpointed before (running) and after (success/failed) execution
    /// - On resume, completed steps can be skipped based on checkpoint data
    #[instrument(
        name = "qontinui.workflow.phase.verification",
        skip(self, steps, logger),
        fields(
            execution_id = %execution_id,
            iteration = iteration,
            workflow_name = %workflow_name,
            step_count = steps.len()
        )
    )]
    pub async fn run_verification(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        iteration: u32,
        workflow_name: &str,
        logger: &StepEventLogger,
        stage_index: Option<u32>,
    ) -> (VerificationPhaseResult, Vec<StepExecutionResult>) {
        // Filter out dev_mode_only steps when not in dev mode
        let steps: Vec<ExecutionStepConfig> = steps
            .iter()
            .filter(|step| {
                if step.dev_mode_only.unwrap_or(false) && !cfg!(debug_assertions) {
                    info!(
                        "VERIFICATION-PHASE: Skipping dev-mode-only step: {:?}",
                        step.name
                    );
                    false
                } else {
                    true
                }
            })
            .cloned()
            .collect();
        let steps = steps.as_slice();

        // Clear console errors before running verification so each iteration
        // only captures its own errors
        clear_console_errors().await;

        // Record verification start time for browser event filtering
        let verification_start_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        if steps.is_empty() {
            info!(
                "VERIFICATION-PHASE: No verification steps defined (iteration {})",
                iteration
            );
            // No verification steps = verification passes
            return (
                VerificationPhaseResult {
                    iteration,
                    all_passed: true,
                    critical_failure: false,
                    total_steps: 0,
                    passed_steps: 0,
                    failed_steps: 0,
                    skipped_steps: 0,
                    total_duration_ms: 0,
                    step_results: Vec::new(),
                    console_errors: None,
                    app_health: None,
                    browser_events: None,
                    network_failures: None,
                },
                Vec::new(),
            );
        }

        // Pre-verification health check: detect if the SDK app is already broken
        // (e.g., showing a framework error overlay) before running expensive verification steps.
        // This gives the AI faster feedback about app-breaking changes.
        let pre_check_health = fetch_health_from_ui_bridge().await;
        if let Some(ref health) = pre_check_health {
            let status = health
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            if status == "broken" {
                let summary = health.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                warn!(
                    "VERIFICATION-PHASE: SDK app health is BROKEN before verification (iteration {}): {}",
                    iteration, summary
                );
            }
        }

        info!(
            "VERIFICATION-PHASE: Running {} steps (iteration {})",
            steps.len(),
            iteration
        );

        // Create checkpoint manager for step-level checkpointing
        let checkpoint_mgr = CheckpointManager::new(self.checkpoint_db.clone(), "unified");

        // Log START events and save checkpoints for each step before execution
        for (idx, step) in steps.iter().enumerate() {
            let step_type =
                StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Playwright);
            let step_name = step.name.as_deref().unwrap_or(&step.step_type);
            let metadata =
                StepMetadata::verification(execution_id, step_type, step_name, idx, iteration);
            let details = StepDetails::default();

            if let Err(e) =
                logger.log_start(StepEventKind::VerificationStepStart, metadata, details)
            {
                warn!("Failed to log verification step start event: {}", e);
            }

            // Save step checkpoint as "running"
            let mut checkpoint = StepCheckpoint::new(
                execution_id,
                "unified",
                "verification",
                Some(iteration),
                idx,
                step_type.as_str(),
            )
            .with_step_name(step_name)
            .with_stage_index(stage_index);
            checkpoint.mark_started();
            if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                warn!("Failed to save verification step checkpoint: {}", e);
            }
        }

        // Use the new method that emits completion events as each step finishes
        // This allows the UI to show real-time progress instead of waiting for all steps
        let result = self
            .executor
            .execute_verification_steps_with_events(
                steps,
                execution_id,
                iteration,
                Some(workflow_name),
            )
            .await;

        info!(
            "VERIFICATION-PHASE: Iteration {} result: all_passed={}, critical_failure={}, passed={}/{}, failed={}",
            iteration,
            result.all_passed,
            result.critical_failure,
            result.passed_steps,
            result.total_steps,
            result.failed_steps
        );

        // Save completion checkpoints for each step
        for (idx, step_result) in result.step_results.iter().enumerate() {
            let step = &steps[idx];
            let step_type =
                StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Playwright);
            let step_name = step.name.as_deref().unwrap_or(&step.step_type);

            let mut checkpoint = StepCheckpoint::new(
                execution_id,
                "unified",
                "verification",
                Some(iteration),
                idx,
                step_type.as_str(),
            )
            .with_step_name(step_name)
            .with_stage_index(stage_index);

            let duration_ms = step_result.duration_ms as i64;
            if step_result.success {
                checkpoint.mark_success(serde_json::to_string(step_result).ok(), duration_ms);
            } else {
                checkpoint.mark_failed(
                    step_result.error.as_deref().unwrap_or("Unknown error"),
                    duration_ms,
                );
            }

            if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                warn!(
                    "Failed to save verification step completion checkpoint: {}",
                    e
                );
            }
        }

        // Fetch UI Bridge diagnostics concurrently (all best-effort)
        let (console_errors_result, app_health, browser_events, network_failures) = tokio::join!(
            fetch_console_errors_from_ui_bridge(),
            fetch_health_from_ui_bridge(),
            fetch_browser_events_from_ui_bridge(verification_start_ms),
            fetch_network_failures_from_ui_bridge(verification_start_ms),
        );

        let console_errors = match console_errors_result {
            Ok(errors) => {
                if !errors.is_empty() {
                    debug!(
                        "VERIFICATION-PHASE: Captured {} console errors during verification",
                        errors.len()
                    );
                }
                Some(errors).filter(|e| !e.is_empty())
            }
            Err(e) => {
                debug!("VERIFICATION-PHASE: Could not fetch console errors: {}", e);
                None
            }
        };

        if app_health.is_some() || !browser_events.is_empty() || !network_failures.is_empty() {
            debug!(
                "VERIFICATION-PHASE: UI Bridge diagnostics: health={}, browser_events={}, network_failures={}",
                app_health.is_some(),
                browser_events.len(),
                network_failures.len()
            );
        }

        // Use post-verification health if available, fall back to pre-check health
        let effective_health = app_health.or(pre_check_health);

        let mut result = result;
        result.console_errors = console_errors;
        result.app_health = effective_health;
        result.browser_events = Some(browser_events).filter(|e| !e.is_empty());
        result.network_failures = Some(network_failures).filter(|e| !e.is_empty());

        let step_results = result.step_results.clone();
        (result, step_results)
    }

    /// Run verification and return a unified ExecutionOutcome.
    ///
    /// This uses the IntoOutcome trait to convert the VerificationResult into a
    /// standardized ExecutionOutcome, which is useful for consistent result handling.
    ///
    /// # Arguments
    /// * `config` - The verification configuration
    /// * `logger` - Logger for step events
    ///
    /// # Returns
    /// An `ExecutionOutcome` summarizing the verification phase execution.
    pub async fn run_verification_to_outcome(
        &self,
        config: &VerificationConfig,
        logger: &StepEventLogger,
    ) -> ExecutionOutcome {
        let start = std::time::Instant::now();

        let (phase_result, step_results) = self
            .run_verification(
                &config.steps,
                &config.execution_id,
                config.iteration,
                &config.workflow_name,
                logger,
                None,
            )
            .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        // Use the IntoOutcome trait for consistent conversion
        let result = VerificationResult {
            phase_result,
            step_results,
        };
        result.into_outcome(duration_ms)
    }

    /// Run a subset of verification steps by their indices.
    ///
    /// Used by the multi-agent fixer to run targeted verification after each
    /// fix agent completes, providing fast feedback without running all steps.
    pub async fn run_targeted_verification(
        &self,
        all_steps: &[ExecutionStepConfig],
        step_indices: &[usize],
        execution_id: &str,
        iteration: u32,
        workflow_name: &str,
    ) -> VerificationPhaseResult {
        let targeted_steps: Vec<ExecutionStepConfig> = step_indices
            .iter()
            .filter_map(|&idx| all_steps.get(idx).cloned())
            .collect();

        if targeted_steps.is_empty() {
            return VerificationPhaseResult {
                iteration,
                all_passed: true,
                total_steps: 0,
                passed_steps: 0,
                failed_steps: 0,
                skipped_steps: 0,
                total_duration_ms: 0,
                step_results: vec![],
                critical_failure: false,
                console_errors: None,
                app_health: None,
                browser_events: None,
                network_failures: None,
            };
        }

        info!(
            "MULTI-AGENT: Running targeted verification for {} step(s) (iteration {})",
            targeted_steps.len(),
            iteration
        );

        let result = self
            .executor
            .execute_verification_steps_with_events(
                &targeted_steps,
                execution_id,
                iteration,
                Some(workflow_name),
            )
            .await;

        info!(
            "MULTI-AGENT: Targeted verification: passed={}/{}, failed={}",
            result.passed_steps, result.total_steps, result.failed_steps
        );

        result
    }
}

// =============================================================================
// Agentic Phase Executor
// =============================================================================

/// Executes the AI agentic phase with failure context.
/// AI session execution is delegated to the UnifiedAiSessionExecutor.
pub struct AgenticExecutor {
    app_state: Arc<AppState>,
    ai_executor: UnifiedAiSessionExecutor,
    checkpoint_db: Arc<CheckpointDb>,
    reflection_fix_ctx: Option<crate::mcp::shared::ReflectionFixContext>,
    step_injection_ctx: Option<crate::step_injection::types::StepInjectionContext>,
}

impl AgenticExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        let checkpoint_db = app_state.checkpoint_db.clone();
        Self {
            app_state: app_state.clone(),
            ai_executor: UnifiedAiSessionExecutor::new(app_state, app_handle, pid_tracker),
            checkpoint_db,
            reflection_fix_ctx: None,
            step_injection_ctx: None,
        }
    }

    /// Enable interactive sessions via the session manager.
    pub fn set_session_manager(&mut self, sm: Arc<crate::claude_session::SessionManager>) {
        self.ai_executor.session_manager = Some(sm);
    }

    /// Set the reflection fix context for parsing [REFLECTION_FIX:...] markers.
    pub fn set_reflection_fix_ctx(&mut self, ctx: crate::mcp::shared::ReflectionFixContext) {
        self.reflection_fix_ctx = Some(ctx);
    }

    /// Set the step injection context for parsing [INJECT_STEP]...[/INJECT_STEP] markers.
    pub fn set_step_injection_ctx(
        &mut self,
        ctx: crate::step_injection::types::StepInjectionContext,
    ) {
        self.step_injection_ctx = Some(ctx);
    }

    /// Run the AI with the given prompt and failure context.
    ///
    /// This calls Claude directly (no session system, no orchestrator).
    /// The logger is required for consistent step event logging.
    ///
    /// Step checkpointing is integrated for resume capability.
    /// Progress markers from previous sessions are included in the context
    /// to help the AI understand where to resume long operations.
    #[instrument(
        name = "qontinui.workflow.phase.agentic",
        skip(self, config, failure_context, agentic_steps, logger),
        fields(
            execution_id = %config.execution_id,
            iteration = iteration,
            workflow_name = %config.workflow_name,
            has_steps = has_agentic_steps
        )
    )]
    pub async fn run_agentic(
        &self,
        config: &LoopConfig,
        iteration: u32,
        failure_context: &str,
        has_agentic_steps: bool,
        agentic_steps: &[ExecutionStepConfig],
        logger: &StepEventLogger,
    ) -> (AgenticOutcome, Vec<ExecutionStepConfig>) {
        if !has_agentic_steps && config.base_prompt.is_empty() {
            info!(
                "AGENTIC-PHASE: No agentic steps and no base prompt, skipping (iteration {})",
                iteration
            );
            return (AgenticOutcome::Skipped, Vec::new());
        }

        // Filter out dev_mode_only steps when not in dev mode
        let agentic_steps: Vec<ExecutionStepConfig> = agentic_steps
            .iter()
            .filter(|step| {
                if step.dev_mode_only.unwrap_or(false) && !cfg!(debug_assertions) {
                    info!(
                        "AGENTIC-PHASE: Skipping dev-mode-only step: {:?}",
                        step.name
                    );
                    false
                } else {
                    true
                }
            })
            .cloned()
            .collect();
        let agentic_steps = agentic_steps.as_slice();

        if agentic_steps.is_empty() && config.base_prompt.is_empty() {
            info!(
                "AGENTIC-PHASE: No remaining agentic steps and no base prompt, skipping (iteration {})",
                iteration
            );
            return (AgenticOutcome::Skipped, Vec::new());
        }

        // Check if any agentic step uses response mode (only relevant when steps exist)
        let has_response_mode = !agentic_steps.is_empty()
            && agentic_steps
                .iter()
                .any(|s| s.prompt_mode.as_deref() == Some("response"));

        // If response mode, handle with simple prompt->response instead of full session
        if has_response_mode {
            info!(
                "AGENTIC-PHASE: Using response mode for iteration {}",
                iteration
            );

            // Emit start event for Active Dashboard
            let parent_id = get_parent_task_id(&config.execution_id);
            let resp_action_id = format!("agentic-response-{}-0", parent_id);
            let resp_start_event = CreateTaskRunEventInput {
                task_run_id: parent_id.clone(),
                event_type: "step_execution".to_string(),
                event_subtype: Some("start".to_string()),
                message: format!(
                    "Starting agentic response-mode prompt (iteration {})",
                    iteration
                ),
                data: Some(
                    serde_json::to_string(&serde_json::json!({
                        "step_index": 0,
                        "step_type": "prompt",
                        "step_name": "Agentic Response Prompt",
                        "phase": "agentic",
                        "iteration": iteration,
                    }))
                    .unwrap_or_default(),
                ),
                workflow_name: None,
                state_name: None,
                action_id: Some(resp_action_id.clone()),
                timestamp: chrono::Utc::now().to_rfc3339(),
                duration_ms: None,
            };
            if let Err(e) = self.checkpoint_db.create_task_run_event(&resp_start_event) {
                warn!("Failed to emit agentic response-mode start event: {}", e);
            }
            let resp_mode_start = std::time::Instant::now();

            // Build a temporary step with failure context appended to the prompt
            for step in agentic_steps {
                if step.prompt_mode.as_deref() != Some("response") {
                    continue;
                }

                let step_name = step.name.as_deref().unwrap_or("Agentic Response Prompt");

                // Checkpoint the response-mode agentic step as "running"
                let checkpoint_mgr = CheckpointManager::new(self.checkpoint_db.clone(), "unified");
                let mut resp_checkpoint = StepCheckpoint::new(
                    &config.execution_id,
                    "unified",
                    "agentic",
                    Some(iteration),
                    0,
                    "prompt",
                )
                .with_step_name(step_name)
                .with_stage_index(config.stage_index);
                resp_checkpoint.mark_started();
                if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                    warn!(
                        "Failed to save agentic response-mode step checkpoint: {}",
                        e
                    );
                }

                // Create a modified step with failure context appended to the prompt
                let mut modified_step = step.clone();
                let base_prompt = modified_step.prompt_content.clone().unwrap_or_default();
                let enhanced = if failure_context.is_empty() {
                    base_prompt
                } else {
                    format!(
                        "{}\n\n---\n\nThe following verification checks FAILED. Please fix these issues:\n\n{}\n\nFix the issues above and ensure all checks pass.",
                        base_prompt, failure_context
                    )
                };
                modified_step.prompt_content = Some(enhanced);

                let doctor_handle = self.app_state.doctor_handle.lock().await.clone();
                let start = std::time::Instant::now();
                // Step-level overrides take precedence over phase-level
                let step_model = modified_step
                    .model
                    .clone()
                    .or_else(|| config.resolve_model_for_phase("agentic"));
                let step_provider = modified_step
                    .provider
                    .clone()
                    .or_else(|| config.resolve_provider_for_phase("agentic"));
                match execute_prompt_response_mode(
                    &modified_step,
                    &self.checkpoint_db,
                    Some(&config.execution_id),
                    doctor_handle,
                    step_model.clone(),
                    step_provider.clone(),
                    config.resolve_temperature_for_phase("agentic"),
                    config.resolve_max_tokens_for_phase("agentic"),
                    config.resolve_fallback_model_for_phase("agentic"),
                    config.resolve_fallback_provider_for_phase("agentic"),
                )
                .await
                {
                    Ok(resp) => {
                        let duration_ms = start.elapsed().as_millis() as u64;
                        record_phase_token_usage(
                            &self.checkpoint_db,
                            &config.execution_id,
                            "agentic",
                            config.stage_index,
                            Some(iteration),
                            step_model.as_deref(),
                            step_provider.as_deref(),
                            resp.input_tokens,
                            resp.output_tokens,
                            Some(duration_ms),
                        );
                        let output = resp.output;
                        info!(
                            "AGENTIC-PHASE: Response-mode step '{}' completed ({} bytes, {}ms)",
                            step_name,
                            output.len(),
                            duration_ms
                        );
                        // Save completion checkpoint
                        let mut resp_checkpoint = StepCheckpoint::new(
                            &config.execution_id,
                            "unified",
                            "agentic",
                            Some(iteration),
                            0,
                            "prompt",
                        )
                        .with_step_name(step_name)
                        .with_stage_index(config.stage_index);
                        resp_checkpoint.mark_success(Some(output.clone()), duration_ms as i64);
                        if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                            warn!("Failed to save agentic response-mode step completion checkpoint: {}", e);
                        }
                        // Emit completion event for Active Dashboard
                        let resp_duration = resp_mode_start.elapsed().as_millis() as i64;
                        let complete_event = CreateTaskRunEventInput {
                            task_run_id: parent_id.clone(),
                            event_type: "step_execution".to_string(),
                            event_subtype: Some("complete".to_string()),
                            message: format!(
                                "Agentic response-mode completed (iteration {}, {}ms)",
                                iteration, resp_duration
                            ),
                            data: Some(
                                serde_json::to_string(&serde_json::json!({
                                    "step_index": 0,
                                    "step_type": "prompt",
                                    "step_name": "Agentic Response Prompt",
                                    "phase": "agentic",
                                    "iteration": iteration,
                                    "success": true,
                                }))
                                .unwrap_or_default(),
                            ),
                            workflow_name: None,
                            state_name: None,
                            action_id: Some(resp_action_id.clone()),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            duration_ms: Some(resp_duration),
                        };
                        if let Err(e) = self.checkpoint_db.create_task_run_event(&complete_event) {
                            warn!(
                                "Failed to emit agentic response-mode completion event: {}",
                                e
                            );
                        }
                        // Response-mode steps produce raw text output without structured
                        // agentic markers, so there is no AgenticPhaseOutput to parse.
                        // The loop controller handles `parsed: None` gracefully by falling
                        // back to raw marker checks for unfixable errors, etc.
                        return (
                            AgenticOutcome::Success {
                                output,
                                parsed: None,
                            },
                            Vec::new(),
                        );
                    }
                    Err(e) => {
                        let duration_ms = start.elapsed().as_millis() as u64;
                        warn!(
                            "AGENTIC-PHASE: Response-mode step '{}' failed ({}ms): {}",
                            step_name, duration_ms, e
                        );
                        // Save failure checkpoint
                        let mut resp_checkpoint = StepCheckpoint::new(
                            &config.execution_id,
                            "unified",
                            "agentic",
                            Some(iteration),
                            0,
                            "prompt",
                        )
                        .with_step_name(step_name)
                        .with_stage_index(config.stage_index);
                        resp_checkpoint.mark_failed(&e, duration_ms as i64);
                        if let Err(e2) = checkpoint_mgr.save_step(&resp_checkpoint) {
                            warn!(
                                "Failed to save agentic response-mode step failure checkpoint: {}",
                                e2
                            );
                        }
                        // Emit error event for Active Dashboard
                        let resp_duration = resp_mode_start.elapsed().as_millis() as i64;
                        let error_event = CreateTaskRunEventInput {
                            task_run_id: parent_id.clone(),
                            event_type: "step_execution".to_string(),
                            event_subtype: Some("error".to_string()),
                            message: format!(
                                "Agentic response-mode failed (iteration {}): {}",
                                iteration, e
                            ),
                            data: Some(
                                serde_json::to_string(&serde_json::json!({
                                    "step_index": 0,
                                    "step_type": "prompt",
                                    "step_name": "Agentic Response Prompt",
                                    "phase": "agentic",
                                    "iteration": iteration,
                                    "success": false,
                                }))
                                .unwrap_or_default(),
                            ),
                            workflow_name: None,
                            state_name: None,
                            action_id: Some(resp_action_id.clone()),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            duration_ms: Some(resp_duration),
                        };
                        if let Err(e2) = self.checkpoint_db.create_task_run_event(&error_event) {
                            warn!("Failed to emit agentic response-mode error event: {}", e2);
                        }
                        return (AgenticOutcome::Error { error: e }, Vec::new());
                    }
                }
            }

            // If we get here, no response-mode steps were found (shouldn't happen)
            return (AgenticOutcome::Skipped, Vec::new());
        }

        // Create checkpoint manager for step-level checkpointing
        let checkpoint_mgr = CheckpointManager::new(self.checkpoint_db.clone(), "unified");

        // Try to get the latest progress marker from previous checkpoints
        // This helps the AI understand where to resume if a previous session was interrupted
        let progress_context = self.get_progress_marker_context(&config.execution_id, iteration);

        // Checkpoint the agentic phase as a single step
        let mut checkpoint = StepCheckpoint::new(
            &config.execution_id,
            "unified",
            "agentic",
            Some(iteration),
            0, // Agentic is a single-step phase
            "ai_session",
        )
        .with_step_name("AI Fixing Issues")
        .with_stage_index(config.stage_index);
        checkpoint.mark_started();
        if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
            warn!("Failed to save agentic step checkpoint: {}", e);
        }

        // Emit step event so the Active Dashboard timeline shows the agentic phase
        let parent_id = get_parent_task_id(&config.execution_id);
        let action_id = format!("agentic-ai_session-{}-0", parent_id);
        let start_event = CreateTaskRunEventInput {
            task_run_id: parent_id.clone(),
            event_type: "step_execution".to_string(),
            event_subtype: Some("start".to_string()),
            message: format!("Starting agentic AI session (iteration {})", iteration),
            data: Some(
                serde_json::to_string(&serde_json::json!({
                    "step_index": 0,
                    "step_type": "ai_session",
                    "step_name": "AI Fixing Issues",
                    "phase": "agentic",
                    "iteration": iteration,
                }))
                .unwrap_or_default(),
            ),
            workflow_name: None,
            state_name: None,
            action_id: Some(action_id.clone()),
            timestamp: chrono::Utc::now().to_rfc3339(),
            duration_ms: None,
        };
        if let Err(e) = self.checkpoint_db.create_task_run_event(&start_event) {
            warn!("Failed to emit agentic start event: {}", e);
        }

        let agentic_start = std::time::Instant::now();

        // Build enhanced prompt with failure context and progress marker
        // Note: The UnifiedAiSessionExecutor will handle:
        // - Adding autonomous context (configured in AiSessionConfig::agentic)
        // - Stripping completion markers
        // - Appending finding instructions
        let enhanced_prompt = if failure_context.is_empty() {
            warn!(
                "AGENTIC-PHASE: No failure context provided for iteration {} - AI won't know what to fix!",
                iteration
            );
            // Still include progress context if available
            if let Some(ref progress) = progress_context {
                format!("{}\n\n{}", config.base_prompt, progress)
            } else {
                config.base_prompt.clone()
            }
        } else {
            info!(
                "AGENTIC-PHASE: Building prompt with {} chars of failure context (iteration {})",
                failure_context.len(),
                iteration
            );
            let base = if config.reflection_mode {
                format!(
                    "{}\n\n---\n\n{}\n\nThe following verification checks FAILED:\n\n{}\n\nAfter your investigation, implement fixes that address root causes.",
                    config.base_prompt, REFLECTION_MODE_PREAMBLE, failure_context
                )
            } else {
                format!(
                    "{}\n\n---\n\nThe following verification checks FAILED. Please fix these issues:\n\n{}\n\nFix the issues above and ensure all checks pass.",
                    config.base_prompt, failure_context
                )
            };
            // Append progress context if available
            if let Some(ref progress) = progress_context {
                format!("{}\n\n{}", base, progress)
            } else {
                base
            }
        };

        // Append execution timing context if available (from iteration 2+ or cross-stage)
        let enhanced_prompt = if iteration > 1 || config.stage_index.is_some_and(|idx| idx > 0) {
            match build_execution_timing_context(&self.checkpoint_db, &config.execution_id) {
                Some(timing) => {
                    info!(
                        "AGENTIC-PHASE: Appending execution timing context ({} chars)",
                        timing.len()
                    );
                    format!("{}\n\n{}", enhanced_prompt, timing)
                }
                None => enhanced_prompt,
            }
        } else {
            enhanced_prompt
        };

        // Pre-build managed process status summary for iteration context
        let process_status_summary = {
            let mgr_lock = self.app_state.process_capture_manager.lock().await;
            if let Some(ref mgr) = *mgr_lock {
                let statuses = mgr.get_all_status().await;
                if !statuses.is_empty() {
                    let mut lines = vec!["## Managed Process Status".to_string()];
                    lines.push("The following dev processes are being managed:".to_string());
                    for s in &statuses {
                        let health = s
                            .port_healthy
                            .map(|h| {
                                if h {
                                    "port healthy"
                                } else {
                                    "port not responding"
                                }
                            })
                            .unwrap_or("no health check");
                        let uptime = s
                            .uptime_secs
                            .map(|u| format!("uptime {}s", u))
                            .unwrap_or_default();
                        lines.push(format!(
                            "- {} [{}]: {} ({}, {} errors)",
                            s.name,
                            s.category,
                            s.state,
                            if uptime.is_empty() {
                                health.to_string()
                            } else {
                                format!("{}, {}", uptime, health)
                            },
                            s.error_count
                        ));
                    }
                    Some(lines.join("\n"))
                } else {
                    None
                }
            } else {
                None
            }
        };

        // Pre-build error monitor summary for iteration context.
        // Only inject when the workflow is specifically targeting errors (error-fix workflows),
        // otherwise it distracts the AI from its actual task.
        let error_monitor_summary = if !config.targeted_error_ids.is_empty() {
            match self.app_state.checkpoint_db.get_conn() {
                Ok(conn) => {
                    match crate::error_monitor::ErrorEventStorage::get_unresolved(&conn, None, 20) {
                        Ok(errors) if !errors.is_empty() => {
                            let mut lines = vec!["## Recent Errors (Error Monitor)".to_string()];
                            lines.push(format!(
                                "The error monitor has detected {} unresolved error(s):",
                                errors.len()
                            ));
                            for e in errors.iter().take(15) {
                                let count_str = if e.occurrence_count > 1 {
                                    format!(" ({} occurrences)", e.occurrence_count)
                                } else {
                                    String::new()
                                };
                                lines.push(format!(
                                    "- [{}] {}{}",
                                    e.log_source_name,
                                    e.message.chars().take(200).collect::<String>(),
                                    count_str
                                ));
                            }
                            if errors.len() > 15 {
                                lines.push(format!("... and {} more", errors.len() - 15));
                            }
                            Some(lines.join("\n"))
                        }
                        _ => None,
                    }
                }
                Err(_) => None,
            }
        } else {
            None
        };

        // For iteration 2+, add context from previous iterations (findings + verification results)
        // Also for stage > 0 at iteration 1, inject cross-stage context so the AI
        // has visibility into prior stages' findings and knowledge.
        let needs_iteration_context =
            iteration > 1 || config.stage_index.is_some_and(|idx| idx > 0);
        let enhanced_prompt = if needs_iteration_context {
            match build_compressed_iteration_history(
                &self.checkpoint_db,
                &config.execution_id,
                iteration,
                process_status_summary.as_deref(),
                error_monitor_summary.as_deref(),
                Some(&config.workflow_name),
                config.max_context_tokens,
                config.cross_workflow_learning,
                config.project_path.as_deref(),
            ) {
                Some(ctx) => {
                    let label = if iteration == 1 {
                        format!(
                            "cross-stage context for stage {}",
                            config.stage_index.unwrap_or(0)
                        )
                    } else {
                        format!("iteration context for iteration {}", iteration)
                    };
                    info!("AGENTIC-PHASE: Appending {} ({} chars)", label, ctx.len(),);
                    format!("{}\n\n{}", enhanced_prompt, ctx)
                }
                None => enhanced_prompt,
            }
        } else {
            enhanced_prompt
        };

        // Append safety and focus instructions
        let enhanced_prompt = format!(
            "{}\n\n## Important Constraints\n\n\
            - **STAY FOCUSED**: ONLY work on fixing the failed verification checks listed above. Do NOT investigate, diagnose, or fix unrelated errors, warnings, or issues you find in log files or elsewhere.\n\
            - Do NOT modify the runner's SQLite database directly. Configuration changes must go through the runner UI or API.\n\
            - Do NOT modify workflow JSON files in the parent directory. Fix the application code instead.\n\
            - Focus exclusively on the source code that the verification checks are testing. When all checks pass, your work is done.",
            enhanced_prompt
        );

        // === Enrichment #1: Pre-read files referenced in failure context ===
        // Extract file paths from verification failure output and pre-read their contents
        // so the AI has them immediately without needing tool calls.
        let enhanced_prompt = {
            let preread = extract_and_preread_failure_files(
                failure_context,
                config.project_path.as_deref(),
                15,
                300,
                60_000,
            );
            if !preread.is_empty() {
                format!(
                    "{}\n\n## Pre-loaded Source Files\n\nThese files were referenced in the verification failures above. Read them here instead of using tool calls.\n\n{}",
                    enhanced_prompt, preread
                )
            } else {
                enhanced_prompt
            }
        };

        // === Enrichment #2: Pre-read previously edited files ===
        // On iteration 2+, read the current state of files edited in prior iterations
        // so the AI can see the cumulative changes without tool calls.
        let enhanced_prompt = if iteration > 1 {
            let preread = preread_previously_edited_files(
                &self.checkpoint_db,
                &config.execution_id,
                iteration,
                config.project_path.as_deref(),
                10,
                300,
                40_000,
            );
            if !preread.is_empty() {
                format!(
                    "{}\n\n## Previously Edited Files (Current State)\n\nThese files were modified in prior iterations. Their current contents are shown below.\n\n{}",
                    enhanced_prompt, preread
                )
            } else {
                enhanced_prompt
            }
        } else {
            enhanced_prompt
        };

        // Record activity heartbeat before AI session spawn
        {
            let persist_id = super::get_parent_task_id(&config.execution_id);
            let now = chrono::Utc::now().to_rfc3339();
            let ctx_json = serde_json::json!({
                "last_activity": format!("agentic_session_spawn_iter_{}", iteration),
                "last_activity_at": now,
            });
            if let Ok(json) = serde_json::to_string(&ctx_json) {
                let _ = self
                    .checkpoint_db
                    .update_task_run_runtime_context(&persist_id, &json);
            }
        }

        // Use the unified AI session executor with timing
        // Step-level model override takes precedence over phase-level
        let agentic_model = agentic_steps
            .first()
            .and_then(|s| s.model.clone())
            .or_else(|| config.resolve_model_for_phase("agentic"));
        let mut ai_config =
            AiSessionConfig::agentic(&config.execution_id, &config.workflow_name, iteration)
                .with_checkpoint_id(&checkpoint.id)
                .with_model_override(agentic_model);

        // CLI session context for restart survival.
        // Check if there's an interrupted session we can resume via `--resume`.
        // If so, reuse its CLI session ID; otherwise generate a fresh one.
        let parent_task_id = super::get_parent_task_id(&config.execution_id);
        let (cli_session_id, is_resume) = match self.checkpoint_db.get_workflow_ai_session(
            &parent_task_id,
            iteration as i32,
            "agentic",
        ) {
            Ok(Some((prev_cli_id, prev_status))) if prev_status == "interrupted" => {
                info!(
                    "AGENTIC-PHASE: Found interrupted CLI session {} for iteration {} — will resume",
                    prev_cli_id, iteration
                );
                (prev_cli_id, true)
            }
            Err(e) => {
                warn!(
                    "AGENTIC-PHASE: Failed to check for interrupted AI session: {} — starting fresh",
                    e
                );
                (uuid::Uuid::new_v4().to_string(), false)
            }
            _ => (uuid::Uuid::new_v4().to_string(), false),
        };

        ai_config.cli_session_ctx = Some(crate::claude_session::runner::CliSessionContext {
            cli_session_id: cli_session_id.clone(),
            is_resume,
        });

        // Record the AI session in the database for restart recovery
        if let Err(e) = self.checkpoint_db.create_workflow_ai_session(
            &parent_task_id,
            iteration as i32,
            "agentic",
            config.stage_index.map(|i| i as i32),
            &cli_session_id,
        ) {
            warn!("Failed to create workflow AI session record: {}", e);
        }

        // Attach DB flush context for periodic output persistence
        ai_config.db_flush_ctx = Some(crate::claude_session::runner::DbFlushContext {
            db: self.checkpoint_db.clone(),
            task_run_id: parent_task_id.clone(),
            iteration: iteration as i32,
        });

        // Attach reflection fix context if this is a reflection workflow
        if let Some(ref ctx) = self.reflection_fix_ctx {
            ai_config = ai_config.with_reflection_fix_ctx(ctx.clone());
        }

        // Attach step injection context if set
        if let Some(ref ctx) = self.step_injection_ctx {
            ai_config = ai_config.with_step_injection_ctx(ctx.clone());
        }

        // When resuming an interrupted CLI session, send a brief continuation message
        // instead of the full prompt. The CLI already has the full conversation history.
        let resume_prompt = if is_resume {
            let resume_msg = format!(
                "The runner was restarted while you were working on iteration {}. \
                 Your previous Claude Code session has been resumed — you have full context \
                 of everything you did before the interruption. \
                 Continue where you left off. Complete the remaining work for this iteration.",
                iteration
            );
            info!(
                "AGENTIC-PHASE: Using resume prompt ({} chars) instead of full prompt ({} chars)",
                resume_msg.len(),
                enhanced_prompt.len()
            );
            Some(resume_msg)
        } else {
            None
        };
        let final_prompt = resume_prompt.as_deref().unwrap_or(&enhanced_prompt);

        let (mut result, duration) = timeout_helper::timed_result_async(self.ai_executor.execute(
            &ai_config,
            final_prompt,
            logger,
        ))
        .await;
        let mut duration_ms = duration as i64;

        // Fallback: if --resume failed, retry with a fresh session.
        // This handles cases where the CLI session was not persisted, expired, or corrupted.
        // We check for failure regardless of output content, since the CLI may emit
        // error text (e.g., "Error: session not found") as non-empty output.
        if is_resume && !result.success {
            warn!(
                "AGENTIC-PHASE: CLI session resume failed (error: {}, output_len: {}). Falling back to fresh session.",
                result.error,
                result.output.len()
            );
            // Create a fresh CLI session
            let fresh_cli_id = uuid::Uuid::new_v4().to_string();
            ai_config.cli_session_ctx = Some(crate::claude_session::runner::CliSessionContext {
                cli_session_id: fresh_cli_id.clone(),
                is_resume: false,
            });
            // Update the DB record with the new session ID
            if let Err(e) = self.checkpoint_db.create_workflow_ai_session(
                &parent_task_id,
                iteration as i32,
                "agentic",
                config.stage_index.map(|i| i as i32),
                &fresh_cli_id,
            ) {
                warn!(
                    "Failed to create fallback workflow AI session record: {}",
                    e
                );
            }
            // Retry with the full enhanced prompt
            let (retry_result, retry_duration) = timeout_helper::timed_result_async(
                self.ai_executor
                    .execute(&ai_config, &enhanced_prompt, logger),
            )
            .await;
            result = retry_result;
            duration_ms = retry_duration as i64;
        }

        // Checkpoint completion
        let mut completion_checkpoint = StepCheckpoint::new(
            &config.execution_id,
            "unified",
            "agentic",
            Some(iteration),
            0,
            "ai_session",
        )
        .with_step_name("AI Fixing Issues")
        .with_stage_index(config.stage_index);

        let injected_steps = result.injected_steps;

        // Parse structured output from the AI response
        let parsed_output = if !result.output.is_empty() {
            Some(super::output_parser::parse_agentic_output(&result.output))
        } else {
            None
        };

        let outcome = if result.success {
            completion_checkpoint.mark_success(Some(result.output.clone()), duration_ms);
            AgenticOutcome::Success {
                output: result.output,
                parsed: parsed_output,
            }
        } else if result.output.is_empty() {
            let error_msg = if result.error.is_empty() {
                "AI session failed (no output, no error details)".to_string()
            } else {
                format!("AI session failed: {}", result.error)
            };
            completion_checkpoint.mark_failed(&error_msg, duration_ms);
            AgenticOutcome::Error { error: error_msg }
        } else {
            let error_msg = if result.error.is_empty() {
                "AI reported failure".to_string()
            } else {
                format!("AI reported failure: {}", result.error)
            };
            completion_checkpoint.mark_failed(&error_msg, duration_ms);
            AgenticOutcome::Failed {
                output: result.output,
                error: error_msg,
                parsed: parsed_output,
            }
        };

        if let Err(e) = checkpoint_mgr.save_step(&completion_checkpoint) {
            warn!("Failed to save agentic step completion checkpoint: {}", e);
        }

        // Mark the workflow AI session as completed/failed and clean up partial output
        {
            let session_status = match &outcome {
                AgenticOutcome::Success { .. } => "completed",
                AgenticOutcome::Failed { .. } => "failed",
                AgenticOutcome::Error { .. } => "failed",
                AgenticOutcome::Skipped => "completed",
            };
            let output_len = outcome.output().map(|o| o.len() as i64).unwrap_or(0);
            if let Err(e) = self.checkpoint_db.complete_workflow_ai_session(
                &parent_task_id,
                iteration as i32,
                "agentic",
                config.stage_index.map(|i| i as i32),
                session_status,
                output_len,
            ) {
                warn!("Failed to complete workflow AI session: {}", e);
            }
            // Delete the partial in-progress output now that final output will be written
            if let Err(e) = self
                .checkpoint_db
                .delete_partial_ai_output(&parent_task_id, iteration as i32)
            {
                warn!("Failed to delete partial AI output: {}", e);
            }
        }

        // Emit completion event so the Active Dashboard timeline shows agentic phase result
        let agentic_duration_ms = agentic_start.elapsed().as_millis() as i64;
        let (event_subtype, event_message) = match &outcome {
            AgenticOutcome::Success { .. } => (
                "complete",
                format!(
                    "Agentic AI session completed successfully (iteration {}, {}ms)",
                    iteration, agentic_duration_ms
                ),
            ),
            AgenticOutcome::Failed { error, .. } => (
                "error",
                format!(
                    "Agentic AI session failed (iteration {}): {}",
                    iteration, error
                ),
            ),
            AgenticOutcome::Error { error } => (
                "error",
                format!(
                    "Agentic AI session error (iteration {}): {}",
                    iteration, error
                ),
            ),
            AgenticOutcome::Skipped => ("complete", "Agentic phase skipped".to_string()),
        };
        let completion_event = CreateTaskRunEventInput {
            task_run_id: parent_id.clone(),
            event_type: "step_execution".to_string(),
            event_subtype: Some(event_subtype.to_string()),
            message: event_message,
            data: Some(
                serde_json::to_string(&serde_json::json!({
                    "step_index": 0,
                    "step_type": "ai_session",
                    "step_name": "AI Fixing Issues",
                    "phase": "agentic",
                    "iteration": iteration,
                    "success": matches!(&outcome, AgenticOutcome::Success { .. }),
                }))
                .unwrap_or_default(),
            ),
            workflow_name: None,
            state_name: None,
            action_id: Some(action_id),
            timestamp: chrono::Utc::now().to_rfc3339(),
            duration_ms: Some(agentic_duration_ms),
        };
        if let Err(e) = self.checkpoint_db.create_task_run_event(&completion_event) {
            warn!("Failed to emit agentic completion event: {}", e);
        }

        if !injected_steps.is_empty() {
            info!(
                "AGENTIC-PHASE: Collected {} injected verification step(s) from AI output",
                injected_steps.len()
            );
        }

        (outcome, injected_steps)
    }

    /// Run a focused AI session with a custom prompt.
    ///
    /// Unlike `run_agentic()`, this doesn't build the prompt from config.base_prompt.
    /// It runs the provided prompt directly. Used by the multi-agent fixer to spawn
    /// specialized fix agents with narrow, targeted prompts.
    ///
    /// Returns (success, output, duration_ms).
    pub async fn run_focused_session(
        &self,
        execution_id: &str,
        workflow_name: &str,
        iteration: u32,
        agent_label: &str,
        prompt: &str,
        model_override: Option<String>,
        logger: &StepEventLogger,
    ) -> (bool, String, u64) {
        let start = std::time::Instant::now();
        let parent_task_id = super::types::get_parent_task_id(execution_id);

        info!(
            "MULTI-AGENT: Running focused session '{}' (iteration {})",
            agent_label, iteration
        );

        let mut ai_config = crate::unified_ai_session::AiSessionConfig::agentic(
            execution_id,
            workflow_name,
            iteration,
        )
        .with_model_override(model_override);

        // Create a fresh CLI session for each focused agent
        let cli_session_id = uuid::Uuid::new_v4().to_string();
        ai_config.cli_session_ctx = Some(crate::claude_session::runner::CliSessionContext {
            cli_session_id,
            is_resume: false,
        });
        ai_config.db_flush_ctx = Some(crate::claude_session::runner::DbFlushContext {
            db: self.checkpoint_db.clone(),
            task_run_id: parent_task_id.clone(),
            iteration: iteration as i32,
        });

        let result = self.ai_executor.execute(&ai_config, prompt, logger).await;

        let duration_ms = start.elapsed().as_millis() as u64;

        info!(
            "MULTI-AGENT: Focused session '{}' completed in {}ms (success={})",
            agent_label, duration_ms, result.success
        );

        (result.success, result.output, duration_ms)
    }

    /// Run a triage prompt in response mode (fast, no session state).
    ///
    /// Used by the multi-agent fixer to classify verification failures
    /// before spawning specialized fix agents.
    pub async fn run_triage_prompt(
        &self,
        prompt: &str,
        model_override: Option<String>,
    ) -> Result<String, String> {
        let step = ExecutionStepConfig {
            step_type: "prompt".to_string(),
            name: Some("Multi-agent triage".to_string()),
            prompt_content: Some(prompt.to_string()),
            prompt_mode: Some("response".to_string()),
            model: model_override.clone(),
            ..Default::default()
        };

        let result = execute_prompt_response_mode(
            &step,
            &self.checkpoint_db,
            None,
            None,
            model_override,
            None,
            None,
            None,
            None,
            None,
        )
        .await?;

        Ok(result.output)
    }

    /// Get progress marker context from previous checkpoints.
    ///
    /// This queries for the most recent checkpoint from a previous agentic session
    /// and retrieves its latest progress marker. This information helps the AI
    /// understand where to resume long operations.
    ///
    /// Returns a formatted string like:
    /// "Last progress: file_progress 50/100. Continue from where you left off."
    fn get_progress_marker_context(&self, execution_id: &str, iteration: u32) -> Option<String> {
        // Get checkpoints for the agentic phase at this iteration
        let checkpoints = self
            .checkpoint_db
            .get_workflow_step_checkpoints(execution_id, "agentic", Some(iteration))
            .ok()?;

        // Find the most recent checkpoint (by step_index descending, or just take the last one)
        // There should typically only be one checkpoint per iteration, but we want the latest
        let latest_checkpoint = checkpoints.into_iter().last()?;

        // Query for the latest progress marker using the checkpoint's id
        let progress_marker = self
            .checkpoint_db
            .get_latest_step_progress_marker(&latest_checkpoint.id)
            .ok()
            .flatten()?;

        // Format the progress context message
        let progress_string = progress_marker.progress_string();
        let marker_type = &progress_marker.marker_type;

        let mut message = format!(
            "---\n\n**Resume Context:** Last progress: {} {}.",
            marker_type, progress_string
        );

        // Add description if available
        if let Some(description) = &progress_marker.description {
            message.push_str(&format!(" ({})", description));
        }

        message.push_str(" Continue from where you left off.");

        info!(
            "AGENTIC-PHASE: Including progress marker context: {} {}/{}",
            marker_type,
            progress_marker.current_value,
            progress_marker
                .total_value
                .map_or("?".to_string(), |v| v.to_string())
        );

        Some(message)
    }
}

// =============================================================================
// Completion Phase Executor
// =============================================================================

/// Executes the completion phase (runs once after verification passes).
/// AI session execution is delegated to the UnifiedAiSessionExecutor.
pub struct CompletionExecutor {
    app_state: Arc<AppState>,
    executor: StepExecutor,
    ai_executor: UnifiedAiSessionExecutor,
    checkpoint_db: Arc<CheckpointDb>,
}

impl CompletionExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        let checkpoint_db = app_state.checkpoint_db.clone();
        Self {
            app_state: app_state.clone(),
            executor: StepExecutor::with_app_handle(
                app_state.clone(),
                config_storage,
                app_handle.clone(),
            ),
            ai_executor: UnifiedAiSessionExecutor::new(app_state, app_handle, pid_tracker),
            checkpoint_db,
        }
    }

    /// Enable interactive sessions via the session manager.
    pub fn set_session_manager(&mut self, sm: Arc<crate::claude_session::SessionManager>) {
        self.ai_executor.session_manager = Some(sm);
    }

    /// Set the task run ID on the inner step executor for database logging.
    pub fn set_task_run_id(&mut self, task_run_id: String) {
        self.executor.set_task_run_id(task_run_id);
    }

    /// Get the shared variable store from the inner step executor.
    ///
    /// After completion automation steps run, this contains output variables
    /// (e.g., `evaluation_results`) that need substitution into prompt step content.
    pub fn shared_variables(
        &self,
    ) -> &crate::orchestrator::context_propagation::SharedVariableStore {
        self.executor.shared_variables()
    }

    /// Run completion steps.
    ///
    /// This should ONLY be called when verification has passed.
    ///
    /// # Arguments
    /// * `iterations_run` - Number of verification-agentic iterations that were executed.
    ///   Used to calculate the correct turn number for the completion phase.
    /// * `logger` - Required logger for consistent step event logging.
    ///
    /// Step checkpointing is integrated for resume capability.
    #[instrument(
        name = "qontinui.workflow.phase.completion",
        skip(self, automation_steps, prompt_steps, logger),
        fields(
            execution_id = %execution_id,
            workflow_name = %workflow_name,
            automation_step_count = automation_steps.len(),
            prompt_step_count = prompt_steps.len(),
            iterations_run = iterations_run
        )
    )]
    pub async fn run_completion(
        &self,
        automation_steps: &[ExecutionStepConfig],
        prompt_steps: &[ExecutionStepConfig],
        execution_id: &str,
        workflow_name: &str,
        iterations_run: u32,
        logger: &StepEventLogger,
        stage_index: Option<u32>,
        model_override: Option<String>,
        provider_override: Option<String>,
        completion_prompts_first: bool,
    ) -> (bool, Vec<StepExecutionResult>) {
        let mut all_results = Vec::new();
        let mut overall_success = true;

        // Filter out dev_mode_only automation steps when not in dev mode
        let automation_steps: Vec<ExecutionStepConfig> = automation_steps
            .iter()
            .filter(|step| {
                if step.dev_mode_only.unwrap_or(false) && !cfg!(debug_assertions) {
                    info!(
                        "COMPLETION-PHASE: Skipping dev-mode-only automation step: {:?}",
                        step.name
                    );
                    false
                } else {
                    true
                }
            })
            .cloned()
            .collect();
        let automation_steps = automation_steps.as_slice();

        // Create checkpoint manager for step-level checkpointing
        let checkpoint_mgr = CheckpointManager::new(self.checkpoint_db.clone(), "unified");

        // When completion_prompts_first is set, run prompts before automation.
        // This is used by meta-workflows where the AI hardener must run before
        // save_workflow_artifact persists the final result.
        if completion_prompts_first {
            info!("COMPLETION-PHASE: Running prompts-first order (completion_prompts_first=true)");

            // Run prompts first
            let (prompt_ok, prompt_results) = self
                .run_completion_prompts(
                    prompt_steps,
                    execution_id,
                    workflow_name,
                    iterations_run,
                    logger,
                    stage_index,
                    model_override.clone(),
                    provider_override.clone(),
                    &checkpoint_mgr,
                    0, // prompts run first, so prior_step_count is 0
                )
                .await;
            overall_success = overall_success && prompt_ok;
            let prompt_count = prompt_results.len();
            all_results.extend(prompt_results);

            // Then run automation
            let (auto_ok, auto_results) = self
                .run_completion_automation(
                    automation_steps,
                    execution_id,
                    logger,
                    stage_index,
                    &checkpoint_mgr,
                    prompt_count, // automation runs second
                )
                .await;
            overall_success = overall_success && auto_ok;
            all_results.extend(auto_results);

            return (overall_success, all_results);
        }

        // Default order: automation first, then prompts
        let (auto_ok, auto_results) = self
            .run_completion_automation(
                automation_steps,
                execution_id,
                logger,
                stage_index,
                &checkpoint_mgr,
                0, // automation runs first
            )
            .await;
        overall_success = overall_success && auto_ok;
        let auto_count = auto_results.len();
        all_results.extend(auto_results);

        let (prompt_ok, prompt_results) = self
            .run_completion_prompts(
                prompt_steps,
                execution_id,
                workflow_name,
                iterations_run,
                logger,
                stage_index,
                model_override,
                provider_override,
                &checkpoint_mgr,
                auto_count, // prompts run second
            )
            .await;
        overall_success = overall_success && prompt_ok;
        all_results.extend(prompt_results);

        (overall_success, all_results)
    }

    /// Run completion automation steps with checkpointing.
    ///
    /// This is extracted from `run_completion` so both the default order
    /// (automation-first) and the prompts-first order can share the same code.
    ///
    /// `step_index_offset` is used to offset checkpoint step indices when
    /// another phase has already run (e.g., prompts ran first).
    async fn run_completion_automation(
        &self,
        automation_steps: &[ExecutionStepConfig],
        execution_id: &str,
        _logger: &StepEventLogger,
        stage_index: Option<u32>,
        checkpoint_mgr: &CheckpointManager,
        step_index_offset: usize,
    ) -> (bool, Vec<StepExecutionResult>) {
        let mut all_results = Vec::new();
        let mut overall_success = true;

        if !automation_steps.is_empty() {
            info!(
                "COMPLETION-PHASE: Running {} automation steps",
                automation_steps.len()
            );

            // Checkpoint each automation step
            for (idx, step) in automation_steps.iter().enumerate() {
                let step_type =
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Command);
                let step_name = step.name.as_deref().unwrap_or(&step.step_type);

                // Use Some(0) instead of None for iteration to ensure SQLite's
                // UNIQUE constraint works correctly (NULL != NULL in SQLite)
                let mut checkpoint = StepCheckpoint::new(
                    execution_id,
                    "unified",
                    "completion",
                    Some(0),
                    step_index_offset + idx,
                    step_type.as_str(),
                )
                .with_step_name(step_name)
                .with_stage_index(stage_index);
                checkpoint.mark_started();
                if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                    warn!("Failed to save completion step checkpoint: {}", e);
                }
            }

            let result = self
                .executor
                .execute_completion_phase(automation_steps, execution_id, &[])
                .await;

            // Checkpoint completion for each step
            for (idx, step_result) in result.steps.iter().enumerate() {
                let step = &automation_steps[idx];
                let step_type =
                    StepType::from_str_compat(&step.step_type).unwrap_or(StepType::Command);
                let step_name = step.name.as_deref().unwrap_or(&step.step_type);

                // Use Some(0) instead of None for iteration to ensure SQLite's
                // UNIQUE constraint works correctly (NULL != NULL in SQLite)
                let mut checkpoint = StepCheckpoint::new(
                    execution_id,
                    "unified",
                    "completion",
                    Some(0),
                    step_index_offset + idx,
                    step_type.as_str(),
                )
                .with_step_name(step_name)
                .with_stage_index(stage_index);

                let duration_ms = step_result.duration_ms as i64;
                if step_result.success {
                    checkpoint.mark_success(serde_json::to_string(step_result).ok(), duration_ms);
                } else {
                    checkpoint.mark_failed(
                        step_result.error.as_deref().unwrap_or("Unknown error"),
                        duration_ms,
                    );
                }

                if let Err(e) = checkpoint_mgr.save_step(&checkpoint) {
                    warn!(
                        "Failed to save completion step completion checkpoint: {}",
                        e
                    );
                }
            }

            overall_success = overall_success && result.success;
            all_results.extend(result.steps);

            if !result.success {
                warn!("COMPLETION-PHASE: Automation steps failed");
            }
        }

        (overall_success, all_results)
    }

    /// Run completion prompt steps (both response-mode and session-mode) with checkpointing.
    ///
    /// This is extracted from `run_completion` so both the default order
    /// (automation-first) and the prompts-first order can share the same code.
    ///
    /// `step_index_offset` is the base offset for checkpoint step indices
    /// (e.g., the number of automation steps that ran before prompts).
    #[allow(clippy::too_many_arguments)]
    async fn run_completion_prompts(
        &self,
        prompt_steps: &[ExecutionStepConfig],
        execution_id: &str,
        workflow_name: &str,
        iterations_run: u32,
        logger: &StepEventLogger,
        stage_index: Option<u32>,
        model_override: Option<String>,
        provider_override: Option<String>,
        checkpoint_mgr: &CheckpointManager,
        step_index_offset: usize,
    ) -> (bool, Vec<StepExecutionResult>) {
        let mut all_results = Vec::new();
        let mut overall_success = true;

        // Expand runtime variables in prompt step content before execution.
        // Variables set by automation steps (e.g., evaluation_results) need to be
        // substituted into {{variable_name}} patterns in prompt content.
        let prompt_steps: Vec<ExecutionStepConfig> = {
            let shared_vars = self.executor.shared_variables().get_all();
            if shared_vars.is_empty() {
                prompt_steps.to_vec()
            } else {
                prompt_steps
                    .iter()
                    .map(|step| {
                        let mut step = step.clone();
                        if let Some(ref mut content) = step.prompt_content {
                            for (name, value) in &shared_vars {
                                let pattern = format!("{{{{{}}}}}", name);
                                if content.contains(&pattern) {
                                    *content = content.replace(&pattern, value);
                                }
                            }
                        }
                        step
                    })
                    .collect()
            }
        };
        let prompt_steps = prompt_steps.as_slice();

        if !prompt_steps.is_empty() {
            info!(
                "COMPLETION-PHASE: Running {} prompt steps (AI summary)",
                prompt_steps.len()
            );

            // Separate response-mode steps from session-mode steps
            let mut session_prompt_steps = Vec::new();
            let mut response_step_count = 0usize;
            for step in prompt_steps {
                // Skip dev_mode_only steps when not in dev mode
                if step.dev_mode_only.unwrap_or(false) && !cfg!(debug_assertions) {
                    info!("Skipping dev-mode-only step: {:?}", step.name);
                    continue;
                }

                if step.prompt_mode.as_deref() == Some("response") {
                    let step_name = step.name.as_deref().unwrap_or("Response Prompt");
                    info!(
                        "COMPLETION-PHASE: Executing response-mode prompt step: {}",
                        step_name
                    );

                    // Checkpoint the response-mode prompt step as "running"
                    let step_idx = step_index_offset + response_step_count;
                    let mut resp_checkpoint = StepCheckpoint::new(
                        execution_id,
                        "unified",
                        "completion",
                        Some(0),
                        step_idx,
                        "prompt",
                    )
                    .with_step_name(step_name)
                    .with_stage_index(stage_index);
                    resp_checkpoint.mark_started();
                    if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                        warn!(
                            "Failed to save completion response-mode step checkpoint: {}",
                            e
                        );
                    }

                    // Log start event for Active Dashboard visibility
                    let metadata = StepMetadata::completion(
                        execution_id,
                        StepType::Prompt,
                        step_name,
                        step_idx,
                    );
                    if let Err(e) = logger.log_start(
                        StepEventKind::CompletionAiStart,
                        metadata,
                        StepDetails::default(),
                    ) {
                        warn!("Failed to log completion AI step start event: {}", e);
                    }

                    let doctor_handle = self.app_state.doctor_handle.lock().await.clone();
                    let start = std::time::Instant::now();
                    // Step-level overrides take precedence over phase-level
                    let step_model = step.model.clone().or_else(|| model_override.clone());
                    let step_provider = step.provider.clone().or_else(|| provider_override.clone());
                    match execute_prompt_response_mode(
                        step,
                        &self.checkpoint_db,
                        Some(execution_id),
                        doctor_handle,
                        step_model.clone(),
                        step_provider.clone(),
                        None,
                        None,
                        None,
                        None,
                    )
                    .await
                    {
                        Ok(resp) => {
                            let duration_ms = start.elapsed().as_millis() as u64;
                            record_phase_token_usage(
                                &self.checkpoint_db,
                                execution_id,
                                "completion",
                                stage_index,
                                None,
                                step_model.as_deref(),
                                step_provider.as_deref(),
                                resp.input_tokens,
                                resp.output_tokens,
                                Some(duration_ms),
                            );
                            let output = resp.output;
                            info!(
                                "COMPLETION-PHASE: Response-mode step '{}' completed successfully ({} bytes)",
                                step_name,
                                output.len()
                            );
                            // Persist AI output to chunks for the /output endpoint
                            if !output.is_empty() {
                                let formatted = format!(
                                    "\n--- AI Completion Output ({}) ---\n{}\n",
                                    step_name, output
                                );
                                if let Err(e) = self.checkpoint_db.append_task_output_ex(
                                    execution_id,
                                    &formatted,
                                    false,
                                    false,
                                ) {
                                    warn!("Failed to persist completion response-mode AI output to chunks: {}", e);
                                }
                            }
                            // Save completion checkpoint
                            let mut resp_checkpoint = StepCheckpoint::new(
                                execution_id,
                                "unified",
                                "completion",
                                Some(0),
                                step_idx,
                                "prompt",
                            )
                            .with_step_name(step_name)
                            .with_stage_index(stage_index);
                            resp_checkpoint.mark_success(Some(output.clone()), duration_ms as i64);
                            if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                                warn!("Failed to save completion response-mode step completion checkpoint: {}", e);
                            }

                            // Log complete event for Active Dashboard visibility
                            let metadata = StepMetadata::completion(
                                execution_id,
                                StepType::Prompt,
                                step_name,
                                step_idx,
                            );
                            if let Err(e) = logger.log_complete(
                                StepEventKind::CompletionAiComplete,
                                metadata,
                                StepDetails::default(),
                                duration_ms as i64,
                            ) {
                                warn!("Failed to log completion AI step complete event: {}", e);
                            }
                            response_step_count += 1;
                            all_results.push(StepExecutionResult {
                                step_index: all_results.len(),
                                step_type: "prompt".to_string(),
                                step_name: step_name.to_string(),
                                step_id: step.id.clone(),
                                success: true,
                                error: None,
                                screenshot_path: None,
                                started_at: None,
                                ended_at: None,
                                duration_ms,
                                config: crate::step_executor::StepExecutionConfig::default(),
                                verification_details: None,
                                output_data: Some(serde_json::json!({ "output": output })),
                                required: None,
                                resolved_inputs: None,
                                extracted_values: None,
                                failure_category: None,
                                interrupted: None,
                            });
                        }
                        Err(e) => {
                            let duration_ms = start.elapsed().as_millis() as u64;
                            warn!(
                                "COMPLETION-PHASE: Response-mode step '{}' failed: {}",
                                step_name, e
                            );
                            // Save failure checkpoint
                            let mut resp_checkpoint = StepCheckpoint::new(
                                execution_id,
                                "unified",
                                "completion",
                                Some(0),
                                step_idx,
                                "prompt",
                            )
                            .with_step_name(step_name)
                            .with_stage_index(stage_index);
                            resp_checkpoint.mark_failed(&e, duration_ms as i64);
                            if let Err(e) = checkpoint_mgr.save_step(&resp_checkpoint) {
                                warn!("Failed to save completion response-mode step failure checkpoint: {}", e);
                            }

                            // Log error event for Active Dashboard visibility
                            let metadata = StepMetadata::completion(
                                execution_id,
                                StepType::Prompt,
                                step_name,
                                step_idx,
                            );
                            if let Err(log_err) = logger.log_error(
                                StepEventKind::CompletionAiError,
                                metadata,
                                StepDetails::default(),
                                duration_ms as i64,
                                Some(&e),
                            ) {
                                warn!("Failed to log completion AI step error event: {}", log_err);
                            }

                            response_step_count += 1;
                            all_results.push(StepExecutionResult {
                                step_index: all_results.len(),
                                step_type: "prompt".to_string(),
                                step_name: step_name.to_string(),
                                step_id: step.id.clone(),
                                success: false,
                                error: Some(e),
                                screenshot_path: None,
                                started_at: None,
                                ended_at: None,
                                duration_ms,
                                config: crate::step_executor::StepExecutionConfig::default(),
                                verification_details: None,
                                output_data: None,
                                required: None,
                                resolved_inputs: None,
                                extracted_values: None,
                                failure_category: None,
                                interrupted: None,
                            });
                            // Completion failures are non-fatal - don't return early
                            overall_success = false;
                        }
                    }
                } else {
                    session_prompt_steps.push(step.clone());
                }
            }

            // Run remaining session-mode prompt steps via consolidated AI session
            if !session_prompt_steps.is_empty() {
                // Checkpoint the AI step as a single step (after any response-mode steps)
                let ai_step_idx = step_index_offset + response_step_count;
                let step_name = prompt_builder::consolidate_step_names_with_default(
                    &session_prompt_steps,
                    "Completion AI Task",
                );

                // Use Some(0) instead of None for iteration to ensure SQLite's
                // UNIQUE constraint works correctly (NULL != NULL in SQLite)
                let mut ai_checkpoint = StepCheckpoint::new(
                    execution_id,
                    "unified",
                    "completion",
                    Some(0),
                    ai_step_idx,
                    "ai_session",
                )
                .with_step_name(&step_name)
                .with_stage_index(stage_index);
                ai_checkpoint.mark_started();
                if let Err(e) = checkpoint_mgr.save_step(&ai_checkpoint) {
                    warn!("Failed to save completion AI step checkpoint: {}", e);
                }

                // Log start event for Active Dashboard visibility
                {
                    let metadata = StepMetadata::completion(
                        execution_id,
                        StepType::Prompt,
                        &step_name,
                        ai_step_idx,
                    );
                    if let Err(e) = logger.log_start(
                        StepEventKind::CompletionAiStart,
                        metadata,
                        StepDetails::default(),
                    ) {
                        warn!("Failed to log completion AI session start event: {}", e);
                    }
                }

                // Use structured prompts for granular sub-step tracking
                let (mut completion_prompt, sub_step_metadata) =
                    prompt_builder::consolidate_prompts_structured(
                        &session_prompt_steps,
                        "completion",
                    );

                // Inject prior phase output context so the completion AI knows what happened
                if !completion_prompt.is_empty() {
                    let prior_context =
                        self.build_prior_phase_context(execution_id, iterations_run);
                    if !prior_context.is_empty() {
                        completion_prompt =
                            format!("{}\n\n---\n\n{}", prior_context, completion_prompt);
                    }
                }

                if !completion_prompt.is_empty() {
                    // Use the unified AI session executor with sub-step metadata
                    let config = AiSessionConfig::completion(
                        execution_id,
                        workflow_name,
                        &step_name,
                        iterations_run,
                    )
                    .with_checkpoint_id(&ai_checkpoint.id)
                    .with_sub_step_metadata(sub_step_metadata)
                    .with_model_override(model_override.clone());

                    let (result, duration_ms) = timeout_helper::timed_result_async(
                        self.ai_executor
                            .execute(&config, &completion_prompt, logger),
                    )
                    .await;
                    let duration_ms = duration_ms as i64;
                    // Use Some(0) instead of None for iteration to ensure SQLite's
                    // UNIQUE constraint works correctly (NULL != NULL in SQLite)
                    let mut ai_completion_checkpoint = StepCheckpoint::new(
                        execution_id,
                        "unified",
                        "completion",
                        Some(0),
                        ai_step_idx,
                        "ai_session",
                    )
                    .with_step_name(&step_name)
                    .with_stage_index(stage_index);

                    if result.success {
                        ai_completion_checkpoint
                            .mark_success(Some(result.output.clone()), duration_ms);
                    } else {
                        ai_completion_checkpoint.mark_failed("AI session failed", duration_ms);
                    }

                    if let Err(e) = checkpoint_mgr.save_step(&ai_completion_checkpoint) {
                        warn!(
                            "Failed to save completion AI step completion checkpoint: {}",
                            e
                        );
                    }

                    // Log complete/error event for Active Dashboard visibility
                    {
                        let metadata = StepMetadata::completion(
                            execution_id,
                            StepType::Prompt,
                            &step_name,
                            ai_step_idx,
                        );
                        if result.success {
                            if let Err(e) = logger.log_complete(
                                StepEventKind::CompletionAiComplete,
                                metadata,
                                StepDetails::default(),
                                duration_ms,
                            ) {
                                warn!("Failed to log completion AI session complete event: {}", e);
                            }
                        } else if let Err(e) = logger.log_error(
                            StepEventKind::CompletionAiError,
                            metadata,
                            StepDetails::default(),
                            duration_ms,
                            Some("AI session failed"),
                        ) {
                            warn!("Failed to log completion AI session error event: {}", e);
                        }
                    }

                    // Don't save completion AI output as summary here --
                    // the async summary generator (summary_generator.rs) produces a proper
                    // aggregated summary across ALL workflow phases after completion.

                    overall_success = overall_success && result.success;
                }
            }
        }

        (overall_success, all_results)
    }

    /// Run completion and return a unified ExecutionOutcome.
    ///
    /// This uses the IntoOutcome trait to convert the CompletionResult into a
    /// standardized ExecutionOutcome, which is useful for consistent result handling.
    ///
    /// # Arguments
    /// * `config` - The completion configuration
    /// * `logger` - Logger for step events
    ///
    /// # Returns
    /// An `ExecutionOutcome` summarizing the completion phase execution.
    pub async fn run_completion_to_outcome(
        &self,
        config: &CompletionConfig,
        logger: &StepEventLogger,
    ) -> ExecutionOutcome {
        let start = std::time::Instant::now();

        let (success, step_results) = self
            .run_completion(
                &config.automation_steps,
                &config.prompt_steps,
                &config.execution_id,
                &config.workflow_name,
                config.iterations_run,
                logger,
                None,
                config.model_override.clone(),
                config.provider_override.clone(),
                config.completion_prompts_first,
            )
            .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        // Use the IntoOutcome trait for consistent conversion
        let result = CompletionResult {
            success,
            step_results,
        };
        result.into_outcome(duration_ms)
    }

    /// Build context from prior phases (setup, verification, agentic) to give the
    /// completion AI knowledge of what happened during the workflow execution.
    ///
    /// This reads the accumulated output_log, verification results, and findings
    /// from the database and formats them as context that gets prepended to the
    /// completion prompt.
    fn build_prior_phase_context(&self, execution_id: &str, iterations_run: u32) -> String {
        let mut sections = Vec::new();

        sections.push("## Prior Workflow Execution Context\n".to_string());
        sections.push(format!(
            "This workflow ran {} verification-agentic iteration(s) before reaching the completion phase.\n\
             Below is the accumulated output from all prior phases.\n",
            iterations_run
        ));

        // Fetch and include verification test results from step checkpoints
        // This is especially important when verification passes on the first try
        // (no agentic phase runs, so output_log would be empty)
        match self
            .checkpoint_db
            .get_workflow_step_checkpoints(execution_id, "verification", None)
        {
            Ok(checkpoints) if !checkpoints.is_empty() => {
                let mut verification_lines = vec!["### Verification Test Results\n".to_string()];
                let mut passed_count = 0;
                let mut failed_count = 0;

                for checkpoint in &checkpoints {
                    use crate::workflow_state::StepCheckpointStatus;
                    let status_emoji = match checkpoint.status {
                        StepCheckpointStatus::Success => {
                            passed_count += 1;
                            "✓"
                        }
                        StepCheckpointStatus::Failed => {
                            failed_count += 1;
                            "✗"
                        }
                        _ => "○",
                    };
                    let duration = checkpoint
                        .duration_ms
                        .map(|ms| format!(" ({}ms)", ms))
                        .unwrap_or_default();

                    verification_lines.push(format!(
                        "- {} **{}**{}",
                        status_emoji,
                        checkpoint
                            .step_name
                            .as_deref()
                            .unwrap_or(&checkpoint.step_type),
                        duration
                    ));

                    // Include error details for failed checks
                    if checkpoint.status == StepCheckpointStatus::Failed {
                        if let Some(ref error) = checkpoint.error {
                            verification_lines.push(format!("  - Error: {}", error));
                        }
                    }
                }

                verification_lines.push(format!(
                    "\n**Summary:** {} passed, {} failed\n",
                    passed_count, failed_count
                ));
                sections.push(verification_lines.join("\n"));
            }
            Ok(_) => {
                sections.push(
                    "### Verification Test Results\n\nNo verification checkpoints recorded.\n"
                        .to_string(),
                );
            }
            Err(e) => {
                warn!(
                    "Failed to read verification checkpoints for completion context: {}",
                    e
                );
            }
        }

        // Fetch and include accumulated output_log (from agentic phases)
        match self.checkpoint_db.get_full_task_output(execution_id) {
            Ok(output) if !output.is_empty() => {
                let cleaned = crate::summary_generator::strip_output_markers(&output);
                // Truncate to last 50k chars to avoid overwhelming the AI
                let max_chars = 50_000;
                let truncated = if cleaned.len() > max_chars {
                    let start = cleaned.len() - max_chars;
                    format!("...[earlier output truncated]...\n{}", &cleaned[start..])
                } else {
                    cleaned
                };
                sections.push(format!(
                    "### AI Session Output ({} chars)\n\n{}\n",
                    truncated.len(),
                    truncated
                ));
            }
            Ok(_) => {
                // Don't add "no output" message if we already have verification results
                // This is expected when verification passes on the first try
            }
            Err(e) => {
                warn!("Failed to read prior output for completion context: {}", e);
            }
        }

        // Fetch and include findings
        match self.checkpoint_db.get_findings_for_task(execution_id) {
            Ok(findings) if !findings.is_empty() => {
                let findings_section =
                    crate::summary_generator::format_findings_for_summary(&findings);
                sections.push(findings_section);
            }
            Ok(_) => {} // No findings, skip section
            Err(e) => {
                warn!("Failed to read findings for completion context: {}", e);
            }
        }

        // Include unresolved errors so the completion AI can report on them.
        // This runs BEFORE the loop_controller marks completion, so workflow-scoped
        // errors are still visible here.
        match self.checkpoint_db.get_conn() {
            Ok(conn) => {
                match crate::error_monitor::ErrorEventStorage::get_unresolved(&conn, None, 20) {
                    Ok(errors) if !errors.is_empty() => {
                        let mut workflow_errors = Vec::new();
                        let mut pre_existing_errors = Vec::new();

                        for e in &errors {
                            let is_workflow_scoped = e
                                .task_run_id
                                .as_deref()
                                .is_some_and(|id| id == execution_id);
                            if is_workflow_scoped {
                                workflow_errors.push(e);
                            } else {
                                pre_existing_errors.push(e);
                            }
                        }

                        let mut lines = vec!["### Unresolved Errors (Error Monitor)\n".to_string()];

                        if !workflow_errors.is_empty() {
                            lines.push(format!(
                                "**Errors from this workflow run ({}):**",
                                workflow_errors.len()
                            ));
                            for e in &workflow_errors {
                                lines.push(format!(
                                    "- [{}] {}",
                                    e.severity.as_str(),
                                    e.message.chars().take(200).collect::<String>()
                                ));
                            }
                            lines.push(String::new());
                        }

                        if !pre_existing_errors.is_empty() {
                            lines.push(format!(
                                "**Pre-existing errors ({}):**",
                                pre_existing_errors.len()
                            ));
                            for e in pre_existing_errors.iter().take(10) {
                                lines.push(format!(
                                    "- [{}] {}",
                                    e.severity.as_str(),
                                    e.message.chars().take(200).collect::<String>()
                                ));
                            }
                            if pre_existing_errors.len() > 10 {
                                lines.push(format!(
                                    "... and {} more",
                                    pre_existing_errors.len() - 10
                                ));
                            }
                            lines.push(String::new());
                        }

                        lines.push(
                            "Include any relevant errors in your completion summary. \
                             Workflow-scoped errors will be auto-resolved if the workflow succeeded."
                                .to_string(),
                        );

                        sections.push(lines.join("\n"));
                    }
                    Ok(_) => {} // No unresolved errors
                    Err(e) => {
                        warn!(
                            "Failed to read unresolved errors for completion context: {}",
                            e
                        );
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Failed to get DB connection for completion error context: {}",
                    e
                );
            }
        }

        sections.join("\n")
    }
}

// =============================================================================
// FromContext Implementations
// =============================================================================

impl FromContext for SetupExecutor {
    fn from_context(context: ExecutorContext) -> Result<Self, ExecutorError> {
        let config_storage = context
            .config_storage()
            .cloned()
            .ok_or(ExecutorError::missing("config_storage"))?;
        let pid_tracker = context
            .pid_tracker()
            .cloned()
            .ok_or(ExecutorError::missing("pid_tracker"))?;

        Ok(Self::new(
            context.app_state,
            config_storage,
            context.app_handle,
            pid_tracker,
        ))
    }
}

impl FromContext for VerificationExecutor {
    fn from_context(context: ExecutorContext) -> Result<Self, ExecutorError> {
        let config_storage = context
            .config_storage()
            .cloned()
            .ok_or(ExecutorError::missing("config_storage"))?;

        Ok(Self::new(
            context.app_state,
            config_storage,
            context.app_handle,
        ))
    }
}

impl FromContext for AgenticExecutor {
    fn from_context(context: ExecutorContext) -> Result<Self, ExecutorError> {
        let pid_tracker = context
            .pid_tracker()
            .cloned()
            .ok_or(ExecutorError::missing("pid_tracker"))?;

        Ok(Self::new(
            context.app_state,
            context.app_handle,
            pid_tracker,
        ))
    }
}

impl FromContext for CompletionExecutor {
    fn from_context(context: ExecutorContext) -> Result<Self, ExecutorError> {
        let config_storage = context
            .config_storage()
            .cloned()
            .ok_or(ExecutorError::missing("config_storage"))?;
        let pid_tracker = context
            .pid_tracker()
            .cloned()
            .ok_or(ExecutorError::missing("pid_tracker"))?;

        Ok(Self::new(
            context.app_state,
            config_storage,
            context.app_handle,
            pid_tracker,
        ))
    }
}

// =============================================================================
// Executor Trait Implementations
// =============================================================================

/// Wrapper to hold a logger for async execution.
/// Since SetupConfig can't own the logger (it's borrowed), we need a separate
/// struct that contains everything needed for execution.
pub struct SetupExecutionRequest<'a> {
    pub config: SetupConfig,
    pub logger: &'a StepEventLogger,
}

#[async_trait]
impl Executor for SetupExecutor {
    type Config = SetupConfig;
    type Output = SetupResult;

    async fn execute(&self, config: Self::Config) -> Result<Self::Output, ExecutorError> {
        // Create a logger for this execution
        // Note: This is a simplified version that doesn't use the StepEventLogger
        // because the trait interface doesn't allow passing borrowed references.
        // For full logging support, use the direct execute() method with a logger.
        let (success, step_results) = self
            .run_setup(
                &config.automation_steps,
                &config.prompt_steps,
                &config.execution_id,
                &config.workflow_name,
                &StepEventLogger::noop(),
                None,
                config.model_override.clone(),
                config.provider_override.clone(),
            )
            .await;

        Ok(SetupResult {
            success,
            step_results,
        })
    }

    fn name(&self) -> &'static str {
        "setup"
    }
}

#[async_trait]
impl Executor for VerificationExecutor {
    type Config = VerificationConfig;
    type Output = VerificationResult;

    async fn execute(&self, config: Self::Config) -> Result<Self::Output, ExecutorError> {
        let (phase_result, step_results) = self
            .run_verification(
                &config.steps,
                &config.execution_id,
                config.iteration,
                &config.workflow_name,
                &StepEventLogger::noop(),
                None,
            )
            .await;

        Ok(VerificationResult {
            phase_result,
            step_results,
        })
    }

    fn name(&self) -> &'static str {
        "verification"
    }
}

#[async_trait]
impl Executor for AgenticExecutor {
    type Config = AgenticConfig;
    type Output = AgenticOutcome;

    async fn execute(&self, config: Self::Config) -> Result<Self::Output, ExecutorError> {
        // Build a LoopConfig from AgenticConfig
        let loop_config = LoopConfig {
            max_iterations: config.max_iterations,
            base_prompt: config.base_prompt,
            workflow_name: config.workflow_name,
            workflow_id: config.workflow_id,
            execution_id: config.execution_id.clone(),
            targeted_error_ids: Vec::new(),
            starting_iteration: 0,
            run_agentic_first: false,
            artifact_dir: None,
            is_dev_mode: false,
            enable_sweep: false,
            max_sweep_iterations: 5,
            stages: Vec::new(),
            stop_on_failure: false,
            constraint_overrides: std::collections::HashMap::new(),
            reflection_mode: false,
            provider_override: None,
            model_override: None,
            model_overrides: std::collections::HashMap::new(),
            stage_index: None,
            max_sessions: None,
            auto_run_generated: false,
            approval_gate: false,
            max_context_tokens: 100_000,
            cross_workflow_learning: true,
            verification_history: std::collections::HashMap::new(),
            routing_context: Default::default(),
            project_path: crate::mcp::shared::current_project_path(),
            acceptance_criteria: None,
            multi_agent_mode: false,
            use_worktree: false,
            worktree_path: None,
            worktree_branch: None,
            workflow_architecture: None,
            agentic_verification_config: None,
            multi_agent_pipeline_config: None,
        };

        let (outcome, _injected_steps) = self
            .run_agentic(
                &loop_config,
                config.iteration,
                &config.failure_context,
                config.has_agentic_steps,
                &[], // No step configs available via trait interface
                &StepEventLogger::noop(),
            )
            .await;

        Ok(outcome)
    }

    fn name(&self) -> &'static str {
        "agentic"
    }
}

#[async_trait]
impl Executor for CompletionExecutor {
    type Config = CompletionConfig;
    type Output = CompletionResult;

    async fn execute(&self, config: Self::Config) -> Result<Self::Output, ExecutorError> {
        let (success, step_results) = self
            .run_completion(
                &config.automation_steps,
                &config.prompt_steps,
                &config.execution_id,
                &config.workflow_name,
                config.iterations_run,
                &StepEventLogger::noop(),
                None,
                config.model_override.clone(),
                config.provider_override.clone(),
                config.completion_prompts_first,
            )
            .await;

        Ok(CompletionResult {
            success,
            step_results,
        })
    }

    fn name(&self) -> &'static str {
        "completion"
    }
}
