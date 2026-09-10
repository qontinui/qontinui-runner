//! UI Bridge exploration + window listing HTTP handlers.
//!
//! Exploration is the qontinui-library driven automation surface: it spawns
//! a Python job that crawls a connected runner/web/mobile UI Bridge and
//! reports back discovered elements/states. `discover_states_from_renders`
//! is the post-hoc state-discovery variant over existing render logs.
//!
//! Window listing (`ui_bridge_list_windows_handler` + its `list_windows_native`
//! helper) lives here too because the only family it would otherwise belong
//! to is screenshots, which hasn't been extracted yet.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use tracing::{error, info};

use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::types::{
    DiscoverStatesRequest, StartUIBridgeExplorationRequest, UIBridgeExplorationStatusRequest,
};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

/// Classify an executor-bridge failure into the `(status, code)` a caller
/// should see.
///
/// "Python executor not running" is a PRECONDITION, not a server fault: the
/// runner is healthy and the request is well-formed — the OPTIONAL Python
/// side simply is not up. Reporting that as a bare 500 with no code told a
/// caller "this route is broken" when the honest answer is "start the
/// executor and retry", and the two have completely different recoveries.
/// 503 + `PYTHON_EXECUTOR_NOT_RUNNING` says which.
///
/// Two more conditions were measured on this build and belong here for the
/// same reason (manual-test-loop iteration 26, item 2) — neither is a server
/// fault, and a 500 sends a caller looking for a runner bug that is not there:
///
/// - **"Job not running (status: failed)"** / **"No exploration in
///   progress"** — the caller asked to stop an exploration that has already
///   finished, failed, or was never started. That is 409-shaped target state:
///   retrying is pointless, and the recovery is to read
///   `GET /ui-bridge/explore/status` (or start a job). Both spellings are
///   matched because the Python side derives the message from where it
///   noticed: `status_derived` produces the second, the job registry the
///   first.
/// - **"No exploration job found"** — nothing to report on. That is a 404,
///   not a 500: the route works, the resource does not exist.
/// - **"UI Bridge SDK not found at http://…"** — the TARGET app's UI Bridge
///   SDK is not answering on the connection URL the job was given. Same
///   precondition shape as the executor arm: the runner is fine, a dependency
///   the request needs is not up, and the recovery is to start it and retry.
///   503 rather than 424 to stay consistent with the executor arm.
///
/// Everything else keeps `500 INTERNAL_ERROR`. That arm is deliberately the
/// fallback rather than the default-for-anything-unrecognised-looking: a bare
/// 500 is the honest answer for a fault we have not characterised, and
/// guessing a friendlier status for one would be the same fabrication this
/// change exists to remove.
pub(crate) fn classify_bridge_error(msg: &str) -> (StatusCode, &'static str) {
    if msg.contains("Python executor not running") {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "PYTHON_EXECUTOR_NOT_RUNNING",
        )
    } else if msg.contains("Job not running") || msg.contains("No exploration in progress") {
        (StatusCode::CONFLICT, "EXPLORATION_NOT_RUNNING")
    } else if msg.contains("No exploration job found") {
        (StatusCode::NOT_FOUND, "NO_EXPLORATION_JOB")
    } else if msg.contains("UI Bridge SDK not found") {
        (StatusCode::SERVICE_UNAVAILABLE, "UI_BRIDGE_SDK_NOT_FOUND")
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR")
    }
}

/// Build the error arm for an exploration handler from a bridge error string.
fn bridge_error_response(msg: String) -> (StatusCode, Json<ApiResponse<()>>) {
    let (status, code) = classify_bridge_error(&msg);
    (status, Json(ApiResponse::<()>::error_with_code(msg, code)))
}

