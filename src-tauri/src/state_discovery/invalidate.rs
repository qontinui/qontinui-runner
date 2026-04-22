//! Invalidation API for co-occurrence observations (Step 5 of the observation
//! pipeline plan).
//!
//! Soft-deletes (`invalidated_at = now()`) matching observation rows so that
//! refactors or source edits can drop stale fingerprints without losing the
//! audit trail. Each invalidation issues an `undo_token` (UUID) stamped on
//! every affected row; `/undo` flips rows back within a 24 h window.
//!
//! These endpoints are registered via `state_discovery::routes()` and merged
//! into the main router in `mcp_api.rs` — no per-endpoint wiring required.
//!
//! # Filter semantics
//!
//! - `fingerprint` (exact): `fingerprints @> $::jsonb` with a single-element
//!   array — exploits the GIN index on `fingerprints`.
//! - `fingerprint_pattern` (glob): `*` is translated to SQL `%` server-side,
//!   matched via `EXISTS (SELECT 1 FROM jsonb_array_elements_text(fingerprints)
//!   e WHERE e LIKE $)`. Patterns without `*` are rejected to avoid silent
//!   "pattern that looks exact" confusion — use `fingerprint` for exact match.
//! - `spec_id`: `AND spec_id = $`.
//!
//! Filters combine with AND. At least one of the three must be present — a
//! bare call with only `reason`/`invalidated_by` is a 400 to prevent
//! accidental nuke-everything operations.
//!
//! # Only-unstamp-unstamped rule
//!
//! The soft-delete UPDATE carries `AND invalidated_at IS NULL`. Without this,
//! re-invalidating already-invalidated rows would overwrite their
//! `invalidated_reason` / `_by` / `_token`, breaking undo and destroying the
//! audit log. Count returned is therefore "newly invalidated", not "matched".

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use tokio_postgres::types::ToSql;
use tracing::{error, info};
use uuid::Uuid;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

#[derive(Debug, Deserialize)]
pub struct InvalidateBody {
    pub fingerprint: Option<String>,
    pub fingerprint_pattern: Option<String>,
    pub spec_id: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub invalidated_by: Option<String>,
}

/// POST /co-occurrence/invalidate
pub async fn invalidate_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<InvalidateBody>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Require at least one narrowing filter. Soft-deleting the entire table
    // by sending an empty body should not be possible via this endpoint.
    if body.fingerprint.is_none() && body.fingerprint_pattern.is_none() && body.spec_id.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "at least one of fingerprint / fingerprint_pattern / spec_id is required",
            )),
        ));
    }

    // Reject patterns without `*`. An exact-match `fingerprint_pattern` would
    // silently work (since `LIKE 'abc'` matches 'abc' literally) but users
    // should use the `fingerprint` field for that — the distinction matters
    // because the exact path uses the GIN index.
    if let Some(pat) = body.fingerprint_pattern.as_deref() {
        if !pat.contains('*') {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "fingerprint_pattern must contain '*' (use `fingerprint` for exact match)",
                )),
            ));
        }
    }

    let undo_token = Uuid::new_v4().to_string();
    let reason = body.reason.clone();
    let invalidated_by = body.invalidated_by.clone();

    let pg_db = state.app_state.pg_db.clone();

    info!(
        "State Discovery: invalidate (fingerprint={:?}, pattern={:?}, spec_id={:?}, by={:?})",
        body.fingerprint, body.fingerprint_pattern, body.spec_id, invalidated_by
    );

    let conn = match pg_db.pool().get().await {
        Ok(c) => c,
        Err(e) => {
            error!("State Discovery: PG pool error during invalidate: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("PG pool error: {}", e))),
            ));
        }
    };

    // Build dynamic SQL with positional parameters. Parameter ordering:
    //   $1 = reason, $2 = invalidated_by, $3 = undo_token
    //   then filters append $4, $5, ... as they apply.
    let mut sql = String::from(
        "UPDATE co_occurrence_observations
         SET invalidated_at = now(),
             invalidated_reason = $1,
             invalidated_by = $2,
             invalidation_token = $3
         WHERE invalidated_at IS NULL",
    );

    // Owned storage so we can drop &dyn ToSql references into a Vec.
    let mut next_idx: usize = 4;
    let fingerprint_json: Option<serde_json::Value> =
        body.fingerprint.as_ref().map(|fp| serde_json::json!([fp]));
    let pattern_like: Option<String> = body
        .fingerprint_pattern
        .as_ref()
        .map(|p| p.replace('*', "%"));
    let spec_id_owned: Option<String> = body.spec_id.clone();

    if fingerprint_json.is_some() {
        sql.push_str(&format!(" AND fingerprints @> ${}::jsonb", next_idx));
        next_idx += 1;
    }
    if pattern_like.is_some() {
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM jsonb_array_elements_text(fingerprints) e WHERE e LIKE ${})",
            next_idx
        ));
        next_idx += 1;
    }
    if spec_id_owned.is_some() {
        sql.push_str(&format!(" AND spec_id = ${}", next_idx));
        // next_idx += 1; // not read after this, but kept symmetric
    }

    let mut params: Vec<&(dyn ToSql + Sync)> = Vec::new();
    params.push(&reason as &(dyn ToSql + Sync));
    params.push(&invalidated_by as &(dyn ToSql + Sync));
    params.push(&undo_token as &(dyn ToSql + Sync));
    if let Some(ref j) = fingerprint_json {
        params.push(j as &(dyn ToSql + Sync));
    }
    if let Some(ref p) = pattern_like {
        params.push(p as &(dyn ToSql + Sync));
    }
    if let Some(ref s) = spec_id_owned {
        params.push(s as &(dyn ToSql + Sync));
    }

    let count = match conn.execute(sql.as_str(), &params[..]).await {
        Ok(n) => n,
        Err(e) => {
            error!("State Discovery: PG update error during invalidate: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("PG update error: {}", e))),
            ));
        }
    };

    info!(
        "State Discovery: invalidated {} observation(s) (undo_token={})",
        count, undo_token
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "count": count,
        "undo_token": undo_token,
    }))))
}

