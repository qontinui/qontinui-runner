//! Runner-hosted UI Bridge command-relay tab protocol.
//!
//! Implements the web-relay wire contract the SDK relay client
//! (`ui-bridge/packages/ui-bridge/src/relay/relay-client.ts`) and the
//! injected transport (`ui-bridge-inject` CLI → `injected/bootstrap.ts`)
//! speak, so a temp runner can host injected tabs without a prod relay:
//!
//!   1. `GET  /ui-bridge/commands/stream?tabId=` — SSE command delivery.
//!      First event is `{"type":"connected","tabId":...}`; subsequent events
//!      are queued command frames `{commandId, action, payload, timestamp}`.
//!      `: heartbeat` keep-alive comments every 15s.
//!   2. `POST /ui-bridge/commands` — the tab posts the result envelope
//!      `{commandId, success, result, tabId, error?}` (registered in
//!      `mod.rs::routes()`, chained onto the existing GET).
//!   3. `POST /ui-bridge/heartbeat` — tab registry upsert; response carries
//!      `data.tabRegistered` so the client can detect silent stream drops
//!      and force a reconnect.
//!   4. `GET  /ui-bridge/tabs` — registry listing; polled by
//!      `ui-bridge-headless`'s `waitForUiBridgeRegistration` (reads
//!      `body.data.tabs[].tabId`).
//!   5. `POST /ui-bridge/relay/dispatch` — runner-side command entry point:
//!      queue a command to a registered tab and await its result.
//!
//! Unlike the qontinui-web relay, heartbeats are LENIENT: the runner is a
//! single-operator loopback surface, so `registrationMetadata`
//! (`{userId, sessionId}`) is stored when present but not required. Plan:
//! `plans/2026-06-12-co-pilot-automation-ui-bridge-remediation.md` item 6(a).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    response::Json,
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

/// Tabs with no live SSE listener whose last sign of life is older than this
/// are evicted from the registry (lazily, on the next registry operation).
/// Heartbeats arrive every 10s; 60s = 6 missed beats.
pub const STALE_TAB_EVICT_MS: u64 = 60_000;

/// Default await window for a dispatched command's result.
const DEFAULT_DISPATCH_TIMEOUT_MS: u64 = 10_000;
/// Cap on caller-supplied dispatch timeouts.
const MAX_DISPATCH_TIMEOUT_MS: u64 = 60_000;

/// SSE keep-alive comment interval (matches the web relay's 15s).
const SSE_KEEPALIVE_SECS: u64 = 15;

/// Heartbeat metadata keys copied into the tab record verbatim.
const HEARTBEAT_METADATA_KEYS: &[&str] = &[
    "url",
    "title",
    "visibility",
    "appId",
    "appName",
    "appType",
    "framework",
    "capabilities",
    "version",
];

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Command frame as delivered over the SSE stream. Field names match the
/// relay client's `QueuedCommand` (camelCase on the wire).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedCommand {
    pub command_id: String,
    pub action: String,
    pub payload: serde_json::Value,
    pub timestamp: u64,
}

/// Result envelope a tab POSTs back for a dispatched command.
#[derive(Debug, Clone)]
pub struct CommandResult {
    pub success: bool,
    pub result: serde_json::Value,
    pub error: Option<String>,
}

/// Why a dispatch could not produce a tab result.
#[derive(Debug)]
pub enum DispatchError {
    /// No tab currently holds a live SSE listener.
    NoTabConnected,
    /// No `tabId` was supplied and more than one tab is connected.
    AmbiguousTab(Vec<String>),
    /// The named tab is unknown or has no live SSE listener.
    TabNotFound(String),
    /// The tab's listener channel closed before a result arrived.
    Disconnected(String),
    /// The tab never posted a result within the window.
    Timeout { command_id: String, timeout_ms: u64 },
}

struct Listener {
    conn_id: u64,
    tx: tokio::sync::mpsc::UnboundedSender<QueuedCommand>,
}

struct TabRecord {
    registered_at_ms: u64,
    last_heartbeat_ms: Option<u64>,
    /// Last moment we positively observed the tab alive (heartbeat OR stream
    /// connect). Drives stale eviction for tabs that connected a stream but
    /// never heartbeat.
    last_seen_ms: u64,
    metadata: serde_json::Map<String, serde_json::Value>,
    listener: Option<Listener>,
}

