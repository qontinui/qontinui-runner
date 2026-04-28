//! CommandRelay — runner-side command relay for WebSocket-transport apps.
//!
//! Ported from `ui-bridge/packages/ui-bridge/src/server/command-relay.ts`
//! (reference source). The TS implementation also handles SSE, multi-tab
//! primary-tab routing, heartbeats and grace periods; for Phase 1 of the
//! wrapper framework we only port the essentials:
//!
//! 1. A `pending` map keyed by `command_id`, each slot holding the routing
//!    `conn_id` plus a `oneshot::Sender<CommandResponse>` that the response
//!    handler completes. The `conn_id` lets cleanup paths target a specific
//!    socket — see `reject_by_conn`.
//! 2. A `dispatch(app_id, action, payload)` call that
//!    - generates a UUID `command_id`,
//!    - looks up the app's WebSocket `conn_id`,
//!    - registers the oneshot in the pending map,
//!    - sends `{type:"command",commandId,action,payload,timestamp}` on the
//!      WS outbound channel,
//!    - awaits the oneshot with a 30s timeout, cleaning up on timeout.
//! 3. A `resolve`/`reject_by_conn` API that the WS receive loop calls when a
//!    `{type:"response", commandId, success, result?, error?}` frame arrives
//!    or when a connection closes / is displaced. Displacement-time rejection
//!    is targeted at the displaced `conn_id` so a sibling tab's pending
//!    commands are not collateral damage.
//! 4. Change-event forwarding: `push_change_event(app_id, event)` feeds
//!    subscribers; for v1 there are no subscribers (caller logs and discards).
//!
//! Multi-tab semantics today: **last-tab-wins routing with graceful
//! displacement.** When a new tab registers with the same `app_id`, the old
//! conn loses its slot in `WsConnectionManager::by_app` and any in-flight
//! commands routed to it fail synchronously with `CommandRelayError::Displaced`
//! (instead of the previous 30s silent timeout). Primary-tab election and a
//! configurable grace period before displacement remain deferred — there is
//! still no protocol for the old tab to either negotiate or block the
//! handoff. Add those when a multi-tab wrapper is actually in production.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

use super::ws_relay::{WsConnectionManager, WsOutboundError};

/// Default per-command timeout — long enough to cover a slow snapshot, short
/// enough that wedged wrappers don't accumulate forever.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Sentinel prefix on the `error` field of a `CommandResponse` that
/// `reject_by_conn` synthesizes. The dispatch await branch recognizes this
/// prefix and maps the synthesized response onto `CommandRelayError::Displaced`
/// (instead of the generic `WrapperError`) so callers can distinguish
/// displacement from a real wrapper-side failure.
const DISPLACED_ERROR_PREFIX: &str = "__qbridge_displaced__:";

/// Result payload returned from a successful `dispatch` call.
///
/// Mirrors the shape of the `{type:"response", success, result, error}` frame
/// the wrapper sends back after executing a command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResponse {
    pub command_id: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Errors surfaced by `CommandRelay::dispatch`.
#[derive(Debug, thiserror::Error)]
pub enum CommandRelayError {
    /// No entry in the WebSocket connection manager for the given app_id.
    #[error("app '{0}' is not connected over WebSocket")]
    NotConnected(String),

    /// The wrapper returned `{success: false, error: "..."}`.
    #[error("wrapper returned error: {0}")]
    WrapperError(String),

    /// The wrapper did not respond within `DEFAULT_COMMAND_TIMEOUT`.
    #[error("command timed out after {}ms waiting for wrapper response", .0.as_millis())]
    Timeout(Duration),

    /// The wrapper's WebSocket outbound channel is closed (socket dropped,
    /// mpsc receiver gone, etc.). Treated as "not connected" by callers.
    #[error("wrapper WebSocket disconnected before command was delivered")]
    Disconnected,

    /// A new tab registered with the same `app_id` and took over the routing
    /// slot, so the wrapper this command was sent to is no longer the active
    /// target. Surfaced synchronously on register-time displacement so the
    /// caller doesn't have to wait for the 30s default timeout to discover
    /// the in-flight command will never resolve.
    #[error("command displaced by new tab connection: {0}")]
    Displaced(String),

