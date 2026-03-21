//! Tauri commands for the meta-optimizer system.
//!
//! Provides frontend access to recommendations, prompt registry, optimizer runs,
//! and manual optimizer triggering.

use std::sync::Arc;
use tauri::State;

use crate::commands::AppState;
use crate::meta_optimizer::types::{
    MetaOptimizerRun, OptimizerType, PromptVariant, Recommendation,
};

// ── Recommendations ────────────────────────────────────────────────────

#[tauri::command]
pub fn get_meta_optimizer_recommendations(
    app_state: State<'_, Arc<AppState>>,
    optimizer_type: Option<String>,
    status: Option<String>,
) -> Result<Vec<Recommendation>, String> {
    crate::meta_optimizer::recommendations::list_recommendations(
        &app_state.checkpoint_db,
        optimizer_type.as_deref(),
        status.as_deref(),
    )
}

#[tauri::command]
pub fn apply_meta_optimizer_recommendation(
    app_state: State<'_, Arc<AppState>>,
    recommendation_id: String,
) -> Result<(), String> {
    crate::meta_optimizer::recommendations::apply_recommendation_with_side_effects(
        &app_state.checkpoint_db,
        &recommendation_id,
    )
}

#[tauri::command]
pub fn reject_meta_optimizer_recommendation(
    app_state: State<'_, Arc<AppState>>,
    recommendation_id: String,
) -> Result<(), String> {
    crate::meta_optimizer::recommendations::reject_recommendation(
        &app_state.checkpoint_db,
        &recommendation_id,
    )
}

#[tauri::command]
pub fn rollback_meta_optimizer_recommendation(
    app_state: State<'_, Arc<AppState>>,
    recommendation_id: String,
) -> Result<(), String> {
    crate::meta_optimizer::recommendations::rollback_recommendation(
        &app_state.checkpoint_db,
        &recommendation_id,
    )
}

// ── Prompt Registry ────────────────────────────────────────────────────

#[tauri::command]
pub fn get_prompt_variants(
    app_state: State<'_, Arc<AppState>>,
    agent_type: Option<String>,
) -> Result<Vec<PromptVariant>, String> {
    crate::meta_optimizer::prompt_registry::list_variants(
        &app_state.checkpoint_db,
        agent_type.as_deref(),
    )
}

#[tauri::command]
pub fn activate_prompt_variant(
    app_state: State<'_, Arc<AppState>>,
    variant_id: String,
) -> Result<(), String> {
    crate::meta_optimizer::prompt_registry::activate_variant(&app_state.checkpoint_db, &variant_id)
}

// ── Optimizer Runs ─────────────────────────────────────────────────────

