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

/// Build the evaluate-store map key.
///
/// Keying by `(window_label, request_id)` rather than `request_id` alone makes a
/// `ui-bridge:evaluate-response` from a window the request was NOT addressed to a
/// no-op (its computed key can't match the stored one). This is what stops the
/// process-global `ui-bridge:evaluate-request` broadcast race once multiple
/// runner windows (main + `term-N` pop-outs) mount the SDK — without it, the
/// first window to answer resolves the oneshot and every other window still runs
/// the expression (so an evaluate that clicks a button fires N times).
///
/// `request_id` is a UUID, so the U+001F delimiter can never collide with a
/// (simple `main` / `term-N`) window label. Mirrors
/// [`crate::mcp::ui_bridge::request::pending_key`] so the two pending stores
/// share one key shape.
pub fn evaluate_pending_key(window_label: &str, request_id: &str) -> String {
    format!("{window_label}\u{1f}{request_id}")
}

/// Keyed store of pending oneshot senders — one per in-flight evaluate.
///
/// Multiple evaluates can be in-flight at once; the HashMap is keyed by
/// `(window_label, request_id)` (see [`evaluate_pending_key`]) so concurrent
/// callers — and concurrent windows — never observe each other's responses.
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

    /// Register a new pending evaluate, scoped to a target window.
    ///
    /// Takes ownership of the oneshot sender and stashes it under
    /// `(window_label, request_id)` (see [`evaluate_pending_key`]). If an entry
    /// for that key already exists (shouldn't happen with uuid v4, but
    /// defensive), it is silently replaced — the prior waiter will observe a
    /// dropped-sender error via `Receiver::await`.
    pub async fn register(
        &self,
        window_label: &str,
        request_id: &str,
        sender: oneshot::Sender<EvaluateResponse>,
    ) {
        let mut guard = self.pending.lock().await;
        guard.insert(evaluate_pending_key(window_label, request_id), sender);
    }

    /// Deliver a response to a pending evaluate by `(window_label, request_id)`.
    ///
    /// Removes the entry from the map and sends the response through the
    /// oneshot. A response from a window the request was NOT addressed to
    /// computes a key that isn't present, so it's a no-op — exactly what kills
    /// the broadcast race. If the receiver has already been dropped (e.g. the
    /// HTTP handler timed out and bailed), the response is silently discarded —
    /// this mirrors the "best effort" semantics of oneshot channels. Returns
    /// `true` if a pending entry existed for this key.
    pub async fn deliver(
        &self,
        window_label: &str,
        request_id: &str,
        response: EvaluateResponse,
    ) -> bool {
        let mut guard = self.pending.lock().await;
        if let Some(sender) = guard.remove(&evaluate_pending_key(window_label, request_id)) {
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
    pub async fn cancel(&self, window_label: &str, request_id: &str) {
        let mut guard = self.pending.lock().await;
        guard.remove(&evaluate_pending_key(window_label, request_id));
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

    // Default window label used by the single-window evaluate path. Kept
    // local to the test module so the test file doesn't reach across crates
    // for the `MAIN_WINDOW_LABEL` constant.
    const MAIN: &str = "main";

    #[test]
    fn evaluate_pending_key_is_distinct_per_window_for_same_request_id() {
        let id = "11111111-2222-3333-4444-555555555555";
        // Same request id in two windows must NOT collide on the store.
        assert_ne!(
            evaluate_pending_key("main", id),
            evaluate_pending_key("term-1", id)
        );
        // Same (window, id) is stable so register and deliver agree.
        assert_eq!(
            evaluate_pending_key("main", id),
            evaluate_pending_key("main", id)
        );
    }

    #[tokio::test]
    async fn register_and_deliver_round_trip() {
        let store = EvaluateRequestStore::new();
        let (tx, rx) = oneshot::channel();
        store.register(MAIN, "req-1", tx).await;

        let response = EvaluateResponse {
            ok: true,
            result: Some(serde_json::json!({ "result": { "value": 42 } })),
            error: None,
        };
        assert!(store.deliver(MAIN, "req-1", response.clone()).await);

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
                MAIN,
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
    async fn deliver_to_wrong_window_is_noop_and_leaves_entry_pending() {
        // The central multi-window regression: window A registers a request,
        // and a response that echoes a DIFFERENT window label must NOT resolve
        // it (that's the broadcast race — every window runs the same eval and
        // the first reply wins). Only the addressed window's response resolves.
        let store = EvaluateRequestStore::new();
        let (tx, _rx) = oneshot::channel();
        store.register("term-1", "req-w", tx).await;
        assert_eq!(store.pending_len().await, 1);

        // A reply tagged with the wrong window label must be a no-op...
        let wrong = store
            .deliver(
                "main",
                "req-w",
                EvaluateResponse {
                    ok: true,
                    result: Some(serde_json::json!("from-main")),
                    error: None,
                },
            )
            .await;
        assert!(!wrong, "wrong-window reply must not resolve the request");
        assert_eq!(
            store.pending_len().await,
            1,
            "entry must still be pending after a wrong-window reply"
        );

        // ...but the addressed window's reply resolves it.
        let right = store
            .deliver(
                "term-1",
                "req-w",
                EvaluateResponse {
                    ok: true,
                    result: Some(serde_json::json!("from-term-1")),
                    error: None,
                },
            )
            .await;
        assert!(right, "addressed-window reply must resolve the request");
        assert_eq!(store.pending_len().await, 0);
    }

    #[tokio::test]
    async fn same_request_id_in_two_windows_is_independent() {
        // Two windows can legitimately mint the same request id (the broadcast
        // emit hands every window the same envelope). The composite key keeps
        // their oneshots independent — each window's reply resolves only its own.
        let store = EvaluateRequestStore::new();
        let (tx_main, rx_main) = oneshot::channel();
        let (tx_term, rx_term) = oneshot::channel();
        store.register("main", "shared-id", tx_main).await;
        store.register("term-1", "shared-id", tx_term).await;
        assert_eq!(store.pending_len().await, 2);

        assert!(
            store
                .deliver(
                    "term-1",
                    "shared-id",
                    EvaluateResponse {
                        ok: true,
                        result: Some(serde_json::json!("term")),
                        error: None,
                    },
                )
                .await
        );

        // term-1's receiver resolved; main is still pending.
        assert_eq!(
            rx_term.await.expect("term receiver resolves").result,
            Some(serde_json::json!("term"))
        );
        assert_eq!(store.pending_len().await, 1);

        assert!(
            store
                .deliver(
                    "main",
                    "shared-id",
                    EvaluateResponse {
                        ok: true,
                        result: Some(serde_json::json!("main")),
                        error: None,
                    },
                )
                .await
        );
        assert_eq!(
            rx_main.await.expect("main receiver resolves").result,
            Some(serde_json::json!("main"))
        );
        assert_eq!(store.pending_len().await, 0);
    }

    #[tokio::test]
    async fn cancel_removes_entry_so_late_deliver_is_noop() {
        let store = EvaluateRequestStore::new();
        let (tx, _rx) = oneshot::channel();
        store.register(MAIN, "req-2", tx).await;
        assert_eq!(store.pending_len().await, 1);

        store.cancel(MAIN, "req-2").await;
        assert_eq!(store.pending_len().await, 0);

        let delivered = store
            .deliver(
                MAIN,
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
        store.register(MAIN, "req-3", tx).await;

        let response = EvaluateResponse {
            ok: false,
            result: None,
            error: Some("SyntaxError: Unexpected token".to_string()),
        };
        assert!(store.deliver(MAIN, "req-3", response.clone()).await);
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
        store.register(MAIN, "req-a", tx_a).await;
        store.register(MAIN, "req-b", tx_b).await;
        assert_eq!(store.pending_len().await, 2);

        // Deliver B first, then A — exercises that arrival order doesn't
        // mix up which oneshot gets which payload.
        let store_b = store.clone();
        let deliver_b = tokio::spawn(async move {
            store_b
                .deliver(
                    MAIN,
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
                    MAIN,
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
