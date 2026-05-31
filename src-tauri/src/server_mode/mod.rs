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

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    /// Legacy plaintext runner bearer token (`qontinui_runner_<64hex>`), or
    /// empty for a Cognito-/pair-code-paired runner. Never logged.
    ///
    /// No longer consulted by the WS relay (it authenticates with the device
    /// JWT from `AuthManager`) — retained only so legacy installs that still
    /// carry one round-trip it unchanged.
    pub runner_token: String,
}

impl ServerModeConfig {
    /// Build from the persisted [`WebIntegrationSettings`].
    ///
    /// Returns `None` unless `enabled=true` AND `backend_url` is non-empty.
    /// Trims a trailing slash from `backend_url` so callers can safely append
    /// `/api/v1/...` paths.
    ///
    /// Deliberately does NOT require `runner_token`. Post-Cognito unification
    /// the relay authenticates with the device JWT, and Cognito-/pair-code-
    /// paired runners have an empty `runner_token`. Requiring it here left
    /// `ServerModeState` uninstalled for those runners, so the relay had
    /// nowhere to publish its connection state and `get_web_integration_status`
    /// reported `ws_connected=false` even while the runner was connected and
    /// heartbeating. Gating only on `enabled` + `backend_url` installs the
    /// state for every connectable runner.
    pub fn from_settings(settings: &WebIntegrationSettings) -> Option<Self> {
        if !settings.enabled {
            return None;
        }
        let backend_url = settings.backend_url.trim();
        if backend_url.is_empty() {
            return None;
        }
        Some(Self {
            web_backend_url: backend_url.trim_end_matches('/').to_string(),
            runner_token: settings.runner_token.trim().to_string(),
        })
    }
}

