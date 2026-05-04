//! Tauri commands for the UI Bridge regression subsystem.
//!
//! Persistence shell for the pure `diagnose` function in `ui-bridge-auto`.
//! These commands are invoked by the runner-side `MemorySink` (Phase A3) and
//! the regression panel (Phase C2) to persist suites, runs, diagnoses, and
//! per-assertion exercise rows.
//!
//! Implementation details: the underlying CRUD lives in
//! [`crate::database::pg::regression`] and uses raw `tokio_postgres` queries
//! (not Clorinde) until the alembic chain catches up with the
//! `regression_*` tables and `schema.pg.sql.generated` can be regenerated.

use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;
use tracing::info;
use uuid::Uuid;

use crate::commands::compartments::StorageCompartment;
use crate::database::pg::regression::{
    AssertionExecutionRow, DiagnosisRow, RunSummaryRow, SuiteFullRow, SuiteRow,
};

// =============================================================================
// Helpers
// =============================================================================

fn parse_uuid(field: &str, raw: &str) -> Result<Uuid, String> {
    Uuid::parse_str(raw).map_err(|e| format!("invalid uuid for {}: {}", field, e))
}

// =============================================================================
// Commands
// =============================================================================

/// Save a regression suite definition. Returns the new suite UUID as a
/// hyphenated string.
#[tauri::command]
pub async fn save_regression_suite(
    suite_json: serde_json::Value,
    ir_doc_id: String,
    storage: State<'_, StorageCompartment>,
) -> Result<String, String> {
    let id = storage
        .pg_db()
        .save_regression_suite(&ir_doc_id, &suite_json)
        .await?;
    info!("Saved regression suite id={} ir_doc_id={}", id, ir_doc_id);
    Ok(id.to_string())
}

/// Record a regression-run summary. Returns the new run UUID as a hyphenated
/// string. `drift_report_json` is optional — when supplied, the runner
/// `/runs/:run_id/drift` HTTP endpoint can later surface those entries to
/// the qontinui-web drift dashboard.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn record_regression_run(
    suite_id: String,
    run_id: String,
    passed: i32,
    failed: i32,
    started_at: String,
    completed_at: String,
    run_result_json: serde_json::Value,
    drift_report_json: Option<serde_json::Value>,
    storage: State<'_, StorageCompartment>,
) -> Result<String, String> {
    let suite_uuid = parse_uuid("suite_id", &suite_id)?;
    let id = storage
        .pg_db()
        .record_regression_run(
            suite_uuid,
            &run_id,
            passed,
            failed,
            &started_at,
            &completed_at,
            &run_result_json,
            drift_report_json.as_ref(),
        )
        .await?;
    info!(
        "Recorded regression run id={} suite_id={} passed={} failed={}",
        id, suite_id, passed, failed
    );
    Ok(id.to_string())
}

/// Record a self-diagnosis memo for a regression run.
#[tauri::command]
pub async fn record_regression_diagnosis(
    run_id: String,
    diagnosis_json: serde_json::Value,
    storage: State<'_, StorageCompartment>,
) -> Result<String, String> {
    let run_uuid = parse_uuid("run_id", &run_id)?;
    let id = storage
        .pg_db()
        .record_regression_diagnosis(run_uuid, &diagnosis_json)
        .await?;
    info!("Recorded regression diagnosis id={} run_id={}", id, run_id);
    Ok(id.to_string())
}

/// Batch-insert per-assertion execution rows. Each entry must be a JSON
/// object with at least `id`, `run_id`, `case_id`, `assertion_id`,
/// `assertion_kind`, `status`, `started_at`, and `duration_ms`. Returns the
/// number of rows inserted.
#[tauri::command]
pub async fn record_assertion_executions_batch(
    executions: Vec<serde_json::Value>,
    storage: State<'_, StorageCompartment>,
) -> Result<usize, String> {
    if executions.is_empty() {
        return Ok(0);
    }
    let arr = serde_json::Value::Array(executions);
    let inserted = storage
        .pg_db()
        .record_assertion_executions_batch(&arr)
        .await?;
    Ok(inserted as usize)
}

/// Query the per-assertion exercise log for a suite. Used by the Phase C2
/// panel to reconstruct case/assertion timelines.
#[tauri::command]
pub async fn query_assertion_executions_for_suite(
    suite_id: String,
    storage: State<'_, StorageCompartment>,
) -> Result<Vec<AssertionExecutionRow>, String> {
    let suite_uuid = parse_uuid("suite_id", &suite_id)?;
    storage
        .pg_db()
        .get_assertion_executions_for_suite(suite_uuid)
        .await
}

/// Recent diagnosis memos for a suite, in reverse-chronological order.
/// `limit` defaults to 25 when omitted.
#[tauri::command]
pub async fn get_recent_diagnoses_for_suite(
    suite_id: String,
    limit: Option<u32>,
    storage: State<'_, StorageCompartment>,
) -> Result<Vec<DiagnosisRow>, String> {
    let suite_uuid = parse_uuid("suite_id", &suite_id)?;
    storage
        .pg_db()
        .get_recent_diagnoses_for_suite(suite_uuid, limit)
        .await
}

/// Catalog of every persisted regression suite, newest first. `limit` caps
/// the returned rows; defaults to 50, hard cap 1000.
#[tauri::command]
pub async fn list_regression_suites(
    limit: Option<u32>,
    storage: State<'_, StorageCompartment>,
) -> Result<Vec<SuiteRow>, String> {
    storage.pg_db().list_regression_suites(limit).await
}

/// Fetch a single suite by id (with its serialized RegressionSuite blob).
/// Returns `None` if the suite is not in the catalog.
#[tauri::command]
pub async fn get_regression_suite_by_id(
    suite_id: String,
    storage: State<'_, StorageCompartment>,
) -> Result<Option<SuiteFullRow>, String> {
    let suite_uuid = parse_uuid("suite_id", &suite_id)?;
    storage.pg_db().get_regression_suite_by_id(suite_uuid).await
}

/// Recent runs for a suite (newest first). `limit` defaults to 25.
#[tauri::command]
pub async fn list_regression_runs_for_suite(
    suite_id: String,
    limit: Option<u32>,
    storage: State<'_, StorageCompartment>,
) -> Result<Vec<RunSummaryRow>, String> {
    let suite_uuid = parse_uuid("suite_id", &suite_id)?;
    storage
        .pg_db()
        .list_regression_runs_for_suite(suite_uuid, limit)
        .await
}

/// Build the Tauri plugin that registers this module's command handlers.
///
/// The runner currently uses the central `tauri::generate_handler![...]`
/// in `main.rs` for command registration, but the module-plugin scaffolding
/// is kept consistent with the rest of `commands/*` for future migration.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_regression")
        .invoke_handler(tauri::generate_handler![
            save_regression_suite,
            record_regression_run,
            record_regression_diagnosis,
            record_assertion_executions_batch,
            query_assertion_executions_for_suite,
            get_recent_diagnoses_for_suite,
            list_regression_suites,
            get_regression_suite_by_id,
            list_regression_runs_for_suite,
        ])
        .build()
}
