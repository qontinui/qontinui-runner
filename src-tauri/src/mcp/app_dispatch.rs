//! App dispatch — single entry point for runner → registered-app requests.
//!
//! Three public entry points:
//!
//! - `AppDispatcher::dispatch(app_id, ...)` — registry-keyed primitive. Looks
//!   up the app in `AppRegistry` and routes to `CommandRelay::dispatch`
//!   (WebSocket) or to the registered `base_url` (HTTP).
//! - `AppDispatcher::dispatch_active(sdk_connection, ...)` — resolves the
//!   active SDK app from `SdkConnectionManager`. Routes to the relay for
//!   WS-registered apps, or to the active connection's `app_url`+`base_path`
//!   for HTTP. Preserves the legacy 10s responsiveness cache: when an app
//!   recently reported `responsive: false`, the call short-circuits with
//!   `DispatchError::NotResponsive` instead of waiting for an HTTP timeout.
//! - `AppDispatcher::dispatch_active_http(...)` — HTTP-only variant of
//!   `dispatch_active` for proxy handlers that target a raw URL path the
//!   wrapper SDK does not relay (`/health`, `/capabilities`, `/auto/...`).
//!
//! All three return a normalized `serde_json::Value` on success. Errors map
//! to `DispatchError` variants suitable for embedding into an
//! `{success:false, error}` response.

use std::sync::Arc;
use std::time::Duration;

use reqwest::Method;
use tokio::sync::Mutex;
use tracing::debug;

use super::app_registry::{AppRegistry, AppTransport};
use super::command_relay::{CommandRelay, CommandRelayError};
use super::sdk_client::SdkConnectionManager;

/// Default timeout for HTTP dispatches to registered apps. WebSocket
/// dispatches use `CommandRelay`'s own 30s timeout.
const HTTP_DISPATCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Errors returned by `AppDispatcher::{dispatch, dispatch_active, dispatch_active_http}`.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("app '{0}' is not registered")]
    NotRegistered(String),

    /// `dispatch_active` could not resolve an active SDK app — either no app
    /// has connected yet, or the previously active connection was closed.
    #[error("No active SDK app connection")]
    NotConnected,

    /// HTTP responsiveness cache reported the app as unresponsive (no active
    /// browser tab) within the cache TTL — the request was short-circuited
    /// to avoid 10s of HTTP retries against a dead app.
    #[error("{0}")]
    NotResponsive(String),

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

