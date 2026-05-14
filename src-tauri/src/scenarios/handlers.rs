//! Axum handlers for `/scenarios/projection` and `/scenarios/current`.
//!
//! Wire envelope (aligned with the rest of the runner HTTP surface):
//!
//! ```text
//! 200 { success: true,  data: <projection JSON> }
//! 404 { success: false, error: "ir-doc-not-found" }
//! 500 { success: false, error: <message> }
//! ```
//!
//! Originally Phase B2 specified `{ ok, data, error }`, but the Phase B3
//! Python MCP client routes through a shared `_request` helper that
//! reads `data.get("success", False)`. To keep the contract consistent
//! across all runner endpoints (and avoid forking the MCP client), the
//! envelope here uses `success` like every other runner endpoint
//! (e.g. `commands::ai_data::ApiResponse`, `commands::discoveries::DiscoveryResponse`).
//!
//! The static endpoint loads the IR from `spec_api::storage` and projects
//! it natively in Rust (deterministic, zero IPC). The runtime endpoint
//! also loads the IR locally, but defers to the React webview to combine
//! it with live registry data — IPC over `ui-bridge:project-current-scenario-request`.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::Emitter;
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::mcp::types::ApiState;
use crate::spec_api::storage;
use crate::spec_api::types::IrPageSpec;
use crate::ui_bridge_invoke::InvokeResponse;

use super::projection::project_scenarios;

// ---------------------------------------------------------------------------
// Query string and envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct IrDocIdQuery {
    pub ir_doc_id: Option<String>,
}

/// Wire envelope shared by both endpoints. Uses the `success: bool` shape
/// the rest of the runner HTTP surface uses — see e.g.
/// `commands::ai_data::ApiResponse`, `commands::discoveries::DiscoveryResponse`,
/// `commands::tiered_info::ApiResponse`. The Phase B3 Python MCP client's
/// shared `_request` helper reads `data.get("success", False)`; aligning
/// here keeps the envelope contract consistent everywhere.
#[derive(Debug, Clone, Serialize)]
pub struct ScenarioEnvelope<T: Serialize> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T: Serialize> ScenarioEnvelope<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }
}

fn err_envelope(msg: impl Into<String>) -> ScenarioEnvelope<Value> {
    ScenarioEnvelope {
        success: false,
        data: None,
        error: Some(msg.into()),
    }
}

// ---------------------------------------------------------------------------
// Shared: load an IR document by id, mapping storage outcomes onto the wire envelope
// ---------------------------------------------------------------------------

enum LoadIr {
    Found(IrPageSpec),
    NotFound,
    Error(String),
}

fn load_ir(ir_doc_id: &str) -> LoadIr {
    let root = storage::resolve_specs_root();
    match storage::read_ir(&root, ir_doc_id) {
        Ok(Some(doc)) => LoadIr::Found(doc),
        Ok(None) => LoadIr::NotFound,
        Err(e) => LoadIr::Error(e),
    }
}

fn validate_id(q: &IrDocIdQuery) -> Result<&str, Response> {
    match q.ir_doc_id.as_deref() {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err((
            StatusCode::BAD_REQUEST,
            Json(err_envelope("missing-ir-doc-id-query-param")),
        )
            .into_response()),
    }
}

// ---------------------------------------------------------------------------
// GET /scenarios/projection?ir_doc_id=<id>
// ---------------------------------------------------------------------------

