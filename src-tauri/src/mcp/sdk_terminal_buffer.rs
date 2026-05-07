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
use crate::terminal::grid::{GridDiff, GridSnapshot, SearchHit, TextSnapshot};
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
    let cap = query
        .lines
        .unwrap_or(MAX_RETURNED_LINES)
        .min(MAX_RETURNED_LINES);
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

/// `GET /ui-bridge/sdk/terminal/sessions/:session_id/text` — compact
/// text-only view for verifiers and external tools.
pub async fn handle_terminal_text(
    State(state): State<Arc<ApiState>>,
    Path(session_id): Path<String>,
) -> Result<Json<ApiResponse<TextSnapshot>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: terminal text snapshot (session_id={})",
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
    let snapshot = {
        let grid = session.grid();
        let g = grid.lock().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Grid lock poisoned: {}", e))),
            )
        })?;
        g.text_snapshot()
    };
    Ok(Json(ApiResponse::success(snapshot)))
}

/// Query parameters for the search endpoint.
#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    /// Pattern to look for. Required.
    pub q: String,
    /// `true` → compile as regex; `false` (default) → case-sensitive substring.
    #[serde(default)]
    pub regex: bool,
    /// Restrict to one session. When omitted, search ALL active sessions.
    pub session_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultPerSession {
    pub session_id: String,
    pub hits: Vec<SearchHit>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponseBody {
    pub query: String,
    pub regex: bool,
    pub results: Vec<SearchResultPerSession>,
    pub total_hits: usize,
}

/// `GET /ui-bridge/sdk/terminal/search?q=&regex=&session_id=`
pub async fn handle_terminal_search(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<ApiResponse<SearchResponseBody>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: terminal grid search (q={:?}, regex={}, session_id={:?})",
        q.q, q.regex, q.session_id
    );
    if q.q.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("query 'q' is required and must be non-empty")),
        ));
    }
    let tm = lookup_terminal_manager(&state)?;

    let pairs: Vec<(String, Arc<crate::terminal::session::TerminalSession>)> = match &q.session_id {
        Some(id) => {
            let sess = tm.get(id).ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    Json(api_error(format!("terminal session '{}' not found", id))),
                )
            })?;
            vec![(id.clone(), sess)]
        }
        None => tm.sessions_snapshot(),
    };

    let mut results: Vec<SearchResultPerSession> = Vec::new();
    let mut total = 0usize;
    for (sid, sess) in pairs {
        let hits = {
            let grid = sess.grid();
            let g = grid.lock().map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Grid lock poisoned: {}", e))),
                )
            })?;
            g.search(&q.q, q.regex)
                .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(e.to_string()))))?
        };
        if !hits.is_empty() {
            total += hits.len();
            results.push(SearchResultPerSession {
                session_id: sid,
                hits,
            });
        }
    }

    Ok(Json(ApiResponse::success(SearchResponseBody {
        query: q.q,
        regex: q.regex,
        results,
        total_hits: total,
    })))
}

/// `GET /ui-bridge/sdk/terminal/sessions/:a/diff/:b` — row-by-row diff
/// between two terminals' rendered grids.
pub async fn handle_terminal_diff(
    State(state): State<Arc<ApiState>>,
    Path((a_id, b_id)): Path<(String, String)>,
) -> Result<Json<ApiResponse<GridDiff>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: terminal grid diff (a={}, b={})", a_id, b_id);
    let tm = lookup_terminal_manager(&state)?;
    let a = tm.get(&a_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("terminal session '{}' not found", a_id))),
        )
    })?;
    let b = tm.get(&b_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("terminal session '{}' not found", b_id))),
        )
    })?;
    // Self-diff would deadlock if we naively locked `a.grid()` and
    // `b.grid()` separately when they point to the same Mutex. Detect
    // that case via Arc::ptr_eq and take a single lock — diff against
    // itself is a no-op (always 0 changes), useful as a smoke check
    // from automated tests.
    let diff = if Arc::ptr_eq(&a.grid(), &b.grid()) {
        let g = a.grid();
        let grid = g.lock().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Grid lock poisoned: {}", e))),
            )
        })?;
        grid.diff_lines(&grid)
    } else {
        let a_grid = a.grid();
        let b_grid = b.grid();
        let ag = a_grid.lock().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Grid A lock poisoned: {}", e))),
            )
        })?;
        let bg = b_grid.lock().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Grid B lock poisoned: {}", e))),
            )
        })?;
        ag.diff_lines(&bg)
    };
    Ok(Json(ApiResponse::success(diff)))
}
