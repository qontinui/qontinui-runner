//! Tauri commands for comparison runs.
//!
//! Thin wrappers over `database::pg::comparison` — the single persistence path
//! for `project.comparison_runs` — providing frontend access via `invoke()`.
//! This module used to call a *second*, near-identical set of DB functions in
//! `database::pg::tiered_info`; those are gone, so the desktop surface and the
//! HTTP surface now record identical rows, axis facts included.
//!
//! Migrated to StorageCompartment + HealthCompartment (Workstream C).

use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;

use crate::commands::compartments::{HealthCompartment, StorageCompartment};
use crate::mcp::comparison_api::ComparisonEntryJson;

/// Start a comparison run. Returns the comparison_id.
#[tauri::command]
pub async fn start_comparison(
    storage: State<'_, StorageCompartment>,
    health: State<'_, HealthCompartment>,
    workflow_id: String,
    variation_type: String,
    use_worktree: Option<bool>,
    // Only meaningful for `variation_type = "custom"`. Present so the desktop
    // UI can express everything the HTTP route can.
    custom_overrides: Option<Vec<serde_json::Value>>,
    // Only meaningful for `variation_type = "model"`.
    models: Option<Vec<String>>,
    // Only meaningful for `variation_type = "context_tokens"`.
    context_token_limits: Option<Vec<usize>>,
) -> Result<String, String> {
    let use_wt = use_worktree.unwrap_or(true);

    // Build entries from the TYPED variation — the same single derivation path
    // the HTTP surface uses. This command used to carry its own string match
    // that handled only "architecture" | "same" and rejected "custom", so the
    // very same variation_type was accepted over HTTP and refused from the
    // desktop UI. It now accepts exactly what the HTTP route accepts.
    let variation = crate::comparison::parse_variation(
        &variation_type,
        crate::comparison::VariationArgs {
            custom_overrides: custom_overrides.unwrap_or_default(),
            models: models.unwrap_or_default(),
            context_token_limits: context_token_limits.unwrap_or_default(),
        },
    )?;
    let entries: Vec<ComparisonEntryJson> =
        crate::comparison::build_comparison_arms(&variation, 3, use_wt)
            .into_iter()
            .map(|arm| ComparisonEntryJson {
                label: arm.label,
                overrides: arm.overrides,
                task_run_id: None,
                status: "pending".to_string(),
                result: None,
            })
            .collect();

    let comparison_id = format!("cmp-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();
    let entries_json = serde_json::to_string(&entries).map_err(|e| e.to_string())?;

    let cid = comparison_id.clone();
    let wid = workflow_id.clone();
    let vtype = variation_type.clone();
    let ejs = entries_json.clone();
    let now_c = now.clone();

    storage
        .pg_db()
        .create_comparison_run(&cid, &wid, &vtype, &ejs, &now_c)
        .await?;

    // Launch runs via local HTTP API in background
    let pg_db_for_spawn = storage.pg_db().clone();
    let api_port = health.api_port().load(std::sync::atomic::Ordering::Relaxed);
    let comp_id = comparison_id.clone();
    let vtype_for_spawn = variation_type.clone();

    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let base = format!("http://127.0.0.1:{}", api_port);
        let mut updated_entries = entries;

        for entry in updated_entries.iter_mut() {
            let url = format!("{}/unified-workflows/{}/run", base, workflow_id);
            let body = serde_json::json!({
                "force_fresh_start": true,
                "overrides": entry.overrides,
            });

            match client.post(&url).json(&body).send().await {
                Ok(resp) => {
                    if let Ok(json) = resp.json::<serde_json::Value>().await {
                        if let Some(trid) = json
                            .get("data")
                            .and_then(|d| d.get("task_run_id"))
                            .and_then(|v| v.as_str())
                        {
                            entry.task_run_id = Some(trid.to_string());
                            entry.status = "running".to_string();
                        } else {
                            entry.status = "failed".to_string();
                        }
                    } else {
                        entry.status = "failed".to_string();
                    }
                }
                Err(_) => {
                    entry.status = "failed".to_string();
                }
            }
        }

        // Persist updated entries
        let ejs = serde_json::to_string(&updated_entries).unwrap_or_default();
        let all_failed = updated_entries.iter().all(|e| e.status == "failed");
        let new_status = if all_failed { "failed" } else { "running" };
        let pg_db = pg_db_for_spawn.clone();
        let comp_id_pg = comp_id.clone();
        let ejs_pg = ejs.clone();
        let status_pg = new_status.to_string();
        let vtype_pg = vtype_for_spawn.clone();
        tokio::spawn(async move {
            let _ = pg_db
                .update_comparison_run_entries(&comp_id_pg, &vtype_pg, &ejs_pg, &status_pg)
                .await;
        });
    });

    Ok(comparison_id)
}

