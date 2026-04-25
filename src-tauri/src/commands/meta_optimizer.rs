//! Tauri commands for the meta-optimizer system.
//!
//! Provides frontend access to recommendations, prompt registry, optimizer runs,
//! and manual optimizer triggering.

use std::sync::Arc;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::State;

// Migrated to StorageCompartment (Workstream C) — 43 of 44 handlers.
// `trigger_meta_optimizer` keeps `Arc<AppState>` (see note above the fn).
use crate::commands::compartments::StorageCompartment;
use crate::commands::AppState;
use crate::error::AppError;
use crate::meta_optimizer::types::{
    MetaOptimizerRun, OptimizerType, PromptVariant, Recommendation,
};

// ── Recommendations ────────────────────────────────────────────────────

// These commands MUST be `async`. They were originally sync, which caused the
// runner to hard-crash when the Cost Control page invoked them: Tauri executes
// sync commands on a single-threaded WebView worker, and the
// `tokio::task::block_in_place` + `Handle::current().block_on(...)` pattern in
// the underlying helpers panics on a thread with no ambient multi-thread
// runtime. That panic crosses the WebView2 FFI boundary and aborts the process
// rather than unwinding. Async commands run on Tauri's tokio multi-thread
// runtime, so we call `_async` helpers that `.await` PG calls directly.
#[tauri::command]
pub async fn get_meta_optimizer_recommendations(
    app_state: State<'_, StorageCompartment>,
    optimizer_type: Option<String>,
    status: Option<String>,
) -> Result<Vec<Recommendation>, String> {
    crate::meta_optimizer::recommendations::list_recommendations_async(
        &app_state.pg_db(),
        optimizer_type.as_deref(),
        status.as_deref(),
    )
    .await
}

#[tauri::command]
pub async fn apply_meta_optimizer_recommendation(
    app_state: State<'_, StorageCompartment>,
    recommendation_id: String,
) -> Result<(), String> {
    crate::meta_optimizer::recommendations::apply_recommendation_with_side_effects_async(
        &app_state.pg_db(),
        &recommendation_id,
    )
    .await
}

#[tauri::command]
pub async fn reject_meta_optimizer_recommendation(
    app_state: State<'_, StorageCompartment>,
    recommendation_id: String,
) -> Result<(), String> {
    crate::meta_optimizer::recommendations::reject_recommendation_async(
        &app_state.pg_db(),
        &recommendation_id,
    )
    .await
}

#[tauri::command]
pub async fn rollback_meta_optimizer_recommendation(
    app_state: State<'_, StorageCompartment>,
    recommendation_id: String,
) -> Result<(), String> {
    crate::meta_optimizer::recommendations::rollback_recommendation_async(
        &app_state.pg_db(),
        &recommendation_id,
    )
    .await
}

// ── Prompt Registry ────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_prompt_variants(
    app_state: State<'_, StorageCompartment>,
    agent_type: Option<String>,
) -> Result<Vec<PromptVariant>, String> {
    app_state.pg_db().list_variants(agent_type.as_deref()).await
}

#[tauri::command]
pub async fn activate_prompt_variant(
    app_state: State<'_, StorageCompartment>,
    variant_id: String,
) -> Result<(), String> {
    app_state.pg_db().activate_variant(&variant_id).await
}

// ── Optimizer Runs ─────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_meta_optimizer_runs(
    app_state: State<'_, StorageCompartment>,
) -> Result<Vec<MetaOptimizerRun>, String> {
    crate::meta_optimizer::recommendations::list_optimizer_runs_async(&app_state.pg_db()).await
}

// ── Progress Tracking ─────────────────────────────────────────────────

