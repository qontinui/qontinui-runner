//! Tauri commands for the spec experimentation system.

use std::sync::Arc;
use tauri::State;

use super::accuracy;
use super::compliance;
use super::versioning;
use crate::commands::AppState;

// ── Compliance commands ───────────────────────────────────────────────

#[tauri::command]
pub async fn get_spec_compliance_history(
    app_state: State<'_, Arc<AppState>>,
    spec_id: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<compliance::SpecComplianceResult>, String> {
    compliance::get_compliance_history(&app_state.pg_db, spec_id.as_deref(), limit).await
}

#[tauri::command]
pub async fn get_spec_compliance_summary(
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<compliance::SpecComplianceSummary>, String> {
    compliance::get_compliance_summary(&app_state.pg_db).await
}

#[tauri::command]
pub async fn extract_spec_compliance(
    app_state: State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<compliance::SpecComplianceResult, String> {
    compliance::extract_compliance(&app_state.pg_db, &task_run_id).await
}

// -- Broken assertion & attention commands ------------------------------------

#[tauri::command]
pub async fn detect_broken_spec_assertions(
    app_state: State<'_, Arc<AppState>>,
    spec_id: String,
) -> Result<Vec<compliance::BrokenAssertion>, String> {
    compliance::detect_broken_assertions(&app_state.pg_db, &spec_id).await
}

#[tauri::command]
pub async fn get_specs_needing_attention(
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<compliance::SpecAttentionItem>, String> {
    compliance::get_specs_needing_attention(&app_state.pg_db).await
}

// ── Accuracy commands ─────────────────────────────────────────────────

#[tauri::command]
pub async fn analyze_spec_element_coverage(
    app_state: State<'_, Arc<AppState>>,
    spec_id: String,
    spec_config: serde_json::Value,
    snapshot_elements: Vec<serde_json::Value>,
) -> Result<accuracy::ElementCoverageResult, String> {
    let result = accuracy::analyze_element_coverage(&spec_config, &snapshot_elements)?;

    // Store the result
    let detail_json = serde_json::to_string(&result).unwrap_or_else(|_| "{}".into());
    accuracy::store_accuracy_result(
        &app_state.pg_db,
        &spec_id,
        "element_coverage",
        result.element_coverage,
        &detail_json,
    )
    .await?;

    Ok(result)
}

#[tauri::command]
pub async fn analyze_cross_page_consistency(
    app_state: State<'_, Arc<AppState>>,
    specs: Vec<(String, serde_json::Value)>,
) -> Result<accuracy::CrossPageConsistencyResult, String> {
    let result = accuracy::analyze_cross_page_consistency(&specs);

    // Store per-spec results
    for spec_score in &result.per_spec_scores {
        let detail_json = serde_json::to_string(spec_score).unwrap_or_else(|_| "{}".into());
        let _ = accuracy::store_accuracy_result(
            &app_state.pg_db,
            &spec_score.spec_id,
            "cross_page",
            spec_score.score,
            &detail_json,
        )
        .await;
    }

    Ok(result)
}

#[tauri::command]
pub async fn run_spec_mutation_test(
    app_state: State<'_, Arc<AppState>>,
    spec_id: String,
    spec_config: serde_json::Value,
    snapshot_elements: Vec<serde_json::Value>,
) -> Result<accuracy::MutationTestResult, String> {
    let result = accuracy::run_mutation_test(&spec_config, &snapshot_elements)?;

    let detail_json = serde_json::to_string(&result).unwrap_or_else(|_| "{}".into());
    accuracy::store_accuracy_result(
        &app_state.pg_db,
        &spec_id,
        "mutation",
        result.mutation_score,
        &detail_json,
    )
    .await?;

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
pub async fn get_spec_accuracy_results(
    app_state: State<'_, Arc<AppState>>,
    spec_id: Option<String>,
    analysis_type: Option<String>,
) -> Result<Vec<accuracy::SpecAccuracyRecord>, String> {
    accuracy::get_accuracy_results(
        &app_state.pg_db,
        spec_id.as_deref(),
        analysis_type.as_deref(),
    )
    .await
}

// ── Versioning commands ───────────────────────────────────────────────

#[tauri::command]
pub async fn snapshot_current_spec(
    app_state: State<'_, Arc<AppState>>,
    spec_id: String,
    spec_json: String,
    change_summary: Option<String>,
    change_type: Option<String>,
) -> Result<versioning::SpecVersion, String> {
    versioning::snapshot_spec_version(
        &app_state.pg_db,
        &spec_id,
        &spec_json,
        change_summary.as_deref(),
        change_type.as_deref().unwrap_or("manual"),
    )
    .await
}

#[tauri::command]
pub async fn get_spec_version_history(
    app_state: State<'_, Arc<AppState>>,
    spec_id: String,
    limit: Option<i64>,
) -> Result<Vec<versioning::SpecVersion>, String> {
    versioning::get_version_history(&app_state.pg_db, &spec_id, limit).await
}

#[tauri::command]
pub async fn diff_spec_versions(
    app_state: State<'_, Arc<AppState>>,
    spec_id: String,
    from_version: i64,
    to_version: i64,
) -> Result<versioning::SpecDiff, String> {
    versioning::diff_spec_versions(&app_state.pg_db, &spec_id, from_version, to_version).await
}

#[tauri::command]
pub fn diff_spec_json(old_json: String, new_json: String) -> Result<versioning::SpecDiff, String> {
    versioning::diff_specs(&old_json, &new_json)
}
