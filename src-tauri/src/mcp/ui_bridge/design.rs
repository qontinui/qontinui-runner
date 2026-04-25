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
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::request::ui_bridge_request_sync;

/// Get extended computed styles for a single element.
pub async fn ui_bridge_design_element_styles_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get element styles for {}", id);

    let payload = serde_json::json!({
        "elementId": id
    });

    match ui_bridge_request_sync(&state, "design_get_element_styles", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design element styles failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
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

    match ui_bridge_request_sync(&state, "design_get_state_styles", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design state styles failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get design snapshot for all or filtered elements.
pub async fn ui_bridge_design_snapshot_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get design snapshot");

    let payload = body.map(|Json(b)| b).unwrap_or(serde_json::json!({}));

    match ui_bridge_request_sync(&state, "design_get_snapshot", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design snapshot failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Capture responsive snapshots at multiple viewport widths.
pub async fn ui_bridge_design_responsive_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get responsive snapshots");

    match ui_bridge_request_sync(&state, "design_get_responsive", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design responsive failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// F2 — Two-tier envelope flattening for design-audit responses.
///
/// The frontend handler in `useDesignEvents.ts` returns
/// `{success:false, error:"..."}` inside the IPC `data` payload when the audit
/// can't run (e.g. no style guide loaded). Without unwrapping, the HTTP
/// response would carry the misleading shape
/// `{success:true, data:{success:false, error:"..."}}` — outer success says
/// "IPC delivered", inner success says "the operation actually failed".
/// Callers had to defensively peek at `data.success` to detect failure.
///
/// This helper detects the inner-failure shape and returns the inner error
/// string so the route handler can convert it to a flat HTTP 400 with
/// `{success:false, error:"..."}`. Returns `None` on a healthy success
/// payload (or when `success` is absent — some response shapes don't include
/// it and we shouldn't misclassify those as failures).
pub(super) fn unwrap_inner_audit_error(data: &serde_json::Value) -> Option<String> {
    if data.get("success").and_then(|v| v.as_bool()) == Some(false) {
        let msg = data
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("design audit failed")
            .to_string();
        Some(msg)
    } else {
        None
    }
}

/// Run a style audit against a loaded or provided style guide.
///
/// On inner-error responses (see `unwrap_inner_audit_error`), this returns
/// HTTP 400 with a flat `{success:false, error:"..."}` body instead of the
/// historical two-tier envelope. The happy path is unchanged: HTTP 200 with
/// `{success:true, data:<audit report>}`.
pub async fn ui_bridge_design_audit_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - run style audit");

    let payload = body.map(|Json(b)| b).unwrap_or(serde_json::json!({}));

    match ui_bridge_request_sync(&state, "design_run_audit", payload).await {
        Ok(data) => {
            // F2: flatten two-tier envelope. The frontend handler emits
            // `{success:false, error:...}` inside `data` for soft failures
            // like "no style guide loaded". Surface that as a flat HTTP 400
            // so callers don't have to inspect `data.success` themselves.
            if let Some(inner_err) = unwrap_inner_audit_error(&data) {
                error!("UI Bridge API: Design audit inner failure: {}", inner_err);
                return Err((StatusCode::BAD_REQUEST, Json(api_error(inner_err))));
            }
            Ok(Json(ApiResponse::success(data)))
        }
        Err(e) => {
            error!("UI Bridge API: Design audit failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Load a style guide for subsequent audits.
pub async fn ui_bridge_design_load_style_guide_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - load style guide");

    match ui_bridge_request_sync(&state, "design_load_style_guide", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design load style guide failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get the currently loaded style guide.
pub async fn ui_bridge_design_get_style_guide_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get style guide");

    let payload = serde_json::json!({});

    match ui_bridge_request_sync(&state, "design_get_style_guide", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design get style guide failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Clear the currently loaded style guide.
pub async fn ui_bridge_design_clear_style_guide_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - clear style guide");

    let payload = serde_json::json!({});

    match ui_bridge_request_sync(&state, "design_clear_style_guide", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design clear style guide failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
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
    //! lock down the pure unwrap helper that the handler delegates to.
    //!
    //! Same shape as `page_navigate_mode_tests`: poke the helper through its
    //! decision points (inner-failure → Some(msg), healthy success → None,
    //! missing fields → safe defaults).
    use super::unwrap_inner_audit_error;

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
        let err = unwrap_inner_audit_error(&data).expect("inner failure must unwrap");
        assert!(err.contains("No style guide provided or loaded"));
        assert!(err.contains("design_load_style_guide"));
    }

    #[test]
    fn inner_failure_without_error_falls_back_to_default() {
        // Defensive: success:false but no error field shouldn't drop the
        // failure signal — we still return Some(_) so the handler sends a 400.
        let data = serde_json::json!({"success": false});
        let err = unwrap_inner_audit_error(&data).expect("must still flag failure");
        assert_eq!(err, "design audit failed");
    }

    #[test]
    fn healthy_success_payload_returns_none() {
        // The audit-report happy path: success:true with results. The
        // handler must NOT convert this into an HTTP 400.
        let data = serde_json::json!({
            "success": true,
            "report": {
                "violations": [],
                "elementsChecked": 42,
            },
        });
        assert!(unwrap_inner_audit_error(&data).is_none());
    }

    #[test]
    fn payload_without_success_field_returns_none() {
        // Some IPC responses don't include `success` at all — those should
        // pass through to the success arm rather than be misclassified.
        let data = serde_json::json!({"report": {"violations": []}});
        assert!(unwrap_inner_audit_error(&data).is_none());
    }

    #[test]
    fn success_field_with_non_bool_value_returns_none() {
        // Robustness: if `success` is a string or number, treat it as
        // "shape unknown, don't flag failure" rather than panicking.
        let data = serde_json::json!({"success": "true"});
        assert!(unwrap_inner_audit_error(&data).is_none());
        let data = serde_json::json!({"success": 1});
        assert!(unwrap_inner_audit_error(&data).is_none());
    }
}
