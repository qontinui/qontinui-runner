//! Terminal-sessions HTTP handlers.
//!
//! `GET /ui-bridge/control/terminal-sessions` — list every PTY-backed
//! terminal tab the runner currently has open (plain + AI sessions),
//! together with the per-tab session-machine `state` and the
//! Claude-Code `task_run_id` once it's been captured.
//!
//! `GET /ui-bridge/control/terminal-sessions/{id}` — single entry lookup
//! keyed by `tab.id`. Returns HTTP 404 when the id isn't a known tab.
//!
//! The authoritative tab list lives in the frontend (`useTerminalManager`
//! inside `TerminalCoreProvider`). The page publishes a snapshot to a
//! module-level registry (`@/lib/terminal-sessions-registry`) on every
//! render; the IPC handler in `useTerminalsEvents.ts` reads that
//! snapshot. This keeps the substrate symmetric with `tabs_list` in
//! `usePageEvents.ts`, which reads from `instanceStorage` for the same
//! "frontend-owned data, stateless handler" reason.
//!
//! These two endpoints unblock autonomous testing of the
//! lock-yield protocol: a caller can issue `create-best-account`,
//! capture the returned `tab_ids`, then poll
//! `/terminal-sessions/{tab_id}` until `state === "ready"` /
//! `task_run_id` is non-null without resorting to screen scraping.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::request::ui_bridge_request_sync;

/// `GET /ui-bridge/control/terminal-sessions`
///
/// Returns `{ sessions: TerminalSessionEntry[] }` covering every tab in
/// the runner's terminal page. Empty array (not a 404) when no terminal
/// tabs are open. The shape is fixed by `useTerminalsEvents.ts` —
/// adding new fields requires matching changes on both sides.
pub async fn ui_bridge_terminal_sessions_list_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: terminal_sessions_list");

    match ui_bridge_request_sync(&state, "terminal_sessions_list", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: terminal_sessions_list failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Classify a `terminal_session_get` IPC response into the
/// (http_status, body) pair the handler should return. Pulled out so
/// the 404/200 decision can be unit-tested without a live frontend.
///
/// Returns:
/// - `Ok(success_body)` — frontend reported `found: true`. Body wraps
///   the IPC `data` as a success ApiResponse.
/// - `Err((NOT_FOUND, body))` — frontend reported `found: false`. Body
///   carries `{error, knownIds, id}` so callers can recover from typos
///   without a second list call.
/// - `Err((INTERNAL_SERVER_ERROR, body))` — shape was unexpected (no
///   `found` key); treat as a server-side bug.
pub(crate) fn classify_terminal_session_get_response(
    id: &str,
    data: serde_json::Value,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<serde_json::Value>>)>
{
    match data.get("found").and_then(serde_json::Value::as_bool) {
        Some(true) => Ok(Json(ApiResponse::success(data))),
        Some(false) => {
            let known_ids = data
                .get("knownIds")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([]));
            let body = ApiResponse {
                success: false,
                data: Some(serde_json::json!({
                    "error": "unknown_terminal_session",
                    "knownIds": known_ids,
                    "id": id,
                })),
                error: Some(format!("unknown terminal session: \"{}\"", id)),
                error_detail: None,
                hint: None,
                code: None,
                suggestions: None,
            };
            Err((StatusCode::NOT_FOUND, Json(body)))
        }
        None => {
            // The frontend handler always sets `found`; missing key means
            // a payload shape regression. Surface as 500 with the raw body
            // for diagnosis rather than silently treating it as a hit.
            let body = ApiResponse {
                success: false,
                data: Some(data),
                error: Some(
                    "terminal_session_get: missing `found` discriminator in IPC response"
                        .to_string(),
                ),
                error_detail: None,
                hint: None,
                code: None,
                suggestions: None,
            };
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(body)))
        }
    }
}

/// `GET /ui-bridge/control/terminal-sessions/{id}`
///
/// Looks up a single terminal session by `tab.id`. Returns HTTP 404
/// (with the known ids in the error body) when no match is found, so
/// callers polling for a freshly-spawned tab can recover without
/// dropping back to the list endpoint.
pub async fn ui_bridge_terminal_session_get_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<serde_json::Value>>)>
{
    info!("UI Bridge API: terminal_session_get -> {}", id);

    let payload = serde_json::json!({ "id": id });
    match ui_bridge_request_sync(&state, "terminal_session_get", payload).await {
        Ok(data) => classify_terminal_session_get_response(&id, data),
        Err(e) => {
            error!("UI Bridge API: terminal_session_get IPC failed: {}", e);
            let body = ApiResponse {
                success: false,
                data: None,
                error: Some(e),
                error_detail: None,
                hint: None,
                code: None,
                suggestions: None,
            };
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(body)))
        }
    }
}

