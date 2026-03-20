//! Tauri commands for the spec experimentation system.

use std::sync::Arc;
use tauri::State;

use crate::commands::AppState;
use super::accuracy;
use super::compliance;

// ── Compliance commands ───────────────────────────────────────────────

#[tauri::command]
pub fn get_spec_compliance_history(
    app_state: State<'_, Arc<AppState>>,
    spec_id: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<compliance::SpecComplianceResult>, String> {
    compliance::get_compliance_history(
        &app_state.checkpoint_db,
        spec_id.as_deref(),
        limit,
    )
}

#[tauri::command]
pub fn get_spec_compliance_summary(
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<compliance::SpecComplianceSummary>, String> {
    compliance::get_compliance_summary(&app_state.checkpoint_db)
}

#[tauri::command]
pub fn extract_spec_compliance(
    app_state: State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<compliance::SpecComplianceResult, String> {
    compliance::extract_compliance(&app_state.checkpoint_db, &task_run_id)
}

// ── Accuracy commands ─────────────────────────────────────────────────

#[tauri::command]
pub fn analyze_spec_element_coverage(
    app_state: State<'_, Arc<AppState>>,
    spec_id: String,
    spec_config: serde_json::Value,
    snapshot_elements: Vec<serde_json::Value>,
) -> Result<accuracy::ElementCoverageResult, String> {
    let result = accuracy::analyze_element_coverage(&spec_config, &snapshot_elements)?;

    // Store the result
    let detail_json = serde_json::to_string(&result).unwrap_or_else(|_| "{}".into());
    accuracy::store_accuracy_result(
        &app_state.checkpoint_db,
        &spec_id,
        "element_coverage",
        result.element_coverage,
        &detail_json,
    )?;

    Ok(result)
}

#[tauri::command]
pub fn analyze_cross_page_consistency(
    app_state: State<'_, Arc<AppState>>,
    specs: Vec<(String, serde_json::Value)>,
) -> Result<accuracy::CrossPageConsistencyResult, String> {
    let result = accuracy::analyze_cross_page_consistency(&specs);

    // Store per-spec results
    for spec_score in &result.per_spec_scores {
        let detail_json = serde_json::to_string(spec_score).unwrap_or_else(|_| "{}".into());
        let _ = accuracy::store_accuracy_result(
            &app_state.checkpoint_db,
            &spec_score.spec_id,
            "cross_page",
            spec_score.score,
            &detail_json,
        );
    }

    Ok(result)
}

#[tauri::command]
pub fn run_spec_mutation_test(
    app_state: State<'_, Arc<AppState>>,
    spec_id: String,
    spec_config: serde_json::Value,
    snapshot_elements: Vec<serde_json::Value>,
) -> Result<accuracy::MutationTestResult, String> {
    let result = accuracy::run_mutation_test(&spec_config, &snapshot_elements)?;

    let detail_json = serde_json::to_string(&result).unwrap_or_else(|_| "{}".into());
    accuracy::store_accuracy_result(
        &app_state.checkpoint_db,
        &spec_id,
        "mutation",
        result.mutation_score,
        &detail_json,
    )?;

    Ok(result)
}

#[tauri::command]
pub fn analyze_spec_freshness(
    _app_state: State<'_, Arc<AppState>>,
    specs_dir: String,
    components_dir: String,
) -> Result<Vec<(String, accuracy::FreshnessResult)>, String> {
    let specs_path = std::path::Path::new(&specs_dir);
    let components_path = std::path::Path::new(&components_dir);

    if !specs_path.exists() {
        return Err(format!("Specs directory not found: {}", specs_dir));
    }

    Ok(accuracy::analyze_all_freshness(specs_path, components_path))
}

#[tauri::command]
pub fn get_spec_accuracy_results(
    app_state: State<'_, Arc<AppState>>,
    spec_id: Option<String>,
    analysis_type: Option<String>,
) -> Result<Vec<accuracy::SpecAccuracyRecord>, String> {
    accuracy::get_accuracy_results(
        &app_state.checkpoint_db,
        spec_id.as_deref(),
        analysis_type.as_deref(),
    )
}