/// Start UI Bridge exploration (spawns a Python background job).
pub async fn start_ui_bridge_exploration(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let request: StartUIBridgeExplorationRequest = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(e) => {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ApiResponse::error(format!(
                    "Invalid request: {}. Required fields: connection_url (string). \
                     Optional: target_type (\"web\"|\"desktop\"|\"mobile\", default \"web\"), \
                     max_depth (int, default 2), max_elements_per_page (int, default 20), \
                     max_total_elements (int, default 100), action_delay_ms (int, default 500), \
                     blocked_keywords (string[]), safe_keywords (string[]), \
                     blocked_selectors (string[]), capture_screenshots (bool, default false), \
                     run_state_discovery (bool, default true). \
                     Example: {{\"connection_url\": \"http://localhost:3001\", \"target_type\": \"web\"}}",
                    e
                ))),
            ));
        }
    };
    info!(
        "MCP API: Starting UI Bridge exploration for URL: {} (type: {})",
        request.connection_url, request.target_type
    );

    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "target_type": request.target_type,
        "connection_url": request.connection_url,
        "max_depth": request.max_depth.unwrap_or(2),
        "max_elements_per_page": request.max_elements_per_page.unwrap_or(20),
        "max_total_elements": request.max_total_elements.unwrap_or(100),
        "action_delay_ms": request.action_delay_ms.unwrap_or(500),
        "blocked_keywords": request.blocked_keywords.clone().unwrap_or_default(),
        "safe_keywords": request.safe_keywords.clone().unwrap_or_default(),
        "blocked_selectors": request.blocked_selectors.clone().unwrap_or_default(),
        "capture_screenshots": request.capture_screenshots.unwrap_or(false),
        "run_state_discovery": request.run_state_discovery.unwrap_or(true),
    });

    // Short timeout since this just starts the background job
    let timeout = std::time::Duration::from_secs(30);

    let result = spawn_blocking_tracked(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("start_ui_bridge_exploration", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: UI Bridge exploration job started");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to start UI Bridge exploration".to_string());
                error!(
                    "MCP API: Failed to start UI Bridge exploration: {}",
                    error_msg
                );
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to start UI Bridge exploration: {}", e);
            Err(bridge_error_response(e))
        }
    }
}

/// Get UI Bridge exploration status
pub async fn get_ui_bridge_exploration_status(
    State(state): State<Arc<ApiState>>,
    Query(request): Query<UIBridgeExplorationStatusRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "job_id": request.job_id,
    });

    let timeout = std::time::Duration::from_secs(10);

    let result = spawn_blocking_tracked(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("get_ui_bridge_exploration_status", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "status": "unknown"
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get exploration status".to_string());
                Err(bridge_error_response(error_msg))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to get UI Bridge exploration status: {}", e);
            Err(bridge_error_response(e))
        }
    }
}

/// Get UI Bridge exploration results
pub async fn get_ui_bridge_exploration_results(
    State(state): State<Arc<ApiState>>,
    Query(request): Query<UIBridgeExplorationStatusRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "job_id": request.job_id,
    });

    let timeout = std::time::Duration::from_secs(30);

    let result = spawn_blocking_tracked(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("get_ui_bridge_exploration_results", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "data": null
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get exploration results".to_string());
                Err(bridge_error_response(error_msg))
            }
        }
        Err(e) => {
            error!(
                "MCP API: Failed to get UI Bridge exploration results: {}",
                e
            );
            Err(bridge_error_response(e))
        }
    }
}

/// Stop UI Bridge exploration.
///
/// Errors route through [`bridge_error_response`] — the SAME classifier its
/// `status` and `results` siblings already used. Until iteration 26 this
/// handler had no classifier at all (a bare
/// `(INTERNAL_SERVER_ERROR, api_error(e))` on both error arms), so ONE
/// condition got TWO different answers depending on which of the three routes
/// you asked: "Python executor not running" answered
/// `503 PYTHON_EXECUTOR_NOT_RUNNING` on `status`/`results` and
/// `500 INTERNAL_ERROR` here.
pub async fn stop_ui_bridge_exploration(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping UI Bridge exploration");

    let app_state = state.app_state.clone();

    let timeout = std::time::Duration::from_secs(10);

    let result = spawn_blocking_tracked(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("stop_ui_bridge_exploration", None, timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: UI Bridge exploration stop requested");
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "message": "Stop requested"
                }))))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to stop exploration".to_string());
                Err(bridge_error_response(error_msg))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to stop UI Bridge exploration: {}", e);
            Err(bridge_error_response(e))
        }
    }
}

