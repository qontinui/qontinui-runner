//! Design evaluation routes (`/control/design/evaluate/*`).
//!
//! Separate from the manual-handler-based design endpoints in `design.rs`.
//! These are the macro-generated IPC proxies that forward scoring / baseline
//! comparison work to the webview SDK: single-sample evaluate, baseline
//! evaluate, context enumeration, and diff-between-two-evals.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Json};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::request::ui_bridge_request_sync;
use super::{ipc_handler_get, ipc_handler_post};

ipc_handler_post!(ui_bridge_design_evaluate_handler, "design_evaluate");
ipc_handler_post!(
    ui_bridge_design_evaluate_baseline_handler,
    "design_evaluate_baseline"
);
ipc_handler_get!(
    ui_bridge_design_evaluate_contexts_handler,
    "design_evaluate_contexts"
);
ipc_handler_post!(
    ui_bridge_design_evaluate_diff_handler,
    "design_evaluate_diff"
);

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/ui-bridge/control/design/evaluate",
            post(ui_bridge_design_evaluate_handler),
        )
        .route(
            "/ui-bridge/control/design/evaluate/baseline",
            post(ui_bridge_design_evaluate_baseline_handler),
        )
        .route(
            "/ui-bridge/control/design/evaluate/contexts",
            get(ui_bridge_design_evaluate_contexts_handler),
        )
        .route(
            "/ui-bridge/control/design/evaluate/diff",
            post(ui_bridge_design_evaluate_diff_handler),
        )
        // SDK top-level /design/evaluate/* aliases — same handlers as the
        // /control/design/evaluate/* family above.
        .route(
            "/ui-bridge/design/evaluate",
            post(ui_bridge_design_evaluate_handler),
        )
        .route(
            "/ui-bridge/design/evaluate/baseline",
            post(ui_bridge_design_evaluate_baseline_handler),
        )
        .route(
            "/ui-bridge/design/evaluate/contexts",
            get(ui_bridge_design_evaluate_contexts_handler),
        )
        .route(
            "/ui-bridge/design/evaluate/diff",
            post(ui_bridge_design_evaluate_diff_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("POST", "/ui-bridge/control/design/evaluate"),
        ("POST", "/ui-bridge/control/design/evaluate/baseline"),
        ("GET", "/ui-bridge/control/design/evaluate/contexts"),
        ("POST", "/ui-bridge/control/design/evaluate/diff"),
        // SDK top-level /design/evaluate/* aliases
        ("POST", "/ui-bridge/design/evaluate"),
        ("POST", "/ui-bridge/design/evaluate/baseline"),
        ("GET", "/ui-bridge/design/evaluate/contexts"),
        ("POST", "/ui-bridge/design/evaluate/diff"),
    ]
}
