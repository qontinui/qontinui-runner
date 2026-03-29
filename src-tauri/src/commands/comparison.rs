//! Tauri commands for comparison runs.
//!
//! Thin wrappers that delegate to the HTTP API comparison_api module's
//! database operations, providing frontend access via `invoke()`.

use std::sync::Arc;

use tauri::State;

use crate::commands::AppState;
use crate::mcp::comparison_api::ComparisonEntryJson;

/// Start a comparison run. Returns the comparison_id.
#[tauri::command]
pub async fn start_comparison(
    app_state: State<'_, Arc<AppState>>,
    workflow_id: String,
    variation_type: String,
    use_worktree: Option<bool>,
) -> Result<String, String> {
    let use_wt = use_worktree.unwrap_or(true);

    // Build entries based on variation type
    let entries: Vec<ComparisonEntryJson> = match variation_type.as_str() {
        "architecture" => vec![
            ComparisonEntryJson {
                label: "Traditional".to_string(),
                overrides: serde_json::json!({
                    "workflow_architecture": "traditional",
                    "use_worktree": use_wt,
                }),
                task_run_id: None,
                status: "pending".to_string(),
                result: None,
            },
            ComparisonEntryJson {
                label: "Agentic Verification".to_string(),
                overrides: serde_json::json!({
                    "workflow_architecture": "agentic_verification",
                    "use_worktree": use_wt,
                }),
                task_run_id: None,
                status: "pending".to_string(),
                result: None,
            },
            ComparisonEntryJson {
                label: "Multi-Agent Pipeline".to_string(),
                overrides: serde_json::json!({
                    "workflow_architecture": "multi_agent_pipeline",
                    "use_worktree": use_wt,
                }),
                task_run_id: None,
                status: "pending".to_string(),
                result: None,
            },
        ],
        "same" => (0..3)
            .map(|i| ComparisonEntryJson {
                label: format!("Run {}", i + 1),
                overrides: serde_json::json!({ "use_worktree": use_wt }),
                task_run_id: None,
                status: "pending".to_string(),
                result: None,
            })
            .collect(),
        other => return Err(format!("Unknown variation_type: {}", other)),
    };

    let comparison_id = format!("cmp-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();
    let entries_json = serde_json::to_string(&entries).map_err(|e| e.to_string())?;

    let cid = comparison_id.clone();
    let wid = workflow_id.clone();
    let vtype = variation_type.clone();
    let ejs = entries_json.clone();
    let now_c = now.clone();

    app_state
        .pg_db
        .insert_comparison(&cid, &wid, &vtype, &ejs)
        .await?;

    // Launch runs via local HTTP API in background
    let db = app_state.checkpoint_db.clone();
    let pg_db_for_spawn = app_state.pg_db.clone();
    let api_port = app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);
    let comp_id = comparison_id.clone();

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
        tokio::spawn(async move {
            let _ = pg_db.update_comparison_entries(&comp_id_pg, &ejs_pg, &status_pg).await;
        });
    });

    Ok(comparison_id)
}

/// Get status of a comparison run, enriching entries with live task_run data.
#[tauri::command]
pub async fn get_comparison_status(
    app_state: State<'_, Arc<AppState>>,
    comparison_id: String,
) -> Result<serde_json::Value, String> {
    let db = &app_state.checkpoint_db;

    let cmp_json = app_state
        .pg_db
        .get_comparison(&comparison_id)
        .await?
        .ok_or_else(|| format!("Comparison not found: {}", comparison_id))?;

    let id = cmp_json["id"].as_str().unwrap_or("").to_string();
    let workflow_id = cmp_json["workflow_id"].as_str().unwrap_or("").to_string();
    let variation_type = cmp_json["variation_type"].as_str().unwrap_or("").to_string();
    let status = cmp_json["status"].as_str().unwrap_or("").to_string();
    let entries_json_str = cmp_json["entries_json"].as_str().unwrap_or("[]").to_string();
    let report: Option<String> = cmp_json["report"].as_str().map(|s| s.to_string());
    let created_at = cmp_json["created_at"].as_str().unwrap_or("").to_string();
    let completed_at: Option<String> = cmp_json["completed_at"].as_str().map(|s| s.to_string());

    let mut entries: Vec<ComparisonEntryJson> =
        serde_json::from_str(&entries_json_str).unwrap_or_default();

    // Enrich with live task_run statuses
    let mut all_done = true;
    for entry in entries.iter_mut() {
        if let Some(ref trid) = entry.task_run_id {
            if let Ok(Some(task_run)) = db.get_task_run(trid) {
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
        let _ = app_state
            .pg_db
            .complete_comparison(&id, &entries_str)
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
    }))
}

/// List recent comparison runs.
#[tauri::command]
pub async fn list_comparisons(
    app_state: State<'_, Arc<AppState>>,
) -> Result<Vec<serde_json::Value>, String> {
    app_state.pg_db.list_comparisons().await
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
