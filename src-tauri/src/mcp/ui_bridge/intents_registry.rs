//! Intent registry routes (`/control/intents/*`).
//!
//! This is the intent **registry** surface — list, register, find, execute,
//! and execute-from-query — distinct from the wait-for-intent polling
//! handlers in `intents.rs`. Each endpoint proxies to the webview SDK via
//! `ui_bridge_request_sync` using the shared `ipc_handler_*` macros.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Json};

use crate::mcp::types::{ApiResponse, ApiState};

use super::request::ui_bridge_request_sync;
use super::{ipc_handler_get, ipc_handler_post};

ipc_handler_get!(ui_bridge_get_intents_handler, "get_intents");
ipc_handler_post!(ui_bridge_register_intent_handler, "register_intent");
ipc_handler_post!(ui_bridge_find_intent_handler, "find_intent");
ipc_handler_post!(ui_bridge_execute_intent_handler, "execute_intent");
ipc_handler_post!(
    ui_bridge_execute_intent_from_query_handler,
    "execute_intent_from_query"
);

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/ui-bridge/control/intents",
            get(ui_bridge_get_intents_handler).post(ui_bridge_register_intent_handler),
        )
        .route(
            "/ui-bridge/control/intents/find",
            post(ui_bridge_find_intent_handler),
        )
        .route(
            "/ui-bridge/control/intents/execute",
            post(ui_bridge_execute_intent_handler),
        )
        .route(
            "/ui-bridge/control/intents/execute-from-query",
            post(ui_bridge_execute_intent_from_query_handler),
        )
        // SDK /ai/intents/* aliases — same handlers as the /control/intents/*
        // family above. /ai/intents GET = listIntents, /ai/intents/register POST
        // = registerIntent (the SDK splits register out into its own path; the
        // runner's /control/intents POST does the same job).
        .route("/ui-bridge/ai/intents", get(ui_bridge_get_intents_handler))
        .route(
            "/ui-bridge/ai/intents/register",
            post(ui_bridge_register_intent_handler),
        )
        .route(
            "/ui-bridge/ai/intents/find",
            post(ui_bridge_find_intent_handler),
        )
        .route(
            "/ui-bridge/ai/intents/execute",
            post(ui_bridge_execute_intent_handler),
        )
        .route(
            "/ui-bridge/ai/intents/execute-from-query",
            post(ui_bridge_execute_intent_from_query_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/control/intents"),
        ("POST", "/ui-bridge/control/intents"),
        ("POST", "/ui-bridge/control/intents/find"),
        ("POST", "/ui-bridge/control/intents/execute"),
        ("POST", "/ui-bridge/control/intents/execute-from-query"),
        // SDK /ai/intents/* aliases
        ("GET", "/ui-bridge/ai/intents"),
        ("POST", "/ui-bridge/ai/intents/register"),
        ("POST", "/ui-bridge/ai/intents/find"),
        ("POST", "/ui-bridge/ai/intents/execute"),
        ("POST", "/ui-bridge/ai/intents/execute-from-query"),
    ]
}