#[derive(Debug, Deserialize)]
pub struct UndoBody {
    pub undo_token: String,
}

/// POST /co-occurrence/invalidate/undo
pub async fn undo_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<UndoBody>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    if body.undo_token.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("undo_token is required")),
        ));
    }

    let pg_db = state.app_state.pg_db.clone();
    let conn = match pg_db.pool().get().await {
        Ok(c) => c,
        Err(e) => {
            error!("State Discovery: PG pool error during undo: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("PG pool error: {}", e))),
            ));
        }
    };

    // The `invalidated_at > now() - interval '24 hours'` guard enforces the
    // 24 h undo window at the DB layer, so racing clients can't flip an old
    // invalidation back.
    let count = match conn
        .execute(
            r#"UPDATE co_occurrence_observations
               SET invalidated_at = NULL,
                   invalidated_reason = NULL,
                   invalidated_by = NULL,
                   invalidation_token = NULL
               WHERE invalidation_token = $1
                 AND invalidated_at IS NOT NULL
                 AND invalidated_at > now() - interval '24 hours'"#,
            &[&body.undo_token],
        )
        .await
    {
        Ok(n) => n,
        Err(e) => {
            error!("State Discovery: PG update error during undo: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("PG update error: {}", e))),
            ));
        }
    };

    info!(
        "State Discovery: undo restored {} observation(s) (undo_token={})",
        count, body.undo_token
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "count": count,
    }))))
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub spec_id: Option<String>,
}

/// GET /co-occurrence/invalidations
pub async fn list_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg_db = state.app_state.pg_db.clone();
    let conn = match pg_db.pool().get().await {
        Ok(c) => c,
        Err(e) => {
            error!("State Discovery: PG pool error during list: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("PG pool error: {}", e))),
            ));
        }
    };

    // `jsonb_array_length` on the fingerprints column gives us a cheap count
    // for the audit-log view without having to ship the full array over.
    let rows_result = if let Some(ref sid) = query.spec_id {
        conn.query(
            r#"SELECT id::text,
                      spec_id,
                      invalidated_at,
                      invalidated_reason,
                      invalidated_by,
                      invalidation_token,
                      jsonb_array_length(fingerprints) AS fingerprints_count
               FROM co_occurrence_observations
               WHERE invalidated_at IS NOT NULL
                 AND spec_id = $1
               ORDER BY invalidated_at DESC
               LIMIT 500"#,
            &[sid],
        )
        .await
    } else {
        conn.query(
            r#"SELECT id::text,
                      spec_id,
                      invalidated_at,
                      invalidated_reason,
                      invalidated_by,
                      invalidation_token,
                      jsonb_array_length(fingerprints) AS fingerprints_count
               FROM co_occurrence_observations
               WHERE invalidated_at IS NOT NULL
               ORDER BY invalidated_at DESC
               LIMIT 500"#,
            &[],
        )
        .await
    };

    let rows = match rows_result {
        Ok(r) => r,
        Err(e) => {
            error!("State Discovery: PG query error during list: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("PG query error: {}", e))),
            ));
        }
    };

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.get(0);
        let spec_id: Option<String> = row.get(1);
        let invalidated_at: Option<chrono::DateTime<chrono::Utc>> = row.get(2);
        let invalidated_reason: Option<String> = row.get(3);
        let invalidated_by: Option<String> = row.get(4);
        let invalidation_token: Option<String> = row.get(5);
        let fingerprints_count: Option<i32> = row.get(6);

        out.push(serde_json::json!({
            "id": id,
            "spec_id": spec_id,
            "invalidated_at": invalidated_at.map(|t| t.to_rfc3339()),
            "invalidated_reason": invalidated_reason,
            "invalidated_by": invalidated_by,
            "invalidation_token": invalidation_token,
            "fingerprints_count": fingerprints_count.unwrap_or(0),
        }));
    }

    Ok(Json(ApiResponse::success(serde_json::json!(out))))
}
