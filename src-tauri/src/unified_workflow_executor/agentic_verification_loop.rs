//! Agentic Verification Loop — alternative workflow architecture.
//!
//! Runs a Verification Agent → Worker Agent loop where verification is performed
//! by an AI agent rather than deterministic steps.

use tracing::{info, warn};

use crate::step_executor::ExecutionStepConfig;
use crate::step_registry::StepEventLogger;

use super::health_monitor::fetch_verifier_ui_context;
use super::loop_controller::LoopController;
use super::types::{AgenticOutcome, LoopConfig, LoopResult};

// =============================================================================
// Iteration History — compressed cross-iteration context for agentic loops
// =============================================================================

/// Truncate a string to at most `max_chars` characters, appending "..." if truncated.
/// Safe for multi-byte UTF-8 (uses char boundaries, not byte indices).
fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{}...", truncated)
}

/// Summary of a single iteration's approach and outcome.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct IterationSummary {
    /// Iteration number (or range label for merged entries).
    iteration: String,
    /// What was tried (truncated to ~100 chars).
    approach: String,
    /// What happened (truncated to ~100 chars).
    result: String,
    /// Files that were touched during this iteration.
    files_touched: Vec<String>,
    /// Change in confidence from previous iteration.
    confidence_delta: f64,
}

/// Accumulates compressed history across iterations.
///
/// Inspired by bytedance/deer-flow: summarize completed sub-tasks into compressed
/// context so the worker agent knows what was already tried without token bloat.
/// Grows logarithmically by merging oldest entries when over budget.
struct IterationHistory {
    entries: Vec<IterationSummary>,
    max_total_chars: usize,
}

impl IterationHistory {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            max_total_chars: 4000,
        }
    }

    fn add(&mut self, summary: IterationSummary) {
        self.entries.push(summary);
        self.compress_if_needed();
    }

    /// Total estimated character count across all entries.
    fn total_chars(&self) -> usize {
        self.entries
            .iter()
            .map(|e| {
                e.approach.len()
                    + e.result.len()
                    + e.iteration.len()
                    + e.files_touched.iter().map(|f| f.len() + 2).sum::<usize>()
                    + 40 // formatting overhead per entry
            })
            .sum()
    }

    /// Merge oldest two entries into one when over budget, preserving at least 3 entries.
    fn compress_if_needed(&mut self) {
        while self.total_chars() > self.max_total_chars && self.entries.len() > 3 {
            let second = self.entries.remove(1);
            let first = &mut self.entries[0];

            // Merge iteration labels
            let merged_label = if first.iteration.contains('-') {
                // Already a range like "1-2", extend it
                let prefix = first.iteration.split('-').next().unwrap_or("?");
                format!("{}-{}", prefix, second.iteration)
            } else {
                format!("{}-{}", first.iteration, second.iteration)
            };

            // Merge approaches (truncate combined to ~100 chars)
            let merged_approach = format!("{} | {}", first.approach, second.approach);
            let merged_approach = truncate_with_ellipsis(&merged_approach, 100);

            // Merge results
            let merged_result = format!("{} | {}", first.result, second.result);
            let merged_result = truncate_with_ellipsis(&merged_result, 100);

            // Merge files (deduplicated)
            let mut merged_files = first.files_touched.clone();
            for f in &second.files_touched {
                if !merged_files.contains(f) {
                    merged_files.push(f.clone());
                }
            }

            first.iteration = merged_label;
            first.approach = merged_approach;
            first.result = merged_result;
            first.files_touched = merged_files;
            first.confidence_delta += second.confidence_delta;
        }
    }

    /// Format as compact history for injection into the worker agent prompt.
    fn to_context_string(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        let mut ctx = String::from("## Previous Attempts\n");
        for entry in &self.entries {
            ctx.push_str(&format!(
                "- Iter {}: {} → {} (confidence: {:+.1})\n",
                entry.iteration, entry.approach, entry.result, entry.confidence_delta
            ));
            if !entry.files_touched.is_empty() {
                ctx.push_str(&format!("  Files: {}\n", entry.files_touched.join(", ")));
            }
        }
        ctx
    }

    /// Serialize entries to JSON for database persistence.
    fn to_json(&self) -> String {
        serde_json::to_string(&self.entries).unwrap_or_else(|_| "[]".to_string())
    }
}

