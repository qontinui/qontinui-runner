//! Design Review HTTP handlers.
//!
//! These are the "control mode" design endpoints that wrap a handful of
//! `design_*` IPC commands (computed styles, state styles, snapshot,
//! responsive snapshots, style-guide load/get/clear, audit). The macro-
//! generated `design/evaluate*` handlers stay in `mod.rs` because their
//! definitions live in a different idiom (`ipc_handler_post!` /
//! `ipc_handler_get!`) and don't fit the manual-handler shape extracted
//! here.
//!
//! The `/ai/design-audit` alias also lives here since it points at the
//! design-audit handler defined below.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use tracing::info;

use crate::mcp::types::{ApiResponse, ApiState};

use super::request::{ui_bridge_request_sync, wrap_ipc_result};

/// Get extended computed styles for a single element.
pub async fn ui_bridge_design_element_styles_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get element styles for {}", id);

    let payload = serde_json::json!({
        "elementId": id
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "design_get_element_styles", payload).await)
}

/// Get styles across interaction states (hover, focus, active, disabled).
pub async fn ui_bridge_design_state_styles_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get state styles for {}", id);

    let mut payload = serde_json::json!({
        "elementId": id
    });

    if let Some(Json(body)) = body {
        if let (Some(base), Some(extra)) = (payload.as_object_mut(), body.as_object()) {
            for (key, value) in extra {
                base.insert(key.clone(), value.clone());
            }
        }
    }

    wrap_ipc_result(ui_bridge_request_sync(&state, "design_get_state_styles", payload).await)
}

/// Get design snapshot for all or filtered elements.
pub async fn ui_bridge_design_snapshot_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get design snapshot");

    let payload = body.map(|Json(b)| b).unwrap_or(serde_json::json!({}));

    wrap_ipc_result(ui_bridge_request_sync(&state, "design_get_snapshot", payload).await)
}

/// Capture responsive snapshots at multiple viewport widths.
pub async fn ui_bridge_design_responsive_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get responsive snapshots");

    wrap_ipc_result(ui_bridge_request_sync(&state, "design_get_responsive", body).await)
}

/// Run a style audit against a loaded or provided style guide.
///
/// On inner-error responses (e.g. no style guide loaded), `wrap_ipc_result`
/// flattens the two-tier `{success:true, data:{success:false}}` envelope
/// into a flat HTTP 400 with `{success:false, error:"..."}`. The happy path
/// is unchanged: HTTP 200 with `{success:true, data:<audit report>}`. This
/// is the F2 fix originally landed here as `unwrap_inner_audit_error` and
/// now centralized in `wrap_ipc_result` (sweep 2026-04-22).
pub async fn ui_bridge_design_audit_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - run style audit");

    let payload = body.map(|Json(b)| b).unwrap_or(serde_json::json!({}));

    wrap_ipc_result(ui_bridge_request_sync(&state, "design_run_audit", payload).await)
}

/// Load a style guide for subsequent audits.
pub async fn ui_bridge_design_load_style_guide_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - load style guide");

    wrap_ipc_result(ui_bridge_request_sync(&state, "design_load_style_guide", body).await)
}

/// Get the currently loaded style guide.
pub async fn ui_bridge_design_get_style_guide_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get style guide");

    let payload = serde_json::json!({});

    wrap_ipc_result(ui_bridge_request_sync(&state, "design_get_style_guide", payload).await)
}

/// Clear the currently loaded style guide.
pub async fn ui_bridge_design_clear_style_guide_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - clear style guide");

    let payload = serde_json::json!({});

    wrap_ipc_result(ui_bridge_request_sync(&state, "design_clear_style_guide", payload).await)
}

/// Manual design-review routes (does not include the macro-generated
/// `design/evaluate*` family — those stay in `mod.rs`).
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/ui-bridge/control/design/element/{id}/styles",
            get(ui_bridge_design_element_styles_handler),
        )
        .route(
            "/ui-bridge/control/design/element/{id}/state-styles",
            post(ui_bridge_design_state_styles_handler),
        )
        .route(
            "/ui-bridge/control/design/snapshot",
            post(ui_bridge_design_snapshot_handler),
        )
        .route(
            "/ui-bridge/control/design/responsive",
            post(ui_bridge_design_responsive_handler),
        )
        .route(
            "/ui-bridge/control/design/audit",
            post(ui_bridge_design_audit_handler),
        )
        .route(
            "/ui-bridge/control/design/style-guide/load",
            post(ui_bridge_design_load_style_guide_handler),
        )
        .route(
            "/ui-bridge/control/design/style-guide",
            get(ui_bridge_design_get_style_guide_handler),
        )
        .route(
            "/ui-bridge/control/design/style-guide/clear",
            post(ui_bridge_design_clear_style_guide_handler),
        )
        // /ai/* alias for the design audit handler
        .route(
            "/ui-bridge/ai/design-audit",
            post(ui_bridge_design_audit_handler),
        )
}

