//! Debug-profile wire hold on a remote tab's SOURCE (plan
//! `2026-09-26-a5-reattach-acceptance-has-no-drivable-trigger-so-have-offset-ships-unexercised`,
//! Phase 2): `POST /__debug/terminals/{id}/wire-hold` with `{"held": bool}`
//! sets the `held` input of that session's `WireFlow` and answers
//! `{terminal_id, held, wire_paused, remote}`.
//!
//! The A5 reattach acceptance needs a remote pane to fall MORE than
//! `REMOTE_ATTACH_TAIL_BYTES` behind before a relay kick, or the reattach arms
//! are indistinguishable. A live pane receives output as it is produced, so
//! the drift only grows behind a paused wire — and the production pause
//! (`EmissionGate` backpressure, the `Unwatched` tier) can be neither driven
//! nor observed by a harness. This route is that drive, and its answer is the
//! observation.
//!
//! It goes through `WireFlow` rather than calling `PaneIo::set_paused`
//! directly, because `WireFlow::sync` owns the "last successfully sent" state
//! under one lock and a bypass would desynchronise it — the silent-tab failure
//! its `sent` field records.
//!
//! # Local panes
//!
//! For a LOCAL PTY session the hold is a no-op on the wire:
//! `LocalPaneIo::set_paused` does nothing, so no output is held even though
//! `wire_paused` then reads `true` (it reports the state `WireFlow` recorded as
//! sent, and the local "send" trivially succeeds). `remote: false` in the
//! answer says which case the caller is in.
//!
//! # Where it exists
//!
//! Declared `#[cfg(debug_assertions)]` in `mcp/mod.rs` and merged under the
//! same gate in `mcp_api`, following `mcp::debug_graceful_exit`. That keeps it
//! out of release builds, but it is NOT test-only: the supervisor builds
//! runners in the dev profile, so every supervisor-built runner serves it. It
//! can silence a tab's output until released, which is why it is a
//! `CREDENTIAL_DOORS` entry in `origin_guard` (no browser origin but the
//! runner's own webview reaches it) and is on no relay allowlist.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use tauri::Manager;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::terminal::TerminalManager;

#[derive(Debug, Deserialize)]
pub struct WireHoldRequest {
    /// `true` pauses the wire (as far as this input decides it); `false`
    /// releases this input — the wire resumes only if no other input
    /// (backpressure gate, `Unwatched` tier) still wants it paused.
    pub held: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct WireHoldOutcome {
    pub terminal_id: String,
    pub held: bool,
    /// The wire state last successfully sent to the source.
    pub wire_paused: bool,
    /// Whether this terminal is a remote tab. `false` means the hold has no
    /// effect on output — see the module docs.
    pub remote: bool,
}

type HandlerError = (StatusCode, Json<ApiResponse<()>>);

/// Apply the hold to terminal `id` on `manager`; `None` when no such terminal.
pub fn apply_wire_hold(manager: &TerminalManager, id: &str, held: bool) -> Option<WireHoldOutcome> {
    let session = manager.get(id)?;
    let wire_paused = session.set_wire_held(held);
    Some(WireHoldOutcome {
        terminal_id: id.to_string(),
        held,
        wire_paused,
        remote: manager.remote_pane(id).is_some(),
    })
}

/// `POST /__debug/terminals/{id}/wire-hold` — see the module docs.
pub async fn wire_hold_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(req): Json<WireHoldRequest>,
) -> Result<Json<ApiResponse<WireHoldOutcome>>, HandlerError> {
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
    let outcome = apply_wire_hold(&manager, &id, req.held).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Terminal not found: {id}"))),
        )
    })?;
    tracing::info!(
        terminal_id = %outcome.terminal_id,
        held = outcome.held,
        wire_paused = outcome.wire_paused,
        remote = outcome.remote,
        "debug wire-hold applied"
    );
    Ok(Json(ApiResponse::success(outcome)))
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    axum::Router::new().route(
        "/__debug/terminals/{id}/wire-hold",
        axum::routing::post(wire_hold_handler),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_terminal_is_not_found() {
        let tm = TerminalManager::new();
        assert_eq!(apply_wire_hold(&tm, "no-such-terminal", true), None);
    }

    #[test]
    fn the_request_body_is_a_single_bool() {
        let r: WireHoldRequest = serde_json::from_str(r#"{"held":true}"#).unwrap();
        assert!(r.held);
        assert!(serde_json::from_str::<WireHoldRequest>("{}").is_err());
    }
}