// These commands are `async` on purpose: their sync predecessors invoked
// `snapshots::{get_progress_summary, capture_baseline, list_snapshots}`,
// each of which bottoms out in `Handle::current().block_on(...)`. Sync
// Tauri commands run on worker threads with no ambient tokio runtime, so
// `Handle::current()` panics with "there is no reactor running" — a
// non-unwinding panic that aborts the whole process when it unwinds
// across the WebView2 FFI boundary. Making the command async puts it on
// Tauri's tokio runtime, and the `_async` helpers below `.await` the PG
// calls directly instead of cross-runtime blocking.
#[tauri::command]
pub async fn get_meta_optimizer_progress(
    app_state: State<'_, StorageCompartment>,
    category: Option<String>,
) -> Result<serde_json::Value, String> {
    let cat = crate::meta_optimizer::types::WorkflowCategory::from_str_opt(category.as_deref());
    let summary =
        crate::meta_optimizer::snapshots::get_progress_summary_async(&app_state.pg_db(), cat).await?;
    serde_json::to_value(summary).map_err(|e: serde_json::Error| String::from(AppError::from(e)))
}

#[tauri::command]
pub async fn capture_meta_optimizer_baseline(
    app_state: State<'_, StorageCompartment>,
    category: Option<String>,
) -> Result<serde_json::Value, String> {
    let cat = crate::meta_optimizer::types::WorkflowCategory::from_str_opt(category.as_deref());
    let snapshot =
        crate::meta_optimizer::snapshots::capture_baseline_async(&app_state.pg_db(), cat).await?;
    serde_json::to_value(snapshot).map_err(|e: serde_json::Error| String::from(AppError::from(e)))
}

#[tauri::command]
pub async fn get_meta_optimizer_snapshots(
    app_state: State<'_, StorageCompartment>,
    snapshot_type: Option<String>,
) -> Result<serde_json::Value, String> {
    let snapshots = crate::meta_optimizer::snapshots::list_snapshots_async(
        &app_state.pg_db(),
        snapshot_type.as_deref(),
    )
    .await?;
    serde_json::to_value(snapshots).map_err(|e: serde_json::Error| String::from(AppError::from(e)))
}

// ── Agent Effectiveness ───────────────────────────────────────────────

#[tauri::command]
pub async fn get_agent_effectiveness(
    app_state: State<'_, StorageCompartment>,
    limit: Option<u32>,
) -> Result<Vec<crate::database::pipeline_traces::AgentTraceAggregate>, String> {
    let _ = app_state;
    crate::database::pipeline_traces::get_agent_trace_aggregates(limit.unwrap_or(200))
}

// ── Failure Analysis ─────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_meta_optimizer_failure_analysis(
    app_state: State<'_, StorageCompartment>,
    days: Option<u32>,
    category: Option<String>,
) -> Result<crate::meta_optimizer::failure_analysis::FailureAnalysis, String> {
    let cat = crate::meta_optimizer::types::WorkflowCategory::from_str_opt(category.as_deref());
    crate::meta_optimizer::failure_analysis::get_failure_analysis(
        &app_state.pg_db(),
        days.unwrap_or(30),
        cat,
    )
}

// ── Recommendation Outcomes (Regression Detection) ───────────────────

#[tauri::command]
pub async fn get_recommendation_outcomes(
    app_state: State<'_, StorageCompartment>,
) -> Result<Vec<serde_json::Value>, String> {
    let recs = crate::meta_optimizer::recommendations::list_recommendations_async(
        &app_state.pg_db(),
        None,
        Some("applied"),
    )
    .await?;

    let mut results = Vec::new();
    for rec in recs {
        let mut val =
            serde_json::to_value(&rec).map_err(|e: serde_json::Error| String::from(AppError::from(e)))?;
        if let Some(outcome_str) = &rec.outcome_after_apply {
            if let Ok(outcome) = serde_json::from_str::<serde_json::Value>(outcome_str) {
                val["outcome_parsed"] = outcome;
            }
        }
        results.push(val);
    }
    Ok(results)
}

#[tauri::command]
pub async fn reevaluate_recommendation_outcome(
    app_state: State<'_, StorageCompartment>,
    recommendation_id: String,
) -> Result<serde_json::Value, String> {
    let outcome = crate::meta_optimizer::snapshots::evaluate_recommendation_outcome(
        &app_state.pg_db(),
        &recommendation_id,
    )?;
    serde_json::to_value(outcome).map_err(|e: serde_json::Error| String::from(AppError::from(e)))
}

// ── Cost-Effectiveness ──────────────────────────────────────────────────