#[derive(Default)]
struct RegistryInner {
    tabs: HashMap<String, TabRecord>,
    pending: HashMap<String, tokio::sync::oneshot::Sender<CommandResult>>,
}

/// Process-wide relay tab registry. One per runner (held on `ApiState`).
pub struct RelayRegistry {
    inner: Mutex<RegistryInner>,
    conn_seq: AtomicU64,
}

impl Default for RelayRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl RelayRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RegistryInner::default()),
            conn_seq: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RegistryInner> {
        // Lock poisoning would mean a panic mid-registry-op; the registry
        // holds no invariants that a half-applied op can corrupt beyond the
        // op itself, so recover rather than wedge every relay route.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Register (or replace) the SSE listener for `tab_id`. Returns the
    /// connection id (for drop-time deregistration) and the command receiver.
    pub fn connect_stream(
        &self,
        tab_id: &str,
    ) -> (u64, tokio::sync::mpsc::UnboundedReceiver<QueuedCommand>) {
        let conn_id = self.conn_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let now = now_ms();
        let mut inner = self.lock();
        evict_stale(&mut inner, now);
        let record = inner
            .tabs
            .entry(tab_id.to_string())
            .or_insert_with(|| TabRecord {
                registered_at_ms: now,
                last_heartbeat_ms: None,
                last_seen_ms: now,
                metadata: serde_json::Map::new(),
                listener: None,
            });
        record.last_seen_ms = now;
        record.listener = Some(Listener { conn_id, tx });
        (conn_id, rx)
    }

    /// Deregister the SSE listener for `tab_id`, but only if it is still the
    /// connection identified by `conn_id` — a reconnect that already replaced
    /// the listener must not be torn down by the old stream's drop.
    pub fn disconnect_stream(&self, tab_id: &str, conn_id: u64) {
        let mut inner = self.lock();
        if let Some(record) = inner.tabs.get_mut(tab_id) {
            if record
                .listener
                .as_ref()
                .is_some_and(|l| l.conn_id == conn_id)
            {
                record.listener = None;
                record.last_seen_ms = now_ms();
            }
        }
    }

    /// Upsert a tab from a heartbeat body. Returns whether the tab currently
    /// holds a live SSE listener (`tabRegistered` in the response — the
    /// client's silent-drop recovery signal).
    pub fn heartbeat(&self, tab_id: &str, body: &serde_json::Value) -> bool {
        let now = now_ms();
        let mut inner = self.lock();
        evict_stale(&mut inner, now);
        let record = inner
            .tabs
            .entry(tab_id.to_string())
            .or_insert_with(|| TabRecord {
                registered_at_ms: now,
                last_heartbeat_ms: None,
                last_seen_ms: now,
                metadata: serde_json::Map::new(),
                listener: None,
            });
        record.last_heartbeat_ms = Some(now);
        record.last_seen_ms = now;
        for key in HEARTBEAT_METADATA_KEYS {
            if let Some(value) = body.get(*key) {
                if !value.is_null() {
                    record.metadata.insert((*key).to_string(), value.clone());
                }
            }
        }
        // Lenient per-user scoping: store the envelope when supplied.
        if let Some(meta) = body.get("registrationMetadata") {
            for key in ["userId", "sessionId"] {
                if let Some(value) = meta.get(key).and_then(|v| v.as_str()) {
                    if !value.trim().is_empty() {
                        record
                            .metadata
                            .insert(key.to_string(), serde_json::json!(value.trim()));
                    }
                }
            }
        }
        record.listener.is_some()
    }

    /// Snapshot the registry as JSON tab entries (stale tabs evicted first).
    /// Shape mirrors the web relay's `GET /tabs`: each entry carries `tabId`,
    /// flattened heartbeat metadata, `lastHeartbeat`, `connected`, and
    /// `isPrimary` (oldest registered tab).
    pub fn list_tabs(&self) -> Vec<serde_json::Value> {
        let now = now_ms();
        let mut inner = self.lock();
        evict_stale(&mut inner, now);
        let primary = inner
            .tabs
            .iter()
            .min_by_key(|(id, t)| (t.registered_at_ms, (*id).clone()))
            .map(|(id, _)| id.clone());
        let mut tabs: Vec<(u64, serde_json::Value)> = inner
            .tabs
            .iter()
            .map(|(tab_id, record)| {
                let mut entry = record.metadata.clone();
                entry.insert("tabId".to_string(), serde_json::json!(tab_id));
                entry.insert(
                    "lastHeartbeat".to_string(),
                    serde_json::json!(record.last_heartbeat_ms),
                );
                entry.insert(
                    "registeredAt".to_string(),
                    serde_json::json!(record.registered_at_ms),
                );
                entry.insert(
                    "connected".to_string(),
                    serde_json::json!(record.listener.is_some()),
                );
                entry.insert(
                    "isPrimary".to_string(),
                    serde_json::json!(primary.as_deref() == Some(tab_id.as_str())),
                );
                (record.registered_at_ms, serde_json::Value::Object(entry))
            })
            .collect();
        // Deterministic ordering: oldest first (primary leads).
        tabs.sort_by_key(|(registered_at, _)| *registered_at);
        tabs.into_iter().map(|(_, entry)| entry).collect()
    }

    /// Ids of tabs that currently hold a live SSE listener.
    pub fn connected_tab_ids(&self) -> Vec<String> {
        let mut inner = self.lock();
        evict_stale(&mut inner, now_ms());
        let mut ids: Vec<String> = inner
            .tabs
            .iter()
            .filter(|(_, t)| t.listener.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Deliver a result envelope for `command_id`. Returns `false` when no
    /// dispatch is awaiting that id (already timed out, or unknown).
    pub fn complete(&self, command_id: &str, result: CommandResult) -> bool {
        let sender = self.lock().pending.remove(command_id);
        match sender {
            Some(tx) => tx.send(result).is_ok(),
            None => false,
        }
    }

    /// Queue a command to a registered tab and await its result envelope.
    ///
    /// Target resolution: an explicit `tab_id` must name a tab with a live
    /// listener; with no `tab_id`, exactly one connected tab must exist.
    /// Returns `(tab_id, command_id, result)` on success.
    pub async fn dispatch(
        &self,
        tab_id: Option<&str>,
        action: &str,
        payload: serde_json::Value,
        timeout: Duration,
    ) -> Result<(String, String, CommandResult), DispatchError> {
        let command_id = uuid::Uuid::new_v4().to_string();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        // Resolve the target + enqueue while holding the lock; await outside.
        let target = {
            let mut inner = self.lock();
            evict_stale(&mut inner, now_ms());
            let target = match tab_id {
                Some(id) => {
                    let record = inner
                        .tabs
                        .get(id)
                        .ok_or_else(|| DispatchError::TabNotFound(id.to_string()))?;
                    if record.listener.is_none() {
                        return Err(DispatchError::TabNotFound(id.to_string()));
                    }
                    id.to_string()
                }
                None => {
                    let connected: Vec<&String> = inner
                        .tabs
                        .iter()
                        .filter(|(_, t)| t.listener.is_some())
                        .map(|(id, _)| id)
                        .collect();
                    match connected.len() {
                        0 => return Err(DispatchError::NoTabConnected),
                        1 => connected[0].clone(),
                        _ => {
                            let mut ids: Vec<String> = connected.into_iter().cloned().collect();
                            ids.sort();
                            return Err(DispatchError::AmbiguousTab(ids));
                        }
                    }
                }
            };
            inner.pending.insert(command_id.clone(), result_tx);
            let command = QueuedCommand {
                command_id: command_id.clone(),
                action: action.to_string(),
                payload,
                timestamp: now_ms(),
            };
            let record = inner
                .tabs
                .get_mut(&target)
                .expect("target resolved from this map under the same lock");
            let listener = record
                .listener
                .as_ref()
                .expect("listener checked under the same lock");
            if listener.tx.send(command).is_err() {
                // Receiver dropped (stream gone) — clean up and report.
                record.listener = None;
                inner.pending.remove(&command_id);
                return Err(DispatchError::TabNotFound(target));
            }
            target
        };

        match tokio::time::timeout(timeout, result_rx).await {
            Ok(Ok(result)) => Ok((target, command_id, result)),
            Ok(Err(_)) => Err(DispatchError::Disconnected(target)),
            Err(_) => {
                self.lock().pending.remove(&command_id);
                Err(DispatchError::Timeout {
                    command_id,
                    timeout_ms: timeout.as_millis() as u64,
                })
            }
        }
    }

    /// Test hook: run stale eviction as if the clock read `now`.
    #[cfg(test)]
    fn evict_stale_at(&self, now: u64) {
        evict_stale(&mut self.lock(), now);
    }
}

/// Remove tabs with no live SSE listener whose last sign of life is older
/// than [`STALE_TAB_EVICT_MS`]. Connected tabs are never evicted — the SSE
/// drop guard handles their lifecycle.
fn evict_stale(inner: &mut RegistryInner, now: u64) {
    inner.tabs.retain(|_, record| {
        record.listener.is_some() || now.saturating_sub(record.last_seen_ms) < STALE_TAB_EVICT_MS
    });
}

/// Drop guard owned by the SSE generator: deregisters this connection's
/// listener when the client goes away and axum drops the stream.
struct StreamGuard {
    registry: Arc<RelayRegistry>,
    tab_id: String,
    conn_id: u64,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.registry.disconnect_stream(&self.tab_id, self.conn_id);
        info!(
            "UI Bridge relay: SSE stream closed for tab {} (conn {})",
            self.tab_id, self.conn_id
        );
    }
}

// ============================================================================
// Handlers
// ============================================================================

/// GET /ui-bridge/commands/stream?tabId=
///
/// SSE command stream for a relay tab. Registers (or re-registers) the tab's
/// listener; a missing `tabId` gets a generated one, echoed in the initial
/// `connected` event. The drop guard deregisters the listener when the
/// client disconnects.
pub async fn ui_bridge_relay_command_stream_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let registry = state.ui_bridge_relay.clone();
    let tab_id = query
        .get("tabId")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let (conn_id, mut rx) = registry.connect_stream(&tab_id);
    info!(
        "UI Bridge relay: SSE stream opened for tab {} (conn {})",
        tab_id, conn_id
    );

    let stream = async_stream::stream! {
        let _guard = StreamGuard {
            registry,
            tab_id: tab_id.clone(),
            conn_id,
        };
        yield Ok(Event::default()
            .data(serde_json::json!({ "type": "connected", "tabId": tab_id }).to_string()));
        while let Some(command) = rx.recv().await {
            match serde_json::to_string(&command) {
                Ok(json) => yield Ok(Event::default().data(json)),
                Err(e) => {
                    warn!("UI Bridge relay: failed to serialize command frame: {e}");
                }
            }
        }
    };

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(SSE_KEEPALIVE_SECS))
            .text("heartbeat"),
    )
}

