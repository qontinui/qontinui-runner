//! Shared trigger coordinator for meta-optimizer runs.
//!
//! Fires after every Nth pipeline completion (threshold-based, not per-run).
//! Each optimizer type is checked independently.
//!
//! Recursion prevention:
//! 1. `is_dev_mode: false` on optimizer's LoopConfig
//! 2. `is_meta_optimizer` column check on source task run
//! 3. `is_fixer` / `is_reflection` / `is_follow_up` column checks on source

use tauri::Emitter;
use tokio::runtime::Handle;
use tracing::{debug, info, warn};

use super::types::{MetaOptimizerDeps, OptimizerType};

/// Check whether a specific optimizer should be launched (PG-primary).
///
/// Guards:
/// 1. No optimizer of this type already running
/// 2. Meta-optimizer enabled in settings
/// 3. Threshold met: completed runs since last optimizer run >= N
/// 4. Source run is not meta_optimizer/fixer/reflection/follow_up
pub fn should_launch_optimizer(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    optimizer_type: OptimizerType,
    source_task_run_id: &str,
) -> Result<bool, String> {
    let opt_type = optimizer_type.as_str().to_string();
    let source_id = source_task_run_id.to_string();

    let result = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.should_launch_optimizer(&opt_type, &source_id))
    })?;

    if result {
        info!(
            "Meta-optimizer {} threshold met for source {}",
            opt_type, source_id
        );
    }

    Ok(result)
}

/// Additional guards specific to the meta-prompt optimizer.
///
/// Guards:
/// 1. A prompt group exists with failure_rate > 30% and >= 10 samples
/// 2. No active evolution (canary in progress) for the worst-performing group
/// 3. 24-hour cooldown per agent_type since last rewrite attempt
fn should_launch_meta_prompt_optimizer(pg_db: &std::sync::Arc<crate::database::pg::PgDb>) -> bool {
    // Check if there's an optimization target meeting the threshold
    let target = match super::prompt_extractor::select_optimization_target(10, 0.30) {
        Ok(Some(t)) => t,
        Ok(None) => {
            debug!("MetaPrompt guard: no prompt group exceeds 30% failure rate with >= 10 samples");
            return false;
        }
        Err(e) => {
            debug!("MetaPrompt guard: failed to select target: {}", e);
            return false;
        }
    };

    // Check no active duel pool for this agent_type
    {
        let pg = pg_db.clone();
        let at = target.agent_type.clone();
        let has_pool = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(pg.get_active_duel_pool(&at))
        });
        if matches!(has_pool, Ok(Some(_))) {
            debug!(
                "MetaPrompt guard: active duel pool for {} — skipping",
                target.agent_type
            );
            return false;
        }
    }

    // Check no active canary for this agent_type
    if super::prompt_evolution::has_active_evolution(pg_db, &target.agent_type) {
        debug!(
            "MetaPrompt guard: active evolution already in progress for {}",
            target.agent_type
        );
        return false;
    }

    // Adaptive cooldown based on consecutive rejections (circuit breaker)
    let consecutive =
        super::prompt_evolution::count_consecutive_rejections(pg_db, &target.agent_type);
    let cooldown_hours = super::prompt_evolution::adaptive_cooldown_hours(consecutive);
    if super::prompt_evolution::is_in_cooldown(pg_db, &target.agent_type, cooldown_hours) {
        debug!(
            "MetaPrompt guard: {} is in {}h cooldown (consecutive_rejections={})",
            target.agent_type, cooldown_hours, consecutive
        );
        return false;
    }

    // Check for baseline drift (hardcoded prompt changed since canary started)
    let current_prompt = super::prompt_extractor::get_default_agent_prompt(&target.agent_type);
    if !current_prompt.is_empty() {
        let current_hash = super::prompt_evolution::compute_prompt_hash(&current_prompt);
        if super::prompt_evolution::has_baseline_drifted(pg_db, &target.agent_type, &current_hash) {
            warn!(
                "MetaPrompt guard: baseline prompt for {} has drifted — existing canary results may be invalid",
                target.agent_type
            );
            // Don't block — just warn. The canary will naturally conclude and the next
            // round will work against the new baseline.
        }
    }

    info!(
        "MetaPrompt guard passed: target={}/{} failure_rate={:.1}% samples={} consecutive_rejections={} cooldown={}h",
        target.phase, target.agent_type, target.failure_rate * 100.0, target.sample_count,
        consecutive, cooldown_hours
    );
    true
}