#[tauri::command]
pub async fn get_agent_cost_effectiveness(
    app_state: State<'_, StorageCompartment>,
    agent_type: Option<String>,
    days: Option<i64>,
) -> Result<Vec<crate::database::pipeline_traces::CostEffectivenessPoint>, String> {
    let _ = app_state;
    crate::database::pipeline_traces::get_agent_cost_effectiveness(
        agent_type.as_deref(),
        days.unwrap_or(90),
    )
}

// ── Cross-Agent Interaction ─────────────────────────────────────────────

#[tauri::command]
pub async fn get_agent_interaction_matrix(
    app_state: State<'_, StorageCompartment>,
    days: Option<i64>,
) -> Result<Vec<crate::database::pipeline_traces::AgentInteraction>, String> {
    let _ = app_state;
    crate::database::pipeline_traces::get_agent_interaction_matrix(days.unwrap_or(30))
}

#[tauri::command]
pub async fn get_agent_cascade_effect(
    app_state: State<'_, StorageCompartment>,
    recommendation_id: String,
) -> Result<Vec<crate::database::pipeline_traces::CascadeEffect>, String> {
    let _ = app_state;
    crate::database::pipeline_traces::get_agent_cascade_effect(&recommendation_id)
}

// ── Canary Rollout ──────────────────────────────────────────────────────

#[tauri::command]
pub async fn start_canary_rollout(
    app_state: State<'_, StorageCompartment>,
    recommendation_id: String,
    percentage: Option<i64>,
) -> Result<String, String> {
    crate::meta_optimizer::canary::start_canary(
        &app_state.pg_db(),
        &recommendation_id,
        percentage.unwrap_or(10),
    )
}

#[tauri::command]
pub async fn get_canary_rollouts(
    app_state: State<'_, StorageCompartment>,
) -> Result<Vec<serde_json::Value>, String> {
    let rollouts = crate::meta_optimizer::canary::get_active_canaries(&app_state.pg_db())?;
    let recs = crate::meta_optimizer::recommendations::list_recommendations_async(
        &app_state.pg_db(),
        None,
        None,
    )
    .await
    .unwrap_or_default();

    let values: Vec<serde_json::Value> = rollouts
        .into_iter()
        .map(|r| {
            let mut val = serde_json::to_value(&r).unwrap_or_default();
            // Enrich with recommendation title for display
            if let Some(rec) = recs.iter().find(|rec| rec.id == r.recommendation_id) {
                val["recommendation_title"] = serde_json::json!(rec.title);
                val["target_agent"] = serde_json::json!(rec.target_agent);
            }
            val
        })
        .collect();
    Ok(values)
}

#[tauri::command]
pub async fn promote_canary_rollout(
    app_state: State<'_, StorageCompartment>,
    canary_id: String,
) -> Result<(), String> {
    crate::meta_optimizer::canary::promote_canary(&app_state.pg_db(), &canary_id)
}

#[tauri::command]
pub async fn rollback_canary_rollout(
    app_state: State<'_, StorageCompartment>,
    canary_id: String,
) -> Result<(), String> {
    crate::meta_optimizer::canary::rollback_canary(&app_state.pg_db(), &canary_id)
}

// ── Prompt Template A/B Testing ──────────────────────────────────────────

#[tauri::command]
pub async fn create_prompt_canary(
    app_state: State<'_, StorageCompartment>,
    template_id: String,
    baseline_version: i32,
    candidate_version: i32,
    traffic_pct: f64,
) -> Result<String, String> {
    app_state
        .pg_db()
        .create_template_canary(
            &template_id,
            baseline_version,
            candidate_version,
            traffic_pct,
        )
        .await
}

#[tauri::command]
pub async fn get_prompt_canary_status(
    app_state: State<'_, StorageCompartment>,
    canary_id: String,
) -> Result<serde_json::Value, String> {
    let canary = app_state
        .pg_db()
        .get_template_canary(&canary_id)
        .await?
        .ok_or_else(|| format!("Prompt template canary not found: {}", canary_id))?;
    let evaluation = crate::meta_optimizer::canary::evaluate_prompt_canary(&canary);

    let mut val =
        serde_json::to_value(&canary).map_err(|e: serde_json::Error| String::from(AppError::from(e)))?;
    val["evaluation"] =
        serde_json::to_value(&evaluation).map_err(|e: serde_json::Error| String::from(AppError::from(e)))?;
    Ok(val)
}

