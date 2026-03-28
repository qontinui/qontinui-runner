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

use tracing::{debug, info};

use crate::database::CheckpointDb;

// Token tracking, UI Bridge, environment readiness, response mode, and token
// estimation extracted to phase_helpers module.
pub(super) use super::phase_helpers::{
    build_llm_metrics, check_environment_readiness, compute_embedding_sync, estimate_tokens,
    execute_prompt_response_mode, get_active_sdk_app_name, record_phase_token_usage,
    record_phase_token_usage_with_target, try_auto_connect_sdk_for_ui_workflow,
    REFLECTION_MODE_PREAMBLE,
};

// Phase executor submodules (extracted from this file)
pub mod agentic;
pub mod completion;
mod executor_impls;
pub mod setup;
pub mod verification;

// Re-export executor structs for backward compatibility
pub use agentic::AgenticExecutor;
pub use completion::CompletionExecutor;
pub use setup::SetupExecutor;
pub use verification::VerificationExecutor;

// Execution Timing Context
// =============================================================================

/// Build a timing context string from execution spans for the current execution.
///
/// Returns None if no spans exist or the query fails.
fn build_execution_timing_context(
    checkpoint_db: &CheckpointDb,
    execution_id: &str,
) -> Option<String> {
    // TODO: Wire to PG when get_execution_spans PG wrapper is implemented
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
async fn build_compressed_iteration_history(
    checkpoint_db: &std::sync::Arc<CheckpointDb>,
    execution_id: &str,
    current_iteration: u32,
    process_status_summary: Option<&str>,
    error_monitor_summary: Option<&str>,
    workflow_name: Option<&str>,
    max_context_tokens: usize,
    cross_workflow_learning: bool,
    project_path: Option<&str>,
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
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
    if let Ok(feedback) = pg_db.list_task_knowledge(execution_id, Some("verification_feedback"), false).await {
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
    let findings_result = pg_db.get_findings_for_task(execution_id).await;
    if let Ok(findings) = findings_result {
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
    let all_observations = pg_db.list_task_knowledge(execution_id, Some("observation"), false).await
        .unwrap_or_default();

    // Load all solutions for fix descriptions
    let all_solutions = pg_db.list_task_knowledge(execution_id, Some("solution"), false).await
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
        if let Ok(Some(result)) = pg_db.get_verification_phase_result(execution_id, recent_iter).await {
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
            if let Ok(Some(result)) = pg_db.get_verification_phase_result(execution_id, iter).await {
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
        if let Ok(all_knowledge) = pg_db.list_task_knowledge(execution_id, None, false).await {
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
            let scored_knowledge = {
                let db_c = checkpoint_db.clone();
                let wf_c = wf_name.to_string();
                tokio::task::spawn_blocking(move || {
                    db_c.with_conn(|conn| {
                        crate::reflection::prediction::score_knowledge_relevance(conn, &wf_c, 10)
                    })
                }).await.unwrap_or_else(|_| Err("spawn_blocking panicked".to_string()))
            };

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
                if let Ok(historical) = pg_db.list_workflow_knowledge(wf_name, execution_id, &["recurring_pattern", "context"], 10).await {
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
            if let Ok(insights) = pg_db.get_cross_workflow_knowledge(wf_name, execution_id, 3).await {
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
            if let Ok(knowledge) = pg_db.list_project_knowledge(pp, execution_id, 10).await {
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
        let findings_result2 = pg_db.get_findings_for_task(execution_id).await;
        if let Ok(findings) = findings_result2 {
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
                    let predicted_result = {
                        let db_c = checkpoint_db.clone();
                        let sig_c = sig.clone();
                        tokio::task::spawn_blocking(move || {
                            db_c.with_conn(|conn| {
                                crate::reflection::prediction::predict_fix_for_error(conn, &sig_c)
                            })
                        }).await.unwrap_or_else(|_| Err("spawn_blocking panicked".to_string()))
                    };
                    if let Ok(Some(predicted)) = predicted_result {
                        if predicted.confidence >= 0.3 {
                            prediction_lines.push(format!(
                                "- **[Predicted Fix, confidence: {:.0}%]** Based on {} previous successful application(s): {}",
                                predicted.confidence * 100.0,
                                predicted.reuse_count,
                                truncate_str(&predicted.fix_description, 300),
                            ));
                            prediction_count += 1;

                            // Record that this fix was shown to the AI (PG: save_fix_application)
                            let _ = pg_db.save_fix_application(
                                &predicted.fix_id,
                                execution_id,
                                Some(sig),
                                "shown",
                            ).await;
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
            let causal_events = {
                let db_c = checkpoint_db.clone();
                let wf_c = wf_name.to_string();
                tokio::task::spawn_blocking(move || {
                    db_c.with_conn(|conn| {
                        crate::reflection::causal::get_causal_events_for_workflow(conn, &wf_c, 10)
                    })
                }).await.unwrap_or_else(|_| Err("spawn_blocking panicked".to_string()))
            };

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
                    {
                        let db_c = checkpoint_db.clone();
                        tokio::task::spawn_blocking(move || {
                            db_c.with_conn(|conn| {
                                crate::database::hybrid_search::hybrid_search_universal_fixes(
                                    conn,
                                    &emb,
                                    &search_config,
                                )
                            })
                        }).await.ok().and_then(|r| r.ok())
                    }
                }
                Err(_) => None,
            }
        } else {
            None
        };

        // Fall back to SQL-only retrieval if hybrid search failed
        let fixes_to_inject: Vec<(String, String, Option<String>)> = match universal_fixes {
            Some(results) if !results.is_empty() => {
                // Capture retrieval event for RAG metric evaluation
                {
                    let retrieval_event =
                        crate::meta_optimizer::agentic_metrics::rag_judge::capture_retrieval_event(
                            &query_text,
                            "universal_fixes",
                            &results,
                            None,
                            |fix| (fix.fix_type.clone(), fix.fix_description.clone()),
                        );
                    let _ = {
                        let db_c = checkpoint_db.clone();
                        let eid_c = execution_id.to_string();
                        let evt_c = retrieval_event.clone();
                        tokio::task::spawn_blocking(move || {
                            db_c.with_conn(|conn| {
                                crate::meta_optimizer::agentic_metrics::rag_judge::persist_retrieval_event(
                                    conn,
                                    &eid_c,
                                    &evt_c,
                                )
                            })
                        }).await
                    };
                }
                results
                    .iter()
                    .map(|r| {
                        (
                            r.item.fix_type.clone(),
                            r.item.fix_description.clone(),
                            r.item.applicability_context.clone(),
                        )
                    })
                    .collect()
            }
            _ => {
                // SQL-only fallback: get universal fixes by reuse_count
                // PG: get_universal_fixes
                pg_db.get_universal_fixes(5).await
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
            if let Some(section) = build_impact_context(checkpoint_db, wf_name, &changed).await {
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
async fn build_impact_context(
    checkpoint_db: &std::sync::Arc<crate::database::CheckpointDb>,
    workflow_name: &str,
    changed_files: &[String],
) -> Option<String> {
    if changed_files.is_empty() {
        return None;
    }

    let mut impact_lines = Vec::new();

    for file_path in changed_files.iter().take(5) {
        let impact = {
            let db_c = checkpoint_db.clone();
            let wf_c = workflow_name.to_string();
            let fp_c = file_path.clone();
            tokio::task::spawn_blocking(move || {
                db_c.with_conn(|conn| {
                    crate::reflection::architecture::get_impact_analysis(conn, &wf_c, &fp_c)
                })
            }).await.ok().and_then(|r| r.ok())
        };

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
