//! Accessibility HTTP API endpoints.
//!
//! Provides REST endpoints for native accessibility tree capture, querying,
//! and interaction via the AccessibilityManager. These endpoints are consumed
//! by the Python `RustBackendCapture` bridge and by external MCP clients.

use axum::{
    extract::State as AxumState, http::StatusCode, response::IntoResponse, routing::post, Json,
    Router,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{info, warn};

use crate::mcp::types::ApiState;
use qontinui_runner_lib::accessibility::model::UnifiedRole;
use qontinui_runner_lib::accessibility::traits::ConnectionTarget;

// ============================================================================
// Request / Response types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct ConnectRequest {
    pub target: String,
    #[serde(default = "default_backend")]
    pub backend: String,
}

fn default_backend() -> String {
    "auto".to_string()
}

#[derive(Debug, Deserialize)]
pub struct CaptureRequest {
    #[serde(default)]
    pub include_hidden: bool,
    pub max_depth: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct QueryRequest {
    pub role: Option<String>,
    pub label: Option<String>,
    pub label_contains: Option<String>,
    #[serde(default)]
    pub interactive_only: bool,
}

#[derive(Debug, Deserialize)]
pub struct ClickRequest {
    pub ref_id: String,
}

#[derive(Debug, Deserialize)]
pub struct TypeTextRequest {
    pub ref_id: String,
    pub text: String,
    #[serde(default)]
    pub clear_first: bool,
}

#[derive(Debug, Deserialize)]
pub struct FocusRequest {
    pub ref_id: String,
}

// ============================================================================
// Handlers
// ============================================================================

/// POST /a11y/connect
async fn handle_connect(
    AxumState(state): AxumState<Arc<ApiState>>,
    Json(req): Json<ConnectRequest>,
) -> impl IntoResponse {
    info!(
        "POST /a11y/connect target={} backend={}",
        req.target, req.backend
    );

    let connection_target = match parse_connection_target(&req.target) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e })),
            );
        }
    };

    let mut mgr = state.accessibility_manager.lock().await;
    match mgr.connect(connection_target, 30_000).await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "connected": true,
                "backend": mgr.backend_name(),
            })),
        ),
        Err(e) => {
            warn!("a11y connect failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("{}", e) })),
            )
        }
    }
}

/// POST /a11y/capture
async fn handle_capture(
    AxumState(state): AxumState<Arc<ApiState>>,
    Json(req): Json<CaptureRequest>,
) -> impl IntoResponse {
    let mut mgr = state.accessibility_manager.lock().await;

    if !mgr.is_connected() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Not connected to an accessibility source" })),
        );
    }

    match mgr.capture(req.max_depth, req.include_hidden).await {
        Ok(snapshot) => match serde_json::to_value(&snapshot) {
            Ok(val) => (StatusCode::OK, Json(val)),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Serialization error: {}", e) })),
            ),
        },
        Err(e) => {
            warn!("a11y capture failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("{}", e) })),
            )
        }
    }
}

/// POST /a11y/query
async fn handle_query(
    AxumState(state): AxumState<Arc<ApiState>>,
    Json(req): Json<QueryRequest>,
) -> impl IntoResponse {
    let mgr = state.accessibility_manager.lock().await;

    let snapshot = match mgr.snapshot().await {
        Some(s) => s,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "No accessibility tree captured yet",
                    "count": 0,
                    "nodes": [],
                })),
            );
        }
    };

    let mut builder = mgr.query();

    if let Some(ref role_str) = req.role {
        match serde_json::from_value::<UnifiedRole>(serde_json::Value::String(role_str.clone())) {
            Ok(role) => builder = builder.by_role(role),
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("Unknown role: {}", role_str) })),
                );
            }
        }
    }

    if let Some(ref lbl) = req.label {
        builder = builder.by_label(lbl.as_str());
    }

    if let Some(ref substr) = req.label_contains {
        builder = builder
            .by_label_contains(substr.as_str())
            .case_insensitive();
    }

    if req.interactive_only {
        builder = builder.interactive();
    }

    let results = builder.find_all(&snapshot.root);

    let nodes: Vec<serde_json::Value> = results
        .iter()
        .map(|n| {
            serde_json::json!({
                "ref_id": n.ref_id,
                "role": n.role.as_str(),
                "name": n.name,
                "value": n.value,
                "is_interactive": n.is_interactive,
                "bounds": n.bounds,
            })
        })
        .collect();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "count": nodes.len(),
            "nodes": nodes,
        })),
    )
}