// ── Manual Trigger ─────────────────────────────────────────────────────

// trigger_meta_optimizer keeps `Arc<AppState>` because it passes a raw
// `Arc<AppState>` into MetaOptimizerDeps and also reads `ai_pid_tracker`
// (ExecutionCompartment) alongside, so no single compartment covers it.
#[tauri::command]
pub async fn trigger_meta_optimizer(
    app_state: State<'_, Arc<AppState>>,
    app_handle: tauri::AppHandle,
    optimizer_type: String,
) -> Result<String, String> {
    let opt_type = match optimizer_type.as_str() {
        "pipeline_prompt" => OptimizerType::PipelinePrompt,
        "architecture" => OptimizerType::Architecture,
        "generation_template" => OptimizerType::GenerationTemplate,
        "meta_prompt" => OptimizerType::MetaPrompt,
        _ => return Err(format!("Unknown optimizer type: {}", optimizer_type)),
    };

    // Create a fresh ConfigStorage for the optimizer workflow
    let config_storage = crate::config_storage::ConfigStorage::new()
        .unwrap_or_else(|_| crate::config_storage::ConfigStorage::new_degraded());

    let deps = crate::meta_optimizer::types::MetaOptimizerDeps {
        app_state: app_state.inner().clone(),
        config_storage: Arc::new(tokio::sync::Mutex::new(config_storage)),
        app_handle,
        pid_tracker: app_state.ai_pid_tracker.clone(),
        session_manager: None,
    };

    crate::meta_optimizer::trigger::launch_optimizer_manual(&deps, opt_type)
}

// ── Eval Specs ────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_eval_specs(
    app_state: State<'_, StorageCompartment>,
    target_agent: Option<String>,
) -> Result<Vec<crate::meta_optimizer::eval_spec::EvalSpec>, String> {
    crate::meta_optimizer::eval_spec::list_eval_specs(&app_state.pg_db(), target_agent.as_deref())
}

#[tauri::command]
pub async fn create_eval_spec(
    app_state: State<'_, StorageCompartment>,
    spec: crate::meta_optimizer::eval_spec::EvalSpec,
) -> Result<(), String> {
    crate::meta_optimizer::eval_spec::save_eval_spec(&app_state.pg_db(), &spec)
}

#[tauri::command]
pub async fn delete_eval_spec(
    app_state: State<'_, StorageCompartment>,
    spec_id: String,
) -> Result<(), String> {
    crate::meta_optimizer::eval_spec::delete_eval_spec(&app_state.pg_db(), &spec_id)
}

#[tauri::command]
pub async fn get_eval_results(
    app_state: State<'_, StorageCompartment>,
    spec_id: Option<String>,
    recommendation_id: Option<String>,
) -> Result<Vec<crate::meta_optimizer::eval_spec::EvalResult>, String> {
    crate::meta_optimizer::eval_spec::list_eval_results(
        &app_state.pg_db(),
        spec_id.as_deref(),
        recommendation_id.as_deref(),
    )
}

#[tauri::command]
pub async fn run_recommendation_eval(
    app_state: State<'_, StorageCompartment>,
    recommendation_id: String,
) -> Result<crate::meta_optimizer::eval_spec::EvalResult, String> {
    crate::meta_optimizer::eval_runner::validate_recommendation(
        &app_state.pg_db(),
        &recommendation_id,
    )
}

#[tauri::command]
pub async fn generate_default_eval_spec(
    app_state: State<'_, StorageCompartment>,
    target_agent: String,
) -> Result<crate::meta_optimizer::eval_spec::EvalSpec, String> {
    let spec = crate::meta_optimizer::eval_spec::generate_default_spec(&target_agent)?;
    crate::meta_optimizer::eval_spec::save_eval_spec(&app_state.pg_db(), &spec)?;
    Ok(spec)
}