/// POST /ui-bridge/heartbeat
///
/// Relay tab registry upsert. LENIENT (unlike the strict web relay): only
/// `tabId` is required; `registrationMetadata` is stored when present. The
/// response's `data.tabRegistered` reports whether this tab currently holds
/// a live SSE listener — the client forces a stream reconnect on `false`.
pub async fn ui_bridge_relay_heartbeat_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let Some(tab_id) = body
        .get("tabId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        let mut err = api_error("heartbeat requires a non-empty tabId field");
        err.code = Some("MISSING_TAB_ID".to_string());
        return Err((StatusCode::BAD_REQUEST, Json(err)));
    };
    let tab_registered = state.ui_bridge_relay.heartbeat(tab_id, &body);
    Ok(Json(ApiResponse::success(serde_json::json!({
        "received": true,
        "tabRegistered": tab_registered,
    }))))
}

/// POST /ui-bridge/commands
///
/// Result ingestion: a relay tab posts the result envelope for a previously
/// streamed command. `matched: false` means no dispatch was awaiting that
/// `commandId` (it timed out, or the id is unknown) — not an error, the tab
/// fire-and-forgets these.
pub async fn ui_bridge_relay_command_result_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let Some(command_id) = body
        .get("commandId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        let mut err = api_error("command result requires a non-empty commandId field");
        err.code = Some("MISSING_COMMAND_ID".to_string());
        return Err((StatusCode::BAD_REQUEST, Json(err)));
    };
    let result = CommandResult {
        success: body
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        result: body
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        error: body
            .get("error")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    };
    let matched = state.ui_bridge_relay.complete(command_id, result);
    Ok(Json(ApiResponse::success(serde_json::json!({
        "received": true,
        "matched": matched,
    }))))
}

