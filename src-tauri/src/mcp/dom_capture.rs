//! DOM Capture handlers for MCP API
//!
//! Provides HTTP handlers for managing DOM captures:
//! list, get, get HTML content, receive DOM capture data.

use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Json},
};
use tracing::{error, info};

use crate::dom_capture::{
    DomCapture, DomCaptureLogger, DomCaptureSource, DomCaptureTrigger, ReceiveExtensionDomRequest,
};
use crate::mcp::types::{api_error, ApiResponse};

// ============================================================================
// Handlers
// ============================================================================

/// List all DOM captures
pub async fn list_dom_captures(
) -> Result<Json<ApiResponse<Vec<DomCapture>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let captures = DomCaptureLogger::list_captures();
    Ok(Json(ApiResponse::success(captures)))
}

/// Get a specific DOM capture by ID
pub async fn get_dom_capture(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<DomCapture>>, (StatusCode, Json<ApiResponse<()>>)> {
    match DomCaptureLogger::get_capture(&id) {
        Some(capture) => Ok(Json(ApiResponse::success(capture))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("DOM capture not found: {}", id))),
        )),
    }
}

/// Get the HTML content of a DOM capture
pub async fn get_dom_capture_html(
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    match DomCaptureLogger::get_capture_html(&id) {
        Ok(html) => Ok(captured_html_response(html)),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Serve captured page HTML. Captures arrive from the browser extension on
/// ARBITRARY sites, and this route serves them from the runner's own loopback
/// origin — which `mcp::origin_guard` admits as first-party. Any script in the
/// capture would run AS that origin. `Content-Security-Policy: sandbox` gives
/// the document an opaque origin (its requests carry `Origin: null`, classed
/// Foreign) and disables script. Plan
/// `2026-09-17-runner-loopback-api-accepts-any-origin` F3.
fn captured_html_response(html: String) -> impl IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (axum::http::header::CONTENT_SECURITY_POLICY, "sandbox"),
        ],
        html,
    )
}

/// Receive DOM capture from browser extension
pub async fn receive_dom_from_extension(
    Json(request): Json<ReceiveExtensionDomRequest>,
) -> Result<Json<ApiResponse<DomCapture>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "Received DOM capture from extension: {} ({} bytes)",
        request.url,
        request.html.len()
    );

    // Use auto-link to find and link recent screenshots
    match DomCaptureLogger::log_capture_with_auto_link(
        &request.url,
        &request.page_title,
        &request.html,
        request.selector.as_deref(),
        DomCaptureSource::Extension,
        DomCaptureTrigger::OnDemand,
        request.task_run_id.as_deref(),
        None, // Will auto-find recent screenshot
    ) {
        Ok(capture) => {
            info!("Stored DOM capture: {} from {}", capture.id, capture.url);
            Ok(Json(ApiResponse::success(capture)))
        }
        Err(e) => {
            error!("Failed to store DOM capture: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to store DOM capture: {}", e))),
            ))
        }
    }
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/dom/captures", get(list_dom_captures))
        .route("/dom/captures/{id}", get(get_dom_capture))
        .route("/dom/captures/{id}/html", get(get_dom_capture_html))
        .route("/dom/receive", post(receive_dom_from_extension))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plan `2026-09-17-runner-loopback-api-accepts-any-origin` Phase 1 test 13.
    #[tokio::test]
    async fn captured_html_is_served_sandboxed() {
        let resp = captured_html_response("<script>fetch('/files/read')</script>".to_string())
            .into_response();
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_SECURITY_POLICY)
                .and_then(|v| v.to_str().ok()),
            Some("sandbox")
        );
        assert!(resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .starts_with("text/html"));
    }
}
