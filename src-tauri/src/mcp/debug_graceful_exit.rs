//! Debug-only door onto `TerminalSession::graceful_exit` (plan
//! `2026-09-13-drained-runner-never-reaches-idle`, Phase 1 falsification step
//! 2): `POST /__debug/terminals/{id}/graceful-exit?deadline_s=N` types `/exit`
//! into that pane, waits for its `claude` to leave, closes the tab through
//! `TerminalManager::close` only once it has, and answers the
//! [`GracefulExitOutcome`].
//!
//! Nothing in the runner calls graceful exit on its own in Phase 1, so this is
//! the one way to exercise the primitive against a real pane on a temp runner.
//!
//! # It cannot ship
//!
//! Declared `#[cfg(debug_assertions)]` in `mcp/mod.rs` and merged under the
//! same gate in `mcp_api`, following `mcp::debug_wedge`. A release build holds
//! neither the handler nor the route.
//!
//! # It never kills a live `claude`
//!
//! The tab close is handed to `graceful_exit_then`, which invokes it only
//! after the pane's subtree holds no `claude`. An `exit_stuck` answer means the
//! process was left running and the tab left open.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use tauri::Manager;
use tracing::warn;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::terminal::graceful_exit::{GracefulExitOutcome, DEFAULT_DEADLINE};
use crate::terminal::TerminalManager;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

/// Ceiling on a requested deadline, so a typo cannot hold a request open for
/// an hour.
pub const MAX_DEADLINE: Duration = Duration::from_secs(300);

#[derive(Debug, Deserialize)]
pub struct GracefulExitQuery {
    /// Seconds to wait for `claude` to leave. Default
    /// [`DEFAULT_DEADLINE`]; clamped to `1..=`[`MAX_DEADLINE`].
    pub deadline_s: Option<u64>,
}

/// Resolve the requested deadline.
pub fn resolve_deadline(raw: Option<u64>) -> Duration {
    raw.map(|secs| Duration::from_secs(secs.clamp(1, MAX_DEADLINE.as_secs())))
        .unwrap_or(DEFAULT_DEADLINE)
}

type HandlerError = (StatusCode, Json<ApiResponse<()>>);

/// `POST /__debug/terminals/{id}/graceful-exit` — see the module docs.
pub async fn graceful_exit_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<GracefulExitQuery>,
) -> Result<Json<ApiResponse<GracefulExitOutcome>>, HandlerError> {
    let manager = state
        .app_handle
        .try_state::<Arc<TerminalManager>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(api_error("TerminalManager is not available")),
            )
        })?;
    let session = manager.get(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Terminal session not found: {id}"))),
        )
    })?;

    let deadline = resolve_deadline(query.deadline_s);
    let terminal_id = id.clone();
    let outcome = session
        .graceful_exit_then(deadline, move || async move {
            let closing = terminal_id.clone();
            match spawn_blocking_tracked(move || manager.close(&closing)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => warn!(terminal_id = %terminal_id, error = %e, "graceful-exit: tab close failed"),
                Err(e) => warn!(terminal_id = %terminal_id, error = %e, "graceful-exit: tab close task failed"),
            }
        })
        .await;
    Ok(Json(ApiResponse::success(outcome)))
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    axum::Router::new().route(
        "/__debug/terminals/{id}/graceful-exit",
        axum::routing::post(graceful_exit_handler),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_defaults_and_clamps() {
        assert_eq!(resolve_deadline(None), DEFAULT_DEADLINE);
        assert_eq!(resolve_deadline(Some(0)), Duration::from_secs(1));
        assert_eq!(resolve_deadline(Some(45)), Duration::from_secs(45));
        assert_eq!(resolve_deadline(Some(86_400)), MAX_DEADLINE);
    }
}