/// GET /ui-bridge/tabs
///
/// List registered relay tabs. Response shape matches what
/// `ui-bridge-headless`'s `waitForUiBridgeRegistration` polls for:
/// `{ success, data: { tabs: [{ tabId, ... }] } }`.
pub async fn ui_bridge_relay_tabs_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tabs = state.ui_bridge_relay.list_tabs();
    Ok(Json(ApiResponse::success(serde_json::json!({
        "count": tabs.len(),
        "tabs": tabs,
        "staleHeartbeatMs": STALE_TAB_EVICT_MS,
    }))))
}

/// Request body for `POST /ui-bridge/relay/dispatch`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayDispatchRequest {
    /// Target tab. Optional when exactly one tab is connected.
    #[serde(default, alias = "targetTabId")]
    pub tab_id: Option<String>,
    /// Relay action id (e.g. `discover`, `find`, `get_snapshot`) — the same
    /// action vocabulary `executeCommand` dispatches browser-side.
    pub action: String,
    /// Action payload, forwarded verbatim.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Result await window in ms (default 10000, capped at 60000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// POST /ui-bridge/relay/dispatch
///
/// Queue a command to a registered relay tab over its SSE stream and await
/// the result envelope the tab POSTs back to `/ui-bridge/commands`. This is
/// the runner-side entry point for driving injected/relay tabs (the analog
/// of the web relay's per-route `queueCommand` dispatch).
pub async fn ui_bridge_relay_dispatch_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RelayDispatchRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    if request.action.trim().is_empty() {
        let mut err = api_error("dispatch requires a non-empty action field");
        err.code = Some("MISSING_ACTION".to_string());
        return Err((StatusCode::BAD_REQUEST, Json(err)));
    }
    let timeout_ms = request
        .timeout_ms
        .unwrap_or(DEFAULT_DISPATCH_TIMEOUT_MS)
        .clamp(1, MAX_DISPATCH_TIMEOUT_MS);
    info!(
        "UI Bridge relay: dispatch '{}' to tab {:?} (timeout {}ms)",
        request.action, request.tab_id, timeout_ms
    );

    match state
        .ui_bridge_relay
        .dispatch(
            request.tab_id.as_deref(),
            &request.action,
            request.payload,
            Duration::from_millis(timeout_ms),
        )
        .await
    {
        Ok((tab_id, command_id, result)) => {
            let payload = serde_json::json!({
                "tabId": tab_id,
                "commandId": command_id,
                "result": result.result,
            });
            if result.success {
                Ok(Json(ApiResponse::success(payload)))
            } else {
                // The tab answered but reported failure — surface its error
                // verbatim with the result payload attached.
                let mut response: ApiResponse<serde_json::Value> = ApiResponse::error(
                    result
                        .error
                        .unwrap_or_else(|| "relay tab reported a failed command".to_string()),
                );
                response.data = Some(payload);
                Ok(Json(response))
            }
        }
        Err(DispatchError::NoTabConnected) => {
            let mut err = api_error(
                "no relay tab is connected — register one via the SSE command stream first",
            );
            err.code = Some("NO_TAB_CONNECTED".to_string());
            Err((StatusCode::SERVICE_UNAVAILABLE, Json(err)))
        }
        Err(DispatchError::AmbiguousTab(ids)) => {
            let mut err = api_error(format!(
                "multiple relay tabs are connected ({}) — supply tabId to pin the target",
                ids.join(", ")
            ));
            err.code = Some("AMBIGUOUS_TAB".to_string());
            err.suggestions = Some(
                ids.iter()
                    .map(|id| format!("retry with {{\"tabId\": \"{id}\"}}"))
                    .collect(),
            );
            Err((StatusCode::CONFLICT, Json(err)))
        }
        Err(DispatchError::TabNotFound(id)) => {
            // Structured envelope so the 404 is discriminable from an
            // unregistered route (see CONTRACT.md "404 discriminator").
            let mut err = api_error(format!(
                "relay tab '{id}' is not connected — discover live tabs via GET /ui-bridge/tabs"
            ));
            err.code = Some("TAB_NOT_FOUND".to_string());
            Err((StatusCode::NOT_FOUND, Json(err)))
        }
        Err(DispatchError::Disconnected(id)) => {
            let mut err = api_error(format!(
                "relay tab '{id}' disconnected before posting a result"
            ));
            err.code = Some("TAB_DISCONNECTED".to_string());
            Err((StatusCode::BAD_GATEWAY, Json(err)))
        }
        Err(DispatchError::Timeout {
            command_id,
            timeout_ms,
        }) => {
            let mut err = api_error(format!(
                "relay tab did not post a result for command {command_id} within {timeout_ms}ms"
            ));
            err.code = Some("TIMEOUT".to_string());
            Err((StatusCode::GATEWAY_TIMEOUT, Json(err)))
        }
    }
}

