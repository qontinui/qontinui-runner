//! Axum handlers for `/runs/:run_id/drift` and `/runs/:run_id/drift/:entry_id`.
//!
//! Wire envelope: `{ success: bool, data?: T, error?: string }` — same as
//! `scenarios::handlers` and the rest of the runner API.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::Value;
use tracing::warn;

use crate::database::pg::PgDb;
use crate::mcp::types::ApiState;

// ---------------------------------------------------------------------------
// Wire envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct DriftEnvelope<T: Serialize> {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl<T: Serialize> DriftEnvelope<T> {
    fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }
}

fn err_envelope(msg: impl Into<String>) -> DriftEnvelope<Value> {
    DriftEnvelope {
        success: false,
        data: None,
        error: Some(msg.into()),
    }
}

// ---------------------------------------------------------------------------
// Storage adapter — looks up the Pg-backed pool from the StorageCompartment
// ---------------------------------------------------------------------------

async fn fetch_drift_report(
    _api_state: &ApiState,
    run_id: &str,
) -> Result<Option<Value>, String> {
    let pg = PgDb::try_global()
        .ok_or_else(|| "PgDb not initialized".to_string())?;
    pg.get_drift_report_for_logical_run_id(run_id).await
}

// ---------------------------------------------------------------------------
// GET /runs/:run_id/drift
// ---------------------------------------------------------------------------

/// Return the persisted `DriftReport` for a regression run.
///
/// Resolution: looks up the most recent `regression_runs` row whose
/// `run_id` text column matches the path parameter, returning that row's
/// `drift_report_json`. If the run has no drift report (older runs, or runs
/// recorded without spec/visual drift inputs), returns 404.
pub async fn get_run_drift_report(
    State(api_state): State<Arc<ApiState>>,
    Path(run_id): Path<String>,
) -> Response {
    if run_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(err_envelope("missing-run-id-path-param")),
        )
            .into_response();
    }

    match fetch_drift_report(&api_state, &run_id).await {
        Ok(Some(report)) => (StatusCode::OK, Json(DriftEnvelope::ok(report))).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(err_envelope("drift-report-not-found")),
        )
            .into_response(),
        Err(e) => {
            warn!("get_run_drift_report failed for run_id={}: {}", run_id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(err_envelope(format!("storage error: {}", e))),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// GET /runs/:run_id/drift/:entry_id
// ---------------------------------------------------------------------------

/// Return a single drift entry by id from the persisted report.
///
/// Walks the report's `entries` array and matches on `entry.id`. If the
/// run's report is missing or the entry id doesn't match any entry, returns
/// 404 — the qontinui-web frontend will render the not-found state.
pub async fn get_run_drift_entry(
    State(api_state): State<Arc<ApiState>>,
    Path((run_id, entry_id)): Path<(String, String)>,
) -> Response {
    if run_id.trim().is_empty() || entry_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(err_envelope("missing-path-param")),
        )
            .into_response();
    }

    let report = match fetch_drift_report(&api_state, &run_id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(err_envelope("drift-report-not-found")),
            )
                .into_response();
        }
        Err(e) => {
            warn!("get_run_drift_entry storage error: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(err_envelope(format!("storage error: {}", e))),
            )
                .into_response();
        }
    };

    let entries = report
        .get("entries")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let needle = entry_id.as_str();
    let matched = entries.into_iter().find(|e| {
        e.get("id")
            .and_then(|v| v.as_str())
            .map(|id| id == needle)
            .unwrap_or(false)
    });

    match matched {
        Some(entry) => (StatusCode::OK, Json(DriftEnvelope::ok(entry))).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(err_envelope("drift-entry-not-found")),
        )
            .into_response(),
    }
}
