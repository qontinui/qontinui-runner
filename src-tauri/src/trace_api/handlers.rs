//! Axum handlers for the Trace API. Endpoints under `/trace/...`.
//!
//! Surface (Section 5b plan):
//! - `GET  /trace/health`                       — health check
//! - `GET  /trace/list?limit=N`                 — list recording sessions
//! - `GET  /trace/session/{id}`                 — full event timeline for a session
//! - `GET  /trace/event/{id}/causal-chain`      — backward walk to roots
//! - `POST /trace/save`                         — persist a recorder session
//! - `POST /trace/replay`                       — 501 (deferred to v2)
//!
//! Every empty/error response carries a `reason` field per the
//! "no silent empty responses" constraint shared with the Spec API.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;

use crate::database::pg::PgDb;
use crate::mcp::types::ApiState;

use super::events::{self, TraceChanged};
use super::responses::{EmptyOk, TraceError};
use super::storage;
use super::types::{CausalChain, InboundRecordingSession};

/// Resolve the global `PgDb` or return a `503` error envelope. The trace API
/// is only useful when PG is wired; we make that explicit rather than
/// silently degrading.
fn require_pg() -> Result<Arc<PgDb>, Response> {
    PgDb::try_global().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(TraceError::new("pg-unavailable")),
        )
            .into_response()
    })
}

// ---------------------------------------------------------------------------
// GET /trace/health
// ---------------------------------------------------------------------------

pub async fn get_health(State(_state): State<Arc<ApiState>>) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "status": "ok",
            "enabled": true,
            "reason": "trace-api-mounted",
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// GET /trace/list?limit=N
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
}

pub async fn get_list(State(_state): State<Arc<ApiState>>, Query(q): Query<ListQuery>) -> Response {
    let pg = match require_pg() {
        Ok(p) => p,
        Err(r) => return r,
    };
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    match storage::list_recording_sessions(&pg, limit).await {
        Ok(sessions) => {
            let reason = if sessions.is_empty() {
                "no-recording-sessions"
            } else {
                "recording-sessions-listed"
            };
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "sessions": sessions,
                    "limit": limit,
                    "reason": reason,
                })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(TraceError::with_detail(
                "list-failed",
                json!({ "error": e }),
            )),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// GET /trace/session/{id}
// ---------------------------------------------------------------------------

pub async fn get_session(
    State(_state): State<Arc<ApiState>>,
    AxPath(id): AxPath<String>,
) -> Response {
    if id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(TraceError::new("missing-recording-session-id")),
        )
            .into_response();
    }
    let pg = match require_pg() {
        Ok(p) => p,
        Err(r) => return r,
    };
    match storage::get_recording_session(&pg, &id).await {
        Ok(events) => {
            if events.is_empty() {
                return (
                    StatusCode::NOT_FOUND,
                    Json(TraceError::with_detail(
                        "recording-session-not-found",
                        json!({ "recordingSessionId": id }),
                    )),
                )
                    .into_response();
            }
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "recordingSessionId": id,
                    "events": events,
                })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(TraceError::with_detail(
                "session-read-failed",
                json!({ "recordingSessionId": id, "error": e }),
            )),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// GET /trace/event/{id}/causal-chain
// ---------------------------------------------------------------------------

pub async fn get_causal_chain(
    State(_state): State<Arc<ApiState>>,
    AxPath(id): AxPath<i64>,
) -> Response {
    let pg = match require_pg() {
        Ok(p) => p,
        Err(r) => return r,
    };
    match storage::get_causal_chain(&pg, id).await {
        Ok(chain) => {
            if chain.is_empty() {
                return (
                    StatusCode::NOT_FOUND,
                    Json(TraceError::with_detail(
                        "event-not-found",
                        json!({ "eventId": id }),
                    )),
                )
                    .into_response();
            }
            let payload = CausalChain {
                event_id: id,
                chain,
            };
            (
                StatusCode::OK,
                Json(json!({ "ok": true, "payload": payload })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(TraceError::with_detail(
                "causal-chain-read-failed",
                json!({ "eventId": id, "error": e }),
            )),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// POST /trace/save
// ---------------------------------------------------------------------------

pub async fn post_save(
    State(_state): State<Arc<ApiState>>,
    Json(body): Json<InboundRecordingSession>,
) -> Response {
    let session_id = body.recording_session_id.trim().to_string();
    if session_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(TraceError::new("missing-recording-session-id")),
        )
            .into_response();
    }
    if body.events.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(TraceError::with_detail(
                "empty-events-array",
                json!({ "recordingSessionId": session_id }),
            )),
        )
            .into_response();
    }

    let pg = match require_pg() {
        Ok(p) => p,
        Err(r) => return r,
    };

    // First pass: assign sequence/timestamp defaults and remember client-id ->
    // db-id wiring so the second pass can resolve `caused_by` references.
    let mut client_to_db: HashMap<String, i64> = HashMap::new();
    let mut inserted: Vec<i64> = Vec::with_capacity(body.events.len());
    let now_ms = events::now_ms() as i64;

    for (idx, event) in body.events.iter().enumerate() {
        let sequence = event.sequence.unwrap_or(idx as i64);
        let timestamp = event.timestamp.unwrap_or(now_ms);
        let caused_by = event
            .caused_by
            .as_ref()
            .and_then(|cid| client_to_db.get(cid).copied());

        match storage::insert_causal_event(&pg, event, &session_id, sequence, timestamp, caused_by)
            .await
        {
            Ok(db_id) => {
                if let Some(client_id) = event.client_id.as_ref() {
                    client_to_db.insert(client_id.clone(), db_id);
                }
                inserted.push(db_id);
            }
            Err(e) => {
                warn!(
                    "trace_api: insert_causal_event failed at index {} of session {}: {}",
                    idx, session_id, e
                );
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(TraceError::with_detail(
                        "insert-failed",
                        json!({
                            "recordingSessionId": session_id,
                            "indexFailed": idx,
                            "insertedSoFar": inserted.len(),
                            "error": e,
                        }),
                    )),
                )
                    .into_response();
            }
        }
    }

    let event_count = inserted.len() as i64;
    events::emit(TraceChanged {
        recording_session_id: session_id.clone(),
        kind: "session-saved".to_string(),
        event_count,
        at_ms: events::now_ms(),
    });

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "recordingSessionId": session_id,
            "insertedCount": event_count,
            "insertedIds": inserted,
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// POST /trace/replay
// ---------------------------------------------------------------------------

/// Deferred to v2 per ADR-005 — the v1 trace API only persists and reads.
pub async fn post_replay(
    State(_state): State<Arc<ApiState>>,
    Json(_body): Json<Value>,
) -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(TraceError::new("replay deferred to v2; see ADR-005")),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Available for unit tests / quick smoke handlers — same envelope as
/// `EmptyOk::new` but returned as a fully-typed JSON response.
#[allow(dead_code)]
pub fn empty_ok(reason: impl Into<String>) -> Response {
    (StatusCode::OK, Json(EmptyOk::new(reason))).into_response()
}
