//! App Registry Module
//!
//! In-memory registry for apps that phone home via
//! `POST /ui-bridge/apps/register`. Replaces the earlier behavior where the
//! register handler echoed a `DiscoveredApp` back without persisting it.
//!
//! Entries are keyed by `appId`. The SDK re-POSTs on a heartbeat (~10s);
//! a background sweeper evicts entries older than `REGISTRATION_TTL_MS`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use super::app_discovery::DiscoveredApp;

/// Entries older than this (in ms) are considered stale and evicted.
pub const REGISTRATION_TTL_MS: i64 = 30_000;
const SWEEP_INTERVAL_MS: u64 = 15_000;

#[derive(Debug, Clone)]
pub struct RegisteredApp {
    pub app: DiscoveredApp,
    /// Origin of the phone-home POST (e.g. `http://localhost:9875`), used for
    /// disambiguation in logs when the same appId is served from multiple
    /// hostnames. Display logic assumes one-live-entry-per-appId (last POST wins).
    pub origin: Option<String>,
    pub last_seen_ms: i64,
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

    pub async fn upsert(&self, app: DiscoveredApp, origin: Option<String>) {
        let now = chrono::Utc::now().timestamp_millis();
        let mut w = self.inner.write().await;
        w.insert(
            app.app_id.clone(),
            RegisteredApp {
                app,
                origin,
                last_seen_ms: now,
            },
        );
    }

    pub async fn remove(&self, app_id: &str) -> bool {
        let mut w = self.inner.write().await;
        w.remove(app_id).is_some()
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
