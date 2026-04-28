//! Web-backend integration state.
//!
//! The runner ↔ qontinui-web channel is a single outbound WebSocket to
//! `WS /api/v1/runners/ws`. This module holds the shared *runtime state*
//! of that connection — runner_id returned in the handshake response,
//! last heartbeat timestamp, last registration error — so other
//! subsystems (`commands::web_integration`) can read it without owning
//! the WS task itself.
//!
//! The WS task lives in [`crate::mcp::backend_relay`]. It updates the fields
//! on this struct via the `set_*` helpers as the connection lifecycle
//! progresses. When `WebIntegrationSettings` change, callers tear down the
//! state via [`ServerModeState::shutdown`] and the relay observes the flag
//! and reconnects.
//!
//! # Token flow
//!
//! The "Connect with web login" OAuth-style flow is independent of the WS
//! relay and lives in [`token_flow`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

use crate::settings::WebIntegrationSettings;

pub mod token_flow;

pub use token_flow::{PendingTokenFlow, TokenFlowStore};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Resolved configuration for web-backend integration.
///
/// Produced from [`WebIntegrationSettings`] via [`ServerModeConfig::from_settings`].
/// Callers that see `None` should skip all web-side reporting (the WS relay
/// will not be started, no phase events emitted).
#[derive(Debug, Clone)]
pub struct ServerModeConfig {
    /// Base URL of the qontinui-web API, e.g. `"https://api.qontinui.io"`.
    /// No trailing slash — callers append `/api/v1/...`.
    pub web_backend_url: String,
    /// Plaintext runner bearer token (`qontinui_runner_<64hex>`).
    /// Never logged.
    pub runner_token: String,
}

impl ServerModeConfig {
    /// Build from the persisted [`WebIntegrationSettings`].
    ///
    /// Returns `None` unless `enabled=true` AND `backend_url` is non-empty
    /// AND `runner_token` is non-empty. Trims a trailing slash from
    /// `backend_url` so callers can safely append `/api/v1/...` paths.
    pub fn from_settings(settings: &WebIntegrationSettings) -> Option<Self> {
        if !settings.enabled {
            return None;
        }
        let backend_url = settings.backend_url.trim();
        let runner_token = settings.runner_token.trim();
        if backend_url.is_empty() || runner_token.is_empty() {
            return None;
        }
        Some(Self {
            web_backend_url: backend_url.trim_end_matches('/').to_string(),
            runner_token: runner_token.to_string(),
        })
    }
}

/// Shared runtime state for the runner ↔ web-backend WebSocket relay.
///
/// Populated and refreshed by [`crate::mcp::backend_relay`] as the WS
/// connection moves through its lifecycle:
///
/// - `runner_id` is set once when the backend's `connected` message arrives
///   following the runner's first `runner_info` send.
/// - `last_heartbeat_at` is updated each time the relay successfully writes
///   a `heartbeat` message.
/// - `connection_error` records the latest connect/handshake failure (e.g.
///   a 401 because the token was revoked) so the Settings UI can surface it.
///
/// Clones cheaply (`Arc` under the hood). When [`ServerModeState::shutdown`]
/// is called, the relay observes the flag on its next iteration and exits;
/// the caller then drops the state and (optionally) builds a new one with
/// fresh settings.
#[derive(Debug, Clone)]
pub struct ServerModeState {
    pub config: ServerModeConfig,
    runner_id: Arc<RwLock<Option<Uuid>>>,
    /// ISO-8601 timestamp of the last heartbeat write. Refreshed by the WS
    /// relay each time it successfully sends a `heartbeat` message.
    last_heartbeat_at: Arc<RwLock<Option<String>>>,
    /// Last connection or handshake error reported by the WS relay (e.g.
    /// 401, network error). Cleared on a successful `connected` message.
    connection_error: Arc<RwLock<Option<String>>>,
    /// Whether the WS is currently connected (post-handshake). Read by the
    /// Settings UI to render an "online" pip.
    ws_connected: Arc<AtomicBool>,
    /// Shutdown flag for hot-reload: when settings change, the settings-save
    /// command calls [`ServerModeState::shutdown`] to flip this to `true`;
    /// the relay loop checks between iterations and returns cleanly.
    shutdown: Arc<AtomicBool>,
}

impl ServerModeState {
    pub fn new(config: ServerModeConfig) -> Self {
        Self {
            config,
            runner_id: Arc::new(RwLock::new(None)),
            last_heartbeat_at: Arc::new(RwLock::new(None)),
            connection_error: Arc::new(RwLock::new(None)),
            ws_connected: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Current runner_id (if the WS handshake has landed at least once).
    pub async fn runner_id(&self) -> Option<Uuid> {
        *self.runner_id.read().await
    }

    /// Set the runner_id. Called from the WS relay when the backend sends
    /// the `connected` message.
    pub async fn set_runner_id(&self, id: Uuid) {
        let mut guard = self.runner_id.write().await;
        *guard = Some(id);
    }

    /// Timestamp (ISO-8601) of the last successful heartbeat write.
    pub async fn last_heartbeat_at(&self) -> Option<String> {
        self.last_heartbeat_at.read().await.clone()
    }

    /// Update the last-heartbeat timestamp. Called by the WS relay after
    /// each successful heartbeat send.
    pub async fn set_last_heartbeat_at(&self, ts: String) {
        let mut guard = self.last_heartbeat_at.write().await;
        *guard = Some(ts);
    }

    /// Most recent connection or handshake error, or `None` if the relay
    /// is currently connected (or has not yet attempted to connect).
    ///
    /// Renamed from the legacy `registration_error` to reflect the new
    /// model: there is no separate "registration" step — registration
    /// happens implicitly via the WS handshake's `runner_info` exchange.
    pub async fn registration_error(&self) -> Option<String> {
        self.connection_error.read().await.clone()
    }

    /// Set or clear the latest connection error. Called by the WS relay
    /// on connect failure (Some) and on successful handshake (None).
    pub async fn set_registration_error(&self, err: Option<String>) {
        let mut guard = self.connection_error.write().await;
        *guard = err;
    }

    /// Whether the WS relay currently has an open, post-handshake connection.
    pub fn is_ws_connected(&self) -> bool {
        self.ws_connected.load(Ordering::Relaxed)
    }

    /// Mark the WS as connected/disconnected. Called by the relay on
    /// handshake completion and on disconnect.
    pub fn set_ws_connected(&self, connected: bool) {
        self.ws_connected.store(connected, Ordering::Relaxed);
    }

    /// Signal the relay task to exit cleanly. After calling this the state
    /// can be dropped; the relay observes the flag and returns.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    /// Whether shutdown has been requested.
    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }
}
