//! Network capture, console-error, browser-event, timeline, and
//! network-request-monitoring HTTP handlers.
//!
//! Extracted from `mod.rs` as part of the per-family handler split. Every
//! handler is `pub` so `mod.rs`'s `pub use network::*;` re-export keeps
//! `crate::mcp::ui_bridge::<name>` resolvable for external callers.

use std::collections::HashSet;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use tracing::info;

use crate::mcp::types::{ApiResponse, ApiState};

use super::helpers::deserialize_timestamp;
use super::request::{ui_bridge_request_sync, wrap_ipc_result};

// ============================================================================
// Console-error level filter (Item 3 of manual-test-remediation-2026-05-10)
// ============================================================================

/// Levels the SDK's `ConsoleCapture` actually emits today (see
/// `@qontinui/ui-bridge/debug/browser-capture-types.ts ConsoleCapturedEvent`).
const SDK_EMITTED_LEVELS: &[&str] = &["error", "warn", "unhandledrejection"];

/// Levels accepted in the `?level=` query param. The first three are the only
/// values that match anything today, but we keep `info`/`log`/`debug` valid so
/// callers can pre-write filters that match what the SDK *might* emit if its
/// capture is broadened upstream — they currently match zero entries.
/// `all` / `*` are sentinels that disable filtering (same as omitting the
/// param).
const VALID_LEVEL_TOKENS: &[&str] = &[
    "error",
    "warn",
    "unhandledrejection",
    "info",
    "log",
    "debug",
    "all",
    "*",
];

/// Parse the `?level=` query value (single value or comma-separated list).
///
/// Returns:
/// - `Ok(None)` when the filter is absent or matches `all`/`*` (caller should
///   pass entries through unfiltered).
/// - `Ok(Some(set))` for a concrete allow-list.
/// - `Err(unknown_tokens)` when any token is not in `VALID_LEVEL_TOKENS`.
///
/// Whitespace is trimmed and tokens lowercased before matching.
pub(super) fn parse_level_filter(raw: &str) -> Result<Option<HashSet<String>>, Vec<String>> {
    let mut wanted: HashSet<String> = HashSet::new();
    let mut unknown: Vec<String> = Vec::new();
    let mut saw_all = false;

    for token in raw.split(',') {
        let normalised = token.trim().to_ascii_lowercase();
        if normalised.is_empty() {
            continue;
        }
        if !VALID_LEVEL_TOKENS.contains(&normalised.as_str()) {
            unknown.push(normalised);
            continue;
        }
        if normalised == "all" || normalised == "*" {
            saw_all = true;
            continue;
        }
        wanted.insert(normalised);
    }

    if !unknown.is_empty() {
        return Err(unknown);
    }
    if saw_all || wanted.is_empty() {
        return Ok(None);
    }
    Ok(Some(wanted))
}

/// Apply the parsed `level` allow-list to the SDK's `get_console_errors`
/// response. Mutates `data["errors"]` in place when present; updates
/// `data["count"]` to the post-filter length so legacy callers see a
/// consistent count. Grouped responses are passed through unchanged because
/// the group fingerprint shape is materially different and out of scope for
/// the simple level filter.
pub(super) fn apply_level_filter(data: &mut serde_json::Value, levels: &HashSet<String>) {
    let Some(errors) = data.get_mut("errors").and_then(|v| v.as_array_mut()) else {
        return;
    };
    errors.retain(|entry| {
        entry
            .get("level")
            .and_then(|l| l.as_str())
            .map(|l| levels.contains(&l.to_ascii_lowercase()))
            .unwrap_or(false)
    });
    let new_len = errors.len();
    if let Some(count) = data.get_mut("count") {
        *count = serde_json::json!(new_len);
    }
}

// ============================================================================
// Query types
// ============================================================================

/// Query parameters for console errors endpoint.
/// Accepts `since` as either numeric (epoch ms) or ISO 8601 string.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleErrorsQuery {
    #[serde(default, deserialize_with = "deserialize_timestamp")]
    since: Option<f64>,
    /// Cursor for the new sinceId-aware BrowserEventCapture query.
    /// When present, the frontend uses the cursor overload of
    /// `getConsoleRecent({sinceId, limit})` and returns the richer
    /// `{errors, nextSinceId, droppedCount, bufferedCount}` shape which
    /// this handler forwards through verbatim.
    #[serde(default)]
    since_id: Option<u64>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    group: Option<bool>,
    #[serde(default)]
    group_by: Option<String>,
    /// Comma-separated allow-list of console levels to keep. Accepts
    /// `error`, `warn`, `unhandledrejection`, `info`, `log`, `debug`,
    /// or `all` / `*` (which disables filtering, same as omitting).
    /// Default unset behaviour is unchanged: every captured entry is
    /// returned. Item 3 of manual-test-remediation-2026-05-10 — the
    /// endpoint name implies error-level only but the SDK captures
    /// `console.warn` too, including info-level startup logs that
    /// devs accidentally route through `console.warn`.
    #[serde(default)]
    level: Option<String>,
}

