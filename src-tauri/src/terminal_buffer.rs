//! Terminal buffer readback request correlation — analog of
//! [`crate::ui_bridge_evaluate`] for the
//! `GET /ui-bridge/sdk/terminal/sessions/:session_id/buffer` route.
//!
//! xterm.js Terminal instances live in the React frontend. The buffer text
//! isn't accessible from Rust directly, so the HTTP handler asks the frontend
//! for it via Tauri events:
//!
//! 1. HTTP handler generates a fresh `request_id` (uuid v4).
//! 2. Creates a `tokio::sync::oneshot` channel; stashes the sender in the
//!    [`TerminalBufferRequestStore`] keyed by `request_id`.
//! 3. Emits Tauri event `ui-bridge:terminal-buffer-request` with
//!    `{ request_id, session_id, max_lines }` to the React frontend.
//! 4. The frontend looks up the xterm backend by session_id, serializes the
//!    buffer, and emits `ui-bridge:terminal-buffer-response` with
//!    `{ request_id, ok, lines?, total_lines?, error? }`.
//! 5. A global Tauri listener installed at `mcp_api.rs` startup parses the
//!    response and calls [`TerminalBufferRequestStore::deliver`] which fires
//!    the matching oneshot.
//! 6. The HTTP handler awaits the receiver with a 5-second timeout, returning
//!    the result (or 504 on timeout, 404 if the frontend says session_not_found).

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio::sync::Mutex;

/// Strongly-typed response from the React frontend for a terminal buffer
/// request.
///
/// `ok=true` means the session was found and the buffer was serialized;
/// `lines` holds the rendered text lines (already truncated to the requested
/// window if `max_lines` was provided); `total_lines` is the full buffer
/// length the frontend observed.
///
/// `ok=false` means the session wasn't found in the frontend registry, or
/// some other frontend-side error occurred. `error` carries a short
/// machine-readable code (e.g. "session_not_found").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalBufferResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_lines: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Keyed store of pending oneshot senders — one per in-flight buffer read.
pub struct TerminalBufferRequestStore {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<TerminalBufferResponse>>>>,
}

impl Default for TerminalBufferRequestStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalBufferRequestStore {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a new pending buffer read.
    pub async fn register(
        &self,
        request_id: String,
        sender: oneshot::Sender<TerminalBufferResponse>,
    ) {
        let mut guard = self.pending.lock().await;
        guard.insert(request_id, sender);
    }

    /// Deliver a response to a pending buffer read by id.
    ///
    /// Returns `true` if a pending entry existed for this id.
    pub async fn deliver(&self, request_id: &str, response: TerminalBufferResponse) -> bool {
        let mut guard = self.pending.lock().await;
        if let Some(sender) = guard.remove(request_id) {
            let _ = sender.send(response);
            true
        } else {
            false
        }
    }

    /// Cancel a pending buffer read — removes the entry without delivering.
    pub async fn cancel(&self, request_id: &str) {
        let mut guard = self.pending.lock().await;
        guard.remove(request_id);
    }

    /// Snapshot of the pending entry count — useful for tests.
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
        let store = TerminalBufferRequestStore::new();
        let (tx, rx) = oneshot::channel();
        store.register("req-1".to_string(), tx).await;

        let response = TerminalBufferResponse {
            ok: true,
            lines: Some(vec!["hello".to_string(), "world".to_string()]),
            total_lines: Some(2),
            error: None,
        };
        assert!(store.deliver("req-1", response.clone()).await);

        let received = rx.await.expect("receiver should observe sent value");
        assert!(received.ok);
        assert_eq!(received.total_lines, Some(2));
        assert_eq!(received.lines.as_ref().map(|v| v.len()), Some(2));
    }

    #[tokio::test]
    async fn cancel_removes_entry() {
        let store = TerminalBufferRequestStore::new();
        let (tx, _rx) = oneshot::channel();
        store.register("req-2".to_string(), tx).await;
        assert_eq!(store.pending_len().await, 1);
        store.cancel("req-2").await;
        assert_eq!(store.pending_len().await, 0);
    }

    #[tokio::test]
    async fn deliver_unknown_id_is_noop() {
        let store = TerminalBufferRequestStore::new();
        let delivered = store
            .deliver(
                "nope",
                TerminalBufferResponse {
                    ok: false,
                    lines: None,
                    total_lines: None,
                    error: Some("session_not_found".to_string()),
                },
            )
            .await;
        assert!(!delivered);
    }

    #[tokio::test]
    async fn error_response_round_trip() {
        let store = TerminalBufferRequestStore::new();
        let (tx, rx) = oneshot::channel();
        store.register("req-3".to_string(), tx).await;

        let response = TerminalBufferResponse {
            ok: false,
            lines: None,
            total_lines: None,
            error: Some("session_not_found".to_string()),
        };
        assert!(store.deliver("req-3", response.clone()).await);
        let received = rx.await.expect("receiver should observe error value");
        assert!(!received.ok);
        assert_eq!(received.error.as_deref(), Some("session_not_found"));
    }
}
