//! POST /ui-bridge/ai/wait-for — unified declarative state-polling endpoint.
//!
//! Accepts a predicate union + timeout and polls the SDK's element snapshot
//! (via the existing IPC channel used by `/control/snapshot`) on a 100ms
//! cadence until the predicate is satisfied or the timeout elapses.
//!
//! Supported predicate shapes:
//! - `{ "type": "textVisible", "text": "..." }` — substring search across
//!   every element's `content`, `label`, `identifier.text`, and
//!   `state.textContent` fields.
//! - `{ "type": "elementVisible", "id": "..." }` — element present with
//!   `state.visible === true`.
//! - `{ "type": "elementDisappeared", "id": "..." }` — element absent, or
//!   present with `state.visible === false`.
//! - `{ "type": "snapshotChanged", "since": <ms> }` — `ApiState`'s
//!   monotonic `ui_bridge_event_sequence` counter has advanced since the
//!   caller's wall-clock timestamp. Inexact by design — we use the action
//!   sequence as a proxy for "something changed," since the counter
//!   ticks on every element action persisted to task_runs.
//! - `{ "type": "elementValue", "id": "...", "equals": "..." }` —
//!   element present with `state.value === equals`.
//!
//! Server-side hard ceiling on `timeoutMs` is 30_000 (requests above this
//! are rejected with 400). If the SDK isn't connected (no pong received
//! yet) the handler returns `satisfied: false, timedOut: true,
//! elapsedMs: 0` immediately rather than waiting out the full timeout.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{extract::State, http::StatusCode, response::Json, routing::post, Router};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::mcp::ui_bridge::ui_bridge_request_sync;

// ============================================================================
// Types
// ============================================================================

