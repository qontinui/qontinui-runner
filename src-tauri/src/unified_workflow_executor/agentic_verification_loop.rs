//! Agentic Verification Loop — alternative workflow architecture.
//!
//! Runs a Verification Agent → Worker Agent loop where verification is performed
//! by an AI agent rather than deterministic steps.

use tracing::{debug, info, warn};

use crate::orchestrator::brain_actor::{BrainActorConfig, BrainActorOrchestrator, BrainOutput};
use crate::orchestrator::structured_output::StructuredSignal;
use crate::step_executor::ExecutionStepConfig;
use crate::step_registry::StepEventLogger;

use super::health_monitor::fetch_verifier_ui_context;
use super::loop_controller::LoopController;
use super::types::{AgenticOutcome, LoopConfig, LoopResult};

/// Query total tokens consumed across all iterations for a given execution.
fn query_total_tokens(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    execution_id: &str,
) -> (Option<u64>, Option<f64>) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let pg = pg_db.clone();
        let id = execution_id.to_string();
        let pg_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle.block_on(async move { pg.get_phase_token_usage(&id).await })
        }));
        if let Ok(Ok(rows)) = pg_result {
            if !rows.is_empty() {
                let total_in: u64 = rows.iter().map(|r| r.input_tokens).sum();
                let total_out: u64 = rows.iter().map(|r| r.output_tokens).sum();
                let total_tokens = total_in + total_out;
                let total_cost = crate::ai_pricing::calculate_cost_usd(
                    total_in,
                    total_out,
                    rows.first()
                        .and_then(|r| r.model_used.as_deref())
                        .unwrap_or("claude-sonnet-4-20250514"),
                );
                return (
                    if total_tokens > 0 { Some(total_tokens) } else { None },
                    if total_cost > 0.0 { Some(total_cost) } else { None },
                );
            }
            return (None, None);
        }
    }

    (None, None)
}

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
    compression_mode: crate::orchestrator::compression::IterationCompressionMode,
}

