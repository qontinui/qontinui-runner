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
//!   - Structural component / element capture (`matches_isolated_render`)
//!   - Page scroll
//!   - Performance entries (read + clear)
//!   - Annotations import
//!   - Debug helpers (element-tree, highlight-element)

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use tracing::info;

use crate::mcp::types::{ApiResponse, ApiState};

use super::request::{
    ui_bridge_request_sync, wrap_ipc_result, wrap_ipc_result_keeping_failure_data,
};
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

/// Journey ledger choke point (plan
/// 2026-09-20-ui-bridge-represents-the-users-path-and-the-passage-of-time, M3):
/// `/control/undo` is an element action with no element id.
pub async fn ui_bridge_undo_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = ui_bridge_undo_handler_dispatch(State(Arc::clone(&state))).await;
    crate::journey::capture::record_control_result(
        &state,
        &result,
        crate::journey::cursor::ActionSpec::untargeted_element("undo"),
    );
    result
}

/// Execute undo via the UI Bridge.
async fn ui_bridge_undo_handler_dispatch(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Undo");

    wrap_ipc_result(ui_bridge_request_sync(&state, "undo", serde_json::json!({})).await)
}

/// Journey ledger choke point (plan
/// 2026-09-20-ui-bridge-represents-the-users-path-and-the-passage-of-time, M3):
/// `/control/redo` is an element action with no element id.
pub async fn ui_bridge_redo_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = ui_bridge_redo_handler_dispatch(State(Arc::clone(&state))).await;
    crate::journey::capture::record_control_result(
        &state,
        &result,
        crate::journey::cursor::ActionSpec::untargeted_element("redo"),
    );
    result
}

/// Execute redo via the UI Bridge.
async fn ui_bridge_redo_handler_dispatch(
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
ipc_handler_post_bumps_mutation!(ui_bridge_navigate_tab_handler_dispatch, "navigate_tab");

/// Journey ledger choke point (plan
/// 2026-09-20-ui-bridge-represents-the-users-path-and-the-passage-of-time, M3):
/// `/control/navigate-tab` is a `navigation` edge (`push`).
pub async fn ui_bridge_navigate_tab_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let action = crate::journey::cursor::ActionSpec::navigation(
        "navigate_tab",
        qontinui_types::journey::NavigationTriggerKind::Push,
    );
    let result =
        ui_bridge_navigate_tab_handler_dispatch(State(Arc::clone(&state)), Json(body)).await;
    crate::journey::capture::record_control_result(&state, &result, action);
    result
}

ipc_handler_post!(ui_bridge_clear_storage_handler, "clear_storage");

// Component state
ipc_handler_path_get!(
    ui_bridge_get_component_state_handler,
    "get_component_state",
    "componentId"
);

// ============================================================================
// Structural capture (matches_isolated_render)
// ============================================================================

/// The SDK's capture refusal marker: the whole `error` of an HTTP refusal, and
/// the prefix of a relayed one (`component_capture_refused:<reason>: <msg>`).
const CAPTURE_REFUSED: &str = "component_capture_refused";

/// The SDK's typed capture refusal: its reason, the HTTP status the SDK
/// answers it with (404 `not_found`, 422 `no_mounted_elements`), and the SDK's
/// own message when the answer carried one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaptureRefusal {
    pub reason: &'static str,
    pub status: StatusCode,
    pub message: Option<String>,
}

impl CaptureRefusal {
    fn from_reason(reason: &str, message: Option<String>) -> Option<Self> {
        let (reason, status) = match reason {
            "not_found" => ("not_found", StatusCode::NOT_FOUND),
            "no_mounted_elements" => ("no_mounted_elements", StatusCode::UNPROCESSABLE_ENTITY),
            _ => return None,
        };
        Some(Self {
            reason,
            status,
            message: message.filter(|m| !m.trim().is_empty()),
        })
    }

    /// The SDK's diagnostic code for the reason.
    pub fn code(&self) -> &'static str {
        if self.reason == "not_found" {
            "UB-ELEM-NOT-FOUND"
        } else {
            "UB-OBS-INPUT-MISSING"
        }
    }

    /// The SDK's HTTP refusal body: `error` and `reason` at the top level, the
    /// refusal repeated under `data`, the message (when known) as the one
    /// suggestion.
    pub fn body(&self) -> serde_json::Value {
        serde_json::json!({
            "success": false,
            "error": CAPTURE_REFUSED,
            "reason": self.reason,
            "code": self.code(),
            "data": { "error": CAPTURE_REFUSED, "reason": self.reason },
            "suggestions": self.message.iter().collect::<Vec<_>>(),
        })
    }

    /// One line for a report: `component_capture_refused: <reason> (<message>)`.
    pub fn describe(&self) -> String {
        match &self.message {
            Some(m) => format!("{CAPTURE_REFUSED}: {} ({m})", self.reason),
            None => format!("{CAPTURE_REFUSED}: {}", self.reason),
        }
    }
}