/// Extract an IterationSummary from a worker's output and the verification verdict.
fn extract_iteration_summary(
    iteration: u32,
    worker_summary: Option<&str>,
    verdict: &crate::autoresearch::agentic_verification::VerificationVerdict,
    prev_confidence: f64,
) -> IterationSummary {
    // Extract approach: first two lines of worker summary, truncated to 100 chars
    let approach = match worker_summary {
        Some(s) => {
            let condensed: String = s.lines().take(2).collect::<Vec<_>>().join(" ");
            truncate_with_ellipsis(&condensed, 100)
        }
        None => "(no worker ran)".to_string(),
    };

    // Extract result from verdict
    let obs_truncated: String = verdict.observations.chars().take(80).collect();
    let result = format!("{}: {}", verdict.status, obs_truncated);

    // Extract file paths from worker output
    let files_touched = worker_summary.map(extract_file_paths).unwrap_or_default();

    IterationSummary {
        iteration: iteration.to_string(),
        approach,
        result,
        files_touched,
        confidence_delta: verdict.confidence - prev_confidence,
    }
}

/// Extract file paths from text by looking for common path patterns.
fn extract_file_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for word in text.split_whitespace() {
        let cleaned = word.trim_matches(|c: char| c == '`' || c == '\'' || c == '"' || c == ',');
        // Match patterns like src/foo/bar.rs, ./components/App.tsx, etc.
        if (cleaned.contains('/') || cleaned.contains('\\'))
            && cleaned.contains('.')
            && cleaned.len() > 4
            && cleaned.len() < 200
            && !cleaned.starts_with("http")
            && !cleaned.starts_with("//")
        {
            // Normalize to forward slashes and take just the filename portion if too long
            let normalized = cleaned.replace('\\', "/");
            let path = if normalized.len() > 60 {
                // Keep just the last path segments
                normalized
                    .rsplit('/')
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("/")
            } else {
                normalized
            };
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        // Cap at 10 files to prevent bloat
        if paths.len() >= 10 {
            break;
        }
    }
    paths
}

impl LoopController {
    // =========================================================================
    // Agentic Verification Loop — alternative architecture
    // =========================================================================

