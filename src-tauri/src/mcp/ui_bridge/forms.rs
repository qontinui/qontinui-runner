//! Form-state and clipboard HTTP handlers.
//!
//! Forms cover `/control/forms`, `/control/fill`, `/control/forms/snapshot`,
//! `/control/forms/diff` plus the `/ai/forms` and `/ai/fill-form` aliases.
//! Clipboard handlers live here too because they're a tiny, related family
//! that's only ever called alongside form interactions in practice.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Json};
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::request::ui_bridge_request_sync;
use super::types::ClipboardWriteRequest;

/// Read the current system clipboard content.
pub async fn ui_bridge_clipboard_read_handler(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<serde_json::Value>> {
    info!("UI Bridge API: Reading clipboard");

    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            let text = clipboard.get_text().ok();
            let has_text = text.is_some();
            Json(ApiResponse::success(serde_json::json!({
                "text": text,
                "formats": if has_text { vec!["text/plain"] } else { vec![] as Vec<&str> },
            })))
        }
        Err(e) => {
            error!("UI Bridge API: Clipboard read failed: {}", e);
            Json(ApiResponse::error(format!("Clipboard read failed: {}", e)))
        }
    }
}

/// Write text to the system clipboard.
pub async fn ui_bridge_clipboard_write_handler(
    State(_state): State<Arc<ApiState>>,
    Json(body): Json<ClipboardWriteRequest>,
) -> Json<ApiResponse<serde_json::Value>> {
    info!("UI Bridge API: Writing to clipboard");

    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            if let Some(html) = &body.html {
                // Write both HTML and plain text alternatives
                let alt_text = body.text.clone();
                match clipboard.set_html(html.as_str(), Some(&alt_text)) {
                    Ok(()) => Json(ApiResponse::success(serde_json::json!({
                        "written": true,
                        "formats": ["text/html", "text/plain"],
                    }))),
                    Err(e) => {
                        error!("UI Bridge API: Clipboard HTML write failed: {}", e);
                        Json(ApiResponse::error(format!("Clipboard write failed: {}", e)))
                    }
                }
            } else {
                match clipboard.set_text(&body.text) {
                    Ok(()) => Json(ApiResponse::success(serde_json::json!({
                        "written": true,
                        "formats": ["text/plain"],
                    }))),
                    Err(e) => {
                        error!("UI Bridge API: Clipboard write failed: {}", e);
                        Json(ApiResponse::error(format!("Clipboard write failed: {}", e)))
                    }
                }
            }
        }
        Err(e) => {
            error!("UI Bridge API: Clipboard init failed: {}", e);
            Json(ApiResponse::error(format!(
                "Clipboard initialization failed: {}",
                e
            )))
        }
    }
}

/// Get form state awareness data from the UI Bridge.
pub async fn ui_bridge_get_forms_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting form state");

    match ui_bridge_request_sync(&state, "get_forms", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Smart form fill action via the UI Bridge.
pub async fn ui_bridge_fill_form_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Fill form");

    match ui_bridge_request_sync(&state, "fill_form", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Fill form failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Capture a form state snapshot via the UI Bridge.
pub async fn ui_bridge_snapshot_forms_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Snapshot forms");

    match ui_bridge_request_sync(&state, "snapshot_forms", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Snapshot forms failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Diff two form snapshots via the UI Bridge.
pub async fn ui_bridge_diff_forms_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Diff forms");

    match ui_bridge_request_sync(&state, "diff_forms", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Diff forms failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Form state + fill + snapshot/diff routes (control + ai aliases) plus
/// the clipboard read/write pair.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use super::routing::add_dual;
    use axum::routing::{get, post};
    let router = axum::Router::new();
    // forms: identical handler under /control + /ai.
    let router = add_dual!(router, get, "forms", ui_bridge_get_forms_handler);
    router
        // Form state awareness
        .route("/ui-bridge/control/fill", post(ui_bridge_fill_form_handler))
        .route(
            "/ui-bridge/control/forms/snapshot",
            post(ui_bridge_snapshot_forms_handler),
        )
        .route(
            "/ui-bridge/control/forms/diff",
            post(ui_bridge_diff_forms_handler),
        )
        // /ai/* alias with a DIFFERENT tail (fill-form vs fill) — not a true
        // alias pair, keep as a second /ai/ registration.
        .route("/ui-bridge/ai/fill-form", post(ui_bridge_fill_form_handler))
        // Clipboard
        .route(
            "/ui-bridge/control/clipboard",
            get(ui_bridge_clipboard_read_handler).post(ui_bridge_clipboard_write_handler),
        )
}

/// Static (method, path) tuples corresponding to every route registered
/// by `routes()`. Concatenated into `route_manifest()` in `mod.rs`.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/control/forms"),
        ("POST", "/ui-bridge/control/fill"),
        ("POST", "/ui-bridge/control/forms/snapshot"),
        ("POST", "/ui-bridge/control/forms/diff"),
        ("GET", "/ui-bridge/ai/forms"),
        ("POST", "/ui-bridge/ai/fill-form"),
        ("GET", "/ui-bridge/control/clipboard"),
        ("POST", "/ui-bridge/control/clipboard"),
    ]
}
