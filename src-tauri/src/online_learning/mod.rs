//! Online/Continual Learning System
//!
//! Provides incremental learning capabilities that operate after every workflow
//! run, augmenting the batch-oriented meta-optimizer pipeline.
//!
//! ## Architecture
//!
//! All subsystems feed into a single async coordinator spawned after each run:
//!
//! 1. **Bandit Framework** — Generic contextual bandit with pluggable policies
//! 2. **Model Router** — Bandit-based LLM model selection (replaces static heuristics)
//! 3. **Credit Assignment** — ADCA step-level attribution for causal scoring
//! 4. **Reflection** — Post-step/post-run experience extraction
//! 5. **Strategy Bank** — Evolvable optimization strategies with Thompson Sampling
//! 6. **Coordinator** — Non-blocking post-run pipeline orchestrator
//!
//! The drift detection engine lives in `meta_optimizer::drift_detection` since
//! it's tightly coupled to the meta-optimizer's trigger system.

pub mod bandit;
pub mod context;
pub mod coordinator;
pub mod credit_assignment;
pub mod model_router;
pub mod policies;
pub mod reflection;
pub mod strategy_bank;
pub mod strategy_evolution;

use std::sync::{Mutex, OnceLock};
use tracing::warn;

// =============================================================================
// Global singletons
// =============================================================================

/// Global model router bandit, loaded from PG at startup.
static GLOBAL_MODEL_ROUTER: OnceLock<Mutex<model_router::ModelRouterBandit>> = OnceLock::new();

/// Global drift monitor, loaded from PG at startup.
static GLOBAL_DRIFT_MONITOR: OnceLock<Mutex<crate::meta_optimizer::drift_detection::DriftMonitor>> =
    OnceLock::new();

/// Initialize the global model router. Call once during app startup.
pub fn init_model_router(router: model_router::ModelRouterBandit) {
    GLOBAL_MODEL_ROUTER
        .set(Mutex::new(router))
        .unwrap_or_else(|_| warn!("Model router already initialized (ignored)"));
}

/// Initialize the global drift monitor. Call once during app startup.
pub fn init_drift_monitor(monitor: crate::meta_optimizer::drift_detection::DriftMonitor) {
    GLOBAL_DRIFT_MONITOR
        .set(Mutex::new(monitor))
        .unwrap_or_else(|_| warn!("Drift monitor already initialized (ignored)"));
}

/// Get the global model router. Returns None if not initialized.
pub fn model_router() -> Option<&'static Mutex<model_router::ModelRouterBandit>> {
    GLOBAL_MODEL_ROUTER.get()
}

/// Get the global drift monitor. Returns None if not initialized.
pub fn drift_monitor() -> Option<&'static Mutex<crate::meta_optimizer::drift_detection::DriftMonitor>> {
    GLOBAL_DRIFT_MONITOR.get()
}

/// Initialize all online learning singletons with defaults.
/// Called during app startup. Loads persisted state from PG if available.
pub async fn initialize(pg_db: &std::sync::Arc<crate::database::pg::PgDb>) {
    // Initialize model router with persisted Q-table
    let mut router = model_router::ModelRouterBandit::new();
    match pg_db.load_model_routing_table().await {
        Ok(rows) if !rows.is_empty() => {
            tracing::info!("Loading {} model routing entries from PG", rows.len());
            router.load_from_rows(rows);
        }
        Ok(_) => tracing::info!("No persisted model routing data — starting fresh"),
        Err(e) => tracing::warn!("Failed to load model routing table: {}", e),
    }
    match pg_db.load_model_routing_overrides().await {
        Ok(overrides) if !overrides.is_empty() => {
            tracing::info!("Loading {} model routing overrides from PG", overrides.len());
            router.load_overrides(overrides);
        }
        _ => {}
    }
    init_model_router(router);

    // Initialize drift monitor with persisted state
    let monitor = match pg_db.load_drift_detector_state("global_monitor").await {
        Ok(Some(json)) => {
            match crate::meta_optimizer::drift_detection::DriftMonitor::deserialize_state(&json) {
                Ok(m) => {
                    tracing::info!("Loaded drift monitor state from PG ({} detectors)", m.detector_count());
                    m
                }
                Err(e) => {
                    tracing::warn!("Failed to deserialize drift monitor state: {} — starting fresh", e);
                    crate::meta_optimizer::drift_detection::DriftMonitor::new()
                }
            }
        }
        _ => {
            tracing::info!("No persisted drift monitor state — starting fresh");
            crate::meta_optimizer::drift_detection::DriftMonitor::new()
        }
    };
    init_drift_monitor(monitor);

    tracing::info!("Online learning system initialized");
}