    /// Run the agentic verification loop: Verification Agent → Worker Agent → repeat.
    ///
    /// Unlike the traditional loop that uses pre-defined deterministic verification steps,
    /// this architecture uses a verification *agent* that reasons about whether the goal
    /// has been achieved, and a worker agent that takes actions based on the verifier's
    /// feedback.
    ///
    /// Returns a standard LoopResult for compatibility with the existing result pipeline.
    pub(crate) async fn run_agentic_verification_loop(
        &self,
        config: &mut LoopConfig,
        has_agentic_steps: bool,
        agentic_steps: &[ExecutionStepConfig],
        _all_step_results: &mut Vec<crate::step_executor::StepExecutionResult>,
        logger: &StepEventLogger,
    ) -> LoopResult {
        use crate::autoresearch::agentic_verification::*;

        let av_config = config
            .agentic_verification_config
            .clone()
            .unwrap_or_default();

        let goal = if av_config.goal.is_empty() {
            config.base_prompt.clone()
        } else {
            av_config.goal.clone()
        };

        let max_iterations = if av_config.max_iterations > 0 {
            av_config.max_iterations
        } else {
            config.max_iterations
        };

        info!(
            "AGENTIC-VERIFICATION: Starting loop (max_iterations={}, confidence_threshold={}, consecutive_passes_required={})",
            max_iterations, av_config.confidence_threshold, av_config.required_consecutive_passes
        );

        let mut iteration_results: Vec<AgenticVerificationIterationResult> = Vec::new();
        let mut consecutive_passes: u32 = 0;
        let mut iteration: u32 = 0;
        let mut iteration_history = IterationHistory::new();
        let mut prev_confidence: f64 = 0.0;

        // Save original model/provider overrides so we can restore after each iteration
        let original_model_override = config.model_override.clone();
        let original_provider_override = config.provider_override.clone();

        loop {
            iteration += 1;

            // Check stop signal
            if self.is_task_stopped(&config.execution_id) {
                info!(
                    "AGENTIC-VERIFICATION: Stopped by user at iteration {}",
                    iteration
                );
                let av_result = AgenticVerificationResult {
                    iterations_run: iteration,
                    goal_achieved: false,
                    unreachable: false,
                    was_stopped: true,
                    max_iterations_reached: false,
                    iteration_results,
                    final_verdict: None,
                };
                self.canvas_manager
                    .lock()
                    .await
                    .on_agentic_verification_exit(&av_result)
                    .await;
                self.persist_iteration_history(&config.execution_id, &iteration_history);
                return av_result.to_loop_result();
            }

            // Check max iterations
            if iteration > max_iterations {
                info!(
                    "AGENTIC-VERIFICATION: Max iterations ({}) reached",
                    max_iterations
                );
                let av_result = AgenticVerificationResult {
                    iterations_run: iteration - 1,
                    goal_achieved: false,
                    unreachable: false,
                    was_stopped: false,
                    max_iterations_reached: true,
                    iteration_results: iteration_results.clone(),
                    final_verdict: iteration_results.last().map(|r| r.verdict.clone()),
                };
                self.canvas_manager
                    .lock()
                    .await
                    .on_agentic_verification_exit(&av_result)
                    .await;
                self.persist_iteration_history(&config.execution_id, &iteration_history);
                return av_result.to_loop_result();
            }

            // Update routing context
            config.set_routing_context(iteration, 0);

            // Record activity
            self.record_activity(
                &config.execution_id,
                &format!("agentic_verification_iteration_{}", iteration),
            );

            // ── STEP 1: Verification Agent ──────────────────────────────
            // The verification agent assesses the current state against the goal.
            // On the first iteration (if verify_first is false), we skip directly
            // to the worker agent.

            let should_verify = iteration > 1 || av_config.verify_first;

            let verdict = if should_verify {
                info!(
                    "AGENTIC-VERIFICATION: Running verification agent (iteration {})",
                    iteration
                );

                let verifier_start = std::time::Instant::now();

                let verifier_system = AgenticVerificationPrompts::verifier_system_prompt(
                    &goal,
                    av_config.verifier.system_preamble.as_deref(),
                );

                // Build context for the verifier: include previous iteration results
                let mut verifier_context = verifier_system;
                if let Some(last_result) = iteration_results.last() {
                    verifier_context.push_str(&format!(
                        "\n\n## Previous Iteration ({}) Summary\nWorker action: {}\n",
                        last_result.iteration,
                        last_result.worker_summary.as_deref().unwrap_or("(none)"),
                    ));
                }

                // Include compressed iteration history for verifier awareness
                let history_ctx = iteration_history.to_context_string();
                if !history_ctx.is_empty() {
                    verifier_context.push_str(&format!("\n\n{}", history_ctx));
                }

                // Fetch live UI Bridge data for the verifier
                let ui_context = fetch_verifier_ui_context(
                    av_config.verifier.use_screenshots,
                    av_config.verifier.include_console_errors,
                    av_config.verifier.include_app_health,
                )
                .await;
                if !ui_context.is_empty() {
                    verifier_context.push_str(&ui_context);
                }

                // Apply verifier model/provider overrides
                config.model_override = av_config
                    .verifier
                    .model
                    .clone()
                    .or_else(|| original_model_override.clone());
                config.provider_override = av_config
                    .verifier
                    .provider
                    .clone()
                    .or_else(|| original_provider_override.clone());

                // Run verifier through the agentic executor with a verification prompt
                let verifier_prompt = ExecutionStepConfig {
                    step_type: "prompt".to_string(),
                    name: Some(format!("Verification Agent (iteration {})", iteration)),
                    prompt_content: Some(verifier_context),
                    ..ExecutionStepConfig::default()
                };

                let (outcome, _injected) = self
                    .agentic_executor
                    .run_agentic(
                        config,
                        iteration,
                        "", // No failure context — the verifier generates its own
                        true,
                        &[verifier_prompt],
                        logger,
                    )
                    .await;

                let verifier_duration = verifier_start.elapsed().as_millis() as u64;

                // Parse the verdict from the AI output
                let verdict = match &outcome {
                    AgenticOutcome::Success { output, .. }
                    | AgenticOutcome::Failed { output, .. } => {
                        parse_verification_verdict(output)
                            .unwrap_or_else(|| {
                                info!("AGENTIC-VERIFICATION: Failed to parse structured verdict, using heuristic");
                                heuristic_verdict(output)
                            })
                    }
                    AgenticOutcome::Error { error } => {
                        warn!("AGENTIC-VERIFICATION: Verifier error: {}", error);
                        VerificationVerdict {
                            status: VerificationStatus::Fail,
                            confidence: 0.0,
                            observations: format!("Verification agent error: {}", error),
                            next_priority: Some("Retry verification".to_string()),
                            issues: vec![VerificationIssue {
                                description: error.clone(),
                                severity: "critical".to_string(),
                                suggestion: None,
                            }],
                            unreachable: false,
                            unreachable_reason: None,
                        }
                    }
                    AgenticOutcome::Skipped => {
                        VerificationVerdict {
                            status: VerificationStatus::Fail,
                            confidence: 0.0,
                            observations: "Verification agent was skipped".to_string(),
                            next_priority: Some("Configure verification agent".to_string()),
                            issues: vec![],
                            unreachable: false,
                            unreachable_reason: None,
                        }
                    }
                };

                info!(
                    "AGENTIC-VERIFICATION: Verdict: status={}, confidence={:.0}%, issues={}  ({}ms)",
                    verdict.status,
                    verdict.confidence * 100.0,
                    verdict.issues.len(),
                    verifier_duration,
                );

                // Emit verdict to task output
                if let Err(e) = self.checkpoint_db.append_task_output_ex(
                    &config.execution_id,
                    &format!(
                        "\n--- Verification Agent (iteration {}) ---\nStatus: {}\nConfidence: {:.0}%\nObservations: {}\n",
                        iteration,
                        verdict.status,
                        verdict.confidence * 100.0,
                        verdict.observations,
                    ),
                    false,
                    false,
                ) {
                    warn!("Failed to append verifier output: {}", e);
                }

                Some((verdict, verifier_duration))
            } else {
                None
            };

            // ── STEP 2: Check if goal achieved ──────────────────────────
            if let Some((ref v, _)) = verdict {
                if v.status == VerificationStatus::Pass
                    && v.confidence >= av_config.confidence_threshold
                {
                    consecutive_passes += 1;
                    if consecutive_passes >= av_config.required_consecutive_passes {
                        info!(
                            "AGENTIC-VERIFICATION: Goal achieved after {} iterations ({} consecutive passes)",
                            iteration, consecutive_passes
                        );
                        iteration_results.push(AgenticVerificationIterationResult {
                            iteration,
                            verdict: v.clone(),
                            worker_ran: false,
                            worker_summary: None,
                            verifier_duration_ms: verdict.as_ref().map(|(_, d)| *d).unwrap_or(0),
                            worker_duration_ms: 0,
                        });
                        // Canvas: emit iteration panel and exit summary
                        {
                            let verifier_ms = verdict.as_ref().map(|(_, d)| *d).unwrap_or(0);
                            self.canvas_manager
                                .lock()
                                .await
                                .on_agentic_verification_iteration(
                                    iteration,
                                    v,
                                    None,
                                    verifier_ms,
                                    0,
                                )
                                .await;
                        }
                        let av_result = AgenticVerificationResult {
                            iterations_run: iteration,
                            goal_achieved: true,
                            unreachable: false,
                            was_stopped: false,
                            max_iterations_reached: false,
                            iteration_results,
                            final_verdict: Some(v.clone()),
                        };
                        self.canvas_manager
                            .lock()
                            .await
                            .on_agentic_verification_exit(&av_result)
                            .await;
                        self.persist_iteration_history(&config.execution_id, &iteration_history);
                        return av_result.to_loop_result();
                    }
                } else {
                    consecutive_passes = 0;
                }

                // Check unreachable
                if v.status == VerificationStatus::Unreachable || v.unreachable {
                    info!(
                        "AGENTIC-VERIFICATION: Goal deemed unreachable: {}",
                        v.unreachable_reason
                            .as_deref()
                            .unwrap_or("(no reason given)")
                    );
                    // Canvas: emit iteration panel for unreachable verdict
                    {
                        let verifier_ms = verdict.as_ref().map(|(_, d)| *d).unwrap_or(0);
                        self.canvas_manager
                            .lock()
                            .await
                            .on_agentic_verification_iteration(iteration, v, None, verifier_ms, 0)
                            .await;
                    }
                    iteration_results.push(AgenticVerificationIterationResult {
                        iteration,
                        verdict: v.clone(),
                        worker_ran: false,
                        worker_summary: None,
                        verifier_duration_ms: verdict.as_ref().map(|(_, d)| *d).unwrap_or(0),
                        worker_duration_ms: 0,
                    });
                    let av_result = AgenticVerificationResult {
                        iterations_run: iteration,
                        goal_achieved: false,
                        unreachable: true,
                        was_stopped: false,
                        max_iterations_reached: false,
                        iteration_results,
                        final_verdict: Some(v.clone()),
                    };
                    self.canvas_manager
                        .lock()
                        .await
                        .on_agentic_verification_exit(&av_result)
                        .await;
                    self.persist_iteration_history(&config.execution_id, &iteration_history);
                    return av_result.to_loop_result();
                }
            }

            // ── STEP 3: Worker Agent ────────────────────────────────────
            // The worker agent receives the verifier's feedback and takes action.

            info!(
                "AGENTIC-VERIFICATION: Running worker agent (iteration {})",
                iteration
            );

            let worker_start = std::time::Instant::now();

            // Apply worker model/provider overrides
            config.model_override = av_config
                .worker
                .model
                .clone()
                .or_else(|| original_model_override.clone());
            config.provider_override = av_config
                .worker
                .provider
                .clone()
                .or_else(|| original_provider_override.clone());

            // Build worker context from the verification verdict
            let mut worker_failure_context = if let Some((ref v, _)) = verdict {
                AgenticVerificationPrompts::worker_context_from_verdict(
                    &goal,
                    v,
                    iteration,
                    max_iterations,
                )
            } else {
                // First iteration without verify_first — just provide the goal
                format!(
                    "## Goal\n{}\n\nThis is the first iteration. Assess the current state and take action toward the goal.\n",
                    goal
                )
            };

            // Inject compressed iteration history so the worker knows what was already tried
            let history_context = iteration_history.to_context_string();
            if !history_context.is_empty() {
                worker_failure_context.push_str(&format!(
                    "\n\n{}\nIMPORTANT: Do NOT repeat approaches that already failed (see Previous Attempts above).\n",
                    history_context
                ));
            }

            let (worker_outcome, _injected_steps) = self
                .agentic_executor
                .run_agentic(
                    config,
                    iteration,
                    &worker_failure_context,
                    has_agentic_steps,
                    agentic_steps,
                    logger,
                )
                .await;

            let worker_duration = worker_start.elapsed().as_millis() as u64;

            let worker_summary = match &worker_outcome {
                AgenticOutcome::Success { output, .. } => {
                    // Extract first 500 chars as summary (char-safe for UTF-8)
                    Some(truncate_with_ellipsis(output, 500))
                }
                AgenticOutcome::Failed { output, error, .. } => {
                    let output_preview: String = output.chars().take(200).collect();
                    Some(format!("Failed: {} (output: {}...)", error, output_preview))
                }
                AgenticOutcome::Error { error } => Some(format!("Error: {}", error)),
                AgenticOutcome::Skipped => None,
            };

            info!(
                "AGENTIC-VERIFICATION: Worker completed in {}ms",
                worker_duration
            );

            // ── Record iteration result ─────────────────────────────────
            let iter_result = AgenticVerificationIterationResult {
                iteration,
                verdict: verdict
                    .as_ref()
                    .map(|(v, _)| v.clone())
                    .unwrap_or(VerificationVerdict {
                        status: VerificationStatus::Partial,
                        confidence: 0.0,
                        observations: "No verification performed (first iteration)".to_string(),
                        next_priority: None,
                        issues: vec![],
                        unreachable: false,
                        unreachable_reason: None,
                    }),
                worker_ran: true,
                worker_summary,
                verifier_duration_ms: verdict.as_ref().map(|(_, d)| *d).unwrap_or(0),
                worker_duration_ms: worker_duration,
            };
            // Build compressed iteration summary for cross-iteration context
            {
                let summary = extract_iteration_summary(
                    iteration,
                    iter_result.worker_summary.as_deref(),
                    &iter_result.verdict,
                    prev_confidence,
                );
                prev_confidence = iter_result.verdict.confidence;
                iteration_history.add(summary);
            }
            iteration_results.push(iter_result);

            // Canvas: emit iteration panel and update tracker
            {
                let last = iteration_results.last().expect("just pushed");
                let mut cm = self.canvas_manager.lock().await;
                cm.on_agentic_verification_iteration(
                    iteration,
                    &last.verdict,
                    last.worker_summary.as_deref(),
                    last.verifier_duration_ms,
                    last.worker_duration_ms,
                )
                .await;
                cm.on_agentic_verification_tracker(&iteration_results).await;
            }

            // Restore original model/provider overrides for next iteration
            config.model_override = original_model_override.clone();
            config.provider_override = original_provider_override.clone();

            // Increment session count in DB
            if let Err(e) = self.checkpoint_db.append_task_output_ex(
                &config.execution_id,
                "",
                true,  // increment_session
                false, // check_completion_marker
            ) {
                warn!("Failed to increment session count: {}", e);
            }
        }
    }

    /// Persist iteration history to the database for post-analysis by the meta-optimizer.
    fn persist_iteration_history(&self, execution_id: &str, history: &IterationHistory) {
        if history.entries.is_empty() {
            return;
        }
        let history_json = history.to_json();
        if let Err(e) = self
            .checkpoint_db
            .update_task_run_iteration_history(execution_id, &history_json)
        {
            warn!(
                "Failed to persist iteration history for {}: {}",
                execution_id, e
            );
        }
    }
}