/// A refusal relayed as a message (`component_capture_refused:<reason>[: msg]`)
/// — what the SDK's in-page relay dispatcher sends over a WebSocket wrapper.
fn refusal_from_message(text: &str) -> Option<CaptureRefusal> {
    let rest = text.strip_prefix(CAPTURE_REFUSED)?.strip_prefix(':')?;
    let (reason, message) = match rest.split_once(':') {
        Some((reason, message)) => (reason, Some(message.trim().to_string())),
        None => (rest, None),
    };
    CaptureRefusal::from_reason(reason.trim(), message)
}

/// The reason a refusal's SDK code stands for.
fn reason_from_code(code: Option<&str>) -> Option<&'static str> {
    match code? {
        "UB-ELEM-NOT-FOUND" => Some("not_found"),
        "UB-OBS-INPUT-MISSING" => Some("no_mounted_elements"),
        _ => None,
    }
}

/// The capture payload inside a capture route's answer — the SDK's
/// `{success, data}` envelope's `data`, else the answer itself (the flat
/// capture the IPC layer and a WebSocket wrapper return) — when it is
/// capture-shaped. Every field of a `ComponentCapture` is optional, so any
/// JSON object would parse as an empty one; an answer carrying none of
/// `schemaVersion`, `root` or `nodes` (an SDK without the route answering
/// `{"error": …}`, a stray object) is `None`, never a capture.
pub(crate) fn capture_payload(answer: &serde_json::Value) -> Option<&serde_json::Value> {
    let payload = match answer.get("data") {
        Some(d) if answer.get("success").is_some() => d,
        _ => answer,
    };
    ["schemaVersion", "root", "nodes"]
        .iter()
        .any(|k| payload.get(k).is_some())
        .then_some(payload)
}

/// The typed refusal in a capture answer — the HTTP envelope (`error` is the
/// marker, `reason` top level or under `data`) or a relayed inner failure
/// (`error` carries the reason). `None` for any other answer.
pub(crate) fn capture_refusal(answer: &serde_json::Value) -> Option<CaptureRefusal> {
    if answer.get("success").and_then(|v| v.as_bool()) != Some(false) {
        return None;
    }
    let error = answer.get("error").and_then(|v| v.as_str())?;
    if error != CAPTURE_REFUSED {
        return refusal_from_message(error);
    }
    let reason = answer
        .get("reason")
        .or_else(|| answer.get("data").and_then(|d| d.get("reason")))
        .and_then(|v| v.as_str())
        .or_else(|| reason_from_code(answer.get("code").and_then(|v| v.as_str())))?;
    let message = answer
        .get("suggestions")
        .and_then(|s| s.get(0))
        .or_else(|| answer.get("message"))
        .or_else(|| answer.get("data").and_then(|d| d.get("message")))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    CaptureRefusal::from_reason(reason, message)
}

/// The typed refusal inside an app-dispatch failure, on either transport: an
/// HTTP app's answered refusal (its reason recovered from the message, the
/// SDK code or the status), an unparsed HTTP error body, or a WebSocket
/// wrapper's relayed refusal message. `None` for every other failure.
pub(crate) fn capture_refusal_from_dispatch(
    err: &crate::mcp::app_dispatch::DispatchError,
) -> Option<CaptureRefusal> {
    use crate::mcp::app_dispatch::DispatchError;
    use crate::mcp::command_relay::CommandRelayError;
    match err {
        DispatchError::AppAnswered {
            error,
            code,
            status,
            ..
        } => refusal_from_message(error).or_else(|| {
            if error != CAPTURE_REFUSED {
                return None;
            }
            let reason = reason_from_code(code.as_deref()).or(match status {
                404 => Some("not_found"),
                422 => Some("no_mounted_elements"),
                _ => None,
            })?;
            CaptureRefusal::from_reason(reason, None)
        }),
        DispatchError::HttpStatus { body, .. } => serde_json::from_str(body)
            .ok()
            .and_then(|json| capture_refusal(&json)),
        DispatchError::WebSocket(CommandRelayError::WrapperError(failure)) => {
            refusal_from_message(&failure.message).or_else(|| {
                (failure.message == CAPTURE_REFUSED)
                    .then(|| reason_from_code(failure.code.as_deref()))
                    .flatten()
                    .and_then(|reason| CaptureRefusal::from_reason(reason, None))
            })
        }
        _ => None,
    }
}