impl IterationHistory {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            max_total_chars: 4000,
            compression_mode: crate::orchestrator::compression::IterationCompressionMode::CharBased,
        }
    }

    fn with_compression_mode(
        mut self,
        mode: crate::orchestrator::compression::IterationCompressionMode,
    ) -> Self {
        self.compression_mode = mode;
        self
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

    /// Compress entries when over budget, preserving at least 3 entries.
    ///
    /// In CharBased mode: merges oldest two entries by concatenating + truncating.
    /// In LlmSummarized mode: sends oldest entries to an LLM for summarization,
    /// falling back to char-based if the LLM call fails.
    fn compress_if_needed(&mut self) {
        if self.total_chars() <= self.max_total_chars || self.entries.len() <= 3 {
            return;
        }

        // Try LLM summarization first if configured
        if self.compression_mode
            == crate::orchestrator::compression::IterationCompressionMode::LlmSummarized
            && self.entries.len() > 4
        {
            // Collect the oldest entries (all except the last 3)
            let keep_count = 3;
            let entries_to_summarize = self.entries.len() - keep_count;
            let old_entries: Vec<String> = self.entries[..entries_to_summarize]
                .iter()
                .map(|e| format!("Iter {}: {} → {}", e.iteration, e.approach, e.result))
                .collect();

            let summary = crate::orchestrator::compression::summarize_iterations(
                &old_entries,
                self.max_total_chars / 3, // budget ~1/3 of total for compressed history
                self.compression_mode,
            );

            if !summary.is_empty() {
                // Replace all old entries with a single summary entry
                let merged_label = format!(
                    "{}-{}",
                    self.entries[0].iteration,
                    self.entries[entries_to_summarize - 1].iteration
                );
                let merged_files: Vec<String> = self.entries[..entries_to_summarize]
                    .iter()
                    .flat_map(|e| e.files_touched.clone())
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();
                let merged_delta: f64 = self.entries[..entries_to_summarize]
                    .iter()
                    .map(|e| e.confidence_delta)
                    .sum();

                let summary_entry = IterationSummary {
                    iteration: merged_label,
                    approach: summary,
                    result: "(LLM summary)".to_string(),
                    files_touched: merged_files,
                    confidence_delta: merged_delta,
                };

                // Remove old entries and insert summary at front
                self.entries.drain(..entries_to_summarize);
                self.entries.insert(0, summary_entry);
                return;
            }
            // If LLM summary returned empty, fall through to char-based
        }

        // Char-based compression: merge oldest two entries
        while self.total_chars() > self.max_total_chars && self.entries.len() > 3 {
            let second = self.entries.remove(1);
            let first = &mut self.entries[0];

            let merged_label = if first.iteration.contains('-') {
                let prefix = first.iteration.split('-').next().unwrap_or("?");
                format!("{}-{}", prefix, second.iteration)
            } else {
                format!("{}-{}", first.iteration, second.iteration)
            };

            let merged_approach = format!("{} | {}", first.approach, second.approach);
            let merged_approach = truncate_with_ellipsis(&merged_approach, 100);

            let merged_result = format!("{} | {}", first.result, second.result);
            let merged_result = truncate_with_ellipsis(&merged_result, 100);

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
    let obs_truncated = truncate_with_ellipsis(&verdict.observations, 80);
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

        // Brain/Actor orchestrator (opt-in: enabled via agentic_verification_config)
        let brain_actor_config = BrainActorConfig {
            enabled: av_config.brain_actor_enabled.unwrap_or(false),
            ..Default::default()
        };
        let mut brain_actor = BrainActorOrchestrator::new(brain_actor_config);

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
                    total_tokens: query_total_tokens(&self.app_state.pg_db, &config.execution_id).0,
                    total_cost_usd: query_total_tokens(&self.app_state.pg_db, &config.execution_id).1,
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
                    total_tokens: query_total_tokens(&self.app_state.pg_db, &config.execution_id).0,
                    total_cost_usd: query_total_tokens(&self.app_state.pg_db, &config.execution_id).1,
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
                    verifier_context.push_str(&ui_context.text);
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
                    AgenticOutcome::BudgetExceeded { reason } => {
                        warn!("AGENTIC-VERIFICATION: Budget exceeded: {}", reason);
                        VerificationVerdict {
                            status: VerificationStatus::Fail,
                            confidence: 0.0,
                            observations: format!("BUDGET EXCEEDED: {}", reason),
                            next_priority: None,
                            issues: vec![VerificationIssue {
                                description: format!("Budget exceeded: {}", reason),
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
                let verifier_output = format!(
                    "\n--- Verification Agent (iteration {}) ---\nStatus: {}\nConfidence: {:.0}%\nObservations: {}\n",
                    iteration,
                    verdict.status,
                    verdict.confidence * 100.0,
                    verdict.observations,
                );
                if let Err(e) = self.app_state.pg_db.append_task_output_ex(&config.execution_id, &verifier_output, false, false).await {
                    warn!("PG append_task_output_ex failed: {}", e);
                }
                // PG write already done above; SQLite fallback removed

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
                            total_tokens: {
                                let (t, _) =
                                    query_total_tokens(&self.app_state.pg_db, &config.execution_id);
                                t
                            },
                            total_cost_usd: {
                                let (_, c) =
                                    query_total_tokens(&self.app_state.pg_db, &config.execution_id);
                                c
                            },
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
                        total_tokens: {
                            let (t, _) =
                                query_total_tokens(&self.app_state.pg_db, &config.execution_id);
                            t
                        },
                        total_cost_usd: {
                            let (_, c) =
                                query_total_tokens(&self.app_state.pg_db, &config.execution_id);
                            c
                        },
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

            // ── Brain/Actor enrichment (when enabled) ──
            // If Brain/Actor mode is active, invoke the Brain for visual analysis
            // and inject its action plan into the worker context.
            if brain_actor.is_enabled() {
                brain_actor.start_iteration();

                if brain_actor.should_invoke_brain(iteration) {
                    // Fetch UI context with elements for the Brain
                    let brain_ui = fetch_verifier_ui_context(true, true, true).await;

                    if !brain_ui.elements.is_empty() {
                        // Build element index text
                        let element_index: String = brain_ui.elements
                            .iter()
                            .map(|e| format!(
                                "[{}] \"{}\" {} at ({:.3}, {:.3})",
                                e.index, e.label, e.element_type,
                                e.normalized_rect.x, e.normalized_rect.y,
                            ))
                            .collect::<Vec<_>>()
                            .join("\n");

                        // Try to capture and annotate a screenshot for multimodal Brain call
                        let brain_screenshot = self.try_capture_annotated_screenshot(&brain_ui.elements).await;

                        // Build Brain prompt
                        let brain_system = format!(
                            "You are a Visual Analysis Brain agent. Analyze the current screen state \
                             and produce an action plan.\n\n\
                             ## Goal\n{}\n\n\
                             ## Element Index\n{}\n\n\
                             ## Current UI State\n{}\n\n\
                             {}\n\n\
                             Respond with JSON:\n\
                             ```json\n{{\n\
                               \"screen_analysis\": \"what you observe on screen\",\n\
                               \"suggested_plan\": [\"step 1\", \"step 2\", ...],\n\
                               \"context_notes\": \"any observations for the Actor\"\n\
                             }}\n```",
                            goal,
                            element_index,
                            brain_ui.text,
                            brain_actor.records_context(),
                        );

                        // Send to Brain model — multimodal if screenshot available, text-only otherwise
                        let brain_response = if let Some(ref screenshot_b64) = brain_screenshot {
                            let prompt = crate::ai_provider::MultimodalPrompt::with_screenshot(
                                &brain_system,
                                screenshot_b64,
                            );
                            crate::ai_provider::run_prompt_with_routing_multimodal(
                                &prompt,
                                &crate::ai_router::TaskContext::from_prompt(&brain_system),
                                self.doctor_handle.as_ref(),
                            )
                        } else {
                            crate::ai_provider::run_prompt_with_routing(
                                &brain_system,
                                &crate::ai_router::TaskContext::from_prompt(&brain_system),
                                self.doctor_handle.as_ref(),
                            )
                        };

                        // Parse Brain response into BrainOutput
                        let brain_output = if brain_response.success {
                            Self::parse_brain_response(
                                &brain_response.output,
                                &element_index,
                                &brain_ui.text,
                                iteration,
                                &brain_actor,
                            )
                        } else {
                            // Fallback: use raw UI context as analysis
                            warn!(
                                "AGENTIC-VERIFICATION: Brain LLM call failed ({}), using UI context fallback",
                                brain_response.error.as_deref().unwrap_or("unknown")
                            );
                            BrainOutput {
                                screen_analysis: brain_ui.text.clone(),
                                element_index: element_index.clone(),
                                suggested_plan: vec![],
                                context_notes: brain_actor.records_context(),
                                iteration_produced: iteration,
                            }
                        };

                        brain_actor.set_brain_output(brain_output);

                        debug!(
                            "AGENTIC-VERIFICATION: Brain analyzed {} elements for iteration {} (multimodal: {})",
                            brain_ui.elements.len(),
                            iteration,
                            brain_screenshot.is_some(),
                        );
                    }
                }

                // Inject Brain context into worker prompt if available
                if let Some(actor_prompt) = brain_actor.build_actor_prompt(&goal) {
                    worker_failure_context.push_str(&format!(
                        "\n\n## Visual Analysis (Brain Agent)\n{}",
                        actor_prompt
                    ));
                }
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
                AgenticOutcome::BudgetExceeded { reason } => Some(format!("Budget exceeded: {}", reason)),
                AgenticOutcome::Skipped => None,
            };

            info!(
                "AGENTIC-VERIFICATION: Worker completed in {}ms",
                worker_duration
            );

            // ── Brain/Actor signal processing ──
            // If Brain/Actor is enabled, process any RequestContext or RecordStore
            // signals from the worker output for the next iteration.
            if brain_actor.is_enabled() {
                if let Some(ref summary) = worker_summary {
                    let parsed =
                        crate::orchestrator::structured_output::parse_worker_output(summary);
                    let output = parsed.get_output(iteration);
                    brain_actor.process_signals(&output.signals);
                }

                // Adapt Brain cache TTL based on observed cache efficiency.
                // Use a conservative heuristic: after the first iteration the cache
                // is being reused, so approximate a hit rate based on iteration depth.
                let cache_hit_rate = if iteration > 1 { 0.7 } else { 0.0 };
                brain_actor.adapt_cache_ttl(cache_hit_rate);
            }

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
            if let Err(e) = self.app_state.pg_db.append_task_output_ex(&config.execution_id, "", true, false).await {
                warn!("PG append_task_output_ex (session increment) failed: {}", e);
            }
            // PG write already done above; SQLite fallback removed
        }
    }

    /// Try to capture a screenshot and annotate it with element markers.
    ///
    /// Returns the annotated screenshot as base64, or None if capture/annotation fails.
    /// Uses the annotation engine to draw numbered boxes on detected elements.
    async fn try_capture_annotated_screenshot(
        &self,
        elements: &[crate::vision::annotator::AnnotatedElement],
    ) -> Option<String> {
        if elements.is_empty() {
            return None;
        }

        // Try to get a screenshot from the UI Bridge
        let port = crate::mcp::types::get_mcp_api_port();
        let url = format!(
            "http://127.0.0.1:{}/ui-bridge/sdk/control/screenshot",
            port
        );

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .ok()?;

        let resp = client.get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }

        let body: serde_json::Value = resp.json().await.ok()?;
        let screenshot_b64 = body
            .get("data")
            .and_then(|d| d.get("screenshot"))
            .or_else(|| body.get("screenshot"))
            .and_then(|v| v.as_str())?;

        // Annotate the screenshot
        let max_width = 1024_u32; // Default resize for token efficiency

        match crate::vision::annotator::annotate_screenshot(screenshot_b64, elements, max_width) {
            Ok(result) => {
                debug!(
                    "Annotated screenshot with {} elements (base64: {} bytes)",
                    result.element_count,
                    result.annotated_base64.len()
                );
                Some(result.annotated_base64)
            }
            Err(e) => {
                debug!("Screenshot annotation failed: {}", e);
                None
            }
        }
    }

    /// Parse the Brain LLM response into a BrainOutput struct.
    ///
    /// Tries to extract JSON from the response, falls back to using the raw
    /// response as screen_analysis.
    fn parse_brain_response(
        output: &str,
        element_index: &str,
        ui_text: &str,
        iteration: u32,
        brain_actor: &BrainActorOrchestrator,
    ) -> BrainOutput {
        // Try to extract JSON block from the response
        if let Some(json_start) = output.find('{') {
            if let Some(json_end) = output.rfind('}') {
                let json_str = &output[json_start..=json_end];
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
                    let screen_analysis = parsed
                        .get("screen_analysis")
                        .and_then(|v| v.as_str())
                        .unwrap_or(ui_text)
                        .to_string();

                    let suggested_plan = parsed
                        .get("suggested_plan")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    let context_notes = parsed
                        .get("context_notes")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    return BrainOutput {
                        screen_analysis,
                        element_index: element_index.to_string(),
                        suggested_plan,
                        context_notes,
                        iteration_produced: iteration,
                    };
                }
            }
        }

        // Fallback: use the raw output as screen analysis
        BrainOutput {
            screen_analysis: output.to_string(),
            element_index: element_index.to_string(),
            suggested_plan: vec![],
            context_notes: brain_actor.records_context(),
            iteration_produced: iteration,
        }
    }

    /// Persist iteration history to the database for post-analysis by the meta-optimizer.
    fn persist_iteration_history(&self, _execution_id: &str, _history: &IterationHistory) {
        // Iteration history persistence removed — all persistence now via PgDb.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── truncate_with_ellipsis ──────────────────────────────────────

    #[test]
    fn test_truncate_short_string_unchanged() {
        assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact_length_unchanged() {
        assert_eq!(truncate_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_adds_ellipsis() {
        let result = truncate_with_ellipsis("hello world this is long", 10);
        assert!(result.ends_with("..."));
        assert!(result.chars().count() <= 10);
    }

    #[test]
    fn test_truncate_multibyte_utf8() {
        // Japanese text: each char is 3 bytes
        let s = "こんにちは世界テスト文字列";
        let result = truncate_with_ellipsis(s, 8);
        assert!(result.ends_with("..."));
        assert!(result.chars().count() <= 8);
        // Should not panic — the whole point of this test
    }

    #[test]
    fn test_truncate_empty_string() {
        assert_eq!(truncate_with_ellipsis("", 10), "");
    }

    #[test]
    fn test_truncate_max_zero() {
        // max_chars=0, saturating_sub(3) = 0, so we get just "..."
        let result = truncate_with_ellipsis("hello", 0);
        assert_eq!(result, "...");
    }

    // ── extract_file_paths ──────────────────────────────────────────

    #[test]
    fn test_extract_paths_basic() {
        let text = "Modified src/main.rs and src/lib.rs successfully";
        let paths = extract_file_paths(text);
        assert!(paths.contains(&"src/main.rs".to_string()));
        assert!(paths.contains(&"src/lib.rs".to_string()));
    }

    #[test]
    fn test_extract_paths_backslash() {
        let text = "Edited src\\components\\App.tsx";
        let paths = extract_file_paths(text);
        assert!(paths.contains(&"src/components/App.tsx".to_string()));
    }

    #[test]
    fn test_extract_paths_ignores_urls() {
        let text = "See https://example.com/foo/bar.html for details";
        let paths = extract_file_paths(text);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_extract_paths_ignores_comments() {
        let text = "// this is a comment without any paths";
        let paths = extract_file_paths(text);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_extract_paths_empty() {
        assert!(extract_file_paths("").is_empty());
    }

    #[test]
    fn test_extract_paths_deduplicates() {
        let text = "Edit src/main.rs then src/main.rs again";
        let paths = extract_file_paths(text);
        assert_eq!(paths.iter().filter(|p| *p == "src/main.rs").count(), 1);
    }

    #[test]
    fn test_extract_paths_caps_at_10() {
        let text = (0..20)
            .map(|i| format!("src/file{}.rs", i))
            .collect::<Vec<_>>()
            .join(" ");
        let paths = extract_file_paths(&text);
        assert!(paths.len() <= 10);
    }

    #[test]
    fn test_extract_paths_strips_quotes() {
        let text = r#"Changed `src/foo.rs` and "src/bar.rs""#;
        let paths = extract_file_paths(text);
        assert!(paths.contains(&"src/foo.rs".to_string()));
        assert!(paths.contains(&"src/bar.rs".to_string()));
    }

    // ── IterationHistory ────────────────────────────────────────────

    fn make_summary(iter: u32, approach: &str, result: &str) -> IterationSummary {
        IterationSummary {
            iteration: iter.to_string(),
            approach: approach.to_string(),
            result: result.to_string(),
            files_touched: vec!["src/main.rs".to_string()],
            confidence_delta: 0.1,
        }
    }

    #[test]
    fn test_history_empty_context_string() {
        let history = IterationHistory::new();
        assert!(history.to_context_string().is_empty());
    }

    #[test]
    fn test_history_single_entry() {
        let mut history = IterationHistory::new();
        history.add(make_summary(
            1,
            "Try button click",
            "Fail: button not found",
        ));
        let ctx = history.to_context_string();
        assert!(ctx.contains("## Previous Attempts"));
        assert!(ctx.contains("Iter 1"));
        assert!(ctx.contains("Try button click"));
        assert!(ctx.contains("Fail: button not found"));
        assert!(ctx.contains("src/main.rs"));
    }

    #[test]
    fn test_history_preserves_three_entries() {
        let mut history = IterationHistory::new();
        history.add(make_summary(1, "A", "R1"));
        history.add(make_summary(2, "B", "R2"));
        history.add(make_summary(3, "C", "R3"));
        // Should NOT compress — exactly 3 entries, and total_chars is well under 4000
        assert_eq!(history.entries.len(), 3);
    }

    #[test]
    fn test_history_compression_merges_oldest() {
        let mut history = IterationHistory::new();
        // Use a small budget to force compression
        history.max_total_chars = 200;
        history.add(make_summary(
            1,
            "Approach one with some detail",
            "Result one",
        ));
        history.add(make_summary(
            2,
            "Approach two with some detail",
            "Result two",
        ));
        history.add(make_summary(
            3,
            "Approach three with detail",
            "Result three",
        ));
        history.add(make_summary(4, "Approach four with detail", "Result four"));
        // With small budget, oldest entries should be merged
        assert!(history.entries.len() <= 4);
        // First entry should be a merged range
        if history.entries.len() < 4 {
            assert!(history.entries[0].iteration.contains('-'));
        }
    }

    #[test]
    fn test_history_never_fewer_than_three() {
        let mut history = IterationHistory::new();
        history.max_total_chars = 50; // Very small budget
        for i in 1..=10 {
            history.add(make_summary(
                i,
                &format!("Approach {}", i),
                &format!("Result {}", i),
            ));
        }
        assert!(history.entries.len() >= 3);
    }

    #[test]
    fn test_history_to_json_roundtrip() {
        let mut history = IterationHistory::new();
        history.add(make_summary(1, "Click button", "Pass: button visible"));
        let json = history.to_json();
        let parsed: Vec<IterationSummary> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].iteration, "1");
        assert_eq!(parsed[0].approach, "Click button");
    }

    #[test]
    fn test_history_empty_json() {
        let history = IterationHistory::new();
        assert_eq!(history.to_json(), "[]");
    }

    #[test]
    fn test_history_confidence_delta_accumulates_on_merge() {
        let mut history = IterationHistory::new();
        history.max_total_chars = 150;
        let mut s1 = make_summary(1, "A", "R");
        s1.confidence_delta = 0.2;
        let mut s2 = make_summary(2, "B", "R");
        s2.confidence_delta = 0.3;
        history.add(s1);
        history.add(s2);
        history.add(make_summary(3, "C", "R"));
        history.add(make_summary(4, "D", "R"));
        // If 1 and 2 were merged, their confidence deltas should be summed
        if history.entries[0].iteration.contains('-') {
            assert!((history.entries[0].confidence_delta - 0.5).abs() < 0.01);
        }
    }

    // ── extract_iteration_summary ───────────────────────────────────

    #[test]
    fn test_extract_iteration_summary_basic() {
        use crate::autoresearch::agentic_verification::*;
        let verdict = VerificationVerdict {
            status: VerificationStatus::Fail,
            confidence: 0.3,
            observations: "Button not clickable".to_string(),
            next_priority: None,
            issues: vec![],
            unreachable: false,
            unreachable_reason: None,
        };
        let summary = extract_iteration_summary(
            1,
            Some("Clicked the submit button\nWaited for response"),
            &verdict,
            0.0,
        );
        assert_eq!(summary.iteration, "1");
        assert!(summary.approach.contains("Clicked the submit button"));
        assert!(summary.result.contains("fail"));
        assert!((summary.confidence_delta - 0.3).abs() < 0.01);
    }

    #[test]
    fn test_extract_iteration_summary_no_worker() {
        use crate::autoresearch::agentic_verification::*;
        let verdict = VerificationVerdict {
            status: VerificationStatus::Pass,
            confidence: 0.9,
            observations: "All checks passed".to_string(),
            next_priority: None,
            issues: vec![],
            unreachable: false,
            unreachable_reason: None,
        };
        let summary = extract_iteration_summary(5, None, &verdict, 0.7);
        assert_eq!(summary.approach, "(no worker ran)");
        assert!((summary.confidence_delta - 0.2).abs() < 0.01);
    }

    #[test]
    fn test_extract_iteration_summary_truncates_long_approach() {
        use crate::autoresearch::agentic_verification::*;
        let verdict = VerificationVerdict {
            status: VerificationStatus::Partial,
            confidence: 0.5,
            observations: "Partial progress".to_string(),
            next_priority: None,
            issues: vec![],
            unreachable: false,
            unreachable_reason: None,
        };
        let long_output = "x".repeat(500);
        let summary = extract_iteration_summary(1, Some(&long_output), &verdict, 0.0);
        assert!(summary.approach.chars().count() <= 100);
        assert!(summary.approach.ends_with("..."));
    }
}
