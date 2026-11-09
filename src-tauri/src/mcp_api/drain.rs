//! `POST /drain` — the runner's graceful-drain door. Moved verbatim out of
//! `mcp_api.rs` (plan
//! `2026-10-04-runner-mcp-api-rs-holds-the-http-composition-root-health-and-five-proxies-in-one-file`
//! Phase 2).

use std::sync::Arc;

use axum::response::Json;

use crate::mcp::types::ApiState;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

/// `POST /drain` — graceful drain on planned restart (Phase 2 of
/// `2026-06-06-runner-dev-loop-and-restart-resilience`).
///
/// Triggers the [`crate::drain::drain`] sequence: flips the global draining
/// flag (refuses new AI turns), flushes in-flight turns to `output_log`,
/// auto-commits each session's dirty worktree to `refs/wip/<agent_session_id>`,
/// and heartbeats coord claims — all bounded by a hard timeout so a stuck
/// session can never block a deploy. The supervisor calls this before its
/// `taskkill`; the in-process exit seam also calls the drain fn directly.
///
/// Safe to call when idle (no sessions → fast no-op). Idempotent: a second
/// call after a completed drain returns `{already_drained: true}` instantly.
pub(super) async fn drain_handler(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> Json<crate::drain::DrainSummary> {
    let app_handle = state.app_handle.clone();
    let timeout = crate::drain::configured_timeout();
    // The drain sequence is synchronous (blocking git + bounded polling), so
    // run it on a blocking thread to keep the async executor free.
    let summary = spawn_blocking_tracked(move || crate::drain::drain(&app_handle, timeout))
        .await
        .unwrap_or_else(|e| {
            tracing::error!("drain task panicked: {e}");
            crate::drain::DrainSummary::default()
        });
    Json(summary)
}