/// Cached HTTP responsiveness status per app — duration in milliseconds the
/// dispatcher will trust a previous "unresponsive" reading before re-probing.
/// Matches the legacy `sdk_request` cache window.
const RESPONSIVENESS_CACHE_MS: i64 = 10_000;

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
                let response = self.command_relay.dispatch(app_id, action, payload).await?;
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
                let resp = req.send().await.map_err(|e| DispatchError::HttpSend {
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

    /// HTTP-only variant of `dispatch_active` for proxy handlers that hit a
    /// raw URL path (e.g. `/health`, `/capabilities`, `/auto/...`) which the
    /// wrapper SDK does not relay over WebSocket. Skips the registry-based
    /// transport probe entirely and goes straight to the HTTP arm —
    /// responsiveness cache + connection-URL routing identical to
    /// `dispatch_active`'s HTTP path.
    pub async fn dispatch_active_http(
        &self,
        sdk_connection: &Arc<Mutex<SdkConnectionManager>>,
        http_method: Method,
        http_path: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, DispatchError> {
        let (app_id, app_url, base_path, http_client) = {
            let guard = sdk_connection.lock().await;
            let conn = guard
                .active_connection()
                .ok_or(DispatchError::NotConnected)?;
            (
                conn.app_info.app_id.clone(),
                conn.app_url.clone(),
                conn.base_path.clone(),
                conn.client.clone(),
            )
        };
        self.http_dispatch_inner(
            sdk_connection,
            &app_id,
            &app_url,
            &base_path,
            &http_client,
            http_method,
            http_path,
            payload,
        )
        .await
    }

    /// Internal HTTP arm shared by `dispatch_active` and `dispatch_active_http`.
    /// Implements the responsiveness cache + JSON parsing + status mapping.
    #[allow(clippy::too_many_arguments)]
    async fn http_dispatch_inner(
        &self,
        sdk_connection: &Arc<Mutex<SdkConnectionManager>>,
        app_id: &str,
        app_url: &str,
        base_path: &str,
        http_client: &reqwest::Client,
        http_method: Method,
        http_path: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, DispatchError> {
        // Cache fast-fail.
        let now_ms = chrono::Utc::now().timestamp_millis();
        let cached = {
            let guard = sdk_connection.lock().await;
            let age = now_ms - guard.active_responsive_checked_at;
            (guard.active_responsive, age)
        };
        if cached.1 < RESPONSIVENESS_CACHE_MS && cached.0 == Some(false) {
            return Err(DispatchError::NotResponsive(
                "SDK app is not responsive (no active browser tab)".to_string(),
            ));
        }
        // Refresh stale cache entries.
        if cached.1 >= RESPONSIVENESS_CACHE_MS {
            let health_url = format!("{}{}/health", app_url, base_path);
            let responsive = match http_client
                .get(&health_url)
                .timeout(Duration::from_secs(2))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => resp
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|h| h.get("data")?.get("responsive")?.as_bool())
                    .unwrap_or(true),
                _ => true,
            };
            let mut guard = sdk_connection.lock().await;
            guard.active_responsive = Some(responsive);
            guard.active_responsive_checked_at = chrono::Utc::now().timestamp_millis();
            if !responsive {
                debug!(
                    "[app-dispatch] {} reports unresponsive — short-circuiting",
                    app_url
                );
                return Err(DispatchError::NotResponsive(
                    "SDK app is not responsive (no active browser tab)".to_string(),
                ));
            }
        }

        let path = if http_path.starts_with('/') {
            http_path.to_string()
        } else {
            format!("/{}", http_path)
        };
        let url = format!("{}{}{}", app_url, base_path, path);
        debug!(
            "[app-dispatch] {} via http (active) → {} (app_id={})",
            http_method, url, app_id
        );

        let mut req = http_client.request(http_method, &url);
        if !payload.is_null() {
            req = req.json(&payload);
        }
        let resp = req.send().await.map_err(|e| DispatchError::HttpSend {
            url: url.clone(),
            source: e,
        })?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| "<unreadable body>".to_string());

        let json: serde_json::Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(_) => {
                if !status.is_success() {
                    return Err(DispatchError::HttpStatus {
                        url,
                        status: status.as_u16(),
                        body,
                    });
                }
                return Err(DispatchError::InvalidJson(body));
            }
        };

        if let Some(false) = json.get("success").and_then(|v| v.as_bool()) {
            let msg = json
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error from SDK app")
                .to_string();
            return Err(DispatchError::HttpStatus {
                url,
                status: status.as_u16(),
                body: msg,
            });
        }
        if !status.is_success() && json.get("success").is_none() {
            return Err(DispatchError::HttpStatus {
                url,
                status: status.as_u16(),
                body,
            });
        }

        Ok(json)
    }

    /// Dispatch to whichever app is currently active in `SdkConnectionManager`.
    ///
    /// Resolves the active app's `app_id` (or returns `DispatchError::NotConnected`
    /// when no connection is active), then routes to either the WebSocket
    /// command relay (if the app registered via `/ui-bridge/ws`) or to the
    /// active connection's HTTP `app_url`+`base_path`.
    ///
    /// HTTP transport additionally enforces a 10s responsiveness cache: when
    /// the app's `/health` endpoint recently reported `responsive: false`
    /// (no active browser tab), the call short-circuits with
    /// `DispatchError::NotResponsive` so handlers can fall back to IPC
    /// without waiting for a 10s HTTP retry. WebSocket-transport apps skip
    /// the cache entirely — their liveness is already tracked by the relay
    /// heartbeat in `WsConnectionManager`.
    ///
    /// The HTTP arm reuses the per-connection `reqwest::Client` from
    /// `SdkConnection` (a 30s timeout configured in `connect_sdk_app`),
    /// matching the legacy `sdk_request` behavior so connection-pinned URLs
    /// (e.g. an SDK-connected dev server distinct from a phone-home registry
    /// entry) keep working unchanged.
    pub async fn dispatch_active(
        &self,
        sdk_connection: &Arc<Mutex<SdkConnectionManager>>,
        action: &str,
        http_method: Method,
        http_path: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, DispatchError> {
        // 1. Snapshot the active connection. Drop the lock before any await
        //    on a remote — responsiveness probe + HTTP request must not pin
        //    the manager Mutex.
        let (app_id, app_url, base_path, http_client) = {
            let guard = sdk_connection.lock().await;
            let conn = guard
                .active_connection()
                .ok_or(DispatchError::NotConnected)?;
            (
                conn.app_info.app_id.clone(),
                conn.app_url.clone(),
                conn.base_path.clone(),
                conn.client.clone(),
            )
        };

        // 2. WS-transport apps short-circuit through the relay — same code
        //    path as `dispatch`, but reached via the active-connection probe
        //    rather than a caller-supplied `app_id`.
        if let Some(entry) = self.registry.get(&app_id).await {
            if matches!(entry.transport, AppTransport::Websocket) {
                debug!(
                    "[app-dispatch] {} via websocket (active) → app='{}' conn_id={:?}",
                    action, app_id, entry.websocket_conn_id
                );
                let response = self
                    .command_relay
                    .dispatch(&app_id, action, payload)
                    .await?;
                return Ok(response.result.unwrap_or(serde_json::Value::Null));
            }
        }

        // 3. HTTP arm — preserve legacy responsiveness cache semantics.
        self.http_dispatch_inner(
            sdk_connection,
            &app_id,
            &app_url,
            &base_path,
            &http_client,
            http_method,
            http_path,
            payload,
        )
        .await
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::app_discovery::DiscoveredApp;
    use crate::mcp::sdk_client::{SdkAppInfo, SdkConnection};
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

    fn manager_with_active(
        app_id: &str,
        url: &str,
        base_path: &str,
    ) -> Arc<Mutex<SdkConnectionManager>> {
        let mut mgr = SdkConnectionManager::new();
        let info = SdkAppInfo {
            app_id: app_id.to_string(),
            app_name: "t".into(),
            app_type: "web".into(),
            framework: None,
            version: None,
            capabilities: vec![],
            port: 0,
        };
        let conn = SdkConnection {
            app_url: url.to_string(),
            base_path: base_path.to_string(),
            app_info: info,
            client: reqwest::Client::new(),
            connected_at: 0,
            transport_kind: None,
            physical_device_id: None,
        };
        mgr.connections.insert(url.to_string(), conn);
        mgr.active_url = Some(url.to_string());
        Arc::new(Mutex::new(mgr))
    }

    #[tokio::test]
    async fn dispatch_active_no_connection_returns_not_connected() {
        let registry = AppRegistry::new();
        let ws = WsConnectionManager::new();
        let relay = CommandRelay::new(ws);
        let dispatcher = AppDispatcher::new(registry, relay);
        let sdk_conn = Arc::new(Mutex::new(SdkConnectionManager::new()));

        let err = dispatcher
            .dispatch_active(&sdk_conn, "noop", Method::GET, "/x", json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::NotConnected), "got {:?}", err);
    }

    #[tokio::test]
    async fn dispatch_active_routes_websocket_via_relay() {
        let registry = AppRegistry::new();
        let ws = WsConnectionManager::new();
        let (_conn_id, mut outbound_rx) = ws.test_register("wapp").await;
        registry
            .upsert(
                sample_app("wapp", "ws-app://wapp"),
                None,
                AppTransport::Websocket,
                Some(1),
            )
            .await;
        let relay = CommandRelay::with_timeout(ws.clone(), Duration::from_secs(2));
        let dispatcher = AppDispatcher::new(registry, relay.clone());
        let sdk_conn = manager_with_active("wapp", "ws-app://wapp", "");

        let dispatcher2 = dispatcher.clone();
        let sdk_conn2 = sdk_conn.clone();
        let handle = tokio::spawn(async move {
            dispatcher2
                .dispatch_active(&sdk_conn2, "snap", Method::GET, "/ignored", json!({"q": 1}))
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

    #[tokio::test]
    async fn dispatch_active_http_fast_fails_when_cache_says_unresponsive() {
        let registry = AppRegistry::new();
        let ws = WsConnectionManager::new();
        let relay = CommandRelay::new(ws);
        let dispatcher = AppDispatcher::new(registry.clone(), relay);
        // Register as HTTP so dispatch_active goes through the responsiveness arm.
        registry
            .upsert(
                sample_app("happ", "http://127.0.0.1:0"),
                None,
                AppTransport::Http,
                None,
            )
            .await;
        let sdk_conn = manager_with_active("happ", "http://127.0.0.1:0", "");
        // Prime the cache: mark unresponsive AS OF NOW so the entry is fresh.
        {
            let mut g = sdk_conn.lock().await;
            g.active_responsive = Some(false);
            g.active_responsive_checked_at = chrono::Utc::now().timestamp_millis();
        }

        let err = dispatcher
            .dispatch_active(&sdk_conn, "noop", Method::GET, "/x", json!({}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, DispatchError::NotResponsive(_)),
            "got {:?}",
            err
        );
        // The error message must be the legacy string handlers may surface verbatim.
        assert_eq!(
            err.to_user_message(),
            "SDK app is not responsive (no active browser tab)"
        );
    }
}
