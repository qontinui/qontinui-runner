//! CommandRelay — runner-side command relay for WebSocket-transport apps.
//!
//! Ported from `ui-bridge/packages/ui-bridge/src/server/command-relay.ts`
//! (reference source). The TS implementation also handles SSE, multi-tab
//! primary-tab routing, heartbeats and grace periods; for Phase 1 of the
//! wrapper framework we only port the essentials:
//!
//! 1. A `pending_commands` map keyed by `command_id`, each slot holding a
//!    `oneshot::Sender<CommandResponse>` that the response handler completes.
//! 2. A `dispatch(app_id, action, payload)` call that
//!    - generates a UUID `command_id`,
//!    - looks up the app's WebSocket `conn_id`,
//!    - registers the oneshot in the pending map,
//!    - sends `{type:"command",commandId,action,payload,timestamp}` on the
//!      WS outbound channel,
//!    - awaits the oneshot with a 30s timeout, cleaning up on timeout.
//! 3. A `resolve`/`reject` API that the WS receive loop calls when a
//!    `{type:"response", commandId, success, result?, error?}` frame arrives.
//! 4. Change-event forwarding: `push_change_event(app_id, event)` feeds
//!    subscribers; for v1 there are no subscribers (caller logs and discards).
//!
//! Multi-tab grace period, primary-tab failover and demotion are explicitly
//! **deferred**. "Last tab wins" is called out in the plan's risk notes.

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

/// CommandRelay — the pending-command state machine.
pub struct CommandRelay {
    pending: Mutex<HashMap<String, oneshot::Sender<CommandResponse>>>,
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
            pending.insert(command_id.clone(), tx);
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
                    let msg = response
                        .error
                        .clone()
                        .unwrap_or_else(|| "wrapper returned success=false without an error message".to_string());
                    Err(CommandRelayError::WrapperError(msg))
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
        let sender = {
            let mut pending = self.pending.lock().await;
            pending.remove(&response.command_id)
        };
        match sender {
            Some(tx) => tx.send(response).is_ok(),
            None => false,
        }
    }

    /// Reject all pending commands for a given connection. Called when the
    /// WS relay removes a `conn_id` (close/error). Without this, `dispatch`
    /// callers would only learn about the disconnect via the 30s timeout.
    ///
    /// Because the `pending` map does not carry per-command `conn_id` (the
    /// command's wrapper is identified by `app_id` and looked up at send
    /// time), this helper simply drops all senders — which causes every
    /// outstanding `rx.await` to return `RecvError` and the caller to see
    /// `CommandRelayError::Disconnected`. This is the conservative choice:
    /// if the last-tab-wins invariant changes later we can route per-app.
    pub async fn reject_all_on_disconnect(&self) {
        let mut pending = self.pending.lock().await;
        pending.clear();
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

        let err = relay.dispatch("ghost", "noop", json!({})).await.unwrap_err();
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
        assert_eq!(relay.pending_count().await, 0, "pending entry must be cleaned up");
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
}
