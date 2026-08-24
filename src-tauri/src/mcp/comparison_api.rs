//! Comparison Run HTTP endpoints.
//!
//! Launches the same workflow with different architectures side-by-side,
//! tracks progress, and returns results for comparison.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
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
    /// `architecture` | `same` | `multi_agent` | `model` | `context_tokens` |
    /// `custom` — the tokens `comparison::parse_variation` accepts, which are
    /// exactly the ones `comparison::declared_axes` can classify.
    #[serde(default = "default_variation_type")]
    pub variation_type: String,
    #[serde(default = "default_true")]
    pub use_worktree: bool,
    /// Only read for `variation_type = "custom"`.
    #[serde(default)]
    pub custom_overrides: Vec<serde_json::Value>,
    /// Only read for `variation_type = "model"`.
    #[serde(default)]
    pub models: Vec<String>,
    /// Only read for `variation_type = "context_tokens"`.
    #[serde(default)]
    pub context_token_limits: Vec<usize>,
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
    /// What the run *declared* would vary between its arms.
    pub variation_type: String,
    pub status: String,
    pub entries: Vec<ComparisonEntryJson>,
    pub report: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
    /// What actually varied: the observed key paths.
    ///
    /// `null` means the axis was **never computed** — a row written before the
    /// runner recorded it. It does not mean "nothing varied"; that is `[]`.
    pub computed_axis: Option<serde_json::Value>,
    /// The declared-vs-actual classification (`none`, `in_place`, `pending`,
    /// `benign_add`, `active_negation`, `divergent`, `unknown`). `unknown` is a
    /// coverage gap, never agreement.
    pub axis_drift_class: String,
}

impl ComparisonRunView {
    fn from_row(
        row: crate::database::pg::comparison::ComparisonRunRow,
        entries: Vec<ComparisonEntryJson>,
        status: String,
    ) -> Self {
        // Project through the parser rather than echoing the stored bytes: a
        // token this build does not know reads out as `unknown` — a coverage
        // gap — instead of leaking an unclassifiable string to the client.
        let axis_drift_class = row.axis_drift().as_wire_str().to_string();
        ComparisonRunView {
            id: row.id,
            workflow_id: row.workflow_id,
            variation_type: row.variation_type,
            status,
            entries,
            report: row.report,
            created_at: row.created_at,
            completed_at: row.completed_at,
            computed_axis: row.computed_axis,
            axis_drift_class,
        }
    }
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

    // Verify workflow exists (PG)
    match state
        .app_state
        .pg_db
        .get_unified_workflow(&req.workflow_id)
        .await
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

    // Build entries from the TYPED variation — the single derivation path in
    // `crate::comparison`. This route used to hand-roll a string match here, and
    // `commands::comparison` hand-rolled a second, narrower one; both are gone.
    let variation_args = crate::comparison::VariationArgs {
        custom_overrides: req.custom_overrides.clone(),
        models: req.models.clone(),
        context_token_limits: req.context_token_limits.clone(),
    };
    let variation = match crate::comparison::parse_variation(&req.variation_type, variation_args) {
        Ok(v) => v,
        Err(e) => {
            return Err((StatusCode::BAD_REQUEST, Json(api_error(e))));
        }
    };
    let entries: Vec<ComparisonEntryJson> =
        crate::comparison::build_comparison_arms(&variation, 3, req.use_worktree)
            .into_iter()
            .map(|arm| ComparisonEntryJson {
                label: arm.label,
                overrides: arm.overrides,
                task_run_id: None,
                status: "pending".to_string(),
                result: None,
            })
            .collect();

    // The observed half of the declared-vs-actual pair, recorded at the only
    // moment it is decided: nothing downstream rewrites an arm's `overrides`.
    let observed = crate::comparison::observe_treatment_axis(
        &req.variation_type,
        &entries
            .iter()
            .map(|e| e.overrides.clone())
            .collect::<Vec<_>>(),
    );

    let comparison_id = format!("cmp-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now();
    let entries_count = entries.len();
    let entries_json_str = serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string());

    // Insert into database
    state
        .app_state
        .pg_db
        .create_comparison_run(
            &comparison_id,
            &req.workflow_id,
            &req.variation_type,
            &entries_json_str,
            now,
            Some(&observed.computed_axis),
            observed.drift_class_token(),
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    // Spawn background task to launch all the workflow runs
    let pg_db = state.app_state.pg_db.clone();
    let api_port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);
    let workflow_id = req.workflow_id.clone();
    let comp_id = comparison_id.clone();

    tokio::spawn(async move {
        launch_comparison_entries(pg_db, api_port, &workflow_id, &comp_id, entries).await;
    });

    Ok(Json(ApiResponse::success(StartComparisonResponse {
        comparison_id,
        entries_count,
    })))
}

/// Background task: launches each entry's workflow run via local HTTP API.
async fn launch_comparison_entries(
    pg_db: Arc<crate::database::pg::PgDb>,
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

    let _ = pg_db
        .update_comparison_run_entries(comparison_id, &entries_json, new_status)
        .await;
}

/// GET /comparison/:id — get comparison run with live entry statuses.
pub async fn get_comparison(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<ComparisonRunView>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;

    let row = pg
        .get_comparison_run(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Comparison run not found: {}", id))),
            )
        })?;

    let status = row.status.clone();
    let mut entries: Vec<ComparisonEntryJson> =
        serde_json::from_str(&row.entries_json).unwrap_or_default();

    // Enrich entries with live task_run status
    let mut all_done = true;
    let mut any_running = false;
    for entry in entries.iter_mut() {
        if let Some(ref trid) = entry.task_run_id {
            if let Ok(Some(task_run)) = state.app_state.pg_db.get_task_run(trid).await {
                match task_run.status.as_str() {
                    "complete" => {
                        entry.status = "completed".to_string();
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
        let entries_str = serde_json::to_string(&entries).unwrap_or_default();
        let _ = pg.complete_comparison_run(&row.id, &entries_str).await;
        "completed".to_string()
    } else if any_running {
        let entries_str = serde_json::to_string(&entries).unwrap_or_default();
        let _ = pg
            .update_comparison_run_entries(&row.id, &entries_str, &status)
            .await;
        status
    } else {
        status
    };

    Ok(Json(ApiResponse::success(ComparisonRunView::from_row(
        row,
        entries,
        final_status,
    ))))
}

/// GET /comparisons — list recent comparison runs.
pub async fn list_comparisons(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<ComparisonRunView>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let rows = state
        .app_state
        .pg_db
        .list_comparison_runs(50i64)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    let views = rows
        .into_iter()
        .map(|r| {
            let entries: Vec<ComparisonEntryJson> =
                serde_json::from_str(&r.entries_json).unwrap_or_default();
            let status = r.status.clone();
            ComparisonRunView::from_row(r, entries, status)
        })
        .collect();

    Ok(Json(ApiResponse::success(views)))
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
