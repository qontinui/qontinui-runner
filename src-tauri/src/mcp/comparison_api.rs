//! Comparison Run HTTP endpoints.
//!
//! Launches the same workflow with different architectures side-by-side,
//! tracks progress, and returns results for comparison.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct StartComparisonRequest {
    pub workflow_id: String,
    /// "architecture" | "same" | "custom"
    #[serde(default = "default_variation_type")]
    pub variation_type: String,
    #[serde(default = "default_true")]
    pub use_worktree: bool,
    #[serde(default)]
    pub custom_overrides: Vec<serde_json::Value>,
}

fn default_variation_type() -> String {
    "architecture".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct StartComparisonResponse {
    pub comparison_id: String,
    pub entries_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonEntryJson {
    pub label: String,
    pub overrides: serde_json::Value,
    #[serde(default)]
    pub task_run_id: Option<String>,
    #[serde(default = "default_pending")]
    pub status: String,
    #[serde(default)]
    pub result: Option<ComparisonEntryResultJson>,
}

fn default_pending() -> String {
    "pending".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonEntryResultJson {
    pub success: bool,
    pub iterations: u32,
    pub duration_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct ComparisonRunView {
    pub id: String,
    pub workflow_id: String,
    pub variation_type: String,
    pub status: String,
    pub entries: Vec<ComparisonEntryJson>,
    pub report: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /comparison/start — launch a new comparison run.
pub async fn start_comparison(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<StartComparisonRequest>,
) -> Result<Json<ApiResponse<StartComparisonResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "Starting comparison run for workflow={} variation={}",
        req.workflow_id, req.variation_type
    );

    // Verify workflow exists
    match state
        .app_state
        .checkpoint_db
        .get_unified_workflow(&req.workflow_id)
    {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!(
                    "Workflow not found: {}",
                    req.workflow_id
                ))),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to fetch workflow: {}", e))),
            ));
        }
    };

    // Build entries based on variation type
    let entries: Vec<ComparisonEntryJson> = match req.variation_type.as_str() {
        "architecture" => vec![
            ComparisonEntryJson {
                label: "Traditional".to_string(),
                overrides: serde_json::json!({
                    "workflow_architecture": "traditional",
                    "use_worktree": req.use_worktree,
                }),
                task_run_id: None,
                status: "pending".to_string(),
                result: None,
            },
            ComparisonEntryJson {
                label: "Agentic Verification".to_string(),
                overrides: serde_json::json!({
                    "workflow_architecture": "agentic_verification",
                    "use_worktree": req.use_worktree,
                }),
                task_run_id: None,
                status: "pending".to_string(),
                result: None,
            },
            ComparisonEntryJson {
                label: "Multi-Agent Pipeline".to_string(),
                overrides: serde_json::json!({
                    "workflow_architecture": "multi_agent_pipeline",
                    "use_worktree": req.use_worktree,
                }),
                task_run_id: None,
                status: "pending".to_string(),
                result: None,
            },
        ],
        "same" => {
            // 3 identical runs to test repeatability
            (0..3)
                .map(|i| ComparisonEntryJson {
                    label: format!("Run {}", i + 1),
                    overrides: serde_json::json!({
                        "use_worktree": req.use_worktree,
                    }),
                    task_run_id: None,
                    status: "pending".to_string(),
                    result: None,
                })
                .collect()
        }
        "custom" => req
            .custom_overrides
            .iter()
            .enumerate()
            .map(|(i, ov)| ComparisonEntryJson {
                label: ov
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&format!("Custom {}", i + 1))
                    .to_string(),
                overrides: {
                    let mut merged = ov.clone();
                    if let Some(obj) = merged.as_object_mut() {
                        obj.insert(
                            "use_worktree".to_string(),
                            serde_json::Value::Bool(req.use_worktree),
                        );
                    }
                    merged
                },
                task_run_id: None,
                status: "pending".to_string(),
                result: None,
            })
            .collect(),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "Unknown variation_type: {}",
                    req.variation_type
                ))),
            ));
        }
    };

    let comparison_id = format!("cmp-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();
    let entries_count = entries.len();
    let entries_json_str = serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string());

    // Insert into database
    let cid = comparison_id.clone();
    let wid = req.workflow_id.clone();
    let vtype = req.variation_type.clone();
    let now_clone = now.clone();
    let ejs = entries_json_str.clone();

    state
        .app_state
        .checkpoint_db
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO comparison_runs (id, workflow_id, variation_type, status, entries_json, created_at) \
                 VALUES (?1, ?2, ?3, 'running', ?4, ?5)",
                params![cid, wid, vtype, ejs, now_clone],
            )
            .map_err(|e| format!("Failed to create comparison run: {}", e))?;
            Ok(())
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e)),
            )
        })?;

    // Spawn background task to launch all the workflow runs
    let db = state.app_state.checkpoint_db.clone();
    let api_port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);
    let workflow_id = req.workflow_id.clone();
    let comp_id = comparison_id.clone();

    tokio::spawn(async move {
        launch_comparison_entries(db, api_port, &workflow_id, &comp_id, entries).await;
    });

    Ok(Json(ApiResponse::success(StartComparisonResponse {
        comparison_id,
        entries_count,
    })))
}

