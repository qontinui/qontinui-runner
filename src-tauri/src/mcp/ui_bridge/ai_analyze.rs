//! AI analysis + semantic diff routes.
//!
//! Macro-generated handlers that proxy AI-side operations (analysis of
//! tabular/region/structured data, cross-app comparison, recovery attempts,
//! semantic search, semantic diff, media compare, pixel-accurate image diff)
//! to the webview SDK via `ui_bridge_request_sync`.
//!
//! Namespace note: most handlers sit under `/ai/*`; `image-diff` also has a
//! `/control/ai/image-diff` alias retained for backwards compatibility with
//! the `compareVisualRegression` caller path.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Json};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::request::ui_bridge_request_sync;
use super::{ipc_handler_get, ipc_handler_post};

// Semantic search & diff
ipc_handler_post!(ui_bridge_ai_semantic_search_handler, "ai_semantic_search");
ipc_handler_get!(ui_bridge_ai_diff_handler, "ai_diff");

// AI analysis
ipc_handler_post!(ui_bridge_ai_analyze_data_handler, "ai_analyze_data");
ipc_handler_post!(ui_bridge_ai_analyze_regions_handler, "ai_analyze_regions");
ipc_handler_post!(
    ui_bridge_ai_analyze_structured_handler,
    "ai_analyze_structured_data"
);
ipc_handler_post!(
    ui_bridge_ai_analyze_cross_app_handler,
    "ai_analyze_cross_app"
);
ipc_handler_post!(ui_bridge_ai_recovery_attempt_handler, "ai_recovery_attempt");

// Media compare
ipc_handler_post!(ui_bridge_media_compare_handler, "media_compare");

// Pixel-accurate image diff (compareVisualRegression alias)
ipc_handler_post!(ui_bridge_image_diff_handler, "image_diff");

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        // AI semantic search & diff
        .route(
            "/ui-bridge/ai/semantic-search",
            post(ui_bridge_ai_semantic_search_handler),
        )
        .route("/ui-bridge/ai/diff", get(ui_bridge_ai_diff_handler))
        // AI analysis
        .route(
            "/ui-bridge/ai/analyze/data",
            post(ui_bridge_ai_analyze_data_handler),
        )
        .route(
            "/ui-bridge/ai/analyze/regions",
            post(ui_bridge_ai_analyze_regions_handler),
        )
        .route(
            "/ui-bridge/ai/analyze/structured-data",
            post(ui_bridge_ai_analyze_structured_handler),
        )
        .route(
            "/ui-bridge/ai/analyze/cross-app-compare",
            post(ui_bridge_ai_analyze_cross_app_handler),
        )
        .route(
            "/ui-bridge/ai/recovery/attempt",
            post(ui_bridge_ai_recovery_attempt_handler),
        )
        // Media compare
        .route(
            "/ui-bridge/ai/media/compare",
            post(ui_bridge_media_compare_handler),
        )
        // Pixel-accurate image diff (canonical visual regression)
        .route(
            "/ui-bridge/ai/image-diff",
            post(ui_bridge_image_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/image-diff",
            post(ui_bridge_image_diff_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("POST", "/ui-bridge/ai/semantic-search"),
        ("GET", "/ui-bridge/ai/diff"),
        ("POST", "/ui-bridge/ai/analyze/data"),
        ("POST", "/ui-bridge/ai/analyze/regions"),
        ("POST", "/ui-bridge/ai/analyze/structured-data"),
        ("POST", "/ui-bridge/ai/analyze/cross-app-compare"),
        ("POST", "/ui-bridge/ai/recovery/attempt"),
        ("POST", "/ui-bridge/ai/media/compare"),
        ("POST", "/ui-bridge/ai/image-diff"),
        ("POST", "/ui-bridge/control/ai/image-diff"),
    ]
}