/// Static (method, path) tuples corresponding to every route registered
/// by `routes()`. Concatenated into `route_manifest()` in `mod.rs` so the
/// `/ui-bridge/_routes` discovery endpoint stays in sync.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/control/design/element/{id}/styles"),
        (
            "POST",
            "/ui-bridge/control/design/element/{id}/state-styles",
        ),
        ("POST", "/ui-bridge/control/design/snapshot"),
        ("POST", "/ui-bridge/control/design/responsive"),
        ("POST", "/ui-bridge/control/design/audit"),
        ("POST", "/ui-bridge/control/design/style-guide/load"),
        ("GET", "/ui-bridge/control/design/style-guide"),
        ("POST", "/ui-bridge/control/design/style-guide/clear"),
        ("POST", "/ui-bridge/ai/design-audit"),
    ]
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod design_audit_envelope_tests {
    //! F2 — Regression tests for the two-tier envelope flattening on
    //! `POST /ui-bridge/ai/design-audit`. The handler can't be exercised
    //! end-to-end without a live frontend + Tauri runtime, so these tests
    //! lock down the centralized `wrap_ipc_result` helper that the handler
    //! delegates to (formerly a local `unwrap_inner_audit_error` helper —
    //! folded into `wrap_ipc_result` during the F2 sweep 2026-04-22).
    //!
    //! Same decision points as before: inner-failure → flat HTTP 400,
    //! healthy success → HTTP 200, missing/non-bool `success` field →
    //! treated as healthy.
    use crate::mcp::ui_bridge::request::wrap_ipc_result;
    use axum::http::StatusCode;
    use std::ops::Deref;

    #[test]
    fn inner_failure_with_explicit_error_is_unwrapped() {
        // The exact wire shape observed during manual testing 2026-04-25 when
        // posting to /ai/design-audit without first loading a style guide.
        let data = serde_json::json!({
            "error": "No style guide provided or loaded. Load one with design_load_style_guide first, or pass a guide in the request.",
            "requestId": "req-123",
            "success": false,
            "timestamp": 1745552000000_i64,
            "type": "design_run_audit",
        });
        let (status, body) = wrap_ipc_result(Ok(data)).expect_err("inner failure must flatten");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let msg = body.deref().error.as_deref().unwrap_or_default();
        assert!(msg.contains("No style guide provided or loaded"));
        assert!(msg.contains("design_load_style_guide"));
    }

    #[test]
    fn inner_failure_without_error_falls_back_to_default() {
        // Defensive: success:false but no error field shouldn't drop the
        // failure signal — we still return HTTP 400 with a generic message.
        let data = serde_json::json!({"success": false});
        let (status, body) =
            wrap_ipc_result(Ok(data)).expect_err("must still flag failure as 400");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let msg = body.deref().error.as_deref().unwrap_or_default();
        assert_eq!(msg, "UI bridge call failed");
    }

    #[test]
    fn healthy_success_payload_returns_200() {
        // The audit-report happy path: success:true with results. The
        // handler must NOT convert this into an HTTP 400.
        let data = serde_json::json!({
            "success": true,
            "report": {
                "violations": [],
                "elementsChecked": 42,
            },
        });
        let resp = wrap_ipc_result(Ok(data)).expect("healthy success must return Ok");
        assert!(resp.deref().success);
    }

    #[test]
    fn payload_without_success_field_returns_200() {
        // Some IPC responses don't include `success` at all — those should
        // pass through to the success arm rather than be misclassified.
        let data = serde_json::json!({"report": {"violations": []}});
        let resp = wrap_ipc_result(Ok(data)).expect("absent success must return Ok");
        assert!(resp.deref().success);
    }

    #[test]
    fn success_field_with_non_bool_value_returns_200() {
        // Robustness: if `success` is a string or number, treat it as
        // "shape unknown, don't flag failure" rather than panicking.
        let data = serde_json::json!({"success": "true"});
        let resp = wrap_ipc_result(Ok(data)).expect("string success must return Ok");
        assert!(resp.deref().success);
        let data = serde_json::json!({"success": 1});
        let resp = wrap_ipc_result(Ok(data)).expect("numeric success must return Ok");
        assert!(resp.deref().success);
    }
}