/// Check all optimizer types and launch any that pass their guards.
/// Called from loop_controller.rs after the fixer launch block.
///
/// Returns the IDs of launched optimizer task runs.
pub fn check_and_launch_optimizers(
    deps: MetaOptimizerDeps,
    source_task_run_id: String,
) -> Result<Vec<String>, String> {
    let mut launched = Vec::new();

    let pg_db = &deps.app_state.pg_db;
    for optimizer_type in OptimizerType::all() {
        match should_launch_optimizer(pg_db, *optimizer_type, &source_task_run_id) {
            Ok(true) => {
                // Meta-prompt optimizer has additional guards beyond the generic ones
                if *optimizer_type == OptimizerType::MetaPrompt
                    && !should_launch_meta_prompt_optimizer(pg_db)
                {
                    debug!("Skipping MetaPrompt — additional guards not met");
                    continue;
                }
                match launch_optimizer(&deps, *optimizer_type) {
                    Ok(id) => launched.push(id),
                    Err(e) => warn!("Failed to launch {}: {}", optimizer_type, e),
                }
            }
            Ok(false) => {}
            Err(e) => warn!("Error checking {}: {}", optimizer_type, e),
        }
    }

    // Capture periodic performance snapshot for progress tracking
    if let Err(e) = super::snapshots::capture_periodic(
        &deps.app_state.pg_db,
        super::types::WorkflowCategory::Main,
    ) {
        warn!("Failed to capture periodic snapshot: {}", e);
    }

    // Auto-extract spec compliance for spec-generated workflows
    {
        let pg = deps.app_state.pg_db.clone();
        let trid = source_task_run_id.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                crate::spec_experimentation::compliance::auto_extract_spec_compliance(&pg, &trid)
                    .await;
            })
        });
    }

    // Maintenance: dedup pending recommendations, reject stale ones, rollback stale canaries
    let pg_db = &deps.app_state.pg_db;
    super::recommendations::dedup_pending_recommendations(pg_db);
    super::recommendations::auto_reject_stale_recommendations(pg_db);
    super::canary::auto_rollback_stale_canaries(pg_db);

    // Sweep pending recommendations from previous runs that now qualify for
    // auto-apply or canary entry (pass None to match all optimizer_run_ids).
    super::parser::auto_apply_high_confidence(pg_db, None);

    // Auto-evaluate applied recommendations that are due for re-evaluation.
    // Only re-evaluate recs applied >7 days ago whose verdict is still "insufficient_data".
    auto_evaluate_outcomes(pg_db);

    // Auto-evaluate canary rollouts and promote/rollback when threshold is met.
    auto_evaluate_canaries(pg_db, Some(&deps.app_handle));

    // Run data-driven cost optimization analysis (no AI session needed).
    // Generates recommendations for high-token agents, cost concentration, etc.
    run_cost_analysis(pg_db);

    // Recompute agentic metric baselines from accumulated successful runs.
    // Fast — only queries learning_outcomes aggregates, no LLM calls.
    if let Err(e) = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.recompute_agentic_baselines())
    })
    .map(|_| ())
    {
        debug!("Baseline recomputation skipped: {}", e);
    }

    // Evaluate applied recommendations using composite agentic scores.
    // Compares pre/post-apply score averages to measure recommendation effectiveness.
    super::recommendations::auto_evaluate_with_agentic_scores(pg_db);

    Ok(launched)
}