/// The SDK capture path relative to the UI Bridge mount, for `kind`
/// `component` or `element`: `/control/<kind>/<id>/capture[?props=true]`, the id
/// percent-encoded.
pub(crate) fn capture_path(kind: &str, id: &str, props: bool) -> String {
    format!(
        "/control/{kind}/{}/capture{}",
        urlencoding::encode(id),
        if props { "?props=true" } else { "" }
    )
}

/// A capture route's failure body. The typed refusal is a plain JSON object
/// because it carries `reason` at the top level beside `error`, which the
/// shared `ApiResponse` envelope has no field for.
type CaptureFailure = (StatusCode, Json<serde_json::Value>);

/// The capture routes' query: `?props=true` opts in to the owning component's
/// scrubbed props (off by default, as on the SDK route). Any other query key
/// (the SDK's `tabId`) is ignored: the runner answers for its own webview.
#[derive(Debug, Default, serde::Deserialize)]
pub struct CaptureQuery {
    #[serde(default)]
    pub props: Option<String>,
}

/// Whether a `props` query value opts in: `true` or `1`, as the SDK reads it.
pub(crate) fn props_opt_in(value: Option<&str>) -> bool {
    matches!(value, Some("true" | "1"))
}

/// Forward a capture request to the runner's own webview and answer the way
/// the SDK route does: the `ComponentCapture` under `data`, or the typed
/// refusal — `error: "component_capture_refused"` and `reason` at the top
/// level, the refusal body repeated under `data`, code `UB-ELEM-NOT-FOUND` /
/// `UB-OBS-INPUT-MISSING` — at 404 / 422.
async fn capture_via_ipc(
    state: &Arc<ApiState>,
    ipc_type: &str,
    id: String,
    props: bool,
) -> Result<Json<ApiResponse<serde_json::Value>>, CaptureFailure> {
    let result = ui_bridge_request_sync(
        state,
        ipc_type,
        serde_json::json!({ "params": { "id": id, "props": props } }),
    )
    .await;
    if let Ok(answer) = &result {
        if let Some(refusal) = capture_refusal(answer) {
            return Err((refusal.status, Json(refusal.body())));
        }
    }
    wrap_ipc_result_keeping_failure_data(result).map_err(|(status, Json(body))| {
        (
            status,
            Json(serde_json::to_value(body).unwrap_or_else(
                |e| serde_json::json!({ "success": false, "error": e.to_string() }),
            )),
        )
    })
}

/// `GET /ui-bridge/control/component/{id}/capture` — the component's
/// structural `ComponentCapture` (plan
/// 2026-11-13-ui-bridge-matches-isolated-render-assertion).
pub async fn ui_bridge_get_component_capture_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(q): Query<CaptureQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, CaptureFailure> {
    capture_via_ipc(
        &state,
        "get_component_capture",
        id,
        props_opt_in(q.props.as_deref()),
    )
    .await
}

/// `GET /ui-bridge/control/element/{id}/capture` — the structural
/// `ComponentCapture` rooted at one registered element.
pub async fn ui_bridge_get_element_capture_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(q): Query<CaptureQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, CaptureFailure> {
    capture_via_ipc(
        &state,
        "get_element_capture",
        id,
        props_opt_in(q.props.as_deref()),
    )
    .await
}

// Page scroll: changes the viewport content → rendered pixels change.
ipc_handler_post_bumps_mutation!(ui_bridge_scroll_page_handler_dispatch, "scroll_page");

/// Journey ledger choke point (plan
/// 2026-09-20-ui-bridge-represents-the-users-path-and-the-passage-of-time, M3):
/// `/control/page/scroll` is an element action with no element id.
pub async fn ui_bridge_scroll_page_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let action = crate::journey::cursor::ActionSpec::untargeted_element("scroll");
    let result =
        ui_bridge_scroll_page_handler_dispatch(State(Arc::clone(&state)), Json(body)).await;
    crate::journey::capture::record_control_result(&state, &result, action);
    result
}

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
        // Structural captures for `matches_isolated_render`.
        .route(
            "/ui-bridge/control/component/{id}/capture",
            get(ui_bridge_get_component_capture_handler),
        )
        .route(
            "/ui-bridge/control/element/{id}/capture",
            get(ui_bridge_get_element_capture_handler),
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
        ("GET", "/ui-bridge/control/component/{id}/capture"),
        ("GET", "/ui-bridge/control/element/{id}/capture"),
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