/// Background task: launches each entry's workflow run via local HTTP API.
async fn launch_comparison_entries(
    db: Arc<crate::database::CheckpointDb>,
    api_port: u16,
    workflow_id: &str,
    comparison_id: &str,
    mut entries: Vec<ComparisonEntryJson>,
) {
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", api_port);

    for entry in entries.iter_mut() {
        let url = format!("{}/unified-workflows/{}/run", base, workflow_id);
        let body = serde_json::json!({
            "force_fresh_start": true,
            "overrides": entry.overrides,
        });

        match client.post(&url).json(&body).send().await {
            Ok(resp) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(task_run_id) = json
                        .get("data")
                        .and_then(|d| d.get("task_run_id"))
                        .and_then(|v| v.as_str())
                    {
                        entry.task_run_id = Some(task_run_id.to_string());
                        entry.status = "running".to_string();
                        info!(
                            "Comparison {}: launched entry '{}' -> task_run_id={}",
                            comparison_id, entry.label, task_run_id
                        );
                    } else {
                        entry.status = "failed".to_string();
                        warn!(
                            "Comparison {}: entry '{}' launch returned unexpected JSON: {:?}",
                            comparison_id, entry.label, json
                        );
                    }
                } else {
                    entry.status = "failed".to_string();
                    warn!(
                        "Comparison {}: entry '{}' response parse failed",
                        comparison_id, entry.label
                    );
                }
            }
            Err(e) => {
                entry.status = "failed".to_string();
                error!(
                    "Comparison {}: failed to launch entry '{}': {}",
                    comparison_id, entry.label, e
                );
            }
        }
    }

    // Update entries in database with task_run_ids
    let entries_json = serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string());
    let all_failed = entries.iter().all(|e| e.status == "failed");
    let new_status = if all_failed { "failed" } else { "running" };

    let _ = db.with_conn(|conn| {
        conn.execute(
            "UPDATE comparison_runs SET entries_json = ?1, status = ?2 WHERE id = ?3",
            params![entries_json, new_status, comparison_id],
        )
        .map_err(|e| format!("Failed to update comparison entries: {}", e))?;
        Ok(())
    });
}

