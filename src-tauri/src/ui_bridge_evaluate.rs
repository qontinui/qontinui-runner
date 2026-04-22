//! UI Bridge page/evaluate request correlation — Plan item D (post-Phase-3I).
//!
//! Supplies a keyed pending-oneshot store for `/ui-bridge/control/page/evaluate`
//! that mirrors the [`crate::ui_bridge_invoke::InvokeRequestStore`] pattern
//! built for the invoke proxy in Phase 3I.1. Each HTTP call allocates a
//! fresh `request_id`, registers its oneshot sender keyed by that id, and
//! the response listener installed in `mcp_api.rs` matches on id when a
//! `ui-bridge:evaluate-response` event arrives.
//!
//! # Why a distinct store and event pair?
//!
//! The pre-existing `ui_bridge_pending` map on `ApiState` already uses a
//! keyed HashMap by `request_id`, but it also multiplexes many other
//! request types (`get_elements`, `discover`, `page_navigate`, etc.) and
//! goes through `ui_bridge_request_sync`'s circuit breaker / semaphore /
//! dedup layers. Coupling the new `/page/evaluate` correlation to that
//! shared machinery would force `eval()`-only callers to share
//! concurrency / readiness / circuit-breaker state with unrelated bridge
//! traffic — and would also leave the existing page_evaluate frontend
//! handler's "single pending-promise slot" hazard intact where other
//! Rust paths emit `ui_bridge_request_sync("page_evaluate", ...)` for
//! internal JS eval (safe_evaluate, batch, evaluate_js_expression).
//!
//! By carving out a dedicated store + event pair
//! (`ui-bridge:evaluate-request` / `ui-bridge:evaluate-response`), the
//! external HTTP `/page/evaluate` route gets explicit per-call
//! correlation without disturbing the legacy internal call sites.
//!
//! # Flow
//!
//! 1. HTTP handler generates a fresh `request_id` (uuid v4).
//! 2. Creates a `tokio::sync::oneshot` channel; stashes the sender in the
//!    [`EvaluateRequestStore`] keyed by `request_id`.
//! 3. Emits Tauri event `ui-bridge:evaluate-request` with
//!    `{ request_id, expression, await_promise, timeout_ms }` to the
//!    React frontend.
//! 4. The React side runs the expression (via the same security-gated
//!    `new Function(...)` path used by the legacy `page_evaluate` handler)
//!    and emits `ui-bridge:evaluate-response` with
//!    `{ request_id, ok, result, error }`.
//! 5. A global Tauri listener installed at `mcp_api.rs` startup parses the
//!    response and calls [`EvaluateRequestStore::deliver`] which fires the
//!    matching oneshot.
//! 6. The HTTP handler awaits the receiver with a configurable timeout,
//!    returning the result (or 504 on timeout, 500 on frontend error).

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio::sync::Mutex;

/// Strongly-typed response from the React frontend for an evaluate request.
///
/// `ok=true` means the JS expression evaluated successfully; `result` holds
/// its JSON-serialized return value (possibly `Value::Null` if the
/// expression returned `undefined` / `null`).
///
/// `ok=false` means the expression threw (or the frontend's security
/// allowlist rejected it, or `eval` itself produced a SyntaxError);
/// `error` carries the string representation of the failure so the HTTP
/// caller sees the real JS-side cause.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluateResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Keyed store of pending oneshot senders — one per in-flight evaluate.
///
/// Multiple evaluates can be in-flight at once; the HashMap is keyed by a
/// fresh uuid-v4 `request_id` so concurrent callers never observe each
/// other's responses.
pub struct EvaluateRequestStore {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<EvaluateResponse>>>>,
}

impl Default for EvaluateRequestStore {
    fn default() -> Self {
        Self::new()
    }
}

impl EvaluateRequestStore {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a new pending evaluate.
    ///
    /// Takes ownership of the oneshot sender and stashes it under
    /// `request_id`. If an entry for that id already exists (shouldn't
    /// happen with uuid v4, but defensive), it is silently replaced —
    /// the prior waiter will observe a dropped-sender error via
    /// `Receiver::await`.
    pub async fn register(&self, request_id: String, sender: oneshot::Sender<EvaluateResponse>) {
        let mut guard = self.pending.lock().await;
        guard.insert(request_id, sender);
    }

    /// Deliver a response to a pending evaluate by id.
    ///
    /// Removes the entry from the map and sends the response through the
    /// oneshot. If the receiver has already been dropped (e.g. the HTTP
    /// handler timed out and bailed), the response is silently discarded —
    /// this mirrors the "best effort" semantics of oneshot channels.
    /// Returns `true` if a pending entry existed for this id.
    pub async fn deliver(&self, request_id: &str, response: EvaluateResponse) -> bool {
        let mut guard = self.pending.lock().await;
        if let Some(sender) = guard.remove(request_id) {
            let _ = sender.send(response);
            true
        } else {
            false
        }
    }

