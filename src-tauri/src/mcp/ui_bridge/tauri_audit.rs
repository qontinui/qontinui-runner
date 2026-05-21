//! Tauri command-fire attestation endpoint.
//!
//! Surfaces the bounded ring buffer maintained by
//! `crate::tauri_command_audit` over HTTP so manual-test operators can
//! verify that a non-allowlisted Tauri command fired without exposing its
//! args or return values. See the module-level doc on
//! `crate::tauri_command_audit` for the motivation (2026-05-21
//! `/manual-test` Phase 8 `get_test_auto_login` verification gap).
//!
//! Runner-only by design: there is no UI Bridge SDK consumer for this
//! endpoint (mobile / web don't have Tauri commands to record). The route
//! is therefore intentionally absent from the SDK's `UI_BRIDGE_ROUTES`
//! table and listed in `mod.rs::sdk_manifest_routes_are_exposed_by_runner`'s
//! `runner_only_baseline` allow-list.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    response::Json,
};
use serde::Deserialize;
use tracing::debug;

use crate::mcp::types::{ApiResponse, ApiState};
use crate::tauri_command_audit::{self, CommandFireEvent};

/// Query parameters for the audit-trail endpoint.
///
/// Both fields are optional:
///   - `since` filters by strict `fired_at_unix_ms > since` (so the operator
///     can record a startup timestamp `T0` and ask "did this fire after
///     React mount?")
///   - `command` filters by exact-equals on the command name
#[derive(Debug, Deserialize)]
pub struct TauriCommandHistoryQuery {
    /// Unix epoch millis; only events strictly greater than this are returned.
    #[serde(default)]
    pub since: Option<u64>,
    /// Tauri command name; only events matching this exact string are
    /// returned.
    #[serde(default)]
    pub command: Option<String>,
}

/// Response payload — a flat events array wrapped in the standard envelope.
#[derive(Debug, serde::Serialize)]
pub struct TauriCommandHistoryResponse {
    pub events: Vec<CommandFireEvent>,
}

/// `GET /ui-bridge/control/tauri-command-history`
///
/// Returns the (filtered) snapshot of the global Tauri command-fire ring
/// buffer. Never fails — an empty or lock-poisoned buffer just yields
/// `events: []`.
pub async fn ui_bridge_tauri_command_history_handler(
    State(_state): State<Arc<ApiState>>,
    Query(query): Query<TauriCommandHistoryQuery>,
) -> Json<ApiResponse<TauriCommandHistoryResponse>> {
    let events = tauri_command_audit::query(query.since, query.command.as_deref());
    debug!(
        since = ?query.since,
        command = ?query.command,
        count = events.len(),
        "ui_bridge tauri-command-history query",
    );
    Json(ApiResponse::success(TauriCommandHistoryResponse { events }))
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::get;
    axum::Router::new().route(
        "/ui-bridge/control/tauri-command-history",
        get(ui_bridge_tauri_command_history_handler),
    )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[("GET", "/ui-bridge/control/tauri-command-history")]
}