/// Terminal-session inspection routes.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::get;
    axum::Router::new()
        .route(
            "/ui-bridge/control/terminal-sessions",
            get(ui_bridge_terminal_sessions_list_handler),
        )
        .route(
            "/ui-bridge/control/terminal-sessions/{id}",
            get(ui_bridge_terminal_session_get_handler),
        )
}

/// Static (method, path) tuples corresponding to every route registered
/// by `routes()`. Concatenated into `route_manifest()` in `mod.rs`.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/control/terminal-sessions"),
        ("GET", "/ui-bridge/control/terminal-sessions/{id}"),
    ]
}

#[cfg(test)]
mod terminal_session_classifier_tests {
    //! Unit tests for `classify_terminal_session_get_response`. The
    //! handler delegates the success/404/500 decision to this pure
    //! helper so we can lock down the wire shape without a live frontend
    //! — same pattern as `tab_activate_tests` / `page_navigate_mode_tests`.

    use super::classify_terminal_session_get_response;
    use axum::http::StatusCode;

    #[test]
    fn found_true_returns_200_with_data_passthrough() {
        let data = serde_json::json!({
            "found": true,
            "session": { "id": "tab-1", "title": "Terminal 1", "state": "idle" }
        });
        let resp = classify_terminal_session_get_response("tab-1", data.clone()).unwrap();
        let body = resp.0;
        assert!(body.success);
        assert_eq!(body.data.as_ref().unwrap(), &data);
    }

    #[test]
    fn found_false_returns_404_with_known_ids_hint() {
        let data = serde_json::json!({
            "found": false,
            "knownIds": ["tab-1", "tab-2"]
        });
        let err = classify_terminal_session_get_response("missing", data).unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        let body = err.1 .0;
        assert!(!body.success);
        let body_data = body.data.expect("404 body carries data");
        assert_eq!(body_data["error"], "unknown_terminal_session");
        assert_eq!(body_data["id"], "missing");
        assert_eq!(body_data["knownIds"], serde_json::json!(["tab-1", "tab-2"]));
    }

    #[test]
    fn found_false_with_no_known_ids_still_404s() {
        // The empty-tabs case: frontend just published `[]`, so `knownIds`
        // is an empty array rather than missing. The 404 still fires and
        // the body shape stays consistent.
        let data = serde_json::json!({ "found": false, "knownIds": [] });
        let err = classify_terminal_session_get_response("anything", data).unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        assert_eq!(err.1 .0.data.unwrap()["knownIds"], serde_json::json!([]));
    }

    #[test]
    fn missing_found_key_returns_500() {
        // Payload-shape regression. Surface as 500 rather than guessing.
        let data = serde_json::json!({ "session": { "id": "x" } });
        let err = classify_terminal_session_get_response("x", data).unwrap_err();
        assert_eq!(err.0, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.1 .0.error.as_deref().unwrap().contains("found"));
    }

    /// Routes/route_entries drift guard. The `manifest_matches_route_calls`
    /// test in `mod.rs` re-scans the source file to verify this, but a
    /// local equality check catches the more common "forgot to add a
    /// tuple" mistake earlier (and runs as part of `cargo test -p
    /// qontinui-runner terminals::`).
    #[test]
    fn route_entries_match_route_count() {
        // Build the router and count its `.route(...)` calls indirectly
        // via `route_entries()` — every entry corresponds to one route.
        // The constant matches the number of `.route(` calls in
        // `routes()`; bumping one without the other fails compile via
        // the manifest test, but this lower-bound check catches local
        // edits before that broader test runs.
        assert_eq!(super::route_entries().len(), 2);
        for (method, path) in super::route_entries() {
            assert_eq!(*method, "GET", "all terminal-session routes are GET");
            assert!(
                path.starts_with("/ui-bridge/control/terminal-sessions"),
                "unexpected path prefix: {}",
                path
            );
        }
    }
}