// ── Live Evaluation with I/O ──────────────────────────────────────────

/// Evaluate an eval spec against live input/output data using LLM-as-judge assertions.
///
/// This is the on-demand evaluation path for when actual I/O from a workflow run
/// is available. Unlike the offline `run_recommendation_eval` which uses aggregate
/// metrics, this evaluates LLM judge assertions (hallucination, relevance, factuality,
/// content safety) against concrete input/output pairs.
#[tauri::command]
pub async fn evaluate_with_io(
    app_state: State<'_, StorageCompartment>,
    eval_spec_json: String,
    input: String,
    output: String,
    context: Option<String>,
) -> Result<Vec<crate::meta_optimizer::eval_spec::AssertionResult>, String> {
    let _ = app_state;
    let spec: crate::meta_optimizer::eval_spec::EvalSpec = serde_json::from_str(&eval_spec_json)
        .map_err(|e: serde_json::Error| String::from(AppError::from(e)))?;

    // Build dummy aggregate metrics for non-judge assertions.
    // If a target agent is specified, try to load real metrics from the database.
    let metrics = if let Some(ref agent) = spec.target_agent {
        let period_start = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let period_end = chrono::Utc::now().to_rfc3339();

        crate::database::pipeline_traces::get_agent_aggregates_for_period(
            agent,
            &period_start,
            &period_end,
        )
        .ok()
        .flatten()
        .map(
            |agg| crate::meta_optimizer::eval_spec::EvalAggregateMetrics {
                success_rate: if agg.run_count > 0 {
                    agg.success_count as f64 / agg.run_count as f64
                } else {
                    0.0
                },
                mean_duration_ms: agg.avg_duration_ms,
                mean_iterations: 0.0,
                mean_cost_cents: agg.avg_cost_usd * 100.0,
                trial_count: agg.run_count as u32,
            },
        )
        .unwrap_or_else(default_aggregate_metrics)
    } else {
        default_aggregate_metrics()
    };

    // Evaluate all test cases, collecting results
    let mut all_results = Vec::new();
    for tc in &spec.test_cases {
        let results = crate::meta_optimizer::eval_runner::evaluate_test_case_with_io(
            tc,
            &input,
            &output,
            context.as_deref(),
            &metrics,
        );
        all_results.extend(results);
    }

    Ok(all_results)
}

/// Build default aggregate metrics when no historical data is available.
fn default_aggregate_metrics() -> crate::meta_optimizer::eval_spec::EvalAggregateMetrics {
    crate::meta_optimizer::eval_spec::EvalAggregateMetrics {
        success_rate: 0.0,
        mean_duration_ms: 0.0,
        mean_iterations: 0.0,
        mean_cost_cents: 0.0,
        trial_count: 0,
    }
}

// ── Robustness Testing ────────────────────────────────────────────────

#[tauri::command]
pub async fn run_robustness_test(
    app_state: State<'_, StorageCompartment>,
    agent_type: String,
    prompt_variant_id: Option<String>,
    recommendation_id: Option<String>,
) -> Result<crate::meta_optimizer::robustness::RobustnessReport, String> {
    crate::meta_optimizer::robustness::run_robustness_test(
        &app_state.pg_db(),
        &agent_type,
        prompt_variant_id.as_deref(),
        recommendation_id.as_deref(),
    )
}

#[tauri::command]
pub async fn get_robustness_reports(
    app_state: State<'_, StorageCompartment>,
    prompt_variant_id: Option<String>,
    recommendation_id: Option<String>,
) -> Result<Vec<crate::meta_optimizer::robustness::RobustnessReport>, String> {
    crate::meta_optimizer::robustness::list_robustness_reports(
        &app_state.pg_db(),
        prompt_variant_id.as_deref(),
        recommendation_id.as_deref(),
    )
}

// ── Golden Datasets ───────────────────────────────────────────────────

#[tauri::command]
pub async fn get_golden_datasets(
    app_state: State<'_, StorageCompartment>,
    agent_type: Option<String>,
) -> Result<Vec<crate::meta_optimizer::golden_dataset::GoldenDataset>, String> {
    crate::meta_optimizer::golden_dataset::list_golden_datasets(
        &app_state.pg_db(),
        agent_type.as_deref(),
    )
}