/// Query parameters for browser events endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserEventsQuery {
    #[serde(default, rename = "type")]
    event_type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_timestamp")]
    since: Option<f64>,
    #[serde(default)]
    limit: Option<u32>,
}

/// Query parameters for timeline endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineQuery {
    #[serde(default, deserialize_with = "deserialize_timestamp")]
    since: Option<f64>,
    #[serde(default)]
    limit: Option<u32>,
}

/// Query parameters for network chains endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkChainsQuery {
    #[serde(default)]
    limit: Option<u32>,
}

/// Query parameters for network requests endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkRequestsQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    url_pattern: Option<String>,
    #[serde(default)]
    failures_only: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_timestamp")]
    since: Option<f64>,
    #[serde(default)]
    limit: Option<u32>,
}

// ============================================================================
// Console errors
// ============================================================================

/// Get console errors captured by the UI Bridge ConsoleCapture.
pub async fn ui_bridge_get_console_errors_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ConsoleErrorsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting console errors");

    // Parse `?level=` before doing any IPC work so an invalid value short-
    // circuits to HTTP 400 without burning a frontend round-trip.
    let level_filter = match query.level.as_deref() {
        None => None,
        Some(raw) => match parse_level_filter(raw) {
            Ok(parsed) => parsed,
            Err(unknown) => {
                let valid = SDK_EMITTED_LEVELS
                    .iter()
                    .copied()
                    .chain(["info", "log", "debug", "all", "*"])
                    .collect::<Vec<_>>()
                    .join(", ");
                let msg = format!(
                    "unknown level value(s): [{}]; valid: {}",
                    unknown.join(", "),
                    valid
                );
                return Err((StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::error(msg))));
            }
        },
    };

    let payload = serde_json::json!({
        "params": {
            "since": query.since,
            "sinceId": query.since_id,
            "limit": query.limit,
            "group": query.group,
            "groupBy": query.group_by
        }
    });

    let is_grouped = query.group.unwrap_or(false);

    let result = ui_bridge_request_sync(&state, "get_console_errors", payload)
        .await
        .map(|mut data| {
            // Apply the level filter before counting / forwarding so the
            // health-endpoint counter reflects what the caller actually
            // received. Grouped responses (`group=true`) keep the legacy
            // pass-through behaviour — the fingerprint shape doesn't carry
            // a flat `errors` array to filter.
            if !is_grouped {
                if let Some(levels) = level_filter.as_ref() {
                    apply_level_filter(&mut data, levels);
                }
            }
            data
        })
        .inspect(|data| {
            // Update the console error count for the health endpoint before
            // forwarding through wrap_ipc_result.
            if !is_grouped {
                if let Some(errors) = data.get("errors").and_then(|e| e.as_array()) {
                    state
                        .ui_bridge_console_error_count
                        .store(errors.len() as u64, std::sync::atomic::Ordering::Relaxed);
                }
            } else if let Some(total) = data.get("totalErrors").and_then(|t| t.as_u64()) {
                state
                    .ui_bridge_console_error_count
                    .store(total, std::sync::atomic::Ordering::Relaxed);
            }
            // The SDK's new cursor overload returns {errors, nextSinceId,
            // droppedCount, bufferedCount}; legacy callers (no sinceId) see
            // only {errors, count}. Either shape passes through unchanged
            // because `data` is the serde_json::Value the frontend emitted.
        });
    wrap_ipc_result(result)
}

/// Clear console errors captured by the UI Bridge ConsoleCapture.
pub async fn ui_bridge_clear_console_errors_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Clearing console errors");

    let result = ui_bridge_request_sync(&state, "clear_console_errors", serde_json::json!({}))
        .await
        .inspect(|_| {
            state
                .ui_bridge_console_error_count
                .store(0, std::sync::atomic::Ordering::Relaxed);
        });
    wrap_ipc_result(result)
}

// ============================================================================
// Browser events / timeline
// ============================================================================