#[tauri::command]
pub fn get_meta_optimizer_runs(
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<MetaOptimizerRun>, String> {
    crate::meta_optimizer::recommendations::list_optimizer_runs(&app_state.checkpoint_db)
}

// ── Progress Tracking ─────────────────────────────────────────────────

#[tauri::command]
pub fn get_meta_optimizer_progress(
    app_state: State<'_, Arc<AppState>>,
    category: Option<String>,
) -> Result<serde_json::Value, String> {
    let cat = crate::meta_optimizer::types::WorkflowCategory::from_str_opt(category.as_deref());
    let summary =
        crate::meta_optimizer::snapshots::get_progress_summary(&app_state.checkpoint_db, cat)?;
    serde_json::to_value(summary).map_err(|e| format!("Serialization error: {}", e))
}

#[tauri::command]
pub fn capture_meta_optimizer_baseline(
    app_state: State<'_, Arc<AppState>>,
    category: Option<String>,
) -> Result<serde_json::Value, String> {
    let cat = crate::meta_optimizer::types::WorkflowCategory::from_str_opt(category.as_deref());
    let snapshot =
        crate::meta_optimizer::snapshots::capture_baseline(&app_state.checkpoint_db, cat)?;
    serde_json::to_value(snapshot).map_err(|e| format!("Serialization error: {}", e))
}

#[tauri::command]
pub fn get_meta_optimizer_snapshots(
    app_state: State<'_, Arc<AppState>>,
    snapshot_type: Option<String>,
) -> Result<serde_json::Value, String> {
    let snapshots = crate::meta_optimizer::snapshots::list_snapshots(
        &app_state.checkpoint_db,
        snapshot_type.as_deref(),
    )?;
    serde_json::to_value(snapshots).map_err(|e| format!("Serialization error: {}", e))
}

// ── Agent Effectiveness ───────────────────────────────────────────────

#[tauri::command]
pub fn get_agent_effectiveness(
    app_state: State<'_, Arc<AppState>>,
    limit: Option<u32>,
) -> Result<Vec<crate::database::pipeline_traces::AgentTraceAggregate>, String> {
    crate::database::pipeline_traces::get_agent_trace_aggregates(
        &app_state.checkpoint_db,
        limit.unwrap_or(200),
    )
}

// ── Failure Analysis ─────────────────────────────────────────────────────

#[tauri::command]
pub fn get_meta_optimizer_failure_analysis(
    app_state: State<'_, Arc<AppState>>,
    days: Option<u32>,
    category: Option<String>,
) -> Result<crate::meta_optimizer::failure_analysis::FailureAnalysis, String> {
    let cat = crate::meta_optimizer::types::WorkflowCategory::from_str_opt(category.as_deref());
    crate::meta_optimizer::failure_analysis::get_failure_analysis(
        &app_state.checkpoint_db,
        days.unwrap_or(30),
        cat,
    )
}

// ── Recommendation Outcomes (Regression Detection) ───────────────────

#[tauri::command]
pub fn get_recommendation_outcomes(
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<serde_json::Value>, String> {
    let recs = crate::meta_optimizer::recommendations::list_recommendations(
        &app_state.checkpoint_db,
        None,
        Some("applied"),
    )?;

    let mut results = Vec::new();
    for rec in recs {
        let mut val =
            serde_json::to_value(&rec).map_err(|e| format!("Serialization error: {}", e))?;
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
pub fn reevaluate_recommendation_outcome(
    app_state: State<'_, Arc<AppState>>,
    recommendation_id: String,
) -> Result<serde_json::Value, String> {
    let outcome = crate::meta_optimizer::snapshots::evaluate_recommendation_outcome(
        &app_state.checkpoint_db,
        &recommendation_id,
    )?;
    serde_json::to_value(outcome).map_err(|e| format!("Serialization error: {}", e))
}

// ── Cost-Effectiveness ──────────────────────────────────────────────────

#[tauri::command]
pub fn get_agent_cost_effectiveness(
    app_state: State<'_, Arc<AppState>>,
    agent_type: Option<String>,
    days: Option<i64>,
) -> Result<Vec<crate::database::pipeline_traces::CostEffectivenessPoint>, String> {
    crate::database::pipeline_traces::get_agent_cost_effectiveness(
        &app_state.checkpoint_db,
        agent_type.as_deref(),
        days.unwrap_or(90),
    )
}

// ── Cross-Agent Interaction ─────────────────────────────────────────────

#[tauri::command]
pub fn get_agent_interaction_matrix(
    app_state: State<'_, Arc<AppState>>,
    days: Option<i64>,
) -> Result<Vec<crate::database::pipeline_traces::AgentInteraction>, String> {
    crate::database::pipeline_traces::get_agent_interaction_matrix(
        &app_state.checkpoint_db,
        days.unwrap_or(30),
    )
}

#[tauri::command]
pub fn get_agent_cascade_effect(
    app_state: State<'_, Arc<AppState>>,
    recommendation_id: String,
) -> Result<Vec<crate::database::pipeline_traces::CascadeEffect>, String> {
    crate::database::pipeline_traces::get_agent_cascade_effect(
        &app_state.checkpoint_db,
        &recommendation_id,
    )
}

// ── Canary Rollout ──────────────────────────────────────────────────────

#[tauri::command]
pub fn start_canary_rollout(
    app_state: State<'_, Arc<AppState>>,
    recommendation_id: String,
    percentage: Option<i64>,
) -> Result<String, String> {
    crate::meta_optimizer::canary::start_canary(
        &app_state.checkpoint_db,
        &recommendation_id,
        percentage.unwrap_or(10),
    )
}

#[tauri::command]
pub fn get_canary_rollouts(
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<serde_json::Value>, String> {
    let rollouts = crate::meta_optimizer::canary::get_active_canaries(&app_state.checkpoint_db)?;
    let recs = crate::meta_optimizer::recommendations::list_recommendations(
        &app_state.checkpoint_db,
        None,
        None,
    )
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
pub fn promote_canary_rollout(
    app_state: State<'_, Arc<AppState>>,
    canary_id: String,
) -> Result<(), String> {
    crate::meta_optimizer::canary::promote_canary(&app_state.checkpoint_db, &canary_id)
}

#[tauri::command]
pub fn rollback_canary_rollout(
    app_state: State<'_, Arc<AppState>>,
    canary_id: String,
) -> Result<(), String> {
    crate::meta_optimizer::canary::rollback_canary(&app_state.checkpoint_db, &canary_id)
}

// ── Manual Trigger ─────────────────────────────────────────────────────

#[tauri::command]
pub fn trigger_meta_optimizer(
    app_state: State<'_, Arc<AppState>>,
    app_handle: tauri::AppHandle,
    optimizer_type: String,
) -> Result<String, String> {
    let opt_type = match optimizer_type.as_str() {
        "pipeline_prompt" => OptimizerType::PipelinePrompt,
        "architecture" => OptimizerType::Architecture,
        "generation_template" => OptimizerType::GenerationTemplate,
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
pub fn get_eval_specs(
    app_state: State<'_, Arc<AppState>>,
    target_agent: Option<String>,
) -> Result<Vec<crate::meta_optimizer::eval_spec::EvalSpec>, String> {
    crate::meta_optimizer::eval_spec::list_eval_specs(
        &app_state.checkpoint_db,
        target_agent.as_deref(),
    )
}

#[tauri::command]
pub fn create_eval_spec(
    app_state: State<'_, Arc<AppState>>,
    spec: crate::meta_optimizer::eval_spec::EvalSpec,
) -> Result<(), String> {
    crate::meta_optimizer::eval_spec::save_eval_spec(&app_state.checkpoint_db, &spec)
}

#[tauri::command]
pub fn delete_eval_spec(
    app_state: State<'_, Arc<AppState>>,
    spec_id: String,
) -> Result<(), String> {
    crate::meta_optimizer::eval_spec::delete_eval_spec(&app_state.checkpoint_db, &spec_id)
}

#[tauri::command]
pub fn get_eval_results(
    app_state: State<'_, Arc<AppState>>,
    spec_id: Option<String>,
    recommendation_id: Option<String>,
) -> Result<Vec<crate::meta_optimizer::eval_spec::EvalResult>, String> {
    crate::meta_optimizer::eval_spec::list_eval_results(
        &app_state.checkpoint_db,
        spec_id.as_deref(),
        recommendation_id.as_deref(),
    )
}

#[tauri::command]
pub fn run_recommendation_eval(
    app_state: State<'_, Arc<AppState>>,
    recommendation_id: String,
) -> Result<crate::meta_optimizer::eval_spec::EvalResult, String> {
    crate::meta_optimizer::eval_runner::validate_recommendation(
        &app_state.checkpoint_db,
        &recommendation_id,
    )
}

#[tauri::command]
pub fn generate_default_eval_spec(
    app_state: State<'_, Arc<AppState>>,
    target_agent: String,
) -> Result<crate::meta_optimizer::eval_spec::EvalSpec, String> {
    let spec = crate::meta_optimizer::eval_spec::generate_default_spec(
        &app_state.checkpoint_db,
        &target_agent,
    )?;
    crate::meta_optimizer::eval_spec::save_eval_spec(&app_state.checkpoint_db, &spec)?;
    Ok(spec)
}

// ── Robustness Testing ────────────────────────────────────────────────

#[tauri::command]
pub fn run_robustness_test(
    app_state: State<'_, Arc<AppState>>,
    agent_type: String,
    prompt_variant_id: Option<String>,
    recommendation_id: Option<String>,
) -> Result<crate::meta_optimizer::robustness::RobustnessReport, String> {
    crate::meta_optimizer::robustness::run_robustness_test(
        &app_state.checkpoint_db,
        &agent_type,
        prompt_variant_id.as_deref(),
        recommendation_id.as_deref(),
    )
}

#[tauri::command]
pub fn get_robustness_reports(
    app_state: State<'_, Arc<AppState>>,
    prompt_variant_id: Option<String>,
    recommendation_id: Option<String>,
) -> Result<Vec<crate::meta_optimizer::robustness::RobustnessReport>, String> {
    crate::meta_optimizer::robustness::list_robustness_reports(
        &app_state.checkpoint_db,
        prompt_variant_id.as_deref(),
        recommendation_id.as_deref(),
    )
}

// ── Golden Datasets ───────────────────────────────────────────────────

#[tauri::command]
pub fn get_golden_datasets(
    app_state: State<'_, Arc<AppState>>,
    agent_type: Option<String>,
) -> Result<Vec<crate::meta_optimizer::golden_dataset::GoldenDataset>, String> {
    crate::meta_optimizer::golden_dataset::list_golden_datasets(
        &app_state.checkpoint_db,
        agent_type.as_deref(),
    )
}

#[tauri::command]
pub fn build_golden_dataset(
    app_state: State<'_, Arc<AppState>>,
    agent_type: String,
    max_entries: Option<usize>,
) -> Result<crate::meta_optimizer::golden_dataset::GoldenDataset, String> {
    crate::meta_optimizer::golden_dataset::build_from_history(
        &app_state.checkpoint_db,
        &agent_type,
        max_entries.unwrap_or(50),
    )
}
