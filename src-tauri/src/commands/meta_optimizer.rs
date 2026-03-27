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

// ── Prompt Template A/B Testing ──────────────────────────────────────────

#[tauri::command]
pub async fn create_prompt_canary(
    app_state: State<'_, Arc<AppState>>,
    template_id: String,
    baseline_version: i32,
    candidate_version: i32,
    traffic_pct: f64,
) -> Result<String, String> {
    app_state.pg_db.create_template_canary(&template_id, baseline_version, candidate_version, traffic_pct)
        .await
}

#[tauri::command]
pub async fn get_prompt_canary_status(
    app_state: State<'_, Arc<AppState>>,
    canary_id: String,
) -> Result<serde_json::Value, String> {
    let canary = app_state.pg_db.get_template_canary(&canary_id)
        .await?
        .ok_or_else(|| format!("Prompt template canary not found: {}", canary_id))?;
    let evaluation = crate::meta_optimizer::canary::evaluate_prompt_canary(&canary);

    let mut val =
        serde_json::to_value(&canary).map_err(|e| format!("Serialization error: {}", e))?;
    val["evaluation"] =
        serde_json::to_value(&evaluation).map_err(|e| format!("Serialization error: {}", e))?;
    Ok(val)
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

// ── Live Evaluation with I/O ──────────────────────────────────────────

/// Evaluate an eval spec against live input/output data using LLM-as-judge assertions.
///
/// This is the on-demand evaluation path for when actual I/O from a workflow run
/// is available. Unlike the offline `run_recommendation_eval` which uses aggregate
/// metrics, this evaluates LLM judge assertions (hallucination, relevance, factuality,
/// content safety) against concrete input/output pairs.
#[tauri::command]
pub fn evaluate_with_io(
    app_state: State<'_, Arc<AppState>>,
    eval_spec_json: String,
    input: String,
    output: String,
    context: Option<String>,
) -> Result<Vec<crate::meta_optimizer::eval_spec::AssertionResult>, String> {
    let spec: crate::meta_optimizer::eval_spec::EvalSpec =
        serde_json::from_str(&eval_spec_json)
            .map_err(|e| format!("Failed to parse eval spec: {}", e))?;

    // Build dummy aggregate metrics for non-judge assertions.
    // If a target agent is specified, try to load real metrics from the database.
    let metrics = if let Some(ref agent) = spec.target_agent {
        let period_start = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let period_end = chrono::Utc::now().to_rfc3339();

        crate::database::pipeline_traces::get_agent_aggregates_for_period(
            &app_state.checkpoint_db,
            agent,
            &period_start,
            &period_end,
        )
        .ok()
        .flatten()
        .map(|agg| crate::meta_optimizer::eval_spec::EvalAggregateMetrics {
            success_rate: if agg.run_count > 0 {
                agg.success_count as f64 / agg.run_count as f64
            } else {
                0.0
            },
            mean_duration_ms: agg.avg_duration_ms,
            mean_iterations: 0.0,
            mean_cost_cents: agg.avg_cost_usd * 100.0,
            trial_count: agg.run_count as u32,
        })
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

// ── Model Profiles ────────────────────────────────────────────────────

#[tauri::command]
pub fn get_model_profiles(
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<crate::autoresearch::model_profiles::ModelProfile>, String> {
    crate::autoresearch::model_profiles::list_model_profiles(&app_state.checkpoint_db)
}

#[tauri::command]
pub fn refresh_model_profiles(
    app_state: State<'_, Arc<AppState>>,
    days: Option<i64>,
) -> Result<Vec<crate::autoresearch::model_profiles::ModelProfile>, String> {
    crate::autoresearch::model_profiles::refresh_all_profiles(
        &app_state.checkpoint_db,
        days.unwrap_or(30),
    )
}

#[tauri::command]
pub fn get_model_recommendations(
    app_state: State<'_, Arc<AppState>>,
    budget_usd: Option<f64>,
) -> Result<Vec<crate::autoresearch::model_profiles::ModelRecommendation>, String> {
    crate::autoresearch::model_profiles::get_model_recommendation(
        &app_state.checkpoint_db,
        budget_usd,
    )
}

// ── Comparison Bridge ─────────────────────────────────────────────────

#[tauri::command]
pub fn convert_comparison_to_recommendation(
    app_state: State<'_, Arc<AppState>>,
    comparison_id: String,
) -> Result<Option<String>, String> {
    crate::meta_optimizer::comparison_bridge::comparison_to_recommendation(
        &app_state.checkpoint_db,
        &comparison_id,
    )
}

// ── Prompt Optimization (Meta-Prompt Optimizer) ────────────────────────

#[tauri::command]
pub fn get_prompt_optimization_status(
    app_state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let db = &app_state.checkpoint_db;

    // Get prompt group metrics
    let samples = crate::meta_optimizer::prompt_extractor::extract_prompt_samples(db, 500)?;
    let groups = crate::meta_optimizer::prompt_extractor::compute_group_metrics_with_db(&samples, db);

    // Get active evolution entries (canaries in progress)
    let evolution =
        crate::meta_optimizer::prompt_evolution::get_evolution_history(db, None, 50)?;
    let active_canaries: Vec<_> = evolution
        .iter()
        .filter(|e| e.canary_verdict.is_none())
        .collect();

    serde_json::to_value(serde_json::json!({
        "prompt_groups": groups,
        "active_canaries": active_canaries,
        "evolution_history": evolution,
    }))
    .map_err(|e| format!("Serialization error: {}", e))
}

#[tauri::command]
pub fn get_prompt_group_metrics(
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<crate::meta_optimizer::prompt_extractor::PromptGroupMetrics>, String> {
    let samples =
        crate::meta_optimizer::prompt_extractor::extract_prompt_samples(&app_state.checkpoint_db, 500)?;
    Ok(crate::meta_optimizer::prompt_extractor::compute_group_metrics_with_db(&samples, &app_state.checkpoint_db))
}

#[tauri::command]
pub fn get_prompt_optimization_evidence(
    app_state: State<'_, Arc<AppState>>,
    phase: String,
    max_failures: Option<usize>,
    max_successes: Option<usize>,
) -> Result<serde_json::Value, String> {
    let (failures, successes) =
        crate::meta_optimizer::prompt_extractor::collect_evidence_samples(
            &app_state.checkpoint_db,
            &phase,
            "",
            max_failures.unwrap_or(5),
            max_successes.unwrap_or(2),
        )?;

    serde_json::to_value(serde_json::json!({
        "failures": failures,
        "successes": successes,
    }))
    .map_err(|e| format!("Serialization error: {}", e))
}

#[tauri::command]
pub fn get_prompt_evolution_history(
    app_state: State<'_, Arc<AppState>>,
    agent_type: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<crate::meta_optimizer::prompt_evolution::PromptEvolutionEntry>, String> {
    crate::meta_optimizer::prompt_evolution::get_evolution_history(
        &app_state.checkpoint_db,
        agent_type.as_deref(),
        limit.unwrap_or(50),
    )
}

#[tauri::command]
pub fn get_prompt_variant_content(
    app_state: State<'_, Arc<AppState>>,
    variant_id: String,
) -> Result<Option<String>, String> {
    let db = &app_state.checkpoint_db;
    // Look up prompt content by variant_id from the prompt_registry
    db.with_conn({
        let vid = variant_id;
        move |conn| {
            conn.query_row(
                "SELECT prompt_content FROM prompt_registry WHERE id = ?1",
                rusqlite::params![vid],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                _ => Err(format!("Failed to get variant content: {}", e)),
            })
        }
    })
}

#[tauri::command]
pub fn get_prompt_evolution_diff(
    app_state: State<'_, Arc<AppState>>,
    evolution_id: String,
) -> Result<serde_json::Value, String> {
    let db = &app_state.checkpoint_db;

    // Get the evolution entry
    let history = crate::meta_optimizer::prompt_evolution::get_evolution_history(db, None, 100)?;
    let entry = history
        .iter()
        .find(|e| e.id == evolution_id)
        .ok_or_else(|| format!("Evolution entry not found: {}", evolution_id))?;

    // Get the new prompt content from the variant
    let new_content: Option<String> = db.with_conn({
        let vid = entry.variant_id.clone();
        move |conn| {
            let result = conn.query_row(
                "SELECT prompt_content FROM prompt_registry WHERE id = ?1",
                rusqlite::params![vid],
                |row| row.get::<_, String>(0),
            );
            match result {
                Ok(content) => Ok(Some(content)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(format!("Failed to get variant content: {}", e)),
            }
        }
    })?;

    // Get the old prompt content (from parent variant or default)
    let old_content: String = if let Some(ref parent_id) = entry.parent_variant_id {
        let parent_content: Option<String> = db.with_conn({
            let pid = parent_id.clone();
            move |conn| {
                let result = conn.query_row(
                    "SELECT prompt_content FROM prompt_registry WHERE id = ?1",
                    rusqlite::params![pid],
                    |row| row.get::<_, String>(0),
                );
                match result {
                    Ok(content) => Ok(Some(content)),
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(e) => Err(format!("Failed to get parent content: {}", e)),
                }
            }
        })?;
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
