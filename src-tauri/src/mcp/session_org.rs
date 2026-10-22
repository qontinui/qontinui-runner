//! Loopback HTTP twins of the Session Manager organization store (plan
//! `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
//! Phase 2, D3): `GET /terminal/session-org` and `POST /terminal/session-org`.
//!
//! They exist so an agent, a script or the UI Bridge can read and change the
//! operator's session groups without the webview — e.g. an orchestrating
//! session filing its children under its plan's group. Both are thin shells
//! over the same [`SessionOrgStore`] the Tauri commands use
//! (`commands::session_org`), so every window refreshes on an HTTP write via
//! the store's change event.
//!
//! AUTH POSTURE: these sit on the existing loopback router under the existing
//! origin guard and widen no allow-set — reviewed NOT a door (they return no
//! secret, read no caller-named path, make no outbound request and drive no
//! process; `REVIEWED_NOT_DOOR` in `mcp/origin_guard/tests.rs`). A browser
//! origin other than the webview is therefore refused under `enforce`, and
//! NonBrowser callers reach them like every other route.
//!
//! Wire shapes:
//! - `GET` → `ApiResponse<SessionOrgSnapshot>`; `data.status` is `"unknown"`
//!   (with `reason` / `preservedAt`, `org: null`) when the file was unreadable.
//! - `POST` body `{"ops": [{"op": "createGroup", "title": "…", …}, …]}` →
//!   `ApiResponse<SessionOrgApplyResponse>` with one result per op. A refused
//!   op is a per-op `ok: false`, not an HTTP error; a 500 means the batch
//!   could not be persisted and NOTHING changed.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use tauri::Manager;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::session::session_org_store::{
    SessionOrgApplyResponse, SessionOrgOp, SessionOrgSnapshot, SessionOrgStore,
};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

type HandlerError = (StatusCode, Json<ApiResponse<()>>);

/// `POST /terminal/session-org` body.
#[derive(Debug, Deserialize)]
pub struct ApplyRequest {
    pub ops: Vec<SessionOrgOp>,
}

/// The managed store, or the 503 that says it is not up yet. `Box`ed only to
/// keep the handler `Result`s small.
fn store(state: &ApiState) -> Result<Arc<SessionOrgStore>, Box<HandlerError>> {
    state
        .app_handle
        .try_state::<Arc<SessionOrgStore>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| {
            Box::new((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(api_error("session org store is not initialized yet")),
            ))
        })
}

/// `GET /terminal/session-org`.
pub async fn get_session_org_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<SessionOrgSnapshot>>, HandlerError> {
    let store = store(&state).map_err(|e| *e)?;
    // `snapshot` takes the store's std mutex, which an apply holds through its
    // fsync — wait for it off the async workers, as the POST does.
    let snapshot = spawn_blocking_tracked(move || store.snapshot())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("session org read task failed: {e}"))),
            )
        })?;
    Ok(Json(ApiResponse::success(snapshot)))
}

/// `POST /terminal/session-org`.
pub async fn apply_session_org_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<ApplyRequest>,
) -> Result<Json<ApiResponse<SessionOrgApplyResponse>>, HandlerError> {
    let store = store(&state).map_err(|e| *e)?;
    // File I/O under a std mutex — keep it off the async workers.
    let result = spawn_blocking_tracked(move || store.apply(&body.ops))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("session org apply task failed: {e}"))),
            )
        })?;
    result
        .map(|resp| Json(ApiResponse::success(resp)))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
}

/// Create routes for this module.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::get;

    axum::Router::new().route(
        "/terminal/session-org",
        get(get_session_org_handler).post(apply_session_org_handler),
    )
}
