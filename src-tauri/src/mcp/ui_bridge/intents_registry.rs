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

/// `DELETE /ui-bridge/control/intent/{name}` — delete a registered intent by
/// name. SDK contract: `relayCommand('deleteIntent', { name })` returning
/// `APIResponse<{ deleted: boolean }>`.
///
/// One-off shape (DELETE with path param, no body), so written inline rather
/// than adding an `ipc_handler_path_delete!` macro for a single caller. Mirrors
/// `ipc_handler_path_get!` body but uses `name` as the IPC param key.
pub async fn ui_bridge_delete_intent_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let payload = serde_json::json!({ "params": { "name": name } });
    super::request::wrap_ipc_result(ui_bridge_request_sync(&state, "delete_intent", payload).await)
}

/// `POST /ui-bridge/control/intent/{name}/execute` — execute a registered
/// intent addressed by name in the URL. SDK contract:
/// `POST /control/intent/:name/execute` (`types.ts`, handler `executeIntent`).
///
/// The runner's older `POST /control/intents/execute` takes the intent name in
/// the JSON body; this thin handler injects the `{name}` path segment into the
/// body and dispatches the same `execute_intent` IPC, so both shapes honor the
/// SDK contract without duplicating intent logic in the webview.
pub async fn ui_bridge_execute_intent_by_name_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut params = body;
    if let Some(obj) = params.as_object_mut() {
        obj.insert("name".to_string(), serde_json::json!(name));
    } else {
        params = serde_json::json!({ "name": name });
    }
    let payload = serde_json::json!({ "params": params });
    super::request::wrap_ipc_result(ui_bridge_request_sync(&state, "execute_intent", payload).await)
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{delete, get, post};
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
        .route(
            "/ui-bridge/control/intent/{name}",
            delete(ui_bridge_delete_intent_handler),
        )
        .route(
            "/ui-bridge/control/intent/{name}/execute",
            post(ui_bridge_execute_intent_by_name_handler),
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
        ("DELETE", "/ui-bridge/control/intent/{name}"),
        ("POST", "/ui-bridge/control/intent/{name}/execute"),
        // SDK /ai/intents/* aliases
        ("GET", "/ui-bridge/ai/intents"),
        ("POST", "/ui-bridge/ai/intents/register"),
        ("POST", "/ui-bridge/ai/intents/find"),
        ("POST", "/ui-bridge/ai/intents/execute"),
        ("POST", "/ui-bridge/ai/intents/execute-from-query"),
    ]
}
