//! Regression drift HTTP API — Section 11 follow-up FU-3.
//!
//! Two endpoints expose the persisted `DriftReport` for a regression run to
//! the qontinui-web backend's drift dashboard proxy
//! (`qontinui-web/backend/app/api/v1/endpoints/runs_drift.py`):
//!
//!   - `GET /runs/:run_id/drift` — returns the most recent persisted
//!     `DriftReport` for a logical run id. Pass-through of the JSON the
//!     executor stored in `regression_runs.drift_report_json`.
//!
//!   - `GET /runs/:run_id/drift/:entry_id` — returns a single entry from
//!     that report. The report has a `entries: DriftEntry[]` array where
//!     each entry has an `id`. We index by that id.
//!
//! Wire envelope: `{ success, data, error }`, matching the rest of the
//! runner HTTP surface (e.g. `scenarios::handlers`, `commands::ai_data`).
//!
//! Storage notes: the executor passes `drift_report_json` through to the
//! `record_regression_run` Tauri command, which stores it on the
//! `regression_runs` row. The backend proxy invokes these endpoints; the
//! frontend `drift-api.ts` consumes the result via the proxy.

pub mod handlers;

use axum::routing::get;
use axum::Router;
use std::sync::Arc;

use crate::mcp::types::ApiState;

/// Routes for the runner-side drift API. Mounted alongside `/spec/...` and
/// `/scenarios/...` in `mcp_api.rs::create_router`.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route(
            "/runs/{run_id}/drift",
            get(handlers::get_run_drift_report),
        )
        .route(
            "/runs/{run_id}/drift/{entry_id}",
            get(handlers::get_run_drift_entry),
        )
}