#[tauri::command]
pub async fn build_golden_dataset(
    app_state: State<'_, StorageCompartment>,
    agent_type: String,
    max_entries: Option<usize>,
) -> Result<crate::meta_optimizer::golden_dataset::GoldenDataset, String> {
    crate::meta_optimizer::golden_dataset::build_from_history(
        &app_state.pg_db(),
        &agent_type,
        max_entries.unwrap_or(50),
    )
}

// ── Model Profiles ────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_model_profiles(
    app_state: State<'_, StorageCompartment>,
) -> Result<Vec<crate::routing::model_profiles::ModelProfile>, String> {
    crate::routing::model_profiles::list_model_profiles(&app_state.pg_db()).await
}

#[tauri::command]
pub async fn refresh_model_profiles(
    app_state: State<'_, StorageCompartment>,
    days: Option<i64>,
) -> Result<Vec<crate::routing::model_profiles::ModelProfile>, String> {
    crate::routing::model_profiles::refresh_all_profiles(&app_state.pg_db(), days.unwrap_or(30)).await
}

#[tauri::command]
pub async fn get_model_recommendations(
    app_state: State<'_, StorageCompartment>,
    budget_usd: Option<f64>,
) -> Result<Vec<crate::routing::model_profiles::ModelRecommendation>, String> {
    crate::routing::model_profiles::get_model_recommendation(&app_state.pg_db(), budget_usd).await
}

// ── Comparison Bridge ─────────────────────────────────────────────────

#[tauri::command]
pub async fn convert_comparison_to_recommendation(
    app_state: State<'_, StorageCompartment>,
    comparison_id: String,
) -> Result<Option<String>, String> {
    crate::meta_optimizer::comparison_bridge::comparison_to_recommendation(
        &app_state.pg_db(),
        &comparison_id,
    )
}

// ── Prompt Optimization (Meta-Prompt Optimizer) ────────────────────────

#[tauri::command]
pub async fn get_prompt_optimization_status(
    app_state: State<'_, StorageCompartment>,
) -> Result<serde_json::Value, String> {
    let pg_db = &app_state.pg_db();

    // Get prompt group metrics
    let samples = crate::meta_optimizer::prompt_extractor::extract_prompt_samples_pg(pg_db, 500)?;
    let groups =
        crate::meta_optimizer::prompt_extractor::compute_group_metrics_with_db_pg(&samples, pg_db);

    // Get active evolution entries (canaries in progress)
    let evolution =
        crate::meta_optimizer::prompt_evolution::get_evolution_history(pg_db, None, 50)?;
    let active_canaries: Vec<_> = evolution
        .iter()
        .filter(|e| e.canary_verdict.is_none())
        .collect();

    serde_json::to_value(serde_json::json!({
        "prompt_groups": groups,
        "active_canaries": active_canaries,
        "evolution_history": evolution,
    }))
    .map_err(|e: serde_json::Error| String::from(AppError::from(e)))
}

#[tauri::command]
pub async fn get_prompt_group_metrics(
    app_state: State<'_, StorageCompartment>,
) -> Result<Vec<crate::meta_optimizer::prompt_extractor::PromptGroupMetrics>, String> {
    let samples =
        crate::meta_optimizer::prompt_extractor::extract_prompt_samples_pg(&app_state.pg_db(), 500)?;
    Ok(
        crate::meta_optimizer::prompt_extractor::compute_group_metrics_with_db_pg(
            &samples,
            &app_state.pg_db(),
        ),
    )
}

#[tauri::command]
pub async fn get_prompt_optimization_evidence(
    app_state: State<'_, StorageCompartment>,
    phase: String,
    max_failures: Option<usize>,
    max_successes: Option<usize>,
) -> Result<serde_json::Value, String> {
    let (failures, successes) =
        crate::meta_optimizer::prompt_extractor::collect_evidence_samples_pg(
            &app_state.pg_db(),
            &phase,
            "",
            max_failures.unwrap_or(5),
            max_successes.unwrap_or(2),
        )?;

    serde_json::to_value(serde_json::json!({
        "failures": failures,
        "successes": successes,
    }))
    .map_err(|e: serde_json::Error| String::from(AppError::from(e)))
}