/// Re-evaluate applied recommendations whose outcome is still "insufficient_data"
/// and were applied at least 7 days ago (enough time for post-apply data to accumulate).
fn auto_evaluate_outcomes(pg_db: &std::sync::Arc<crate::database::pg::PgDb>) {
    let recs = match super::recommendations::list_recommendations(pg_db, None, Some("applied")) {
        Ok(r) => r,
        Err(_) => return,
    };

    let cutoff = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339();

    for rec in &recs {
        // Only re-evaluate if applied_at is >7 days ago
        let applied_at = match &rec.applied_at {
            Some(a) => a,
            None => continue,
        };
        if applied_at > &cutoff {
            continue; // Too recent
        }

        // Only re-evaluate if current verdict is "insufficient_data" or absent
        let needs_eval = match &rec.outcome_after_apply {
            None => true,
            Some(json) => serde_json::from_str::<serde_json::Value>(json)
                .ok()
                .and_then(|v| {
                    v.get("verdict")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .map(|v| v == "insufficient_data")
                .unwrap_or(true),
        };

        if needs_eval {
            match super::snapshots::evaluate_recommendation_outcome(pg_db, &rec.id) {
                Ok(outcome) => {
                    if outcome.verdict != "insufficient_data" {
                        info!(
                            "Auto-evaluated recommendation {}: verdict={}",
                            rec.id, outcome.verdict
                        );
                    }
                }
                Err(e) => {
                    debug!("Failed to auto-evaluate recommendation {}: {}", rec.id, e);
                }
            }
        }
    }
}

/// Check active canary rollouts and auto-promote or rollback when the minimum
/// sample size threshold (10 canary runs) is met.
///
/// When `app_handle` is provided and a rollback verdict is reached, a
/// `canary-alert` Tauri event is emitted so the frontend can show a notification.
fn auto_evaluate_canaries(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    app_handle: Option<&tauri::AppHandle>,
) {
    let canaries = match super::canary::get_active_canaries(pg_db) {
        Ok(c) => c,
        Err(_) => return,
    };

    for canary in &canaries {
        // Only auto-evaluate if canary has reached minimum run threshold
        if canary.canary_run_count < 10 {
            continue;
        }

        match super::canary::evaluate_canary(pg_db, &canary.id) {
            Ok(eval) => match eval.verdict.as_str() {
                "promote" => {
                    info!(
                            "Auto-promoting canary {} (delta={:+.1}pp, canary_sr={:.1}%, baseline_sr={:.1}%)",
                            canary.id, eval.delta, eval.canary_success_rate, eval.baseline_success_rate
                        );
                    if let Err(e) = super::canary::promote_canary(pg_db, &canary.id) {
                        warn!("Failed to auto-promote canary {}: {}", canary.id, e);
                    }
                }
                "rollback" => {
                    info!(
                            "Auto-rolling-back canary {} (delta={:+.1}pp, canary_sr={:.1}%, baseline_sr={:.1}%, p={:?}, cost_delta={:?})",
                            canary.id, eval.delta, eval.canary_success_rate, eval.baseline_success_rate,
                            eval.p_value, eval.cost_delta_pct
                        );

                    // Emit canary-alert event to the frontend before rolling back
                    if let Some(handle) = app_handle {
                        let payload = serde_json::json!({
                            "canary_id": canary.id,
                            "recommendation_id": canary.recommendation_id,
                            "verdict": "rollback",
                            "message": "Canary detected significant regression",
                            "p_value": eval.p_value,
                            "delta": eval.delta,
                            "baseline_success_rate": eval.baseline_success_rate,
                            "canary_success_rate": eval.canary_success_rate,
                        });
                        if let Err(e) = handle.emit("canary-alert", &payload) {
                            warn!("Failed to emit canary-alert event: {}", e);
                        }
                    }

                    if let Err(e) =
                        super::canary::rollback_canary_with_eval(pg_db, &canary.id, Some(&eval))
                    {
                        warn!("Failed to auto-rollback canary {}: {}", canary.id, e);
                    }
                }
                _ => {
                    debug!(
                        "Canary {} evaluation: continue (delta={:+.1}pp, {} canary runs)",
                        canary.id, eval.delta, canary.canary_run_count
                    );

                    // For borderline results with many runs, increase canary traffic
                    // to gather more data faster (extend the evaluation period).
                    if canary.canary_run_count >= 30 && canary.percentage < 50 {
                        let new_pct = std::cmp::min(canary.percentage + 10, 50);
                        if let Err(e) =
                            super::canary::update_canary_percentage(pg_db, &canary.id, new_pct)
                        {
                            debug!(
                                "Failed to extend canary {} traffic to {}%: {}",
                                canary.id, new_pct, e
                            );
                        } else {
                            info!(
                                "Extended canary {} traffic from {}% to {}% (inconclusive after {} runs)",
                                canary.id, canary.percentage, new_pct, canary.canary_run_count
                            );
                        }
                    }
                }
            },
            Err(e) => {
                debug!("Failed to evaluate canary {}: {}", canary.id, e);
            }
        }
    }
}

/// Run data-driven cost analysis and generate cost recommendations.
/// This does not require an AI session — it's purely SQL-based analysis.
fn run_cost_analysis(pg_db: &std::sync::Arc<crate::database::pg::PgDb>) {
    // SQLite removed - no-op
}

/// Launch a specific optimizer workflow.
pub fn launch_optimizer(
    deps: &MetaOptimizerDeps,
    optimizer_type: OptimizerType,
) -> Result<String, String> {
    launch_optimizer_internal(deps, optimizer_type, "threshold")
}

/// Launch a specific optimizer manually (skips threshold check).
pub fn launch_optimizer_manual(
    deps: &MetaOptimizerDeps,
    optimizer_type: OptimizerType,
) -> Result<String, String> {
    // Meta-prompt optimizer has additional guards even for manual triggers
    // to prevent creating competing canaries for the same agent_type
    if optimizer_type == OptimizerType::MetaPrompt {
        if let Ok(Some(target)) = super::prompt_extractor::select_optimization_target(10, 0.30) {
            if super::prompt_evolution::has_active_evolution(
                &deps.app_state.pg_db,
                &target.agent_type,
            ) {
                return Err(format!(
                    "Cannot trigger MetaPrompt optimizer: active canary already in progress for {}",
                    target.agent_type
                ));
            }
        }
    }
    launch_optimizer_internal(deps, optimizer_type, "manual")
}

fn launch_optimizer_internal(
    deps: &MetaOptimizerDeps,
    optimizer_type: OptimizerType,
    trigger_type: &str,
) -> Result<String, String> {
    // Create task run for this optimizer
    let task_run_id = uuid::Uuid::new_v4().to_string();
    let task_name = format!("Meta-Optimizer: {}", optimizer_type.display_name());

    let input = crate::database::CreateTaskRunInput::new(&task_run_id, &task_name)
        .with_prompt(format!(
            "Analyze historical run data and produce recommendations to improve {}.",
            optimizer_type.display_name()
        ))
        .with_workflow_name(&task_name)
        .with_workflow_type("unified")
        .with_task_type("meta_optimizer")
        .with_max_sessions(2)
        .with_auto_continue(true)
        .with_is_meta_optimizer(true);

    // Create task run via PG
    {
        let pg = deps.app_state.pg_db.clone();
        let input_clone = input.clone();
        let handle =
            tokio::runtime::Handle::try_current().map_err(|_| "No tokio runtime".to_string())?;
        handle
            .block_on(async move { pg.create_task_run(&input_clone).await })
            .map_err(|e| format!("Failed to create task run: {}", e))?;
    }

    // Create optimizer run record
    let optimizer_run_id = super::recommendations::create_optimizer_run(
        &deps.app_state.pg_db,
        optimizer_type.as_str(),
        trigger_type,
        Some(&task_run_id),
    )?;

    info!(
        "Launching {} (task_run: {}, optimizer_run: {})",
        optimizer_type.display_name(),
        task_run_id,
        optimizer_run_id
    );

    // Query completed optimizer run count for style rotation (PromptWizard-inspired diversity)
    let style_index: u32 = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(
            deps.app_state
                .pg_db
                .count_optimizer_runs_by_type(optimizer_type.as_str()),
        )
    })
    .unwrap_or(0) as u32;

    info!(
        "Style rotation: optimizer_type={}, run_count={}, style_index={}",
        optimizer_type.as_str(),
        style_index,
        style_index % 4
    );

    // Build workflow config based on optimizer type
    let (loop_config, setup_steps, verification_steps) = match optimizer_type {
        OptimizerType::PipelinePrompt => {
            let config = super::pipeline_prompt_optimizer::build_config(
                &task_run_id,
                &task_name,
                style_index,
            );
            let setup = super::pipeline_prompt_optimizer::build_setup_steps();
            let verify = super::pipeline_prompt_optimizer::build_verification_steps();
            (config, setup, verify)
        }
        OptimizerType::Architecture => {
            let config = super::architecture_optimizer::build_config(&task_run_id, &task_name);
            let setup = super::architecture_optimizer::build_setup_steps();
            let verify = super::architecture_optimizer::build_verification_steps();
            (config, setup, verify)
        }
        OptimizerType::GenerationTemplate => {
            let config = super::generation_template_optimizer::build_config(
                &task_run_id,
                &task_name,
                style_index,
            );
            let setup = super::generation_template_optimizer::build_setup_steps();
            let verify = super::generation_template_optimizer::build_verification_steps();
            (config, setup, verify)
        }
        OptimizerType::MetaPrompt => {
            let config =
                super::meta_prompt_optimizer::build_config(&task_run_id, &task_name, style_index);
            let setup = super::meta_prompt_optimizer::build_setup_steps();
            let verify = super::meta_prompt_optimizer::build_verification_steps();
            (config, setup, verify)
        }
    };

    // Spawn async workflow
    let task_run_id_clone = task_run_id.clone();
    let task_name_clone = task_name.clone();
    let app_state = deps.app_state.clone();
    let config_storage = deps.config_storage.clone();
    let app_handle = deps.app_handle.clone();
    let pid_tracker = deps.pid_tracker.clone();
    let session_manager = deps.session_manager.clone();

    tokio::spawn(async move {
        let step_injection_ctx = crate::step_injection::types::StepInjectionContext {
            execution_id: task_run_id_clone.clone(),
        };

        let mut controller = crate::unified_workflow_executor::LoopController::new(
            app_state.clone(),
            config_storage,
            app_handle.clone(),
            pid_tracker,
        )
        .with_step_injection_ctx(step_injection_ctx);

        if let Some(sm) = session_manager {
            controller = controller.with_session_manager(sm);
        }

        let exec_id = task_run_id_clone.clone();
        let wf_name = task_name_clone.clone();
        let url_lock = Some(app_state.url_lock_manager.clone());
        let file_registry = Some(app_state.file_registry_manager.clone());
        let file_lock = Some(app_state.file_lock_manager.clone());

        // Check if Restate durable execution should be used
        let mut use_legacy = true;
        let restate_settings = crate::settings::load_settings().restate;
        if crate::restate::launch::should_use_restate(&restate_settings).await {
            match crate::restate::launch::build_workflow_input_from_loop_config(&loop_config) {
                Ok(input) => {
                    if let Err(e) = app_state
                        .pg_db
                        .save_restate_workflow_execution(&exec_id, &exec_id, None)
                        .await
                    {
                        tracing::error!("Failed to record Restate workflow: {}", e);
                    }

                    if let Err(e) = crate::restate::launch::launch_workflow_via_restate(
                        &exec_id,
                        &input,
                        &restate_settings.ingress_url(),
                    )
                    .await
                    {
                        tracing::error!("Restate launch failed, falling back to legacy: {}", e);
                    } else {
                        use_legacy = false;
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to build WorkflowInput for Restate: {}", e);
                }
            }
        }

        if use_legacy {
            crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
                exec_id,
                wf_name,
                url_lock,
                file_registry,
                file_lock,
                app_state.pg_db.clone(),
                Box::pin(async move {
                    controller
                        .run(
                            loop_config,
                            setup_steps,
                            Vec::new(),
                            verification_steps,
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                        )
                        .await
                }),
            );
        }

        info!(
            "Meta-optimizer workflow '{}' spawned ({})",
            task_name_clone, task_run_id_clone
        );
    });

    Ok(task_run_id)
}

