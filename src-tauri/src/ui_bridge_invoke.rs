//! UI Bridge invoke proxy — Phase 3I.1 + 3I.2.
//!
//! Allows external HTTP callers to invoke a curated allowlist of Tauri
//! commands over the UI Bridge HTTP surface, without having to go through
//! `page/evaluate + __TAURI_INTERNALS__` gymnastics.
//!
//! # Flow
//!
//! 1. HTTP handler generates a fresh `request_id` (uuid v4).
//! 2. Creates a `tokio::sync::oneshot` channel; stashes the sender in the
//!    [`InvokeRequestStore`] keyed by `request_id`.
//! 3. Emits Tauri event `ui-bridge:invoke-request` with
//!    `{ request_id, command, args }` to the React frontend.
//! 4. The React side calls `invoke(command, args)` and emits
//!    `ui-bridge:invoke-response` with `{ request_id, ok, result, error }`.
//! 5. A global Tauri listener installed at `mcp_api.rs` startup parses the
//!    response and calls [`InvokeRequestStore::deliver`] which fires the
//!    matching oneshot.
//! 6. The HTTP handler awaits the receiver with a configurable timeout,
//!    returning the result (or 504 on timeout, 500 on frontend error).
//!
//! # Allowlist
//!
//! Only commands in [`UI_BRIDGE_COMMANDS`] may be invoked. The HTTP handler
//! returns 400 for anything else. This avoids exposing arbitrary Tauri
//! commands to the HTTP surface — some commands accept filesystem paths,
//! credentials, or PTY-level process control and must not be reachable
//! from external clients without explicit curation.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio::sync::Mutex;

/// Strongly-typed response from the React frontend for an invoke request.
///
/// `ok=true` means the Tauri command completed (`invoke(...)` resolved);
/// `result` holds its return value (possibly `Null` for `()` returns).
/// `ok=false` means `invoke(...)` threw — `error` carries the string the
/// command returned (e.g. `Err(String)` from the Rust side).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokeResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Keyed store of pending oneshot senders — one per in-flight invoke.
///
/// Multiple invokes can be in-flight at once, unlike `TokenFlowStore`
/// which is single-slot. The HashMap is keyed by `request_id` (a uuid v4
/// string) which we generate fresh for every call.
pub struct InvokeRequestStore {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<InvokeResponse>>>>,
}

impl Default for InvokeRequestStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InvokeRequestStore {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a new pending invoke.
    ///
    /// Takes ownership of the oneshot sender and stashes it under
    /// `request_id`. If an entry for that id already exists (shouldn't
    /// happen with uuid v4, but defensive), it is silently replaced —
    /// the prior waiter will observe a dropped-sender error via
    /// `Receiver::await`.
    pub async fn register(&self, request_id: String, sender: oneshot::Sender<InvokeResponse>) {
        let mut guard = self.pending.lock().await;
        guard.insert(request_id, sender);
    }

    /// Deliver a response to a pending invoke by id.
    ///
    /// Removes the entry from the map and sends the response through the
    /// oneshot. If the receiver has already been dropped (e.g. the HTTP
    /// handler timed out and bailed), the response is silently discarded —
    /// this mirrors the "best effort" semantics of oneshot channels.
    /// Returns `true` if a pending entry existed for this id.
    pub async fn deliver(&self, request_id: &str, response: InvokeResponse) -> bool {
        let mut guard = self.pending.lock().await;
        if let Some(sender) = guard.remove(request_id) {
            // Ignore send errors — if the receiver is gone we can't do
            // anything about it, and the caller already logged the timeout.
            let _ = sender.send(response);
            true
        } else {
            false
        }
    }

    /// Cancel a pending invoke — removes the entry without delivering.
    ///
    /// Called by the HTTP handler on timeout so a subsequent late
    /// response doesn't linger in the map. The oneshot sender is dropped,
    /// which closes the channel — any residual receivers would observe a
    /// `RecvError` (but by the time cancel runs, the HTTP handler's
    /// receiver is already discarded).
    pub async fn cancel(&self, request_id: &str) {
        let mut guard = self.pending.lock().await;
        guard.remove(request_id);
    }
}

/// Metadata for one command in the UI Bridge invoke allowlist.
///
/// `args_schema` and `response_schema` are JSON string literals describing
/// the wire shape callers should expect — they're surfaced verbatim via
/// `GET /ui-bridge/commands` so agents can discover the contract without
/// reading Rust source.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ProxyableCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub args_schema: &'static str,
    pub response_schema: &'static str,
}