/// Get status of a comparison run, enriching entries with live task_run data.
#[tauri::command]
pub async fn get_comparison_status(
    storage: State<'_, StorageCompartment>,
    comparison_id: String,
) -> Result<serde_json::Value, String> {
    let row = storage
        .pg_db()
        .get_comparison_run(&comparison_id)
        .await?
        .ok_or_else(|| format!("Comparison not found: {}", comparison_id))?;

    let id = row.id;
    let workflow_id = row.workflow_id;
    let variation_type = row.variation_type;
    let status = row.status;
    let report = row.report;
    let created_at = row.created_at;
    let completed_at = row.completed_at;
    let computed_axis = row.computed_axis;
    let axis_drift_class = row.axis_drift_class;

    let mut entries: Vec<ComparisonEntryJson> =
        serde_json::from_str(&row.entries_json).unwrap_or_default();

    // Enrich with live task_run statuses
    let mut all_done = true;
    for entry in entries.iter_mut() {
        if let Some(ref trid) = entry.task_run_id {
            if let Ok(Some(task_run)) = storage.pg_db().get_task_run(trid).await {
                match task_run.status.as_str() {
                    "complete" => {
                        entry.status = "completed".to_string();
                        let dur = calculate_duration(
                            &task_run.created_at,
                            task_run.completed_at.as_deref(),
                        );
                        entry.result =
                            Some(crate::mcp::comparison_api::ComparisonEntryResultJson {
                                success: true,
                                iterations: task_run.sessions_count,
                                duration_ms: dur,
                            });
                    }
                    "failed" | "stopped" => {
                        entry.status = "failed".to_string();
                        let dur = calculate_duration(
                            &task_run.created_at,
                            task_run.completed_at.as_deref(),
                        );
                        entry.result =
                            Some(crate::mcp::comparison_api::ComparisonEntryResultJson {
                                success: false,
                                iterations: task_run.sessions_count,
                                duration_ms: dur,
                            });
                    }
                    _ => {
                        entry.status = "running".to_string();
                        all_done = false;
                    }
                }
            }
        } else if entry.status == "pending" {
            all_done = false;
        }
    }

    // Auto-complete if all done
    let final_status = if all_done && status == "running" {
        let entries_str = serde_json::to_string(&entries).unwrap_or_default();
        let _ = storage
            .pg_db()
            .complete_comparison_run(&id, &variation_type, &entries_str)
            .await;
        "completed".to_string()
    } else {
        status
    };

    Ok(serde_json::json!({
        "id": id,
        "workflow_id": workflow_id,
        "variation_type": variation_type,
        "status": final_status,
        "entries": entries,
        "report": report,
        "created_at": created_at,
        "completed_at": completed_at,
        // The DECLARED `variation_type` above and these two are the pair. Null
        // `computed_axis` means never computed, NOT "nothing differed".
        "computed_axis": computed_axis,
        "axis_drift_class": axis_drift_class,
    }))
}

/// List recent comparison runs.
#[tauri::command]
pub async fn list_comparisons(
    storage: State<'_, StorageCompartment>,
) -> Result<Vec<serde_json::Value>, String> {
    let rows = storage.pg_db().list_comparison_runs(50i64).await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let entries: serde_json::Value =
                serde_json::from_str(&r.entries_json).unwrap_or_default();
            serde_json::json!({
                "id": r.id,
                "workflow_id": r.workflow_id,
                "variation_type": r.variation_type,
                "status": r.status,
                "entries": entries,
                "report": r.report,
                "created_at": r.created_at,
                "completed_at": r.completed_at,
                "computed_axis": r.computed_axis,
                "axis_drift_class": r.axis_drift_class,
            })
        })
        .collect())
}

fn calculate_duration(created_at: &str, completed_at: Option<&str>) -> u64 {
    let start = chrono::DateTime::parse_from_rfc3339(created_at).ok();
    let end = completed_at
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .or_else(|| Some(chrono::Utc::now().fixed_offset()));
    match (start, end) {
        (Some(s), Some(e)) => e.signed_duration_since(s).num_milliseconds().max(0) as u64,
        _ => 0,
    }
}

/// Build the Tauri plugin that registers this module's command handlers.
///
/// See `commands/mod.rs` for the migration guide explaining the plugin pattern.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_comparison")
        .invoke_handler(tauri::generate_handler![
            start_comparison,
            get_comparison_status,
            list_comparisons,
        ])
        .build()
}
