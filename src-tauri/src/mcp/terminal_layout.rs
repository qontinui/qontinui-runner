//! `GET /terminal-layout`: this runner's terminal tabs and the sessions open
//! in each, read at one instant (plan
//! `2026-11-08-save-per-tab-runner-session-lists-on-sessions-page-kept-14-days`,
//! Phase 1).
//!
//! The body is `qontinui_types::terminal::TerminalLayout`, declared once in
//! qontinui-schemas and validated by qontinui-web with the generated model. It
//! is built by `terminal::layout::build_layout` over the same join
//! `terminal_list` uses: the live terminals, each terminal's CONFIRMED session
//! record, and the tab labels the webview registered. Every value the runner
//! does not know is `null`.
//!
//! # Why a module of its own
//!
//! Not `mcp::terminals`. Every route there is listed in its `route_entries()`,
//! and `terminals::tests::no_terminal_route_is_reachable_over_the_http_relay`
//! and `relay_path_policy::tests::no_terminal_route_is_allowlisted` refuse each
//! of them over the HTTP relay. This route IS relayed, as a GET only
//! (`relay_path_policy::RELAY_ALLOWED`), so it lives outside that table rather
//! than weakening those tests.
//!
//! # Over the relay
//!
//! qontinui-web's saved-session-lists capture reads it through the
//! `http_request` relay arm. A terminal title is PTY-derived session content
//! (OSC 0/2), so the relay withholds each session row's `title` (`null`, with
//! `titleWithheld: true`) when that session's tenant has terminal streaming
//! off (`backend_relay::redact_relay_content`). Ids, accounts, names, labels
//! and finished markers pass. A local caller gets the titles as they are.

use std::sync::Arc;

use axum::{extract::State, response::Json};
use qontinui_types::terminal::{TerminalLayout, TerminalLayoutRunner};
use tauri::Manager;

use crate::mcp::types::ApiState;
use crate::session::session_lifecycle_store::SessionLifecycleStore;
use crate::terminal::layout;

/// `GET /terminal-layout` — see the module docs.
pub async fn terminal_layout_handler(State(state): State<Arc<ApiState>>) -> Json<TerminalLayout> {
    let terminal_manager = crate::mcp::terminals::get_terminal_manager(&state);
    let store = state.app_handle.try_state::<Arc<SessionLifecycleStore>>();
    let joined = layout::join_terminals(
        terminal_manager.list(),
        store.as_deref().map(Arc::as_ref),
        |id| terminal_manager.get(id).and_then(|s| s.spawn_name()),
    );
    let labels = layout::page_labels().snapshot();
    Json(layout::build_layout(
        runner_identity(&state),
        layout::rfc3339(chrono::Utc::now()),
        joined,
        &labels,
    ))
}

/// The runner instance as it reports itself. Read here, outside the pure
/// builder: the device id comes from `~/.qontinui/machine.json`.
///
/// - `instanceName`: the name this instance registers under on its relay
///   socket (`backend_relay::reported_instance_name`), so a capture names the
///   same runner the web already lists.
/// - `port`: the port this instance's API is bound to; `null` before binding.
/// - `buildId`: the same value as `/health` `buildId`.
/// - `deviceId`: the machine's canonical device id; `null` when it cannot be
///   read. Never minted here.
fn runner_identity(state: &ApiState) -> TerminalLayoutRunner {
    let port = crate::mcp::types::runner_api_port(&state.app_state);
    TerminalLayoutRunner {
        instance_name: Some(crate::mcp::backend_relay::reported_instance_name(
            crate::instance::instance_name(),
        )),
        port: (port != 0).then_some(port),
        build_id: Some(env!("RUNNER_BUILD_ID").to_string()),
        device_id: crate::machine_identity::read_device_id().ok(),
    }
}

/// Create routes for this module.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::get;

    axum::Router::new().route("/terminal-layout", get(terminal_layout_handler))
}
