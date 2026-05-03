//! `/plans/decompose` HTTP endpoint — Phase 1 stub.
//!
//! Accepts a decomposed-plan payload (see productivity-stack plan §4 step 6),
//! transactionally inserts the `plans` row plus every `tasks` row with their
//! resolved `depends_on` UUIDs, then populates the in-memory
//! [`UpcomingFileRegistry`] so the Coordinator can see the new claims
//! immediately.
//!
//! Phase 1 is intentionally a thin pass-through: the slash-command does the
//! Markdown parsing, hash computation, and dependency inference. This
//! endpoint only stores what it's given.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::mcp::types::ApiState;

// =============================================================================
// Request / response shapes
// =============================================================================

/// Single task within a decompose-plan request. `depends_on_indices` are
/// indices into the parent payload's `tasks` array so the slash-command
/// can express dependencies before the DB allocates UUIDs.
#[derive(Debug, Clone, Deserialize)]
pub struct DecomposeTaskInput {
    pub phase_name: String,
    pub sequence_in_phase: i32,
    pub description: String,
    #[serde(default)]
    pub expected_file_claims: Vec<String>,
    #[serde(default)]
    pub expected_dirs: Vec<String>,
    #[serde(default)]
    pub depends_on_indices: Vec<usize>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DecomposePlanRequest {
    pub markdown_path: String,
    pub version_hash: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default = "default_status")]
    pub status: String,
    pub tasks: Vec<DecomposeTaskInput>,
}