// ============================================================================
// Routes + manifest
// ============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    // NOTE: POST /ui-bridge/commands (result ingestion,
    // `ui_bridge_relay_command_result_handler`) is registered in
    // `mod.rs::routes()`, chained onto the pre-existing GET for the
    // Tauri-invoke command listing — the path is shared across families.
    axum::Router::new()
        .route(
            "/ui-bridge/commands/stream",
            get(ui_bridge_relay_command_stream_handler),
        )
        // Relay-tab registry heartbeat (web-relay wire contract; the old
        // `receive_heartbeat` IPC forward never had a frontend handler).
        .route(
            "/ui-bridge/heartbeat",
            post(ui_bridge_relay_heartbeat_handler),
        )
        .route("/ui-bridge/tabs", get(ui_bridge_relay_tabs_handler))
        .route(
            "/ui-bridge/relay/dispatch",
            post(ui_bridge_relay_dispatch_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/commands/stream"),
        ("POST", "/ui-bridge/heartbeat"),
        ("GET", "/ui-bridge/tabs"),
        ("POST", "/ui-bridge/relay/dispatch"),
    ]
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn heartbeat_body(tab_id: &str) -> serde_json::Value {
        serde_json::json!({
            "tabId": tab_id,
            "url": "https://example.test/login",
            "title": "Example",
            "visibility": "visible",
            "appType": "injected",
            "appName": "example login",
            "capabilities": ["control", "discovery"],
            "version": "0.19.0",
        })
    }

    #[test]
    fn heartbeat_registers_tab_without_stream() {
        let registry = RelayRegistry::new();
        let tab_registered = registry.heartbeat("tab-a", &heartbeat_body("tab-a"));
        assert!(!tab_registered, "no SSE listener yet");

        let tabs = registry.list_tabs();
        assert_eq!(tabs.len(), 1);
        let tab = &tabs[0];
        assert_eq!(tab["tabId"], "tab-a");
        assert_eq!(tab["connected"], false);
        assert_eq!(tab["url"], "https://example.test/login");
        assert_eq!(tab["appType"], "injected");
        assert_eq!(tab["isPrimary"], true);
        assert!(tab["lastHeartbeat"].is_u64());
    }

    #[test]
    fn heartbeat_is_lenient_about_registration_metadata() {
        let registry = RelayRegistry::new();
        // No registrationMetadata at all — accepted (unlike the strict web relay).
        assert!(!registry.heartbeat("tab-a", &serde_json::json!({ "tabId": "tab-a" })));
        // With metadata — stored and surfaced on the tab entry.
        registry.heartbeat(
            "tab-a",
            &serde_json::json!({
                "tabId": "tab-a",
                "registrationMetadata": { "userId": "user-1", "sessionId": "sess-1" },
            }),
        );
        let tabs = registry.list_tabs();
        assert_eq!(tabs[0]["userId"], "user-1");
        assert_eq!(tabs[0]["sessionId"], "sess-1");
    }

    #[tokio::test]
    async fn connect_stream_flips_tab_registered_and_connected() {
        let registry = RelayRegistry::new();
        let (_conn_id, _rx) = registry.connect_stream("tab-a");
        assert!(registry.heartbeat("tab-a", &heartbeat_body("tab-a")));
        let tabs = registry.list_tabs();
        assert_eq!(tabs[0]["connected"], true);
        assert_eq!(registry.connected_tab_ids(), vec!["tab-a".to_string()]);
    }

    #[tokio::test]
    async fn dispatch_delivers_command_and_returns_result() {
        let registry = Arc::new(RelayRegistry::new());
        let (_conn_id, mut rx) = registry.connect_stream("tab-a");

        let responder = {
            let registry = registry.clone();
            tokio::spawn(async move {
                let command = rx.recv().await.expect("command frame");
                assert_eq!(command.action, "discover");
                assert_eq!(command.payload["query"], "smoke");
                let matched = registry.complete(
                    &command.command_id,
                    CommandResult {
                        success: true,
                        result: serde_json::json!({ "elements": [{ "id": "button-run" }] }),
                        error: None,
                    },
                );
                assert!(matched, "dispatch should be awaiting this commandId");
            })
        };

        let (tab_id, command_id, result) = registry
            .dispatch(
                Some("tab-a"),
                "discover",
                serde_json::json!({ "query": "smoke" }),
                Duration::from_secs(5),
            )
            .await
            .expect("dispatch result");
        assert_eq!(tab_id, "tab-a");
        assert!(!command_id.is_empty());
        assert!(result.success);
        assert_eq!(result.result["elements"][0]["id"], "button-run");
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_without_tab_id_targets_sole_connected_tab() {
        let registry = Arc::new(RelayRegistry::new());
        // A heartbeat-only (not connected) tab must NOT make this ambiguous.
        registry.heartbeat("tab-idle", &heartbeat_body("tab-idle"));
        let (_conn_id, mut rx) = registry.connect_stream("tab-live");

        let responder = {
            let registry = registry.clone();
            tokio::spawn(async move {
                let command = rx.recv().await.expect("command frame");
                registry.complete(
                    &command.command_id,
                    CommandResult {
                        success: true,
                        result: serde_json::Value::Null,
                        error: None,
                    },
                );
            })
        };

        let (tab_id, _, _) = registry
            .dispatch(
                None,
                "ping",
                serde_json::Value::Null,
                Duration::from_secs(5),
            )
            .await
            .expect("dispatch result");
        assert_eq!(tab_id, "tab-live");
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_with_no_connected_tab_is_no_tab_connected() {
        let registry = RelayRegistry::new();
        registry.heartbeat("tab-idle", &heartbeat_body("tab-idle"));
        let err = registry
            .dispatch(
                None,
                "ping",
                serde_json::Value::Null,
                Duration::from_secs(1),
            )
            .await
            .expect_err("no connected tab");
        assert!(matches!(err, DispatchError::NoTabConnected));
    }

    #[tokio::test]
    async fn dispatch_with_two_connected_tabs_is_ambiguous() {
        let registry = RelayRegistry::new();
        let (_c1, _rx1) = registry.connect_stream("tab-a");
        let (_c2, _rx2) = registry.connect_stream("tab-b");
        let err = registry
            .dispatch(
                None,
                "ping",
                serde_json::Value::Null,
                Duration::from_secs(1),
            )
            .await
            .expect_err("ambiguous");
        match err {
            DispatchError::AmbiguousTab(ids) => {
                assert_eq!(ids, vec!["tab-a".to_string(), "tab-b".to_string()]);
            }
            other => panic!("expected AmbiguousTab, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_to_unknown_or_unconnected_tab_is_tab_not_found() {
        let registry = RelayRegistry::new();
        let err = registry
            .dispatch(
                Some("tab-missing"),
                "ping",
                serde_json::Value::Null,
                Duration::from_secs(1),
            )
            .await
            .expect_err("unknown tab");
        assert!(matches!(err, DispatchError::TabNotFound(ref id) if id == "tab-missing"));

        // Known via heartbeat but no live stream — pinned dispatch must NOT
        // silently target some other tab; it reports TAB_NOT_FOUND.
        registry.heartbeat("tab-idle", &heartbeat_body("tab-idle"));
        let err = registry
            .dispatch(
                Some("tab-idle"),
                "ping",
                serde_json::Value::Null,
                Duration::from_secs(1),
            )
            .await
            .expect_err("unconnected tab");
        assert!(matches!(err, DispatchError::TabNotFound(ref id) if id == "tab-idle"));
    }

    #[tokio::test]
    async fn dispatch_times_out_and_cleans_pending() {
        let registry = RelayRegistry::new();
        let (_conn_id, mut rx) = registry.connect_stream("tab-a");
        let err = registry
            .dispatch(
                Some("tab-a"),
                "ping",
                serde_json::Value::Null,
                Duration::from_millis(50),
            )
            .await
            .expect_err("timeout");
        let command_id = match err {
            DispatchError::Timeout { command_id, .. } => command_id,
            other => panic!("expected Timeout, got {other:?}"),
        };
        // The frame still arrived on the stream...
        let frame = rx.recv().await.expect("command frame");
        assert_eq!(frame.command_id, command_id);
        // ...but the pending waiter is gone: a late result no longer matches.
        let matched = registry.complete(
            &command_id,
            CommandResult {
                success: true,
                result: serde_json::Value::Null,
                error: None,
            },
        );
        assert!(!matched, "timed-out dispatch must remove its pending entry");
    }

    #[tokio::test]
    async fn dispatch_to_dropped_stream_is_tab_not_found() {
        let registry = RelayRegistry::new();
        let (_conn_id, rx) = registry.connect_stream("tab-a");
        drop(rx); // client vanished without the guard firing yet
        let err = registry
            .dispatch(
                Some("tab-a"),
                "ping",
                serde_json::Value::Null,
                Duration::from_secs(1),
            )
            .await
            .expect_err("dead stream");
        assert!(matches!(err, DispatchError::TabNotFound(_)));
        // The dead listener was cleared as a side effect.
        assert!(registry.connected_tab_ids().is_empty());
    }

    #[tokio::test]
    async fn reconnect_replaces_listener_and_old_disconnect_is_ignored() {
        let registry = RelayRegistry::new();
        let (old_conn, _old_rx) = registry.connect_stream("tab-a");
        let (_new_conn, mut new_rx) = registry.connect_stream("tab-a");

        // The OLD stream's drop guard fires late — it must not tear down the
        // replacement listener.
        registry.disconnect_stream("tab-a", old_conn);
        assert_eq!(registry.connected_tab_ids(), vec!["tab-a".to_string()]);

        // Commands flow to the new listener.
        let registry = Arc::new(registry);
        let responder = {
            let registry = registry.clone();
            tokio::spawn(async move {
                let command = new_rx.recv().await.expect("command frame");
                registry.complete(
                    &command.command_id,
                    CommandResult {
                        success: true,
                        result: serde_json::Value::Null,
                        error: None,
                    },
                );
            })
        };
        registry
            .dispatch(
                Some("tab-a"),
                "ping",
                serde_json::Value::Null,
                Duration::from_secs(5),
            )
            .await
            .expect("dispatch via replacement listener");
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn matching_disconnect_deregisters_listener() {
        let registry = RelayRegistry::new();
        let (conn_id, _rx) = registry.connect_stream("tab-a");
        registry.disconnect_stream("tab-a", conn_id);
        assert!(registry.connected_tab_ids().is_empty());
        // The tab record survives (until stale eviction) — connected: false.
        let tabs = registry.list_tabs();
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0]["connected"], false);
    }

    #[test]
    fn stale_unconnected_tabs_are_evicted_connected_tabs_are_not() {
        let registry = RelayRegistry::new();
        registry.heartbeat("tab-stale", &heartbeat_body("tab-stale"));
        let (_conn_id, _rx) = registry.connect_stream("tab-live");

        registry.evict_stale_at(now_ms() + STALE_TAB_EVICT_MS + 1);
        let tabs = registry.list_tabs();
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0]["tabId"], "tab-live");
    }

    #[test]
    fn complete_with_unknown_command_id_is_unmatched() {
        let registry = RelayRegistry::new();
        assert!(!registry.complete(
            "no-such-command",
            CommandResult {
                success: true,
                result: serde_json::Value::Null,
                error: None,
            },
        ));
    }

    /// The SSE command frame must use the relay client's camelCase field
    /// names (`relay-client.ts` `QueuedCommand`): commandId, action, payload,
    /// timestamp.
    #[test]
    fn command_frame_serializes_to_relay_client_shape() {
        let frame = QueuedCommand {
            command_id: "cmd-1".to_string(),
            action: "discover".to_string(),
            payload: serde_json::json!({ "query": "smoke" }),
            timestamp: 1234,
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["commandId"], "cmd-1");
        assert_eq!(json["action"], "discover");
        assert_eq!(json["payload"]["query"], "smoke");
        assert_eq!(json["timestamp"], 1234);
        assert!(
            json.get("command_id").is_none(),
            "must be camelCase on the wire"
        );
    }

    /// First-registered tab is primary; ordering is oldest-first.
    #[test]
    fn primary_is_oldest_registered_tab() {
        let registry = RelayRegistry::new();
        registry.heartbeat("tab-first", &heartbeat_body("tab-first"));
        // Force a strictly later registered_at for the second tab.
        std::thread::sleep(std::time::Duration::from_millis(5));
        registry.heartbeat("tab-second", &heartbeat_body("tab-second"));
        let tabs = registry.list_tabs();
        assert_eq!(tabs[0]["tabId"], "tab-first");
        assert_eq!(tabs[0]["isPrimary"], true);
        assert_eq!(tabs[1]["isPrimary"], false);
    }
}