    /// JSON serialization failure — should only happen if `payload` is
    /// non-serializable (it's a `serde_json::Value`, so this is rare but
    /// kept as a distinct variant for diagnostics).
    #[error("failed to encode command frame: {0}")]
    EncodeFailed(#[from] serde_json::Error),
}

impl From<WsOutboundError> for CommandRelayError {
    fn from(err: WsOutboundError) -> Self {
        match err {
            WsOutboundError::NotConnected => CommandRelayError::Disconnected,
            WsOutboundError::Send => CommandRelayError::Disconnected,
        }
    }
}

/// Shape of the "command" frame the runner sends to a wrapper.
/// Kept in sync with `command-relay.ts::sendCommandViaWebSocket`.
#[derive(Debug, Serialize)]
struct CommandFrame<'a> {
    r#type: &'a str,
    #[serde(rename = "commandId")]
    command_id: &'a str,
    action: &'a str,
    payload: &'a serde_json::Value,
    timestamp: i64,
}

/// Per-pending-command bookkeeping.
///
/// Keying on `command_id` alone wasn't enough once we wanted graceful
/// register-time displacement — the cleanup paths needed to know which
/// connection a pending command was routed to so they could reject only
/// the ones tied to the disappearing conn, not every other app's pending
/// commands too.
struct PendingEntry {
    conn_id: u64,
    sender: oneshot::Sender<CommandResponse>,
}

/// CommandRelay — the pending-command state machine.
pub struct CommandRelay {
    pending: Mutex<HashMap<String, PendingEntry>>,
    ws: Arc<WsConnectionManager>,
    timeout: Duration,
}

impl CommandRelay {
    pub fn new(ws: Arc<WsConnectionManager>) -> Arc<Self> {
        Self::with_timeout(ws, DEFAULT_COMMAND_TIMEOUT)
    }