/// Get browser events captured by the UI Bridge BrowserCapture.
pub async fn ui_bridge_get_browser_events_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<BrowserEventsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting browser events");

    let payload = serde_json::json!({
        "params": {
            "type": query.event_type,
            "since": query.since,
            "limit": query.limit
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_browser_events", payload).await)
}

/// Get timeline events captured by the UI Bridge BrowserCapture.
pub async fn ui_bridge_get_timeline_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TimelineQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting timeline");

    let payload = serde_json::json!({
        "params": {
            "since": query.since,
            "limit": query.limit
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_timeline", payload).await)
}

// ============================================================================
// Network chains
// ============================================================================

/// Get network request chains from the UI Bridge BrowserCapture.
pub async fn ui_bridge_get_network_chains_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<NetworkChainsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting network chains");

    let payload = serde_json::json!({
        "params": {
            "limit": query.limit
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_network_chains", payload).await)
}

// ============================================================================
// Network request monitoring
// ============================================================================

/// List network requests with optional filters.
pub async fn ui_bridge_get_network_requests_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<NetworkRequestsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting network requests");

    let payload = serde_json::json!({
        "params": {
            "status": query.status,
            "method": query.method,
            "urlPattern": query.url_pattern,
            "failuresOnly": query.failures_only,
            "since": query.since,
            "limit": query.limit
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_network_requests", payload).await)
}

/// Get currently in-flight network requests.
pub async fn ui_bridge_get_network_requests_in_flight_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting in-flight network requests");

    wrap_ipc_result(
        ui_bridge_request_sync(
            &state,
            "get_network_requests_in_flight",
            serde_json::json!({}),
        )
        .await,
    )
}

/// Wait for a specific network request matching criteria.
pub async fn ui_bridge_wait_for_network_request_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for network request");

    wrap_ipc_result(ui_bridge_request_sync(&state, "wait_for_network_request", body).await)
}

/// Get a specific network request by ID.
pub async fn ui_bridge_get_network_request_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting network request {}", id);

    wrap_ipc_result(
        ui_bridge_request_sync(
            &state,
            "get_network_request",
            serde_json::json!({ "id": id }),
        )
        .await,
    )
}

// ============================================================================
// Route registration
// ============================================================================

/// Network, console-error, browser-event, timeline, and network-request
/// routes.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/ui-bridge/control/console-errors",
            get(ui_bridge_get_console_errors_handler),
        )
        .route(
            "/ui-bridge/control/console-errors/clear",
            post(ui_bridge_clear_console_errors_handler),
        )
        .route(
            "/ui-bridge/control/browser-events",
            get(ui_bridge_get_browser_events_handler),
        )
        .route(
            "/ui-bridge/control/timeline",
            get(ui_bridge_get_timeline_handler),
        )
        .route(
            "/ui-bridge/control/network-chains",
            get(ui_bridge_get_network_chains_handler),
        )
        .route(
            "/ui-bridge/control/network-requests",
            get(ui_bridge_get_network_requests_handler),
        )
        .route(
            "/ui-bridge/control/network-requests/in-flight",
            get(ui_bridge_get_network_requests_in_flight_handler),
        )
        .route(
            "/ui-bridge/control/network-requests/wait",
            post(ui_bridge_wait_for_network_request_handler),
        )
        .route(
            "/ui-bridge/control/network-request/{id}",
            get(ui_bridge_get_network_request_handler),
        )
}

/// Static (method, path) tuples matching every route registered by `routes()`.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/control/console-errors"),
        ("POST", "/ui-bridge/control/console-errors/clear"),
        ("GET", "/ui-bridge/control/browser-events"),
        ("GET", "/ui-bridge/control/timeline"),
        ("GET", "/ui-bridge/control/network-chains"),
        ("GET", "/ui-bridge/control/network-requests"),
        ("GET", "/ui-bridge/control/network-requests/in-flight"),
        ("POST", "/ui-bridge/control/network-requests/wait"),
        ("GET", "/ui-bridge/control/network-request/{id}"),
    ]
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod console_level_filter_tests {
    //! Unit tests for the `?level=` query param added to
    //! `/ui-bridge/control/console-errors`. We exercise the pure helpers
    //! (`parse_level_filter` + `apply_level_filter`) so we don't need an
    //! axum router or a live frontend — same testing pattern as
    //! `tab_activate_tests` and `page_navigate_mode_tests` in `page.rs`.
    //!
    //! Item 3 of `plans/manual-test-remediation-2026-05-10.md`.

    use super::{apply_level_filter, parse_level_filter};
    use std::collections::HashSet;

    fn synthetic_ring() -> serde_json::Value {
        // Mirrors the SDK's `{errors, count}` legacy envelope so the
        // helpers can be exercised the same way the live IPC response
        // would surface them.
        serde_json::json!({
            "errors": [
                {"timestamp": 1, "level": "error", "message": "boom"},
                {"timestamp": 2, "level": "warn",  "message": "[GraphQL WS] Connected"},
                {"timestamp": 3, "level": "warn",  "message": "[BackgroundObserverService] Started"},
                {"timestamp": 4, "level": "unhandledrejection", "message": "promise rejected"},
                {"timestamp": 5, "level": "error", "message": "second error"},
            ],
            "count": 5
        })
    }

    #[test]
    fn level_error_keeps_only_error_entries() {
        let mut data = synthetic_ring();
        let levels: HashSet<String> = ["error".to_string()].into_iter().collect();
        apply_level_filter(&mut data, &levels);

        let errors = data["errors"].as_array().expect("errors is array");
        assert_eq!(errors.len(), 2, "only the two error entries should remain");
        for entry in errors {
            assert_eq!(entry["level"].as_str(), Some("error"));
        }
        assert_eq!(
            data["count"].as_u64(),
            Some(2),
            "count must be updated to post-filter length"
        );
    }

    #[test]
    fn level_error_warn_keeps_both() {
        // The exact callsite from manual-test-remediation: caller asks for
        // `level=error,warn` to drop the `info` startup logs that the
        // codebase routes through `console.warn`. Both warn entries
        // intentionally still come through here — caller is filtering on
        // *level*, not *content*. The fix for the noisy startup messages
        // themselves lives in the runner's own callsites
        // (graphql-client.ts + background-observer-service.ts).
        let mut data = synthetic_ring();
        let levels: HashSet<String> = ["error".to_string(), "warn".to_string()]
            .into_iter()
            .collect();
        apply_level_filter(&mut data, &levels);

        let errors = data["errors"].as_array().expect("errors is array");
        assert_eq!(errors.len(), 4);
        // No unhandledrejection survives.
        assert!(errors
            .iter()
            .all(|e| e["level"].as_str() != Some("unhandledrejection")));
    }

    #[test]
    fn parse_level_handles_single_value() {
        let parsed = parse_level_filter("error").expect("valid");
        let set = parsed.expect("non-all set");
        assert!(set.contains("error") && set.len() == 1);
    }

    #[test]
    fn parse_level_handles_comma_separated() {
        let parsed = parse_level_filter("error,warn").expect("valid");
        let set = parsed.expect("non-all set");
        assert_eq!(set.len(), 2);
        assert!(set.contains("error"));
        assert!(set.contains("warn"));
    }

    #[test]
    fn parse_level_trims_whitespace_and_lowercases() {
        let parsed = parse_level_filter(" Error , WARN ").expect("valid");
        let set = parsed.expect("non-all set");
        assert!(set.contains("error"));
        assert!(set.contains("warn"));
    }

    #[test]
    fn parse_level_all_disables_filtering() {
        assert!(parse_level_filter("all").expect("valid").is_none());
        assert!(parse_level_filter("*").expect("valid").is_none());
        // Mixing `all` with other tokens still disables filtering — the
        // caller asked to see everything.
        assert!(parse_level_filter("error,all").expect("valid").is_none());
    }

    #[test]
    fn parse_level_unknown_token_returns_err() {
        let err = parse_level_filter("error,banana").expect_err("banana is not a level");
        assert_eq!(err, vec!["banana".to_string()]);
    }

    #[test]
    fn parse_level_empty_string_disables_filtering() {
        // An explicitly empty `?level=` is treated as no-op (returns None)
        // so callers building URLs with empty defaults don't trip a 400.
        assert!(parse_level_filter("").expect("valid").is_none());
        assert!(parse_level_filter("   ").expect("valid").is_none());
    }

    #[test]
    fn apply_level_filter_handles_missing_errors_array() {
        // Edge-case: SDK responded with only a note (`BrowserCapture not
        // installed`). Filter must not panic.
        let mut data = serde_json::json!({"note": "BrowserCapture not installed"});
        let levels: HashSet<String> = ["error".to_string()].into_iter().collect();
        apply_level_filter(&mut data, &levels);
        assert_eq!(data["note"].as_str(), Some("BrowserCapture not installed"));
    }

    #[test]
    fn apply_level_filter_drops_entries_missing_level_field() {
        // Defensive: an entry without a `level` field shouldn't survive a
        // concrete filter — there's no way to know if it matches.
        let mut data = serde_json::json!({
            "errors": [
                {"timestamp": 1, "level": "error", "message": "ok"},
                {"timestamp": 2, "message": "no level"},
            ],
            "count": 2
        });
        let levels: HashSet<String> = ["error".to_string()].into_iter().collect();
        apply_level_filter(&mut data, &levels);
        assert_eq!(data["errors"].as_array().unwrap().len(), 1);
    }
}