/// GET /comparison/:id — get comparison run with live entry statuses.
pub async fn get_comparison(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<ComparisonRunView>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = &state.app_state.checkpoint_db;

    let row = db
        .with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, workflow_id, variation_type, status, entries_json, report, created_at, completed_at \
                     FROM comparison_runs WHERE id = ?1",
                )
                .map_err(|e| format!("Prepare failed: {}", e))?;

            let result = stmt
                .query_row(params![id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                })
                .map_err(|e| format!("Comparison run not found: {}", e))?;
            Ok(result)
        })
        .map_err(|e| (StatusCode::NOT_FOUND, Json(api_error(e))))?;

    let (
        id,
        workflow_id,
        variation_type,
        status,
        entries_json_str,
        report,
        created_at,
        completed_at,
    ) = row;

    let mut entries: Vec<ComparisonEntryJson> =
        serde_json::from_str(&entries_json_str).unwrap_or_default();

    // Enrich entries with live task_run status
    let mut all_done = true;
    let mut any_running = false;
    for entry in entries.iter_mut() {
        if let Some(ref trid) = entry.task_run_id {
            if let Ok(Some(task_run)) = db.get_task_run(trid) {
                match task_run.status.as_str() {
                    "complete" => {
                        entry.status = "completed".to_string();
                        // Extract metrics from task_run
                        entry.result = Some(ComparisonEntryResultJson {
                            success: true,
                            iterations: task_run.sessions_count,
                            duration_ms: calculate_duration_ms(
                                &task_run.created_at,
                                task_run.completed_at.as_deref(),
                            ),
                        });
                    }
                    "failed" | "stopped" => {
                        entry.status = "failed".to_string();
                        entry.result = Some(ComparisonEntryResultJson {
                            success: false,
                            iterations: task_run.sessions_count,
                            duration_ms: calculate_duration_ms(
                                &task_run.created_at,
                                task_run.completed_at.as_deref(),
                            ),
                        });
                    }
                    _ => {
                        entry.status = "running".to_string();
                        all_done = false;
                        any_running = true;
                    }
                }
            }
        } else if entry.status == "pending" {
            all_done = false;
        }
    }

    // Update comparison status if all entries are done
    let final_status = if all_done && status == "running" {
        let new_status = "completed";
        let now = chrono::Utc::now().to_rfc3339();
        let entries_str = serde_json::to_string(&entries).unwrap_or_default();
        let _ = db.with_conn(|conn| {
            conn.execute(
                "UPDATE comparison_runs SET status = ?1, completed_at = ?2, entries_json = ?3 WHERE id = ?4",
                params![new_status, now, entries_str, id],
            )
            .map_err(|e| format!("{}", e))?;
            Ok(())
        });
        new_status.to_string()
    } else if any_running {
        // Also persist the updated entries_json with live statuses
        let entries_str = serde_json::to_string(&entries).unwrap_or_default();
        let _ = db.with_conn(|conn| {
            conn.execute(
                "UPDATE comparison_runs SET entries_json = ?1 WHERE id = ?2",
                params![entries_str, id],
            )
            .map_err(|e| format!("{}", e))?;
            Ok(())
        });
        status
    } else {
        status
    };

    Ok(Json(ApiResponse::success(ComparisonRunView {
        id,
        workflow_id,
        variation_type,
        status: final_status,
        entries,
        report,
        created_at,
        completed_at,
    })))
}

/// GET /comparisons — list recent comparison runs.
pub async fn list_comparisons(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<ComparisonRunView>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = &state.app_state.checkpoint_db;

    let rows = db
        .with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, workflow_id, variation_type, status, entries_json, report, created_at, completed_at \
                     FROM comparison_runs ORDER BY created_at DESC LIMIT 50",
                )
                .map_err(|e| format!("Prepare failed: {}", e))?;

            let results = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                })
                .map_err(|e| format!("Query failed: {}", e))?
                .filter_map(|r| r.ok())
                .map(
                    |(id, workflow_id, variation_type, status, entries_json_str, report, created_at, completed_at)| {
                        let entries: Vec<ComparisonEntryJson> =
                            serde_json::from_str(&entries_json_str).unwrap_or_default();
                        ComparisonRunView {
                            id,
                            workflow_id,
                            variation_type,
                            status,
                            entries,
                            report,
                            created_at,
                            completed_at,
                        }
                    },
                )
                .collect::<Vec<_>>();
            Ok(results)
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e)),
            )
        })?;

    Ok(Json(ApiResponse::success(rows)))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn calculate_duration_ms(created_at: &str, completed_at: Option<&str>) -> u64 {
    let start = chrono::DateTime::parse_from_rfc3339(created_at).ok();
    let end = completed_at
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .or_else(|| Some(chrono::Utc::now().fixed_offset()));

    match (start, end) {
        (Some(s), Some(e)) => {
            let dur = e.signed_duration_since(s);
            dur.num_milliseconds().max(0) as u64
        }
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Route registration
// ---------------------------------------------------------------------------

/// Register comparison API routes.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};

    axum::Router::new()
        .route("/comparison/start", post(start_comparison))
        .route("/comparison/{id}", get(get_comparison))
        .route("/comparisons", get(list_comparisons))
}
