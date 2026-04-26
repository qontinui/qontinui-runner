//! App Registry Module
//!
//! In-memory registry for apps that phone home via
//! `POST /ui-bridge/apps/register`. Replaces the earlier behavior where the
//! register handler echoed a `DiscoveredApp` back without persisting it.
//!
//! Entries are keyed by `appId`. The SDK re-POSTs on a heartbeat (~10s);
//! a background sweeper evicts entries older than `REGISTRATION_TTL_MS`.
//!
//! Phase 1 (wrapper framework): entries now carry an `AppTransport` discriminator.
//! Apps that speak HTTP register as `Http` (the runner reaches them via
//! their `base_url`); apps that speak WebSocket register as `Websocket` and
//! the runner dispatches commands through `CommandRelay` over the
//! `/ui-bridge/ws` connection identified by `websocket_conn_id`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use super::app_discovery::DiscoveredApp;

/// Entries older than this (in ms) are considered stale and evicted.
pub const REGISTRATION_TTL_MS: i64 = 30_000;
const SWEEP_INTERVAL_MS: u64 = 15_000;

/// Transport an app uses to receive commands from the runner.
///
/// - `Http`: runner reaches the app by POSTing to its `base_url`. This is the
///   legacy / same-origin case (desktop apps hosting their own UI Bridge HTTP
///   server, dev servers that proxy runner requests, etc.).
/// - `Websocket`: the app opened a WebSocket to `/ui-bridge/ws` on the runner
///   and is receiving commands on that socket. This is the wrapper-framework
///   case — it works for third-party browser tabs, headless Playwright shims,
///   extensions, and anywhere else the runner cannot reach the app via HTTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AppTransport {
    #[default]
    Http,
    Websocket,
}

#[derive(Debug, Clone)]
pub struct RegisteredApp {
    pub app: DiscoveredApp,
    /// Origin of the phone-home POST (e.g. `http://localhost:9875`), used for
    /// disambiguation in logs when the same appId is served from multiple
    /// hostnames. Display logic assumes one-live-entry-per-appId (last POST wins).
    pub origin: Option<String>,
    pub last_seen_ms: i64,
    /// How the runner delivers commands to this app. Defaults to `Http`.
    pub transport: AppTransport,
    /// If `transport == Websocket`, the `conn_id` of the active
    /// `/ui-bridge/ws` connection. `None` for HTTP apps.
    pub websocket_conn_id: Option<u64>,
}

pub struct AppRegistry {
    inner: RwLock<HashMap<String, RegisteredApp>>,
}

impl AppRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(HashMap::new()),
        })
    }

    /// Insert or refresh an entry. The `transport` + `websocket_conn_id` pair
    /// must be consistent: `Websocket` requires `Some(conn_id)`, `Http`
    /// requires `None`. The caller (register handler / WS relay) is
    /// responsible for this invariant.
    pub async fn upsert(
        &self,
        app: DiscoveredApp,
        origin: Option<String>,
        transport: AppTransport,
        websocket_conn_id: Option<u64>,
    ) {
        let now = chrono::Utc::now().timestamp_millis();
        let mut w = self.inner.write().await;
        w.insert(
            app.app_id.clone(),
            RegisteredApp {
                app,
                origin,
                last_seen_ms: now,
                transport,
                websocket_conn_id,
            },
        );
    }

    pub async fn remove(&self, app_id: &str) -> bool {
        let mut w = self.inner.write().await;
        w.remove(app_id).is_some()
    }

    /// Lightweight liveness refresh — bumps `last_seen_ms` to "now" without
    /// requiring the caller to provide a full `DiscoveredApp`. Intended for
    /// the WS relay's hot path (every inbound frame and every outbound
    /// heartbeat tick) where cloning the `DiscoveredApp` for `upsert` would
    /// be wasteful. Returns `true` if an entry exists for `app_id`.
    pub async fn touch(&self, app_id: &str) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        let mut w = self.inner.write().await;
        if let Some(entry) = w.get_mut(app_id) {
            entry.last_seen_ms = now;
            true
        } else {
            false
        }
    }

    /// Look up an entry by app_id (regardless of freshness). Returns a clone
    /// so callers can inspect the transport without holding the lock.
    pub async fn get(&self, app_id: &str) -> Option<RegisteredApp> {
        let r = self.inner.read().await;
        r.get(app_id).cloned()
    }

    /// Returns entries that haven't been stale-evicted (last_seen_ms within TTL).
    pub async fn list_live(&self) -> Vec<RegisteredApp> {
        let now = chrono::Utc::now().timestamp_millis();
        let r = self.inner.read().await;
        r.values()
            .filter(|e| now - e.last_seen_ms <= REGISTRATION_TTL_MS)
            .cloned()
            .collect()
    }

    /// Evict entries older than TTL. Returns the number of evicted entries.
    pub async fn sweep(&self) -> usize {
        let now = chrono::Utc::now().timestamp_millis();
        let mut w = self.inner.write().await;
        let before = w.len();
        w.retain(|_, e| now - e.last_seen_ms <= REGISTRATION_TTL_MS);
        before - w.len()
    }
}