/// Install a [`ServerModeState`] built from `settings` into `slot` if the slot
/// is currently empty; return the live state (existing or freshly installed),
/// or `None` if `settings` don't form a valid config (see
/// [`ServerModeConfig::from_settings`]).
///
/// A single write-lock makes the check-then-install atomic (no TOCTOU with a
/// concurrent installer such as `apply_web_integration_settings`). An existing
/// state is never clobbered.
///
/// Closes the "integration disabled at boot" gap: boot installs a
/// `ServerModeState` only when `from_settings` is `Some`. A later Cognito /
/// pair-code sign-in enables integration via `save_settings` + a relay kick
/// WITHOUT routing through `apply_web_integration_settings`, so the slot can
/// still be empty when the relay connects. The relay calls this on a
/// successful connect so it always has somewhere to publish `ws_connected` /
/// `runner_id` / heartbeat — otherwise `get_web_integration_status` would
/// report the runner disconnected until the next process restart.
pub async fn install_if_absent(
    slot: &RwLock<Option<ServerModeState>>,
    settings: &WebIntegrationSettings,
) -> Option<ServerModeState> {
    let mut guard = slot.write().await;
    if let Some(existing) = guard.as_ref() {
        return Some(existing.clone());
    }
    let cfg = ServerModeConfig::from_settings(settings)?;
    let state = ServerModeState::new(cfg);
    *guard = Some(state.clone());
    Some(state)
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
    /// Number of backend-side subscribers currently interested in terminal
    /// output frames. The backend sends `terminal_subscribe` /
    /// `terminal_unsubscribe` messages over the relay; the outbound forwarder
    /// reads this and *skips* sending `terminal-output` / `terminal-exit`
    /// frames entirely when this is `0`, preventing a WS flood when no
    /// consumer is attached.
    terminal_subscriber_count: Arc<AtomicUsize>,
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
            terminal_subscriber_count: Arc::new(AtomicUsize::new(0)),
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

    /// Current number of backend-side terminal-output subscribers.
    ///
    /// The outbound relay forwarder consults this before forwarding a
    /// `terminal-output` / `terminal-exit` frame; when it is `0` the frame
    /// is dropped (never serialized or sent) so an unattended terminal
    /// session cannot flood the relay WebSocket.
    pub fn terminal_subscriber_count(&self) -> usize {
        self.terminal_subscriber_count.load(Ordering::SeqCst)
    }

    /// Register one additional backend-side terminal subscriber. Called by
    /// the relay command handler on an inbound `terminal_subscribe`.
    pub fn incr_terminal_subscribers(&self) {
        self.terminal_subscriber_count
            .fetch_add(1, Ordering::SeqCst);
    }

    /// Remove one backend-side terminal subscriber, saturating at `0` so a
    /// stray / duplicate `terminal_unsubscribe` can never make the count
    /// underflow (which would re-enable the flood forever).
    pub fn decr_terminal_subscribers(&self) {
        let _ = self.terminal_subscriber_count.fetch_update(
            Ordering::SeqCst,
            Ordering::SeqCst,
            |cur| {
                if cur == 0 {
                    None
                } else {
                    Some(cur - 1)
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> ServerModeState {
        ServerModeState::new(ServerModeConfig {
            web_backend_url: "https://example.test".to_string(),
            runner_token: "qontinui_runner_test".to_string(),
        })
    }

    #[test]
    fn terminal_subscriber_count_starts_at_zero() {
        let s = test_state();
        assert_eq!(s.terminal_subscriber_count(), 0);
    }

    #[test]
    fn incr_decr_terminal_subscribers_roundtrip() {
        let s = test_state();
        s.incr_terminal_subscribers();
        s.incr_terminal_subscribers();
        assert_eq!(s.terminal_subscriber_count(), 2);
        s.decr_terminal_subscribers();
        assert_eq!(s.terminal_subscriber_count(), 1);
        s.decr_terminal_subscribers();
        assert_eq!(s.terminal_subscriber_count(), 0);
    }

    #[test]
    fn decr_terminal_subscribers_saturates_at_zero() {
        let s = test_state();
        s.decr_terminal_subscribers();
        s.decr_terminal_subscribers();
        assert_eq!(s.terminal_subscriber_count(), 0);
        // and a subsequent incr still works correctly (no underflow wrap).
        s.incr_terminal_subscribers();
        assert_eq!(s.terminal_subscriber_count(), 1);
    }

    #[test]
    fn terminal_subscriber_count_shared_across_clones() {
        let s = test_state();
        let c = s.clone();
        s.incr_terminal_subscribers();
        assert_eq!(c.terminal_subscriber_count(), 1);
    }

    fn web_settings(
        enabled: bool,
        backend_url: &str,
        runner_token: &str,
    ) -> WebIntegrationSettings {
        WebIntegrationSettings {
            enabled,
            backend_url: backend_url.to_string(),
            runner_token: runner_token.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn from_settings_builds_with_empty_runner_token() {
        // Regression: a Cognito-/pair-code-paired runner has an empty
        // runner_token but must still get a ServerModeState installed so the
        // relay can publish connection status. Previously this returned None.
        let cfg =
            ServerModeConfig::from_settings(&web_settings(true, "https://api.qontinui.io/", ""))
                .expect("should build a config without a runner_token");
        assert_eq!(cfg.web_backend_url, "https://api.qontinui.io");
        assert_eq!(cfg.runner_token, "");
    }

    #[test]
    fn from_settings_still_carries_a_legacy_runner_token() {
        let cfg = ServerModeConfig::from_settings(&web_settings(
            true,
            "https://api.qontinui.io",
            "qontinui_runner_abc",
        ))
        .expect("should build");
        assert_eq!(cfg.runner_token, "qontinui_runner_abc");
    }

    #[test]
    fn from_settings_none_when_disabled_or_no_backend_url() {
        assert!(ServerModeConfig::from_settings(&web_settings(
            false,
            "https://api.qontinui.io",
            ""
        ))
        .is_none());
        assert!(ServerModeConfig::from_settings(&web_settings(true, "   ", "tok")).is_none());
    }

    #[tokio::test]
    async fn install_if_absent_installs_into_empty_slot() {
        // The enabled=false-at-boot edge: boot left the slot empty; a later
        // sign-in enabled integration. On connect the relay self-installs.
        let slot = RwLock::new(None);
        let s = install_if_absent(&slot, &web_settings(true, "https://api.qontinui.io", ""))
            .await
            .expect("valid settings should install");
        assert_eq!(s.config.web_backend_url, "https://api.qontinui.io");
        assert!(
            slot.read().await.is_some(),
            "slot should now hold the state"
        );
    }

    #[tokio::test]
    async fn install_if_absent_does_not_clobber_existing() {
        let slot = RwLock::new(Some(ServerModeState::new(ServerModeConfig {
            web_backend_url: "https://original.test".to_string(),
            runner_token: String::new(),
        })));
        // Different settings must NOT replace the already-installed state.
        let s = install_if_absent(&slot, &web_settings(true, "https://different.test", ""))
            .await
            .expect("existing state should be returned");
        assert_eq!(s.config.web_backend_url, "https://original.test");
        assert_eq!(
            slot.read().await.as_ref().unwrap().config.web_backend_url,
            "https://original.test"
        );
    }

    #[tokio::test]
    async fn install_if_absent_returns_none_for_invalid_settings() {
        let slot = RwLock::new(None);
        // Integration disabled → no valid config → nothing installed.
        assert!(
            install_if_absent(&slot, &web_settings(false, "https://api.qontinui.io", ""))
                .await
                .is_none()
        );
        assert!(slot.read().await.is_none(), "slot must stay empty");
    }
}
