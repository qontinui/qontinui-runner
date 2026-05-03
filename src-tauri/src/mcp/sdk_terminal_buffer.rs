//! Terminal buffer + grid readback HTTP handlers.
//!
//! `GET /ui-bridge/sdk/terminal/sessions/:session_id/buffer?lines=N` returns
//! the rendered text of a PTY session by sourcing from the Rust-side `Grid`
//! cell parser — no Tauri IPC round-trip, no frontend xterm dependency.
//!
//! `GET /ui-bridge/sdk/terminal/sessions/:session_id/grid` returns the full
//! `GridSnapshot` for callers that want cell-level data (Playwright, mobile
//! bridge, automated tests).
//!
//! See `plans/terminal-grid-snapshot.md`.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use tauri::Manager;
use tracing::info;

use super::types::{api_error, ApiResponse, ApiState};
use crate::terminal::grid::GridSnapshot;
use crate::terminal::TerminalManager;

/// Hard cap on the number of returned lines, regardless of the `lines` query
/// parameter. Prevents runaway responses from misbehaving callers.
const MAX_RETURNED_LINES: usize = 10_000;

/// Query parameters for the buffer-readback endpoint.
#[derive(Debug, Deserialize)]
pub struct BufferQuery {
    /// If provided, return only the last N lines of the buffer (most recent).
    /// If absent, returns all available buffer lines (still capped at
    /// [`MAX_RETURNED_LINES`] for safety).
    pub lines: Option<usize>,
}

/// Successful buffer-readback response body.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferResponseBody {
    pub session_id: String,
    /// Rendered text lines, oldest first within the returned window.
    pub lines: Vec<String>,
    /// Full grid line count the server observed, regardless of any `lines`
    /// truncation applied to the returned slice.
    pub total_lines: usize,
    /// True iff the returned `lines.len()` is less than `total_lines`.
    pub truncated: bool,
}

fn lookup_terminal_manager(
    state: &Arc<ApiState>,
) -> Result<Arc<TerminalManager>, (StatusCode, Json<ApiResponse<()>>)> {
    state
        .app_handle
        .try_state::<Arc<TerminalManager>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error("TerminalManager not registered as Tauri state")),
            )
        })
}

/// `GET /ui-bridge/sdk/terminal/sessions/:session_id/buffer`
pub async fn handle_terminal_buffer(
    State(state): State<Arc<ApiState>>,
    Path(session_id): Path<String>,
    Query(query): Query<BufferQuery>,
) -> Result<Json<ApiResponse<BufferResponseBody>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: terminal buffer readback (session_id={}, lines={:?})",
        session_id, query.lines
    );

    let tm = lookup_terminal_manager(&state)?;
    let session = tm.get(&session_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!(
                "terminal session '{}' not found",
                session_id
            ))),
        )
    })?;

    let grid_handle = session.grid();
    let all_lines: Vec<String> = {
        let grid = grid_handle.lock().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Grid lock poisoned: {}", e))),
            )
        })?;
        grid.lines()
    };

    let total_lines = all_lines.len();
    let cap = query.lines.unwrap_or(MAX_RETURNED_LINES).min(MAX_RETURNED_LINES);
    let lines = if total_lines > cap {
        all_lines[total_lines - cap..].to_vec()
    } else {
        all_lines
    };
    let truncated = lines.len() < total_lines;

    Ok(Json(ApiResponse::success(BufferResponseBody {
        session_id,
        lines,
        total_lines,
        truncated,
    })))
}

/// `GET /ui-bridge/sdk/terminal/sessions/:session_id/grid`
pub async fn handle_terminal_grid(
    State(state): State<Arc<ApiState>>,
    Path(session_id): Path<String>,
) -> Result<Json<ApiResponse<GridSnapshot>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: terminal grid snapshot (session_id={})",
        session_id
    );

    let tm = lookup_terminal_manager(&state)?;
    let session = tm.get(&session_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!(
                "terminal session '{}' not found",
                session_id
            ))),
        )
    })?;

    let grid_handle = session.grid();
    let snapshot = {
        let grid = grid_handle.lock().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Grid lock poisoned: {}", e))),
            )
        })?;
        grid.snapshot()
    };

    Ok(Json(ApiResponse::success(snapshot)))
}