/// Spawn a background sweeper. Call once from AppHandle setup.
pub fn spawn_sweeper(registry: Arc<AppRegistry>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(SWEEP_INTERVAL_MS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let evicted = registry.sweep().await;
            if evicted > 0 {
                tracing::debug!("[app-registry] evicted {} stale app(s)", evicted);
            }
        }
    });
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_app(app_id: &str) -> DiscoveredApp {
        DiscoveredApp {
            app_id: app_id.to_string(),
            app_name: "Sample".to_string(),
            app_type: "web".to_string(),
            framework: None,
            url: "http://127.0.0.1:3000".to_string(),
            port: 3000,
            base_path: "".to_string(),
            version: None,
            capabilities: vec![],
            element_count: None,
            component_count: None,
            discovered_at: 0,
        }
    }

    #[test]
    fn transport_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&AppTransport::Http).unwrap(),
            "\"http\""
        );
        assert_eq!(
            serde_json::to_string(&AppTransport::Websocket).unwrap(),
            "\"websocket\""
        );
    }

    #[test]
    fn transport_deserializes_lowercase() {
        let http: AppTransport = serde_json::from_str("\"http\"").unwrap();
        let ws: AppTransport = serde_json::from_str("\"websocket\"").unwrap();
        assert_eq!(http, AppTransport::Http);
        assert_eq!(ws, AppTransport::Websocket);
    }

    #[test]
    fn transport_default_is_http() {
        let t: AppTransport = Default::default();
        assert_eq!(t, AppTransport::Http);
    }

    #[tokio::test]
    async fn upsert_http_then_upgrade_to_websocket() {
        let reg = AppRegistry::new();
        reg.upsert(sample_app("a1"), None, AppTransport::Http, None)
            .await;
        let entry = reg.get("a1").await.unwrap();
        assert_eq!(entry.transport, AppTransport::Http);
        assert_eq!(entry.websocket_conn_id, None);

        reg.upsert(sample_app("a1"), None, AppTransport::Websocket, Some(42))
            .await;
        let entry = reg.get("a1").await.unwrap();
        assert_eq!(entry.transport, AppTransport::Websocket);
        assert_eq!(entry.websocket_conn_id, Some(42));
    }

    #[tokio::test]
    async fn remove_returns_true_when_present() {
        let reg = AppRegistry::new();
        reg.upsert(sample_app("a1"), None, AppTransport::Http, None)
            .await;
        assert!(reg.remove("a1").await);
        assert!(!reg.remove("a1").await);
    }

    #[tokio::test]
    async fn list_live_filters_stale() {
        let reg = AppRegistry::new();
        reg.upsert(sample_app("a1"), None, AppTransport::Http, None)
            .await;
        // Manually backdate the entry.
        {
            let mut w = reg.inner.write().await;
            w.get_mut("a1").unwrap().last_seen_ms -= REGISTRATION_TTL_MS + 1;
        }
        assert!(reg.list_live().await.is_empty());
    }

    #[tokio::test]
    async fn touch_refreshes_last_seen_ms() {
        let reg = AppRegistry::new();
        reg.upsert(sample_app("a1"), None, AppTransport::Websocket, Some(1))
            .await;
        // Backdate so the entry is "stale" and would be filtered by list_live().
        {
            let mut w = reg.inner.write().await;
            w.get_mut("a1").unwrap().last_seen_ms -= REGISTRATION_TTL_MS + 1;
        }
        assert!(
            reg.list_live().await.is_empty(),
            "precondition: backdated entry should not be live"
        );

        let refreshed = reg.touch("a1").await;
        assert!(refreshed, "touch must return true for known app_id");

        let live = reg.list_live().await;
        assert_eq!(live.len(), 1, "touched entry should be live again");
        assert_eq!(live[0].app.app_id, "a1");

        // last_seen_ms should now be very close to "now".
        let now = chrono::Utc::now().timestamp_millis();
        assert!(
            now - live[0].last_seen_ms < 1_000,
            "touch should set last_seen_ms close to now (delta={}ms)",
            now - live[0].last_seen_ms
        );
    }

    #[tokio::test]
    async fn touch_returns_false_for_unknown_app() {
        let reg = AppRegistry::new();
        assert!(
            !reg.touch("nope").await,
            "touch on empty registry must return false"
        );
        // Sanity: registry is still empty (touch doesn't create entries).
        assert!(reg.list_live().await.is_empty());
    }
}