    pub fn with_timeout(ws: Arc<WsConnectionManager>, timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(HashMap::new()),
            ws,
            timeout,
        })
    }

    /// Dispatch a command to the wrapper identified by `app_id`. Resolves
    /// with the wrapper's response or returns a `CommandRelayError` on
    /// disconnect/timeout/error.
    pub async fn dispatch(
        &self,
        app_id: &str,
        action: &str,
        payload: serde_json::Value,
    ) -> Result<CommandResponse, CommandRelayError> {
        // Look up the wrapper's WS conn_id. Fail fast if the app is not
        // currently connected — the caller (usually a control-route handler)
        // can surface that as an HTTP error.
        let conn_id = self
            .ws
            .conn_for_app(app_id)
            .await
            .ok_or_else(|| CommandRelayError::NotConnected(app_id.to_string()))?;

        let command_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        // Register the pending command BEFORE sending the frame — otherwise a
        // ludicrously fast response could race the insertion.
        {
            let mut pending = self.pending.lock().await;
            pending.insert(
                command_id.clone(),
                PendingEntry {
                    conn_id,
                    sender: tx,
                },
            );
        }

        // Send the outbound frame. If this fails, drop the pending entry so
        // it doesn't leak, and surface the error.
        let frame = CommandFrame {
            r#type: "command",
            command_id: &command_id,
            action,
            payload: &payload,
            timestamp: chrono::Utc::now().timestamp_millis(),
        };
        let frame_json = serde_json::to_string(&frame)?;

        if let Err(send_err) = self.ws.send_text(conn_id, frame_json).await {
            self.pending.lock().await.remove(&command_id);
            return Err(send_err.into());
        }

        // Await the oneshot with a timeout. On either branch we remove the
        // entry from the pending map so it cannot be double-resolved or leak.
        let outcome = tokio::time::timeout(self.timeout, rx).await;
        match outcome {
            Ok(Ok(response)) => {
                if response.success {
                    Ok(response)
                } else {
                    let msg = response.error.clone().unwrap_or_else(|| {
                        "wrapper returned success=false without an error message".to_string()
                    });
                    if let Some(reason) = msg.strip_prefix(DISPLACED_ERROR_PREFIX) {
                        Err(CommandRelayError::Displaced(reason.to_string()))
                    } else {
                        Err(CommandRelayError::WrapperError(msg))
                    }
                }
            }
            Ok(Err(_recv_err)) => {
                // Sender dropped without sending — WS closed while the command
                // was in flight. Cleanup already happened in on_disconnect.
                Err(CommandRelayError::Disconnected)
            }
            Err(_elapsed) => {
                self.pending.lock().await.remove(&command_id);
                Err(CommandRelayError::Timeout(self.timeout))
            }
        }
    }

    /// Called by the WS receive loop when a `{type:"response"}` frame arrives.
    /// Returns `true` if a pending command matched the `command_id`.
    pub async fn resolve(&self, response: CommandResponse) -> bool {
        let entry = {
            let mut pending = self.pending.lock().await;
            pending.remove(&response.command_id)
        };
        match entry {
            Some(entry) => entry.sender.send(response).is_ok(),
            None => false,
        }
    }

    /// Reject every pending command tied to `conn_id`, resolving each
    /// dispatch caller's `rx.await` with a `Displaced(reason)` response so
    /// they fail fast instead of waiting for the 30s timeout.
    ///
    /// Used in two places:
    /// - `WsConnectionManager::register` displacement path, where a new tab
    ///   for the same `app_id` takes the routing slot from an older one.
    /// - The disconnect cleanup path in `ws_relay::drive_connection`, where
    ///   a wrapper's socket closes (with `reason = "wrapper disconnected"`).
    ///
    /// Returns the number of pending commands that were rejected, mostly for
    /// diagnostics and tests.
    pub async fn reject_by_conn(&self, conn_id: u64, reason: &str) -> usize {
        // Drain matching entries under the lock, then resolve their oneshots
        // outside the lock — `tx.send` only consumes the sender (no .await),
        // but doing it inside the critical section adds work other dispatch
        // callers would block on.
        let drained: Vec<(String, PendingEntry)> = {
            let mut pending = self.pending.lock().await;
            let keys: Vec<String> = pending
                .iter()
                .filter(|(_, e)| e.conn_id == conn_id)
                .map(|(k, _)| k.clone())
                .collect();
            keys.into_iter()
                .filter_map(|k| pending.remove(&k).map(|v| (k, v)))
                .collect()
        };
        let count = drained.len();
        for (command_id, entry) in drained {
            // Surface as a normal CommandResponse with success=false; the
            // dispatch await branch maps this to CommandRelayError::Displaced
            // via the new `displaced_reason` field encoded in `error`. We use
            // a stable sentinel prefix the dispatcher recognizes so callers
            // can distinguish displacement from generic wrapper errors.
            let _ = entry.sender.send(CommandResponse {
                command_id,
                success: false,
                result: None,
                error: Some(format!("{}{}", DISPLACED_ERROR_PREFIX, reason)),
            });
        }
        count
    }

    /// Number of commands awaiting a response. For diagnostics / tests.
    pub async fn pending_count(&self) -> usize {
        self.pending.lock().await.len()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Drive a fully in-memory round-trip: dispatch → the WS relay's outbound
    /// channel receives the frame → the test acts as the wrapper and calls
    /// `resolve` → `dispatch` returns Ok.
    #[tokio::test]
    async fn dispatch_round_trip() {
        let ws = WsConnectionManager::new();
        // Simulate a registered WS client.
        let (conn_id, mut outbound_rx) = ws.test_register("app-1").await;
        let relay = CommandRelay::with_timeout(ws.clone(), Duration::from_secs(2));

        // Kick off the dispatch on a background task so we can drive the
        // "wrapper" side of the conversation.
        let relay_task = relay.clone();
        let dispatch_handle = tokio::spawn(async move {
            relay_task
                .dispatch("app-1", "getControlSnapshot", json!({}))
                .await
        });

        // Pull the command frame off the wrapper's inbound queue.
        let frame_json = outbound_rx.recv().await.expect("outbound frame");
        let frame: serde_json::Value = serde_json::from_str(&frame_json).unwrap();
        assert_eq!(frame["type"], "command");
        assert_eq!(frame["action"], "getControlSnapshot");
        let command_id = frame["commandId"].as_str().unwrap().to_string();

        // Simulate the wrapper's success response.
        let accepted = relay
            .resolve(CommandResponse {
                command_id: command_id.clone(),
                success: true,
                result: Some(json!({"elements": []})),
                error: None,
            })
            .await;
        assert!(accepted, "resolve must match a pending command");

        let response = dispatch_handle.await.unwrap().expect("dispatch ok");
        assert!(response.success);
        assert_eq!(response.result.unwrap()["elements"], json!([]));
        assert_eq!(conn_id, 1); // first registration in a fresh manager
    }

    #[tokio::test]
    async fn dispatch_returns_wrapper_error() {
        let ws = WsConnectionManager::new();
        let (_conn_id, mut outbound_rx) = ws.test_register("app-2").await;
        let relay = CommandRelay::with_timeout(ws.clone(), Duration::from_secs(2));

        let relay_task = relay.clone();
        let dispatch_handle =
            tokio::spawn(async move { relay_task.dispatch("app-2", "click", json!({})).await });

        let frame_json = outbound_rx.recv().await.expect("frame");
        let command_id = serde_json::from_str::<serde_json::Value>(&frame_json).unwrap()
            ["commandId"]
            .as_str()
            .unwrap()
            .to_string();

        relay
            .resolve(CommandResponse {
                command_id,
                success: false,
                result: None,
                error: Some("element not found".into()),
            })
            .await;

        let err = dispatch_handle.await.unwrap().unwrap_err();
        match err {
            CommandRelayError::WrapperError(msg) => assert!(msg.contains("element not found")),
            other => panic!("expected WrapperError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn dispatch_not_connected_when_app_missing() {
        let ws = WsConnectionManager::new();
        let relay = CommandRelay::new(ws);

        let err = relay
            .dispatch("ghost", "noop", json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, CommandRelayError::NotConnected(ref id) if id == "ghost"));
    }

    #[tokio::test]
    async fn dispatch_times_out_without_response() {
        let ws = WsConnectionManager::new();
        let (_conn_id, _outbound_rx) = ws.test_register("slow").await;
        // Drop the outbound receiver's consumption side? Actually keep it
        // alive but never reply — dispatch should hit the timeout.
        let relay = CommandRelay::with_timeout(ws, Duration::from_millis(50));

        let err = relay.dispatch("slow", "noop", json!({})).await.unwrap_err();
        assert!(matches!(err, CommandRelayError::Timeout(_)));
        assert_eq!(
            relay.pending_count().await,
            0,
            "pending entry must be cleaned up"
        );
    }

    #[tokio::test]
    async fn dispatch_errors_when_outbound_send_fails() {
        let ws = WsConnectionManager::new();
        let (_conn_id, outbound_rx) = ws.test_register("gone").await;
        // Drop the outbound receiver — the mpsc send will fail.
        drop(outbound_rx);

        let relay = CommandRelay::with_timeout(ws, Duration::from_secs(1));
        let err = relay.dispatch("gone", "noop", json!({})).await.unwrap_err();
        assert!(matches!(err, CommandRelayError::Disconnected));
        assert_eq!(relay.pending_count().await, 0);
    }

    /// reject_by_conn finishes any in-flight dispatch keyed to that conn_id
    /// with `CommandRelayError::Displaced`, leaves dispatches for other
    /// conns alone, and returns the count it actually rejected.
    #[tokio::test]
    async fn reject_by_conn_translates_into_displaced_error() {
        let ws = WsConnectionManager::new();
        let (conn_a, mut outbound_a) = ws.test_register("app-a").await;
        let (conn_b, mut outbound_b) = ws.test_register("app-b").await;
        // Long timeout — we want this to resolve via reject_by_conn, not the
        // per-command timer.
        let relay = CommandRelay::with_timeout(ws.clone(), Duration::from_secs(60));

        let relay_a = relay.clone();
        let dispatch_a =
            tokio::spawn(async move { relay_a.dispatch("app-a", "snapshot", json!({})).await });
        let relay_b = relay.clone();
        let dispatch_b =
            tokio::spawn(async move { relay_b.dispatch("app-b", "snapshot", json!({})).await });

        // Drain the outbound frames so the dispatch send paths don't stall.
        let _ = outbound_a.recv().await.expect("frame for app-a");
        let _ = outbound_b.recv().await.expect("frame for app-b");

        // Reject only conn_a's pending command. app-b's dispatch must keep
        // waiting (we resolve it explicitly below to clean up the test).
        let rejected = relay.reject_by_conn(conn_a, "test displacement").await;
        assert_eq!(rejected, 1);

        let result_a = dispatch_a.await.unwrap();
        match result_a {
            Err(CommandRelayError::Displaced(reason)) => {
                assert!(
                    reason.contains("test displacement"),
                    "got reason: {}",
                    reason
                );
            }
            other => panic!("expected Displaced for app-a, got {:?}", other),
        }

        // app-b's dispatch is still pending. Resolve it normally.
        let frame_b_id_extracted = {
            // We already consumed the frame via outbound_b.recv() above, so
            // peek the pending map for conn_b's command_id.
            let pending = relay.pending.lock().await;
            pending
                .iter()
                .find_map(|(k, v)| {
                    if v.conn_id == conn_b {
                        Some(k.clone())
                    } else {
                        None
                    }
                })
                .expect("app-b should still have a pending command")
        };
        relay
            .resolve(CommandResponse {
                command_id: frame_b_id_extracted,
                success: true,
                result: Some(json!({"ok": true})),
                error: None,
            })
            .await;
        let result_b = dispatch_b.await.unwrap();
        assert!(result_b.is_ok(), "app-b dispatch must succeed");
    }

    /// reject_by_conn is the cleanup primitive for the disconnect path too.
    /// The failure mode is Displaced rather than the previous bulk-clear-
    /// triggered Disconnected, but the caller behavior — fail-fast instead
    /// of waiting for the 30s timeout — is the same.
    #[tokio::test]
    async fn reject_by_conn_with_no_matches_returns_zero() {
        let ws = WsConnectionManager::new();
        let relay = CommandRelay::new(ws);
        // No registrations, no pending — rejecting any conn id is a no-op.
        let rejected = relay.reject_by_conn(999, "nothing here").await;
        assert_eq!(rejected, 0);
    }
}