fn default_status() -> String {
    "decomposed".to_string()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecomposeTaskResult {
    pub index: usize,
    pub task_id: String,
    pub phase_name: String,
    pub sequence_in_phase: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecomposePlanResponse {
    pub plan_id: String,
    pub plan_version_hash: String,
    pub tasks: Vec<DecomposeTaskResult>,
    pub upcoming_claims_registered: usize,
}

// =============================================================================
// Handler
// =============================================================================

/// `POST /plans/decompose` — see productivity-stack plan §4 step 6.
async fn decompose_plan(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<DecomposePlanRequest>,
) -> Result<Json<DecomposePlanResponse>, (StatusCode, String)> {
    if req.markdown_path.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "markdown_path is required".to_string(),
        ));
    }
    if req.version_hash.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "version_hash is required".to_string(),
        ));
    }

    // Validate every depends_on_index points to a slot earlier than its
    // owner. Doing this up-front means a bad payload fails before any DB
    // work happens.
    for (i, t) in req.tasks.iter().enumerate() {
        for &dep in &t.depends_on_indices {
            if dep >= req.tasks.len() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("task[{}].depends_on_indices[{}] is out of range", i, dep),
                ));
            }
            if dep == i {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("task[{}] cannot depend on itself", i),
                ));
            }
        }
    }

    let pg = state.app_state.pg_db.clone();
    let mut conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("PG pool: {}", e)))?;
    let txn = conn
        .transaction()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("PG txn: {}", e)))?;

    // 1. UPSERT plan: re-running /decompose-plan against an existing path
    //    refreshes the version_hash and metadata in place. Cascade behaviour
    //    on the old tasks is handled by the slash command (it calls drop on
    //    the registry first); we replace tasks atomically below by deleting
    //    any pre-existing rows for this plan within the same txn.
    let plan_row = txn
        .query_one(
            r#"
            INSERT INTO coord.plans (markdown_path, version_hash, status, title, summary)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (markdown_path) DO UPDATE
                SET version_hash = EXCLUDED.version_hash,
                    status       = EXCLUDED.status,
                    title        = COALESCE(EXCLUDED.title, coord.plans.title),
                    summary      = COALESCE(EXCLUDED.summary, coord.plans.summary),
                    updated_at   = NOW()
            RETURNING id::text, version_hash
            "#,
            &[
                &req.markdown_path,
                &req.version_hash,
                &req.status,
                &req.title,
                &req.summary,
            ],
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("plans upsert failed: {}", e),
            )
        })?;
    let plan_id: String = plan_row.get(0);
    let plan_version_hash: String = plan_row.get(1);
    // tokio-postgres needs a Uuid-typed parameter for UUID columns; the
    // RETURNING id::text on the upsert hands back a String, so parse once
    // here and reuse for every $1::uuid below. Passing the String directly
    // failed with "error serializing parameter 0" because String::to_sql
    // refuses to encode for UUID type.
    let plan_uuid: Uuid = plan_id.parse().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("plan_id is not a valid UUID: {}", e),
        )
    })?;

    // 2. Replace tasks for this plan. The cascade-on-delete also removes
    //    any review/snapshot rows once those tables exist (later phases).
    txn.execute(
        "DELETE FROM coord.tasks WHERE plan_id = $1::uuid",
        &[&plan_uuid],
    )
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("delete prior tasks failed: {}", e),
        )
    })?;

    // 3. Insert tasks in two passes so depends_on_indices can be resolved
    //    against the previously-allocated UUIDs.
    let mut allocated_ids: Vec<String> = Vec::with_capacity(req.tasks.len());
    for (i, t) in req.tasks.iter().enumerate() {
        // Stage 1: insert with empty depends_on; capture the new UUID.
        let row = txn
            .query_one(
                r#"
                INSERT INTO coord.tasks (
                    plan_id, plan_version_hash, phase_name, sequence_in_phase,
                    description, expected_file_claims, expected_dirs,
                    depends_on, status, notes
                )
                VALUES ($1::uuid, $2, $3, $4, $5, $6, $7, '{}'::uuid[], 'pending', $8)
                RETURNING id::text
                "#,
                &[
                    &plan_uuid,
                    &plan_version_hash,
                    &t.phase_name,
                    &t.sequence_in_phase,
                    &t.description,
                    &t.expected_file_claims,
                    &t.expected_dirs,
                    &t.notes,
                ],
            )
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("task[{}] insert failed: {}", i, e),
                )
            })?;
        allocated_ids.push(row.get(0));
    }

    // Stage 2: stitch depends_on UUIDs and update each row that has any.
    for (i, t) in req.tasks.iter().enumerate() {
        if t.depends_on_indices.is_empty() {
            continue;
        }
        let dep_ids: Vec<String> = t
            .depends_on_indices
            .iter()
            .map(|&d| allocated_ids[d].clone())
            .collect();
        let task_uuid: Uuid = allocated_ids[i].parse().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("task[{}] id is not a valid UUID: {}", i, e),
            )
        })?;
        txn.execute(
            r#"
            UPDATE coord.tasks
            SET depends_on = (
                SELECT COALESCE(array_agg(d::uuid), '{}'::uuid[])
                FROM unnest($2::text[]) d
            ),
                updated_at = NOW()
            WHERE id = $1::uuid
            "#,
            &[&task_uuid, &dep_ids],
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("task[{}] depends_on update failed: {}", i, e),
            )
        })?;
    }

    // 4. Tasks whose depends_on is empty are immediately ready. Same SQL
    //    used elsewhere by mark_ready_for_unblocked, scoped to this plan.
    txn.execute(
        r#"
        UPDATE coord.tasks
        SET status = 'ready', updated_at = NOW()
        WHERE plan_id = $1::uuid
          AND status = 'pending'
          AND cardinality(depends_on) = 0
        "#,
        &[&plan_uuid],
    )
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("ready-flip failed: {}", e),
        )
    })?;

    txn.commit().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("PG commit failed: {}", e),
        )
    })?;

    // 5. Replace upcoming-claim entries for this plan in the in-memory
    //    registry. Drop-then-readd is safe because we hold no other locks
    //    on the registry across these calls.
    let registry = &state.app_state.upcoming_file_registry;
    registry.drop_plan_claims(&plan_id).await;
    let mut total_claims = 0usize;
    let mut task_results = Vec::with_capacity(req.tasks.len());
    for (i, t) in req.tasks.iter().enumerate() {
        let task_id = &allocated_ids[i];
        if !t.expected_file_claims.is_empty() {
            registry
                .add_task_claims(
                    &plan_id,
                    task_id,
                    &plan_version_hash,
                    &t.expected_file_claims,
                )
                .await;
            total_claims += t.expected_file_claims.len();
        }
        task_results.push(DecomposeTaskResult {
            index: i,
            task_id: task_id.clone(),
            phase_name: t.phase_name.clone(),
            sequence_in_phase: t.sequence_in_phase,
        });
    }

    info!(
        "Decomposed plan {} ({}) into {} task(s), {} upcoming claim(s)",
        plan_id,
        req.markdown_path,
        req.tasks.len(),
        total_claims
    );

    if total_claims == 0 && !req.tasks.is_empty() {
        warn!(
            "Plan {} decomposed without any expected_file_claims — Coordinator pre-conflict layer is empty",
            plan_id
        );
    }

    Ok(Json(DecomposePlanResponse {
        plan_id,
        plan_version_hash,
        tasks: task_results,
        upcoming_claims_registered: total_claims,
    }))
}

// =============================================================================
// Routes
// =============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/plans/decompose", post(decompose_plan))
}