pub async fn get_scenarios_projection(
    State(_state): State<Arc<ApiState>>,
    Query(q): Query<IrDocIdQuery>,
) -> Response {
    let ir_doc_id = match validate_id(&q) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    match load_ir(ir_doc_id) {
        LoadIr::Found(ir) => {
            let projection = project_scenarios(&ir);
            (StatusCode::OK, Json(ScenarioEnvelope::ok(projection))).into_response()
        }
        LoadIr::NotFound => (
            StatusCode::NOT_FOUND,
            Json(err_envelope("ir-doc-not-found")),
        )
            .into_response(),
        LoadIr::Error(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(err_envelope(format!("ir-read-failed: {}", e))),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// GET /scenarios/current?ir_doc_id=<id>
// ---------------------------------------------------------------------------

/// Tauri event channel name for the request side of the projection IPC.
/// The TS hook (`useScenarioProjectionHandler`) listens on this name and
/// emits its response on [`PROJECTION_RESPONSE_EVENT`].
const PROJECTION_REQUEST_EVENT: &str = "ui-bridge:project-current-scenario-request";

/// Tauri event channel name for the response side of the projection IPC.
/// The Tauri-side global listener (set up in `mcp_api.rs`) routes incoming
/// responses into the shared [`InvokeRequestStore`] keyed by `request_id`.
#[allow(dead_code)]
pub const PROJECTION_RESPONSE_EVENT: &str = "ui-bridge:project-current-scenario-response";

/// Default timeout for the runtime-aware projection IPC. Keep it tight —
/// the TS side just walks the registry, no network. 10s is plenty.
const PROJECTION_TIMEOUT_MS: u64 = 10_000;

pub async fn get_scenarios_current(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<IrDocIdQuery>,
) -> Response {
    let ir_doc_id = match validate_id(&q) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    let ir = match load_ir(ir_doc_id) {
        LoadIr::Found(ir) => ir,
        LoadIr::NotFound => {
            return (
                StatusCode::NOT_FOUND,
                Json(err_envelope("ir-doc-not-found")),
            )
                .into_response();
        }
        LoadIr::Error(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(err_envelope(format!("ir-read-failed: {}", e))),
            )
                .into_response();
        }
    };

    // Reserve the pending slot before emitting the event so the React
    // side can never deliver faster than we register. Same invariant as
    // the existing `ui_bridge_invoke` proxy — see
    // `mcp/ui_bridge_invoke_handlers.rs::ui_bridge_invoke_handler`.
    let request_id = uuid::Uuid::new_v4().to_string();
    let (sender, receiver) = oneshot::channel::<InvokeResponse>();
    state
        .ui_bridge_invoke_store
        .register(request_id.clone(), sender)
        .await;

    let payload = json!({
        "request_id": request_id,
        "ir": ir,
    });

    info!(
        ir_doc_id,
        request_id = %request_id,
        "scenarios/current: emitting {}",
        PROJECTION_REQUEST_EVENT
    );
    if let Err(e) = state.app_handle.emit(PROJECTION_REQUEST_EVENT, &payload) {
        state.ui_bridge_invoke_store.cancel(&request_id).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(err_envelope(format!(
                "scenarios/current: failed to emit projection request: {}",
                e
            ))),
        )
            .into_response();
    }

    match tokio::time::timeout(Duration::from_millis(PROJECTION_TIMEOUT_MS), receiver).await {
        Ok(Ok(resp)) => {
            if resp.ok {
                let data = resp.result.unwrap_or(Value::Null);
                (StatusCode::OK, Json(ScenarioEnvelope::ok(data))).into_response()
            } else {
                let err = resp.error.unwrap_or_else(|| {
                    "frontend reported projection failure without an error message".to_string()
                });
                warn!(
                    ir_doc_id,
                    request_id = %request_id,
                    error = %err,
                    "scenarios/current: frontend projection returned error"
                );
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(err_envelope(format!(
                        "scenarios/current: frontend projection failed: {}",
                        err
                    ))),
                )
                    .into_response()
            }
        }
        Ok(Err(_recv_err)) => {
            state.ui_bridge_invoke_store.cancel(&request_id).await;
            warn!(
                ir_doc_id,
                request_id = %request_id,
                "scenarios/current: oneshot sender dropped (frontend response lost)"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(err_envelope(format!(
                    "scenarios/current: frontend response channel closed (request_id={})",
                    request_id
                ))),
            )
                .into_response()
        }
        Err(_elapsed) => {
            state.ui_bridge_invoke_store.cancel(&request_id).await;
            warn!(
                ir_doc_id,
                request_id = %request_id,
                "scenarios/current: timed out waiting for frontend projection"
            );
            (
                StatusCode::GATEWAY_TIMEOUT,
                Json(err_envelope(format!(
                    "scenarios/current: timed out waiting for frontend projection (timeout={}ms)",
                    PROJECTION_TIMEOUT_MS
                ))),
            )
                .into_response()
        }
    }
}
