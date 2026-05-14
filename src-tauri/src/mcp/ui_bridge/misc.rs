//! Miscellaneous UI Bridge control + debug routes.
//!
//! A grab-bag of thin IPC proxies that didn't warrant their own per-family
//! submodule. Each handler is a tiny call to `ui_bridge_request_sync` that
//! forwards a message to the webview SDK.
//!
//! Families covered here:
//!   - Undo/redo + undo-state
//!   - Spec store read-only access (`/control/specs`, `/control/spec/{id}`)
//!   - Runner-specific tab navigation + storage clearing
//!   - Single-component state read
//!   - Page scroll
//!   - Performance entries (read + clear)
//!   - Annotations import
//!   - Debug helpers (element-tree, highlight-element)

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use tracing::info;

use crate::mcp::types::{ApiResponse, ApiState};

use super::request::{ui_bridge_request_sync, wrap_ipc_result};
use super::{
    ipc_handler_get, ipc_handler_path_get, ipc_handler_post, ipc_handler_post_bumps_mutation,
};

// ============================================================================
// Undo / Redo
// ============================================================================

/// Get undo/redo state from the UI Bridge.
pub async fn ui_bridge_get_undo_state_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting undo state");

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_undo_state", serde_json::json!({})).await)
}

/// Execute undo via the UI Bridge.
pub async fn ui_bridge_undo_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Undo");

    wrap_ipc_result(ui_bridge_request_sync(&state, "undo", serde_json::json!({})).await)
}

/// Execute redo via the UI Bridge.
pub async fn ui_bridge_redo_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Redo");

    wrap_ipc_result(ui_bridge_request_sync(&state, "redo", serde_json::json!({})).await)
}

// ============================================================================
// Spec store (read-only)
// ============================================================================

/// Get all loaded specs from the SpecStore.
pub async fn ui_bridge_get_specs_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting all specs");

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_specs", serde_json::json!({})).await)
}

/// Get a specific spec by ID from the SpecStore.
pub async fn ui_bridge_get_spec_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting spec {}", id);

    wrap_ipc_result(
        ui_bridge_request_sync(&state, "get_spec", serde_json::json!({ "specId": id })).await,
    )
}

/// Run all assertions in a spec against the current page snapshot.
///
/// Forwards to the frontend SpecExecutor via IPC. Returns a
/// `SpecExecutionResult` with per-group and per-assertion pass/fail counts.
/// Accepts an optional `options` body (maps to `SpecExecutionOptions`) to
/// filter by groupIds, assertionIds, severity, or category.
pub async fn ui_bridge_run_spec_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: Option<axum::extract::Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Running spec {}", id);

    let options = body
        .and_then(|b| b.0.get("options").cloned())
        .unwrap_or(serde_json::Value::Null);

    wrap_ipc_result(
        ui_bridge_request_sync(
            &state,
            "run_spec",
            serde_json::json!({ "specId": id, "options": options }),
        )
        .await,
    )
}

// ============================================================================
// Macro-generated IPC proxies
// ============================================================================

// Runner-specific: tab navigation and storage management
// navigate_tab + scroll_page can change rendered pixels — bump
// vision_mutation_id so subsequent `/vision/capture` calls re-render
// instead of hitting a stale cache entry. clear_storage and
// annotations_import are excluded: storage-only mutations don't
// directly affect rendered pixels, and annotation imports go through
// the React layer whose own DOM changes will be picked up by the
// frontend mutationOccurred signal.
ipc_handler_post_bumps_mutation!(ui_bridge_navigate_tab_handler, "navigate_tab");
ipc_handler_post!(ui_bridge_clear_storage_handler, "clear_storage");

// Component state
ipc_handler_path_get!(
    ui_bridge_get_component_state_handler,
    "get_component_state",
    "componentId"
);

// Page scroll: changes the viewport content → rendered pixels change.
ipc_handler_post_bumps_mutation!(ui_bridge_scroll_page_handler, "scroll_page");

// Performance entries
ipc_handler_get!(
    ui_bridge_get_performance_entries_handler,
    "get_performance_entries"
);
ipc_handler_get!(
    ui_bridge_clear_performance_entries_handler,
    "clear_performance_entries"
);

// Annotations import
ipc_handler_post!(ui_bridge_annotations_import_handler, "annotations_import");

// Debug
ipc_handler_get!(ui_bridge_get_element_tree_handler, "get_element_tree");
ipc_handler_path_get!(
    ui_bridge_highlight_element_handler,
    "highlight_element",
    "elementId"
);

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        // Specs
        .route("/ui-bridge/control/specs", get(ui_bridge_get_specs_handler))
        .route(
            "/ui-bridge/control/spec/{id}",
            get(ui_bridge_get_spec_handler),
        )
        .route(
            "/ui-bridge/control/spec/{id}/run",
            post(ui_bridge_run_spec_handler),
        )
        // Undo/Redo awareness
        .route(
            "/ui-bridge/control/undo-state",
            get(ui_bridge_get_undo_state_handler),
        )
        .route("/ui-bridge/control/undo", post(ui_bridge_undo_handler))
        .route("/ui-bridge/control/redo", post(ui_bridge_redo_handler))
        // Component state
        .route(
            "/ui-bridge/control/component/{id}/state",
            get(ui_bridge_get_component_state_handler),
        )
        // Page scroll
        .route(
            "/ui-bridge/control/page/scroll",
            post(ui_bridge_scroll_page_handler),
        )
        // Performance entries
        .route(
            "/ui-bridge/control/performance-entries",
            get(ui_bridge_get_performance_entries_handler),
        )
        .route(
            "/ui-bridge/control/performance-entries/clear",
            post(ui_bridge_clear_performance_entries_handler),
        )
        // Annotations import
        .route(
            "/ui-bridge/control/annotations/import",
            post(ui_bridge_annotations_import_handler),
        )
        // SDK top-level alias — `/annotations/import` (SDK's importAnnotations).
        .route(
            "/ui-bridge/annotations/import",
            post(ui_bridge_annotations_import_handler),
        )
        // Debug
        .route(
            "/ui-bridge/debug/element-tree",
            get(ui_bridge_get_element_tree_handler),
        )
        .route(
            "/ui-bridge/debug/highlight/{id}",
            post(ui_bridge_highlight_element_handler),
        )
        // Runner-specific: tab navigation and storage management
        .route(
            "/ui-bridge/control/navigate-tab",
            post(ui_bridge_navigate_tab_handler),
        )
        .route(
            "/ui-bridge/control/clear-storage",
            post(ui_bridge_clear_storage_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/control/specs"),
        ("GET", "/ui-bridge/control/spec/{id}"),
        ("POST", "/ui-bridge/control/spec/{id}/run"),
        ("GET", "/ui-bridge/control/undo-state"),
        ("POST", "/ui-bridge/control/undo"),
        ("POST", "/ui-bridge/control/redo"),
        ("GET", "/ui-bridge/control/component/{id}/state"),
        ("POST", "/ui-bridge/control/page/scroll"),
        ("GET", "/ui-bridge/control/performance-entries"),
        ("POST", "/ui-bridge/control/performance-entries/clear"),
        ("POST", "/ui-bridge/control/annotations/import"),
        ("POST", "/ui-bridge/annotations/import"),
        ("GET", "/ui-bridge/debug/element-tree"),
        ("POST", "/ui-bridge/debug/highlight/{id}"),
        ("POST", "/ui-bridge/control/navigate-tab"),
        ("POST", "/ui-bridge/control/clear-storage"),
    ]
}