/// Discover states from render logs using co-occurrence analysis
/// This endpoint runs state discovery on existing render logs without exploration
pub async fn discover_states_from_renders(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<DiscoverStatesRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Discovering states from {} render logs",
        request.render_logs.len()
    );

    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "render_logs": request.render_logs,
    });

    // Allow more time for analysis of large render logs
    let timeout = std::time::Duration::from_secs(60);

    let result = spawn_blocking_tracked(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("discover_states_from_renders", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: State discovery completed successfully");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "states": [],
                        "elements": [],
                        "elementToRenders": {},
                        "renderCount": 0,
                        "uniqueElementCount": 0
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to discover states from renders".to_string());
                error!("MCP API: Failed to discover states: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to discover states from renders: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// =============================================================================
// Window Listing (xcap)
// =============================================================================

/// Info about a capturable window
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    id: u32,
    title: String,
    app_name: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    is_minimized: bool,
    is_maximized: bool,
    is_focused: bool,
}

/// List all capturable windows using xcap.
fn list_windows_native() -> Result<Vec<WindowInfo>, String> {
    use xcap::Window;

    let windows = Window::all().map_err(|e| format!("Failed to enumerate windows: {}", e))?;
    let mut result = Vec::new();

    for w in &windows {
        let id = w.id().unwrap_or(0);
        let title = w.title().unwrap_or_default();
        let app_name = w.app_name().unwrap_or_default();

        // Skip windows with no title (background/system windows)
        if title.is_empty() {
            continue;
        }

        result.push(WindowInfo {
            id,
            title,
            app_name,
            x: w.x().unwrap_or(0),
            y: w.y().unwrap_or(0),
            width: w.width().unwrap_or(0),
            height: w.height().unwrap_or(0),
            is_minimized: w.is_minimized().unwrap_or(false),
            is_maximized: w.is_maximized().unwrap_or(false),
            is_focused: w.is_focused().unwrap_or(false),
        });
    }

    Ok(result)
}

/// GET /ui-bridge/control/windows — List all capturable windows
pub async fn ui_bridge_list_windows_handler(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<WindowInfo>>> {
    match spawn_blocking_tracked(list_windows_native).await {
        Ok(Ok(windows)) => {
            info!("UI Bridge: Listed {} capturable windows", windows.len());
            Json(ApiResponse::success(windows))
        }
        Ok(Err(e)) => {
            error!("UI Bridge: Failed to list windows: {}", e);
            Json(ApiResponse::error(format!("Failed to list windows: {}", e)))
        }
        Err(e) => {
            error!("UI Bridge: Window list task failed: {}", e);
            Json(ApiResponse::error(format!(
                "Window list task failed: {}",
                e
            )))
        }
    }
}

// ============================================================================
// Route registration
// ============================================================================

/// UI Bridge exploration + window listing routes.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/ui-bridge/explore", post(start_ui_bridge_exploration))
        .route(
            "/ui-bridge/explore/status",
            get(get_ui_bridge_exploration_status),
        )
        .route(
            "/ui-bridge/explore/results",
            get(get_ui_bridge_exploration_results),
        )
        .route("/ui-bridge/explore/stop", post(stop_ui_bridge_exploration))
        .route(
            "/ui-bridge/discover-states",
            post(discover_states_from_renders),
        )
        .route(
            "/ui-bridge/control/windows",
            get(ui_bridge_list_windows_handler),
        )
}

/// Static (method, path) tuples matching every route registered by `routes()`.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("POST", "/ui-bridge/explore"),
        ("GET", "/ui-bridge/explore/status"),
        ("GET", "/ui-bridge/explore/results"),
        ("POST", "/ui-bridge/explore/stop"),
        ("POST", "/ui-bridge/discover-states"),
        ("GET", "/ui-bridge/control/windows"),
    ]
}

#[cfg(test)]
mod bridge_error_classification_tests {
    use super::classify_bridge_error;
    use axum::http::StatusCode;

