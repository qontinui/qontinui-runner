//! Terminal buffer readback HTTP handler.
//!
//! Implements `GET /ui-bridge/sdk/terminal/sessions/:session_id/buffer?lines=N`
//! by asking the React frontend (where xterm.js Terminal instances live) for
//! the current buffer text via Tauri events. Mirrors the
//! `tagged_page_evaluate` pattern in `mcp/ui_bridge/page.rs`.
//!
//! Flow:
//! 1. Allocate a fresh `request_id` and register a oneshot in
//!    [`crate::terminal_buffer::TerminalBufferRequestStore`].
//! 2. Emit Tauri event `ui-bridge:terminal-buffer-request`.
//! 3. The frontend hook (`useTerminalBufferIpc`) looks up the xterm backend
//!    by session_id, serializes lines, and emits
//!    `ui-bridge:terminal-buffer-response`.
//! 4. The global Tauri listener installed at `mcp_api.rs` startup delivers
//!    the response to the matching oneshot.
//! 5. We await with a 5-second timeout and shape the JSON response.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tracing::{debug, info, warn};

use super::types::{api_error, ApiResponse, ApiState};

/// 5-second IPC round-trip cap, per the route spec.
const TERMINAL_BUFFER_TIMEOUT_MS: u64 = 5_000;

/// Hard cap on the number of returned lines, regardless of the `lines` query
/// parameter. Prevents runaway responses if a misbehaving caller asks for
/// millions of lines on a 10k-scrollback buffer (or the frontend gets a stuck
/// buffer after a runaway PTY producer).
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
    /// Full buffer length the frontend observed, regardless of any `lines`
    /// truncation applied to the returned slice.
    pub total_lines: usize,
    /// True iff the returned `lines.len()` is less than `total_lines` —
    /// either because the caller passed a `lines` query parameter or because
    /// the buffer exceeded [`MAX_RETURNED_LINES`].
    pub truncated: bool,
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

    let request_id = uuid::Uuid::new_v4().to_string();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    state
        .terminal_buffer_store
        .register(request_id.clone(), sender)
        .await;

    // Forward the requested-window cap to the frontend so it can avoid
    // serializing the full buffer when the caller only wants the tail.
    // Cap at MAX_RETURNED_LINES so even an unrequested-N caller can't
    // pull a multi-million-line buffer over IPC.
    let max_lines = query.lines.unwrap_or(MAX_RETURNED_LINES).min(MAX_RETURNED_LINES);

    let payload = serde_json::json!({
        "request_id": request_id,
        "session_id": session_id,
        "max_lines": max_lines,
    });

    if let Err(e) = state
        .app_handle
        .emit("ui-bridge:terminal-buffer-request", &payload)
    {
        state.terminal_buffer_store.cancel(&request_id).await;
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to emit ui-bridge:terminal-buffer-request: {}",
                e
            ))),
        ));
    }

    match tokio::time::timeout(Duration::from_millis(TERMINAL_BUFFER_TIMEOUT_MS), receiver).await {
        Ok(Ok(resp)) => {
            if !resp.ok {
                let err_code = resp.error.as_deref().unwrap_or("unknown_error");
                debug!(
                    "terminal-buffer: frontend reported error '{}' for session_id={}",
                    err_code, session_id
                );
                if err_code == "session_not_found" {
                    return Err((
                        StatusCode::NOT_FOUND,
                        Json(api_error(format!(
                            "terminal session '{}' not found in frontend registry",
                            session_id
                        ))),
                    ));
                }
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!(
                        "terminal-buffer: frontend error: {}",
                        err_code
                    ))),
                ));
            }

            let total_lines = resp.total_lines.unwrap_or(0);
            let mut lines = resp.lines.unwrap_or_default();

            // Defense in depth: even though we forwarded max_lines to the
            // frontend, clamp again here in case a future frontend change
            // forgets to apply the cap.
            if lines.len() > MAX_RETURNED_LINES {
                let drop_count = lines.len() - MAX_RETURNED_LINES;
                lines.drain(0..drop_count);
            }

            let truncated = lines.len() < total_lines;

            Ok(Json(ApiResponse::success(BufferResponseBody {
                session_id,
                lines,
                total_lines,
                truncated,
            })))
        }
        Ok(Err(_)) => {
            state.terminal_buffer_store.cancel(&request_id).await;
            warn!(
                "terminal-buffer: response channel closed before delivery (request_id={}, session_id={})",
                request_id, session_id
            );
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(
                    "terminal-buffer: response channel closed before delivery".to_string(),
                )),
            ))
        }
        Err(_) => {
            state.terminal_buffer_store.cancel(&request_id).await;
            warn!(
                "terminal-buffer: IPC timeout after {}ms (request_id={}, session_id={})",
                TERMINAL_BUFFER_TIMEOUT_MS, request_id, session_id
            );
            // 504 Gateway Timeout — the frontend never responded.
            Err((
                StatusCode::GATEWAY_TIMEOUT,
                Json(api_error("ipc_timeout".to_string())),
            ))
        }
    }
}