const HARD_CEILING_MS: u64 = 30_000;
const POLL_INTERVAL_MS: u64 = 100;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WaitForPredicate {
    #[serde(rename = "textVisible")]
    TextVisible { text: String },
    #[serde(rename = "elementVisible")]
    ElementVisible { id: String },
    #[serde(rename = "elementDisappeared")]
    ElementDisappeared { id: String },
    #[serde(rename = "snapshotChanged")]
    SnapshotChanged { since: i64 },
    #[serde(rename = "elementValue")]
    ElementValue { id: String, equals: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitForRequest {
    pub predicate: WaitForPredicate,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_timeout_ms() -> u64 {
    3_000
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitForResponse {
    pub satisfied: bool,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timed_out: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

// ============================================================================
// Predicate evaluation
// ============================================================================

/// Evaluate a non-`snapshotChanged` predicate against a fresh snapshot JSON
/// value. Returns `true` if the predicate is satisfied.
fn evaluate_snapshot_predicate(snapshot: &serde_json::Value, pred: &WaitForPredicate) -> bool {
    let empty = Vec::new();
    let elements = snapshot
        .get("elements")
        .and_then(|e| e.as_array())
        .unwrap_or(&empty);

    match pred {
        WaitForPredicate::TextVisible { text } => {
            let needle = text.as_str();
            if needle.is_empty() {
                return true;
            }
            for el in elements {
                // Search common text-bearing fields
                let label = el.get("label").and_then(|v| v.as_str()).unwrap_or("");
                let content = el.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let ident_text = el
                    .get("identifier")
                    .and_then(|i| i.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let text_content = el
                    .get("state")
                    .and_then(|s| s.get("textContent"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let accessible_name = el
                    .get("state")
                    .and_then(|s| s.get("accessibleName"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if label.contains(needle)
                    || content.contains(needle)
                    || ident_text.contains(needle)
                    || text_content.contains(needle)
                    || accessible_name.contains(needle)
                {
                    return true;
                }
            }
            false
        }
        WaitForPredicate::ElementVisible { id } => {
            for el in elements {
                if el.get("id").and_then(|v| v.as_str()) == Some(id.as_str()) {
                    let visible = el
                        .get("state")
                        .and_then(|s| s.get("visible"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true); // Missing state.visible -> treat as visible
                    return visible;
                }
            }
            false
        }
        WaitForPredicate::ElementDisappeared { id } => {
            for el in elements {
                if el.get("id").and_then(|v| v.as_str()) == Some(id.as_str()) {
                    let visible = el
                        .get("state")
                        .and_then(|s| s.get("visible"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    return !visible;
                }
            }
            // Not in snapshot at all => disappeared
            true
        }
        WaitForPredicate::ElementValue { id, equals } => {
            for el in elements {
                if el.get("id").and_then(|v| v.as_str()) == Some(id.as_str()) {
                    let value = el
                        .get("state")
                        .and_then(|s| s.get("value"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    return value == equals.as_str();
                }
            }
            false
        }
        WaitForPredicate::SnapshotChanged { .. } => {
            // Handled at the caller: this branch is never reached.
            false
        }
    }
}

// ============================================================================
// Handler
// ============================================================================

pub async fn ai_wait_for_handler(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<WaitForRequest>,
) -> Result<Json<ApiResponse<WaitForResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let start = Instant::now();

    // Validate timeout upper bound
    if req.timeout_ms > HARD_CEILING_MS {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "timeoutMs {} exceeds server-side ceiling of {}ms",
                req.timeout_ms, HARD_CEILING_MS
            ))),
        ));
    }
    if req.timeout_ms == 0 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("timeoutMs must be > 0")),
        ));
    }

    // Short-circuit when the SDK hasn't connected yet — don't burn the
    // full timeout waiting for a snapshot that will never come.
    let last_pong = state
        .ui_bridge_last_pong
        .load(std::sync::atomic::Ordering::Relaxed);
    if last_pong == 0 {
        debug!("ai/wait-for: SDK not connected (no pong), short-circuiting");
        return Ok(Json(ApiResponse::success(WaitForResponse {
            satisfied: false,
            elapsed_ms: 0,
            timed_out: Some(true),
            last_error: Some("SDK not connected".to_string()),
        })));
    }

    info!(
        "ai/wait-for: predicate={:?} timeout_ms={}",
        std::mem::discriminant(&req.predicate),
        req.timeout_ms
    );

    // snapshotChanged is handled without hitting the IPC channel: we just
    // compare the monotonic event sequence's wall-clock-approximation with
    // `since`. Ticks when any persisted UI Bridge action is recorded.
    if let WaitForPredicate::SnapshotChanged { since } = &req.predicate {
        // Poll `ui_bridge_event_sequence` for changes. We capture the
        // sequence at entry and satisfy the predicate the moment it ticks,
        // but only if the caller's `since` timestamp is in the past
        // (otherwise the predicate is trivially unsatisfiable).
        let entry_seq = state
            .ui_bridge_event_sequence
            .load(std::sync::atomic::Ordering::Relaxed);
        let caller_since = *since;
        let now_ms = chrono::Utc::now().timestamp_millis();
        if caller_since > now_ms {
            // Future `since` is a programming error — return timeout.
            return Ok(Json(ApiResponse::success(WaitForResponse {
                satisfied: false,
                elapsed_ms: start.elapsed().as_millis() as u64,
                timed_out: Some(true),
                last_error: Some("since is in the future".to_string()),
            })));
        }

        let deadline = start + Duration::from_millis(req.timeout_ms);
        loop {
            let cur = state
                .ui_bridge_event_sequence
                .load(std::sync::atomic::Ordering::Relaxed);
            if cur > entry_seq {
                return Ok(Json(ApiResponse::success(WaitForResponse {
                    satisfied: true,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    timed_out: None,
                    last_error: None,
                })));
            }
            if Instant::now() + Duration::from_millis(POLL_INTERVAL_MS) > deadline {
                return Ok(Json(ApiResponse::success(WaitForResponse {
                    satisfied: false,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    timed_out: Some(true),
                    last_error: None,
                })));
            }
            tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
        }
    }

    // Snapshot-based predicates: poll `get_snapshot` at POLL_INTERVAL_MS.
    let deadline = start + Duration::from_millis(req.timeout_ms);
    let mut last_error: Option<String> = None;
    let mut ticker = tokio::time::interval(Duration::from_millis(POLL_INTERVAL_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        // Tick before the snapshot request: the first tick fires immediately,
        // so we get an initial check, then each subsequent iteration waits
        // 100ms before the next probe.
        ticker.tick().await;

        // Enforce the outer deadline before making another IPC round-trip.
        if Instant::now() >= deadline {
            break;
        }

        // Use a tokio::time::timeout on the IPC call bounded by whatever
        // remains of the outer deadline so one slow snapshot doesn't
        // consume the whole budget.
        let remaining = deadline.saturating_duration_since(Instant::now());
        let snap_fut = ui_bridge_request_sync(&state, "get_snapshot", serde_json::json!({}));
        match tokio::time::timeout(remaining, snap_fut).await {
            Ok(Ok(snapshot)) => {
                if evaluate_snapshot_predicate(&snapshot, &req.predicate) {
                    return Ok(Json(ApiResponse::success(WaitForResponse {
                        satisfied: true,
                        elapsed_ms: start.elapsed().as_millis() as u64,
                        timed_out: None,
                        last_error: None,
                    })));
                }
            }
            Ok(Err(e)) => {
                last_error = Some(e);
            }
            Err(_) => {
                // outer deadline reached during the IPC call
                break;
            }
        }
    }

    Ok(Json(ApiResponse::success(WaitForResponse {
        satisfied: false,
        elapsed_ms: start.elapsed().as_millis() as u64,
        timed_out: Some(true),
        last_error,
    })))
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/ui-bridge/ai/wait-for", post(ai_wait_for_handler))
}