#[tauri::command]
pub async fn get_prompt_evolution_history(
    app_state: State<'_, StorageCompartment>,
    agent_type: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<crate::meta_optimizer::prompt_evolution::PromptEvolutionEntry>, String> {
    crate::meta_optimizer::prompt_evolution::get_evolution_history(
        &app_state.pg_db(),
        agent_type.as_deref(),
        limit.unwrap_or(50),
    )
}

#[tauri::command]
pub async fn get_prompt_variant_content(
    app_state: State<'_, StorageCompartment>,
    variant_id: String,
) -> Result<Option<String>, String> {
    app_state
        .pg_db()
        .get_prompt_variant_content(&variant_id)
        .await
}

#[tauri::command]
pub async fn get_prompt_evolution_diff(
    app_state: State<'_, StorageCompartment>,
    evolution_id: String,
) -> Result<serde_json::Value, String> {
    let pg_db = &app_state.pg_db();

    // Get the evolution entry
    let history = crate::meta_optimizer::prompt_evolution::get_evolution_history(pg_db, None, 100)?;
    let entry = history
        .iter()
        .find(|e| e.id == evolution_id)
        .ok_or_else(|| format!("Evolution entry not found: {}", evolution_id))?;

    // Get the new prompt content from the variant via PG
    let new_content = app_state
        .pg_db()
        .get_prompt_variant_content(&entry.variant_id)
        .await?;

    // Get the old prompt content (from parent variant or default)
    let old_content: String = if let Some(ref parent_id) = entry.parent_variant_id {
        let parent_content = app_state
            .pg_db()
            .get_prompt_variant_content(parent_id)
            .await?;
        parent_content.unwrap_or_else(|| {
            crate::meta_optimizer::prompt_extractor::get_default_agent_prompt(&entry.agent_type)
        })
    } else {
        crate::meta_optimizer::prompt_extractor::get_default_agent_prompt(&entry.agent_type)
    };

    Ok(serde_json::json!({
        "old_content": old_content,
        "new_content": new_content.unwrap_or_default(),
        "agent_type": entry.agent_type,
        "critique": entry.critique,
        "changes_summary": entry.changes_summary,
    }))
}

pub fn plugin() -> TauriPlugin<tauri::Wry> {
    PluginBuilder::<tauri::Wry>::new("qontinui_meta_optimizer")
        .invoke_handler(tauri::generate_handler![
            get_meta_optimizer_recommendations,
            apply_meta_optimizer_recommendation,
            reject_meta_optimizer_recommendation,
            rollback_meta_optimizer_recommendation,
            get_prompt_variants,
            activate_prompt_variant,
            get_meta_optimizer_runs,
            get_meta_optimizer_progress,
            capture_meta_optimizer_baseline,
            get_meta_optimizer_snapshots,
            get_agent_effectiveness,
            get_meta_optimizer_failure_analysis,
            get_recommendation_outcomes,
            reevaluate_recommendation_outcome,
            get_agent_cost_effectiveness,
            get_agent_interaction_matrix,
            get_agent_cascade_effect,
            start_canary_rollout,
            get_canary_rollouts,
            promote_canary_rollout,
            rollback_canary_rollout,
            create_prompt_canary,
            get_prompt_canary_status,
            trigger_meta_optimizer,
            get_eval_specs,
            create_eval_spec,
            delete_eval_spec,
            get_eval_results,
            run_recommendation_eval,
            generate_default_eval_spec,
            evaluate_with_io,
            run_robustness_test,
            get_robustness_reports,
            get_golden_datasets,
            build_golden_dataset,
            get_model_profiles,
            refresh_model_profiles,
            get_model_recommendations,
            convert_comparison_to_recommendation,
            get_prompt_optimization_status,
            get_prompt_group_metrics,
            get_prompt_optimization_evidence,
            get_prompt_evolution_history,
            get_prompt_variant_content,
            get_prompt_evolution_diff,
        ])
        .build()
}