/// The static allowlist of Tauri commands reachable via
/// `POST /ui-bridge/invoke/{command_name}`.
///
/// Schemas reflect the HTTP caller's wire contract (camelCase top-level
/// arg names), which Tauri's IPC converts to the Rust command's
/// snake_case parameter names. See
/// `src-tauri/src/commands/web_integration.rs` for the authoritative Rust
/// signatures. Adding a command here makes it callable over HTTP; do not
/// add commands that accept arbitrary filesystem paths or PTY handles
/// without a dedicated threat review.
pub const UI_BRIDGE_COMMANDS: &[ProxyableCommand] = &[
    ProxyableCommand {
        name: "get_web_integration_status",
        description: "Return the persisted web-integration settings plus live registration state (runner id, last heartbeat, last registration error).",
        args_schema: "{}",
        response_schema: "{ \"enabled\": boolean, \"backendUrl\": string, \"runnerTokenMasked\": string, \"runnerId\": string | null, \"lastHeartbeatAt\": string | null, \"registrationError\": string | null }",
    },
    ProxyableCommand {
        name: "save_web_integration_settings",
        description: "Persist web-integration settings (enable flag, backend URL, runner token, optional web base URL) and trigger re-registration with the configured backend. `webBaseUrl` is optional and only needed when the Next.js web UI runs on a different host than the API backend.",
        args_schema: "{ \"enabled\": boolean, \"backendUrl\": string, \"runnerToken\": string, \"webBaseUrl\"?: string | null }",
        response_schema: "null",
    },
    ProxyableCommand {
        name: "test_web_integration_connection",
        description: "Probe the given backend URL + runner token by making a throwaway runner-registration call and immediately deleting the created entry. Returns the transient runner id (for debugging only; do not reuse it).",
        args_schema: "{ \"backendUrl\": string, \"runnerToken\": string }",
        response_schema: "{ \"runner_id\": string }",
    },
    ProxyableCommand {
        name: "start_web_token_flow",
        description: "Open the user's browser at `{backend_url}/connect-runner?state=...&callback=...&runner_name=...`, stashing state for the eventual callback that applies the issued runner token. `backendUrl` is optional — when omitted, uses the persisted backend URL.",
        args_schema: "{ \"backendUrl\"?: string | null }",
        response_schema: "null",
    },
];

/// Whether a command name is in the UI Bridge invoke allowlist.
///
/// Used by the HTTP handler to gate the dispatch — anything not in the
/// list should return 400 before the request_id is even allocated.
pub fn is_allowlisted(name: &str) -> bool {
    UI_BRIDGE_COMMANDS.iter().any(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_and_deliver_round_trip() {
        let store = InvokeRequestStore::new();
        let (tx, rx) = oneshot::channel();
        store.register("req-1".to_string(), tx).await;

        let response = InvokeResponse {
            ok: true,
            result: Some(serde_json::json!({ "hello": "world" })),
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
        let store = InvokeRequestStore::new();
        let delivered = store
            .deliver(
                "does-not-exist",
                InvokeResponse {
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
        let store = InvokeRequestStore::new();
        let (tx, _rx) = oneshot::channel();
        store.register("req-2".to_string(), tx).await;

        store.cancel("req-2").await;

        let delivered = store
            .deliver(
                "req-2",
                InvokeResponse {
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
        let store = InvokeRequestStore::new();
        let (tx, rx) = oneshot::channel();
        store.register("req-3".to_string(), tx).await;

        let response = InvokeResponse {
            ok: false,
            result: None,
            error: Some("frontend invoke failed: missing required key settings".to_string()),
        };
        assert!(store.deliver("req-3", response.clone()).await);
        let received = rx.await.expect("receiver should observe error value");
        assert!(!received.ok);
        assert_eq!(received.error, response.error);
    }

    #[test]
    fn is_allowlisted_recognizes_known_commands() {
        assert!(is_allowlisted("get_web_integration_status"));
        assert!(is_allowlisted("save_web_integration_settings"));
        assert!(is_allowlisted("test_web_integration_connection"));
        assert!(is_allowlisted("start_web_token_flow"));
    }

    #[test]
    fn is_allowlisted_rejects_unknown_commands() {
        assert!(!is_allowlisted("get_ui_error"));
        assert!(!is_allowlisted("rm_rf_my_disk"));
        assert!(!is_allowlisted(""));
    }

    #[test]
    fn allowlist_is_not_empty_and_schemas_are_populated() {
        assert!(!UI_BRIDGE_COMMANDS.is_empty());
        for cmd in UI_BRIDGE_COMMANDS {
            assert!(!cmd.name.is_empty(), "command name must not be empty");
            assert!(
                !cmd.description.is_empty(),
                "command {} needs a description",
                cmd.name
            );
            assert!(
                !cmd.args_schema.is_empty(),
                "command {} needs an args_schema",
                cmd.name
            );
            assert!(
                !cmd.response_schema.is_empty(),
                "command {} needs a response_schema",
                cmd.name
            );
        }
    }
}