    /// `explore/status` and `explore/results` answered 500 with no code when
    /// the OPTIONAL Python executor was simply not up. That is a
    /// precondition, not a server fault, and the two have different
    /// recoveries — start the executor vs. report a broken route.
    #[test]
    fn a_missing_python_executor_is_a_precondition_not_a_server_fault() {
        let (status, code) = classify_bridge_error("Python executor not running");
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "PYTHON_EXECUTOR_NOT_RUNNING");
    }

    #[test]
    fn other_bridge_failures_stay_500_but_are_still_typed() {
        let (status, code) = classify_bridge_error("send_command_and_wait timed out");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(code, "INTERNAL_ERROR");
    }

    /// Reported by manual-test-loop iteration 26 on
    /// `POST /ui-bridge/explore/stop`:
    /// `500 INTERNAL_ERROR "Job not running (status: failed)"`. Nothing is
    /// broken — the caller asked to stop a job that is not running, which is
    /// target state, not a server fault. Retrying cannot help; reading
    /// `explore/status` can.
    #[test]
    fn stopping_a_job_that_is_not_running_is_a_conflict_not_a_server_fault() {
        let (status, code) = classify_bridge_error("Job not running (status: failed)");
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code, "EXPLORATION_NOT_RUNNING");
    }

    /// Measured live on a temp runner with no exploration ever started:
    /// `POST /ui-bridge/explore/stop` -> `500 INTERNAL_ERROR "No exploration
    /// in progress"` (`error_detail.context.code_source: "status_derived"`).
    /// Same target state as "Job not running", reported from a different
    /// place on the Python side, so it must classify the same way.
    #[test]
    fn the_status_derived_spelling_of_not_running_classifies_the_same() {
        let (status, code) = classify_bridge_error("No exploration in progress");
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code, "EXPLORATION_NOT_RUNNING");
    }

    /// Measured live on `GET /ui-bridge/explore/results` with no job:
    /// `500 INTERNAL_ERROR "No exploration job found"`. The route works and
    /// the resource does not exist — that is a 404, and a caller polling for
    /// results before starting a job should not be told the runner is broken.
    #[test]
    fn results_for_a_job_that_does_not_exist_is_a_404() {
        let (status, code) = classify_bridge_error("No exploration job found");
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(code, "NO_EXPLORATION_JOB");
    }

    /// Reported by manual-test-loop iteration 26 on
    /// `GET /ui-bridge/explore/results`:
    /// `500 INTERNAL_ERROR "UI Bridge SDK not found at http://127.0.0.1:9893…"`.
    /// The TARGET app's SDK is not answering — the same precondition shape as
    /// a missing Python executor, and the same recovery (start it, retry).
    ///
    /// Not re-reached live when this fix was written (the target SDK was up),
    /// so this arm is covered by the classifier test alone. Said plainly
    /// rather than implied — see the module note on `stop`'s wiring test.
    #[test]
    fn an_unreachable_target_sdk_is_a_precondition_not_a_server_fault() {
        let (status, code) =
            classify_bridge_error("UI Bridge SDK not found at http://127.0.0.1:9893/ui-bridge");
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "UI_BRIDGE_SDK_NOT_FOUND");
    }
}

/// `explore/stop` must answer through the SAME classifier as its `status` and
/// `results` siblings.
///
/// This is a SOURCE test rather than a behavioural one on purpose. The defect
/// was never in `classify_bridge_error` — it was that `stop` did not call it,
/// so one condition ("Python executor not running") got `503
/// PYTHON_EXECUTOR_NOT_RUNNING` on two routes and `500 INTERNAL_ERROR` on the
/// third. Exercising that live needs a stopped Python executor, which the
/// iteration-26 loop could not produce (the executor was up throughout). The
/// wiring is what regressed and the wiring is what this guards: a future edit
/// that reintroduces a bare `INTERNAL_SERVER_ERROR` arm in this handler fails
/// here even with the classifier itself perfectly correct.
#[cfg(test)]
mod stop_uses_the_shared_classifier_tests {
    /// The body of `stop_ui_bridge_exploration`, production source only.
    fn stop_handler_body() -> &'static str {
        let src = include_str!("exploration.rs");
        let start = src
            .find("pub async fn stop_ui_bridge_exploration")
            .expect("the stop handler is in this file");
        let rest = &src[start..];
        let end = rest
            .find("\n/// Discover states from render logs")
            .expect("the stop handler is followed by discover_states_from_renders");
        &rest[..end]
    }

    #[test]
    fn stop_routes_both_error_arms_through_bridge_error_response() {
        let body = stop_handler_body();
        assert_eq!(
            body.matches("bridge_error_response").count(),
            2,
            "both of stop's error arms (inner-failure envelope and transport \
             error) must classify; body was:\n{body}"
        );
    }

    #[test]
    fn stop_serves_no_unclassified_500() {
        let body = stop_handler_body();
        // The one legitimate 500 left is the `spawn_blocking` JoinError arm —
        // a genuine server fault, and it is not a bridge error. Anything else
        // is the defect returning.
        let bare_500s = body.matches("StatusCode::INTERNAL_SERVER_ERROR").count();
        assert_eq!(
            bare_500s, 1,
            "only the spawn_blocking JoinError arm may answer a bare 500; \
             bridge failures must go through the classifier. Body was:\n{body}"
        );
    }
}