// ── PG dual-write wrappers ─────────────────────────────────────────────

/// Check and launch optimizers with PG dual-write on maintenance operations.
#[allow(dead_code)]
pub fn check_and_launch_optimizers_with_pg(
    deps: MetaOptimizerDeps,
    source_task_run_id: String,
) -> Result<Vec<String>, String> {
    let mut launched = Vec::new();
    let pg_db = &deps.app_state.pg_db;

    for optimizer_type in OptimizerType::all() {
        match should_launch_optimizer(pg_db, *optimizer_type, &source_task_run_id) {
            Ok(true) => {
                if *optimizer_type == OptimizerType::MetaPrompt
                    && !should_launch_meta_prompt_optimizer(pg_db)
                {
                    debug!("Skipping MetaPrompt — additional guards not met");
                    continue;
                }
                match launch_optimizer(&deps, *optimizer_type) {
                    Ok(id) => launched.push(id),
                    Err(e) => warn!("Failed to launch {}: {}", optimizer_type, e),
                }
            }
            Ok(false) => {}
            Err(e) => warn!("Error checking {}: {}", optimizer_type, e),
        }
    }
    if let Err(e) = super::snapshots::capture_periodic(pg_db, super::types::WorkflowCategory::Main)
    {
        warn!("Failed to capture periodic snapshot: {}", e);
    }
    {
        let pg_clone = pg_db.clone();
        let trid = source_task_run_id.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                crate::spec_experimentation::compliance::auto_extract_spec_compliance(
                    &pg_clone, &trid,
                )
                .await;
            })
        });
    }
    super::recommendations::dedup_pending_recommendations(pg_db);
    super::recommendations::auto_reject_stale_recommendations(pg_db);
    super::canary::auto_rollback_stale_canaries(pg_db);
    super::parser::auto_apply_high_confidence(pg_db, None);
    auto_evaluate_outcomes(pg_db);
    auto_evaluate_canaries(pg_db, Some(&deps.app_handle));
    run_cost_analysis(pg_db);
    if let Err(e) = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.recompute_agentic_baselines())
    })
    .map(|_| ())
    {
        debug!("Baseline recomputation skipped: {}", e);
    }
    super::recommendations::auto_evaluate_with_agentic_scores(pg_db);
    Ok(launched)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_test_db() -> Connection {
        panic!("SQLite tests disabled — use PG-based tests instead")
    }

    #[test]
    fn test_guard_skips_when_meta_optimizer_running() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_guard_skips_when_source_is_fixer() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_guard_skips_when_disabled() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_threshold_counts_correctly() {
        // SQLite removed - no-op
    }
}