/// POST /a11y/click
async fn handle_click(
    AxumState(state): AxumState<Arc<ApiState>>,
    Json(req): Json<ClickRequest>,
) -> impl IntoResponse {
    info!("POST /a11y/click ref_id={}", req.ref_id);
    let mgr = state.accessibility_manager.lock().await;

    match mgr.click(&req.ref_id).await {
        Ok(result) => match serde_json::to_value(&result) {
            Ok(val) => (StatusCode::OK, Json(val)),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Serialization error: {}", e) })),
            ),
        },
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "success": false,
                "error": format!("{}", e),
            })),
        ),
    }
}

/// POST /a11y/type_text
async fn handle_type_text(
    AxumState(state): AxumState<Arc<ApiState>>,
    Json(req): Json<TypeTextRequest>,
) -> impl IntoResponse {
    info!("POST /a11y/type_text ref_id={}", req.ref_id);
    let mgr = state.accessibility_manager.lock().await;

    match mgr.type_text(&req.ref_id, &req.text, req.clear_first).await {
        Ok(result) => match serde_json::to_value(&result) {
            Ok(val) => (StatusCode::OK, Json(val)),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Serialization error: {}", e) })),
            ),
        },
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "success": false,
                "error": format!("{}", e),
            })),
        ),
    }
}

/// POST /a11y/focus
async fn handle_focus(
    AxumState(state): AxumState<Arc<ApiState>>,
    Json(req): Json<FocusRequest>,
) -> impl IntoResponse {
    info!("POST /a11y/focus ref_id={}", req.ref_id);
    let mgr = state.accessibility_manager.lock().await;

    match mgr.focus(&req.ref_id).await {
        Ok(result) => match serde_json::to_value(&result) {
            Ok(val) => (StatusCode::OK, Json(val)),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Serialization error: {}", e) })),
            ),
        },
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "success": false,
                "error": format!("{}", e),
            })),
        ),
    }
}

/// POST /a11y/disconnect
async fn handle_disconnect(AxumState(state): AxumState<Arc<ApiState>>) -> impl IntoResponse {
    info!("POST /a11y/disconnect");
    let mut mgr = state.accessibility_manager.lock().await;

    match mgr.disconnect().await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({ "connected": false })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{}", e) })),
        ),
    }
}

/// POST /a11y/ai_context
async fn handle_ai_context(
    AxumState(state): AxumState<Arc<ApiState>>,
    Json(req): Json<AiContextRequest>,
) -> impl IntoResponse {
    let mgr = state.accessibility_manager.lock().await;
    let max = req.max_elements.unwrap_or(100);
    let text = mgr.to_ai_context(max, req.interactive_only).await;
    (StatusCode::OK, Json(serde_json::json!({ "context": text })))
}

#[derive(Debug, Deserialize)]
pub struct AiContextRequest {
    pub max_elements: Option<usize>,
    #[serde(default = "default_true")]
    pub interactive_only: bool,
}

fn default_true() -> bool {
    true
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/a11y/connect", post(handle_connect))
        .route("/a11y/capture", post(handle_capture))
        .route("/a11y/query", post(handle_query))
        .route("/a11y/click", post(handle_click))
        .route("/a11y/type_text", post(handle_type_text))
        .route("/a11y/focus", post(handle_focus))
        .route("/a11y/disconnect", post(handle_disconnect))
        .route("/a11y/ai_context", post(handle_ai_context))
}

// ============================================================================
// Helpers
// ============================================================================

/// Parse a user-facing target string into a `ConnectionTarget`.
fn parse_connection_target(target: &str) -> Result<ConnectionTarget, String> {
    let lower = target.to_lowercase();

    if lower == "desktop" {
        return Ok(ConnectionTarget::Desktop);
    }

    if let Some(pid_str) = lower.strip_prefix("pid:") {
        let pid: u32 = pid_str
            .trim()
            .parse()
            .map_err(|_| format!("Invalid PID: {}", pid_str))?;
        return Ok(ConnectionTarget::ProcessId(pid));
    }

    Ok(ConnectionTarget::WindowTitle(target.to_string()))
}
