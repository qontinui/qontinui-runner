//! App dispatch — single entry point for runner → registered-app requests.
//!
//! Handlers that proxy to a registered UI Bridge app (identified by `app_id`)
//! should call `dispatch_to_app` instead of reqwest-ing the app's `base_url`
//! directly. This lets HTTP apps keep their existing same-origin path while
//! WebSocket-transport wrappers (Phase 1) receive commands over their
//! `/ui-bridge/ws` socket.
//!
//! The helper:
//! - Looks up the app in `AppRegistry`.
//! - For `AppTransport::Websocket`: calls `CommandRelay::dispatch`.
//! - For `AppTransport::Http`: POSTs a JSON body to
//!   `{base_url}/ui-bridge{path}` via the shared reqwest client.
//!
//! Both branches return a normalized `serde_json::Value`. On error we surface
//! a message suitable for embedding into an `{success:false, error}` response.

use std::sync::Arc;
use std::time::Duration;

use reqwest::Method;
use tracing::debug;

use super::app_registry::{AppRegistry, AppTransport};
use super::command_relay::{CommandRelay, CommandRelayError};

/// Default timeout for HTTP dispatches to registered apps. WebSocket
/// dispatches use `CommandRelay`'s own 30s timeout.
const HTTP_DISPATCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Errors returned by `dispatch_to_app`.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("app '{0}' is not registered")]
    NotRegistered(String),

    #[error("websocket dispatch failed: {0}")]
    WebSocket(#[from] CommandRelayError),

    #[error("http dispatch to '{url}' failed: {source}")]
    HttpSend {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("http dispatch to '{url}' returned status {status}: {body}")]
    HttpStatus {
        url: String,
        status: u16,
        body: String,
    },

    #[error("app returned non-JSON response: {0}")]
    InvalidJson(String),
}

impl DispatchError {
    /// Compact human-readable form suitable for `{error: "..."}` responses.
    pub fn to_user_message(&self) -> String {
        self.to_string()
    }
}

/// A reusable wrapper bundling the registry + relay + http client so handlers
/// can inject one `Arc<AppDispatcher>` via `ApiState`.
pub struct AppDispatcher {
    registry: Arc<AppRegistry>,
    command_relay: Arc<CommandRelay>,
    http: reqwest::Client,
}

impl AppDispatcher {
    pub fn new(registry: Arc<AppRegistry>, command_relay: Arc<CommandRelay>) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_DISPATCH_TIMEOUT)
            .pool_max_idle_per_host(4)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Arc::new(Self {
            registry,
            command_relay,
            http,
        })
    }

    /// Dispatch a command to a registered app.
    ///
    /// - `app_id`: the registry key.
    /// - `action`: used only by WebSocket dispatch as the command action name
    ///   (e.g. `"executeComponentAction"`, `"getControlSnapshot"`).
    /// - `http_method` / `http_path`: used only by HTTP dispatch. Path is
    ///   joined against the app's `base_url` verbatim (caller supplies any
    ///   `/ui-bridge/...` prefix the app expects).
    /// - `payload`: the JSON body. For WebSocket, it becomes the frame's
    ///   `payload` field; for HTTP, it's the request body.
    pub async fn dispatch(
        &self,
        app_id: &str,
        action: &str,
        http_method: Method,
        http_path: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, DispatchError> {
        let entry = self
            .registry
            .get(app_id)
            .await
            .ok_or_else(|| DispatchError::NotRegistered(app_id.to_string()))?;

        match entry.transport {
            AppTransport::Websocket => {
                debug!(
                    "[app-dispatch] {} via websocket → app='{}' conn_id={:?}",
                    action, app_id, entry.websocket_conn_id
                );
                let response = self
                    .command_relay
                    .dispatch(app_id, action, payload)
                    .await?;
                Ok(response.result.unwrap_or(serde_json::Value::Null))
            }
            AppTransport::Http => {
                let base = entry.app.url.trim_end_matches('/');
                let base_path = entry.app.base_path.trim_end_matches('/');
                let path = if http_path.starts_with('/') {
                    http_path.to_string()
                } else {
                    format!("/{}", http_path)
                };
                let url = format!("{}{}{}", base, base_path, path);
                debug!(
                    "[app-dispatch] {} via http → {} (app_id={})",
                    http_method, url, app_id
                );
                let mut req = self.http.request(http_method, &url);
                if !payload.is_null() {
                    req = req.json(&payload);
                }
                let resp = req
                    .send()
                    .await
                    .map_err(|e| DispatchError::HttpSend {
                        url: url.clone(),
                        source: e,
                    })?;
                let status = resp.status();
                let body = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "<unreadable body>".to_string());
                if !status.is_success() {
                    return Err(DispatchError::HttpStatus {
                        url,
                        status: status.as_u16(),
                        body,
                    });
                }
                serde_json::from_str(&body).map_err(|_| DispatchError::InvalidJson(body))
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::app_discovery::DiscoveredApp;
    use crate::mcp::ws_relay::WsConnectionManager;
    use serde_json::json;

    fn sample_app(app_id: &str, base_url: &str) -> DiscoveredApp {
        DiscoveredApp {
            app_id: app_id.to_string(),
            app_name: "t".into(),
            app_type: "web".into(),
            framework: None,
            url: base_url.to_string(),
            port: 0,
            base_path: "".into(),
            version: None,
            capabilities: vec![],
            element_count: None,
            component_count: None,
            discovered_at: 0,
        }
    }

    #[tokio::test]
    async fn unregistered_app_returns_not_registered() {
        let registry = AppRegistry::new();
        let ws = WsConnectionManager::new();
        let relay = CommandRelay::new(ws);
        let dispatcher = AppDispatcher::new(registry, relay);

        let err = dispatcher
            .dispatch("nope", "noop", Method::POST, "/x", json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::NotRegistered(ref id) if id == "nope"));
    }

    #[tokio::test]
    async fn websocket_path_round_trips() {
        let registry = AppRegistry::new();
        let ws = WsConnectionManager::new();
        let (_conn_id, mut outbound_rx) = ws.test_register("wapp").await;
        registry
            .upsert(
                sample_app("wapp", "http://unused"),
                None,
                AppTransport::Websocket,
                Some(1),
            )
            .await;
        let relay = CommandRelay::with_timeout(ws.clone(), Duration::from_secs(2));
        let dispatcher = AppDispatcher::new(registry, relay.clone());

        let dispatcher2 = dispatcher.clone();
        let handle = tokio::spawn(async move {
            dispatcher2
                .dispatch("wapp", "snap", Method::GET, "/ignored", json!({"q": 1}))
                .await
        });

        let frame = outbound_rx.recv().await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        let command_id = v["commandId"].as_str().unwrap().to_string();
        assert_eq!(v["action"], "snap");
        assert_eq!(v["payload"]["q"], 1);

        relay
            .resolve(super::super::command_relay::CommandResponse {
                command_id,
                success: true,
                result: Some(json!({"ok": true})),
                error: None,
            })
            .await;

        let out = handle.await.unwrap().unwrap();
        assert_eq!(out, json!({"ok": true}));
    }
}