/// Run the full online learning pipeline for a completed task.
///
/// Self-contained async entry point called from `task_lifecycle.rs` after
/// a task completes (success or failure). Queries PG for all necessary data,
/// runs in-memory computation, and persists results back to PG.
pub async fn run_post_completion(
    pg: &std::sync::Arc<crate::database::pg::PgDb>,
    app_handle: &tauri::AppHandle,
    task_run_id: &str,
) {
    use crate::meta_optimizer::drift_detection::DriftLevel;
    use tauri::Emitter;

    // Brief delay to let deterministic metrics + learning outcome record
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // Skip meta-optimizer/fixer/reflection runs
    if let Ok(Some(task_run)) = pg.get_task_run(task_run_id).await {
        if task_run.is_meta_optimizer || task_run.is_fixer || task_run.is_reflection {
            return;
        }
    }

    // Build outcome from learning_outcomes table (has enriched fields)
    let lo = match pg
        .get_learning_outcome_for_online_learning(task_run_id)
        .await
    {
        Ok(Some(lo)) => lo,
        _ => {
            tracing::debug!(
                "Online learning: no learning outcome for {} — skipping",
                task_run_id
            );
            return;
        }
    };

    let status = lo["status"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();

    let composite_score = lo["composite_agentic_score"].as_f64().filter(|&s| s > 0.0);

    let files_modified: Vec<String> = lo["files_modified"]
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let mut outcome = coordinator::OnlineLearningOutcome {
        task_run_id: task_run_id.to_string(),
        status,
        composite_score,
        cost_usd: lo["total_cost_usd"].as_f64(),
        duration_secs: lo["duration_secs"].as_f64(),
        model_used: lo["model_used"].as_str().map(|s| s.to_string()),
        domain: lo["domain_tags"]
            .as_str()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .and_then(|tags| tags.into_iter().next())
            .unwrap_or_else(|| "backend".to_string()),
        complexity_tier: lo["complexity_tier"]
            .as_str()
            .unwrap_or("moderate")
            .to_string(),
        has_ui: lo["has_ui_bridge"].as_bool().unwrap_or(false),
        prompt_length: 0, // Not available from learning_outcomes
        file_count: files_modified.len(),
        workflow_architecture: lo["workflow_architecture"]
            .as_str()
            .map(|s| s.to_string()),
        strategy_id: None,
        step_attributions: Vec::new(),
        step_reflections: Vec::new(),
        total_iterations: lo["iterations"].as_i64().unwrap_or(0) as u32,
        technology_tags: lo["technology_tags"]
            .as_str()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_default(),
    };

    // Enrich from pipeline traces (adds real step_attributions and step_reflections)
    coordinator::enrich_from_pipeline_traces(pg, &mut outcome).await;

    // Phase 1: In-memory computation (guards dropped before awaits)
    let snapshot = {
        let router_lock = model_router();
        let drift_lock = drift_monitor();
        match (router_lock, drift_lock) {
            (Some(router_mtx), Some(drift_mtx)) => {
                let mut router = router_mtx.lock().unwrap_or_else(|e| e.into_inner());
                let mut drift = drift_mtx.lock().unwrap_or_else(|e| e.into_inner());
                coordinator::post_run_online_learning(&outcome, &mut router, &mut drift);
                Some((
                    router.table_snapshot(),
                    drift.serialize_state().ok(),
                    drift.take_pending_signals(),
                ))
            }
            _ => {
                tracing::debug!("Online learning not initialized — skipping");
                None
            }
        }
    };

    // Phase 2: Async persistence to PG
    if let Some((routing_rows, drift_state, drift_signals)) = snapshot {
        let mut persisted = 0usize;

        // Persist model routing Q-table
        for row in &routing_rows {
            if pg
                .upsert_model_routing_entry(
                    &row.context_key,
                    &row.arm,
                    row.q_value,
                    row.visit_count as i32,
                    row.sum_of_squares,
                )
                .await
                .is_ok()
            {
                persisted += 1;
            }
        }

        // Persist drift signals + emit Tauri events
        for signal in &drift_signals {
            let drift_level_str = match signal.drift_level {
                DriftLevel::Warning => "warning",
                DriftLevel::Drift => "drift",
                DriftLevel::None => continue,
            };
            let id = format!(
                "{}:{}:{}:{}",
                signal.detector_type,
                signal.metric_name,
                signal.context_key,
                chrono::Utc::now().timestamp_millis()
            );
            if pg
                .insert_drift_signal(
                    &id,
                    &signal.detector_type,
                    &signal.metric_name,
                    &signal.context_key,
                    drift_level_str,
                    signal.pre_drift_mean,
                    signal.post_drift_mean,
                    signal.window_size as i64,
                )
                .await
                .is_ok()
            {
                persisted += 1;
            }
            let _ = app_handle.emit(
                crate::orchestrator::realtime_events::EVENT_DRIFT_DETECTED,
                &serde_json::json!({
                    "detector_type": signal.detector_type,
                    "metric_name": signal.metric_name,
                    "context_key": signal.context_key,
                    "drift_level": drift_level_str,
                }),
            );
        }

        // Persist drift monitor state
        if let Some(ref state_json) = drift_state {
            let _ = pg
                .save_drift_detector_state("global_monitor", "composite", state_json)
                .await;
        }

        // Persist step credits
        let credits = credit_assignment::compute_step_credits(
            &outcome.step_attributions,
            outcome.composite_score.unwrap_or(0.0),
            outcome.status == "success" || outcome.status == "completed",
        );
        if !credits.is_empty() {
            if let Ok(n) = pg.save_step_credits(&credits, task_run_id).await {
                persisted += n;
            }
        }

        // Persist experience summary
        let summary = reflection::summarize_experience(
            &reflection::RunReflectionInput {
                task_run_id: task_run_id.to_string(),
                domain: outcome.domain.clone(),
                complexity_tier: outcome.complexity_tier.clone(),
                outcome_status: outcome.status.clone(),
                composite_score: outcome.composite_score,
                steps: outcome.step_reflections.clone(),
                total_iterations: outcome.total_iterations,
                total_cost_usd: outcome.cost_usd,
            },
        );
        let summary_id = format!("exp-{}", task_run_id);
        let kd = serde_json::to_string(&summary.key_decisions).unwrap_or_default();
        let fp = serde_json::to_string(&summary.failure_points).unwrap_or_default();
        let ep = serde_json::to_string(&summary.effective_patterns).unwrap_or_default();
        if pg
            .insert_experience_summary(
                &summary_id,
                task_run_id,
                &outcome.domain,
                &outcome.complexity_tier,
                &outcome.status,
                &kd,
                &fp,
                &ep,
            )
            .await
            .is_ok()
        {
            persisted += 1;
        }

        // Backfill model_used
        if let Some(ref model) = outcome.model_used {
            let _ = pg
                .update_learning_outcome_model(task_run_id, model)
                .await;
        }

        // Strategy extraction: P90+ runs → candidate strategies
        if (outcome.status == "success" || outcome.status == "completed")
            && outcome.composite_score.is_some_and(|s| s > 0.7)
        {
            let score = outcome.composite_score.unwrap();
            let context_key = format!(
                "{}:{}:{}",
                outcome.domain,
                outcome.complexity_tier,
                if outcome.has_ui { "ui" } else { "no_ui" }
            );
            if let Ok(p90) = pg.get_score_percentile(&context_key, 90).await {
                if score > p90 {
                    let strategy =
                        strategy_evolution::extract_strategy_from_run(
                            &strategy_evolution::HighPerformingRun {
                                task_run_id: task_run_id.to_string(),
                                domain: outcome.domain.clone(),
                                complexity_tier: outcome.complexity_tier.clone(),
                                technology_tags: outcome.technology_tags.clone(),
                                workflow_architecture: outcome
                                    .workflow_architecture
                                    .clone()
                                    .unwrap_or_default(),
                                model_used: outcome.model_used.clone(),
                                composite_score: score,
                                cost_usd: outcome.cost_usd.unwrap_or(0.0),
                                duration_secs: outcome.duration_secs.unwrap_or(0.0),
                            },
                        );
                    let app_json =
                        serde_json::to_string(&strategy.applicability).unwrap_or_default();
                    let comp_json =
                        serde_json::to_string(&strategy.components).unwrap_or_default();
                    let prov_json =
                        serde_json::to_string(&strategy.provenance).unwrap_or_default();
                    if pg
                        .insert_strategy(
                            &strategy.id,
                            &strategy.name,
                            &strategy.description,
                            &app_json,
                            &comp_json,
                            &prov_json,
                            "candidate",
                            strategy.parent_strategy_id.as_deref(),
                        )
                        .await
                        .is_ok()
                    {
                        tracing::info!(
                            "Extracted candidate strategy from P90+ run {}",
                            task_run_id
                        );
                        persisted += 1;
                    }
                }
            }
        }

        if persisted > 0 {
            tracing::debug!(
                "Online learning persisted {} items for {}",
                persisted,
                task_run_id
            );
        }
    }
}