    /// Cancel a pending evaluate — removes the entry without delivering.
    ///
    /// Called by the HTTP handler on timeout / emit failure so a subsequent
    /// late response doesn't linger in the map.
    pub async fn cancel(&self, request_id: &str) {
        let mut guard = self.pending.lock().await;
        guard.remove(request_id);
    }

    /// Snapshot of the pending entry count — useful for tests and for
    /// diagnostics surfaced via `/health`.
    #[cfg(test)]
    pub async fn pending_len(&self) -> usize {
        let guard = self.pending.lock().await;
        guard.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_and_deliver_round_trip() {
        let store = EvaluateRequestStore::new();
        let (tx, rx) = oneshot::channel();
        store.register("req-1".to_string(), tx).await;

        let response = EvaluateResponse {
            ok: true,
            result: Some(serde_json::json!({ "result": { "value": 42 } })),
            error: None,
        };
        assert!(store.deliver("req-1", response.clone()).await);

        let received = rx.await.expect("receiver should observe sent value");
        assert!(received.ok);
        assert_eq!(received.result, response.result);
        assert!(received.error.is_none());
    }

    #[tokio::test]
    async fn deliver_unknown_request_id_is_noop() {
        let store = EvaluateRequestStore::new();
        let delivered = store
            .deliver(
                "does-not-exist",
                EvaluateResponse {
                    ok: true,
                    result: Some(serde_json::Value::Null),
                    error: None,
                },
            )
            .await;
        assert!(!delivered);
    }

    #[tokio::test]
    async fn cancel_removes_entry_so_late_deliver_is_noop() {
        let store = EvaluateRequestStore::new();
        let (tx, _rx) = oneshot::channel();
        store.register("req-2".to_string(), tx).await;
        assert_eq!(store.pending_len().await, 1);

        store.cancel("req-2").await;
        assert_eq!(store.pending_len().await, 0);

        let delivered = store
            .deliver(
                "req-2",
                EvaluateResponse {
                    ok: false,
                    result: None,
                    error: Some("too late".to_string()),
                },
            )
            .await;
        assert!(!delivered);
    }

    #[tokio::test]
    async fn error_response_round_trip() {
        let store = EvaluateRequestStore::new();
        let (tx, rx) = oneshot::channel();
        store.register("req-3".to_string(), tx).await;

        let response = EvaluateResponse {
            ok: false,
            result: None,
            error: Some("SyntaxError: Unexpected token".to_string()),
        };
        assert!(store.deliver("req-3", response.clone()).await);
        let received = rx.await.expect("receiver should observe error value");
        assert!(!received.ok);
        assert_eq!(received.error, response.error);
    }

    #[tokio::test]
    async fn concurrent_requests_get_distinct_responses() {
        // The central regression the plan calls out: two simultaneous
        // /page/evaluate callers must never observe each other's results.
        // With per-id keying this is trivially true — but the test exercises
        // the full register → deliver → await round-trip for both in
        // parallel, with deliveries in the opposite order of registration
        // to prove the correlation is id-driven, not arrival-ordered.
        let store = Arc::new(EvaluateRequestStore::new());

        let (tx_a, rx_a) = oneshot::channel();
        let (tx_b, rx_b) = oneshot::channel();
        store.register("req-a".to_string(), tx_a).await;
        store.register("req-b".to_string(), tx_b).await;
        assert_eq!(store.pending_len().await, 2);

        // Deliver B first, then A — exercises that arrival order doesn't
        // mix up which oneshot gets which payload.
        let store_b = store.clone();
        let deliver_b = tokio::spawn(async move {
            store_b
                .deliver(
                    "req-b",
                    EvaluateResponse {
                        ok: true,
                        result: Some(serde_json::json!("beta")),
                        error: None,
                    },
                )
                .await
        });

        let store_a = store.clone();
        let deliver_a = tokio::spawn(async move {
            store_a
                .deliver(
                    "req-a",
                    EvaluateResponse {
                        ok: true,
                        result: Some(serde_json::json!("alpha")),
                        error: None,
                    },
                )
                .await
        });

        assert!(deliver_a.await.expect("deliver_a task should not panic"));
        assert!(deliver_b.await.expect("deliver_b task should not panic"));

        let received_a = rx_a.await.expect("rx_a should resolve");
        let received_b = rx_b.await.expect("rx_b should resolve");

        assert_eq!(received_a.result, Some(serde_json::json!("alpha")));
        assert_eq!(received_b.result, Some(serde_json::json!("beta")));
        assert_eq!(store.pending_len().await, 0);
    }
}
