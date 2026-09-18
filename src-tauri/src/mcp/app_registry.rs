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
use super::relay_binding::{
    app_tombstone_key, BindingMode, Principal, Refusal, RelayBinding, RULE_R1, RULE_R2, RULE_R5,
};

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
    /// The origin the registrant DECLARED in its body / frame. Display only —
    /// a browser cannot be trusted about its own origin, so R4 pins this to
    /// the header origin and [`Self::verified_origin`] is what callers read.
    pub declared_origin: Option<String>,
    /// WHO registered this entry. Only this principal (or operator trust) may
    /// displace, refresh or release it (R1, R2, R5).
    pub principal: Principal,
    pub last_seen_ms: i64,
    /// How the runner delivers commands to this app. Defaults to `Http`.
    pub transport: AppTransport,
    /// If `transport == Websocket`, the `conn_id` of the active
    /// `/ui-bridge/ws` connection. `None` for HTTP apps.
    pub websocket_conn_id: Option<u64>,
    /// Optional per-entry TTL override (in ms). When `Some(ms)`, the registry's
    /// freshness check compares against `ms` instead of the global
    /// `REGISTRATION_TTL_MS`. Useful for tests, scripts, and synthetic
    /// injections that need entries to stay alive longer than the default
    /// 30-second heartbeat window without sending heartbeats. `None` means
    /// "use the global default."
    pub keep_alive_ms: Option<i64>,
}

impl RegisteredApp {
    /// The header origin the runner VERIFIED this registrant at. `None` for
    /// operator trust (which sends none) and for a keyed tab.
    pub fn verified_origin(&self) -> Option<String> {
        self.principal.verified_origin()
    }

    /// The requester class the registration was admitted under.
    pub fn principal_class(&self) -> &'static str {
        self.principal.class_str()
    }

    /// Within its TTL (per-entry `keep_alive_ms`, else the global default) —
    /// exactly the predicate `list_live` and `sweep` use, so the registry
    /// never refuses a claim on an entry it does not serve.
    fn is_live(&self, now: i64) -> bool {
        now - self.last_seen_ms <= self.keep_alive_ms.unwrap_or(REGISTRATION_TTL_MS)
    }
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

    /// Claim `app.app_id` for `principal`, then insert or refresh the entry —
    /// the CHECK and the WRITE under ONE write-lock acquisition, so two
    /// concurrent registrations can never both pass against the same prior
    /// holder (plan `2026-09-17-ui-bridge-relay-registration-is-unauthenticated`,
    /// "Holding a claim is atomic").
    ///
    /// Refuses with `UIB_REGISTRATION_HELD` when a LIVE entry, or a live
    /// tombstone, belongs to a principal this one may not displace (R1, R5).
    ///
    /// The `transport` + `websocket_conn_id` pair must be consistent:
    /// `Websocket` requires `Some(conn_id)`, `Http` requires `None`. The
    /// caller (register handler / WS relay) is responsible for that invariant.
    #[allow(clippy::too_many_arguments)]
    pub async fn claim(
        &self,
        binding: &RelayBinding,
        principal: &Principal,
        route: &'static str,
        app: DiscoveredApp,
        declared_origin: Option<String>,
        transport: AppTransport,
        websocket_conn_id: Option<u64>,
        keep_alive_ms: Option<i64>,
    ) -> Result<(), Refusal> {
        let mode = binding.config.binding;
        let now = chrono::Utc::now().timestamp_millis();
        let key = app_tombstone_key(&app.app_id);
        let mut w = self.inner.write().await;

        let mut keep_alive_ms = keep_alive_ms;
        if mode != BindingMode::Off {
            match w.get(&app.app_id) {
                // R1: a live holder is displaced only by itself or operator trust.
                Some(existing) if existing.is_live(now) => {
                    if !principal.may_displace(&existing.principal) {
                        binding.meter(
                            mode,
                            principal,
                            route,
                            Refusal::registration_held(RULE_R1),
                        )?;
                    }
                }
                // R5: no live entry, but the id may still be tombstoned for
                // the principal that just released it (the reload race).
                _ => {
                    if let Some(prev) = binding.tombstone_holder(&key) {
                        if !principal.may_displace(&prev) {
                            binding.meter(
                                mode,
                                principal,
                                route,
                                Refusal::registration_held(RULE_R5),
                            )?;
                        }
                    }
                }
            }
            // R5's second arm: a browser principal may not park an entry past
            // the heartbeat window. Applied only under `enforce` — in `shadow`
            // no rule may change behaviour.
            if mode == BindingMode::Enforce && !principal.is_operator_trust() {
                keep_alive_ms = keep_alive_ms.map(|ms| ms.min(REGISTRATION_TTL_MS));
            }
        }

        binding.clear_tombstone(&key);
        w.insert(
            app.app_id.clone(),
            RegisteredApp {
                app,
                declared_origin,
                principal: principal.clone(),
                last_seen_ms: now,
                transport,
                websocket_conn_id,
                keep_alive_ms,
            },
        );
        Ok(())
    }

    /// Release `app_id` (R2).
    ///
    /// - `conn_guard: Some(c)` removes the entry only while `c` still owns it,
    ///   so a DISPLACED WebSocket's teardown cannot delete the connection that
    ///   displaced it. `None` is the HTTP `DELETE` path, which has no conn.
    /// - A caller that is neither the holder principal nor operator trust is
    ///   refused with `UIB_REGISTRATION_HELD`.
    /// - A browser principal's released id is tombstoned for
    ///   `BINDING_TOMBSTONE_MS`.
    ///
    /// `Ok(false)` means "nothing to remove", which includes the conn-guard
    /// miss: that is not a refusal, it is a stale teardown.
    pub async fn release(
        &self,
        binding: &RelayBinding,
        app_id: &str,
        principal: &Principal,
        conn_guard: Option<u64>,
        route: &'static str,
    ) -> Result<bool, Refusal> {
        let mode = binding.config.binding;
        let mut w = self.inner.write().await;
        let Some(entry) = w.get(app_id) else {
            return Ok(false);
        };
        if let Some(conn_id) = conn_guard {
            if entry.websocket_conn_id != Some(conn_id) {
                return Ok(false);
            }
        }
        if mode != BindingMode::Off && !principal.may_displace(&entry.principal) {
            binding.meter(mode, principal, route, Refusal::registration_held(RULE_R2))?;
        }
        let removed = w.remove(app_id);
        if mode != BindingMode::Off {
            if let Some(entry) = removed.as_ref() {
                binding.tombstone(app_tombstone_key(app_id), &entry.principal);
            }
        }
        Ok(removed.is_some())
    }

    /// Lightweight liveness refresh — bumps `last_seen_ms` to "now" without
    /// requiring the caller to provide a full `DiscoveredApp`. Intended for
    /// the WS relay's hot path (every inbound frame and every outbound
    /// heartbeat tick) where cloning the `DiscoveredApp` for `claim` would
    /// be wasteful.
    ///
    /// `conn_guard: Some(c)` refreshes only while `c` still owns the entry
    /// (R2's liveness arm): a DISPLACED socket's frames and heartbeat ticks
    /// must not keep the holder's registration alive. `None` skips that check
    /// — the kill-switch-`off` path.
    ///
    /// Returns `true` if an entry was refreshed.
    pub async fn touch(&self, app_id: &str, conn_guard: Option<u64>) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        let mut w = self.inner.write().await;
        if let Some(entry) = w.get_mut(app_id) {
            if let Some(conn_id) = conn_guard {
                if entry.websocket_conn_id != Some(conn_id) {
                    return false;
                }
            }
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

    /// Returns entries that haven't been stale-evicted (last_seen_ms within
    /// each entry's TTL — per-entry `keep_alive_ms` if set, else
    /// `REGISTRATION_TTL_MS`).
    pub async fn list_live(&self) -> Vec<RegisteredApp> {
        let now = chrono::Utc::now().timestamp_millis();
        let r = self.inner.read().await;
        r.values()
            .filter(|e| now - e.last_seen_ms <= e.keep_alive_ms.unwrap_or(REGISTRATION_TTL_MS))
            .cloned()
            .collect()
    }

    /// Evict entries older than each entry's TTL (per-entry `keep_alive_ms`
    /// if set, else `REGISTRATION_TTL_MS`). Returns the number of evicted
    /// entries.
    pub async fn sweep(&self) -> usize {
        let now = chrono::Utc::now().timestamp_millis();
        let mut w = self.inner.write().await;
        let before = w.len();
        w.retain(|_, e| now - e.last_seen_ms <= e.keep_alive_ms.unwrap_or(REGISTRATION_TTL_MS));
        before - w.len()
    }

    /// Test-only shorthand for the pre-binding `upsert`: an operator-trust
    /// claim under a default [`crate::mcp::relay_binding::BindingConfig`],
    /// which every rule admits. Sibling modules' tests register fixture apps
    /// by the dozen and none of them is about the binding — the rules
    /// themselves are exercised in `relay_binding/tests.rs`, over a real
    /// socket, behind the real origin guard. Production has exactly ONE write
    /// path into this map, [`Self::claim`].
    #[cfg(test)]
    pub async fn upsert(
        &self,
        app: DiscoveredApp,
        declared_origin: Option<String>,
        transport: AppTransport,
        websocket_conn_id: Option<u64>,
        keep_alive_ms: Option<i64>,
    ) {
        let binding = RelayBinding::new(crate::mcp::relay_binding::BindingConfig::default());
        let principal = Principal::OperatorTrust {
            class: crate::mcp::origin_guard::OriginClass::NonBrowser,
        };
        self.claim(
            &binding,
            &principal,
            "test",
            app,
            declared_origin,
            transport,
            websocket_conn_id,
            keep_alive_ms,
        )
        .await
        .expect("an operator-trust claim is always admitted");
    }

    /// Test-only shorthand for the pre-binding `remove`. See [`Self::upsert`].
    #[cfg(test)]
    pub async fn remove(&self, app_id: &str) -> bool {
        let binding = RelayBinding::new(crate::mcp::relay_binding::BindingConfig::default());
        let principal = Principal::OperatorTrust {
            class: crate::mcp::origin_guard::OriginClass::NonBrowser,
        };
        self.release(&binding, app_id, &principal, None, "test")
            .await
            .expect("an operator-trust release is always admitted")
    }

    /// Test-only helper that subtracts `delta_ms` from an entry's
    /// `last_seen_ms` to simulate the passage of time. Returns `true` if the
    /// entry exists. Used by integration tests in sibling modules
    /// (`app_discovery`) to drive freshness math without sleeping.
    #[cfg(test)]
    pub async fn test_age_entry(&self, app_id: &str, delta_ms: i64) -> bool {
        let mut w = self.inner.write().await;
        if let Some(entry) = w.get_mut(app_id) {
            entry.last_seen_ms -= delta_ms;
            true
        } else {
            false
        }
    }
}

/// Spawn a background sweeper. Call once from AppHandle setup.
///
/// The same tick sweeps the binding's tombstone map (R5), so the bounded
/// map needs no timer of its own.
pub fn spawn_sweeper(registry: Arc<AppRegistry>, binding: Arc<RelayBinding>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(SWEEP_INTERVAL_MS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let evicted = registry.sweep().await;
            if evicted > 0 {
                tracing::debug!("[app-registry] evicted {} stale app(s)", evicted);
            }
            let expired = binding.sweep_tombstones();
            if expired > 0 {
                tracing::debug!("[app-registry] expired {} binding tombstone(s)", expired);
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
        reg.upsert(sample_app("a1"), None, AppTransport::Http, None, None)
            .await;
        let entry = reg.get("a1").await.unwrap();
        assert_eq!(entry.transport, AppTransport::Http);
        assert_eq!(entry.websocket_conn_id, None);

        reg.upsert(
            sample_app("a1"),
            None,
            AppTransport::Websocket,
            Some(42),
            None,
        )
        .await;
        let entry = reg.get("a1").await.unwrap();
        assert_eq!(entry.transport, AppTransport::Websocket);
        assert_eq!(entry.websocket_conn_id, Some(42));
    }

    #[tokio::test]
    async fn remove_returns_true_when_present() {
        let reg = AppRegistry::new();
        reg.upsert(sample_app("a1"), None, AppTransport::Http, None, None)
            .await;
        assert!(reg.remove("a1").await);
        assert!(!reg.remove("a1").await);
    }

    #[tokio::test]
    async fn list_live_filters_stale() {
        let reg = AppRegistry::new();
        reg.upsert(sample_app("a1"), None, AppTransport::Http, None, None)
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
        reg.upsert(
            sample_app("a1"),
            None,
            AppTransport::Websocket,
            Some(1),
            None,
        )
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

        let refreshed = reg.touch("a1", Some(1)).await;
        assert!(refreshed, "touch must return true for the owning conn");
        assert!(
            !reg.touch("a1", Some(2)).await,
            "R2: a displaced conn's touch must NOT refresh the holder"
        );

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
            !reg.touch("nope", None).await,
            "touch on empty registry must return false"
        );
        // Sanity: registry is still empty (touch doesn't create entries).
        assert!(reg.list_live().await.is_empty());
    }

    #[tokio::test]
    async fn keep_alive_extends_freshness_beyond_default_ttl() {
        let reg = AppRegistry::new();
        // Register with a 5-minute keep-alive (300_000ms). An operator-trust
        // registrant is NOT capped by R5 — only a browser principal is.
        reg.upsert(
            sample_app("long-lived"),
            None,
            AppTransport::Http,
            None,
            Some(300_000),
        )
        .await;

        // Advance "synthetic time" past the global 30s TTL but well within
        // the per-entry 5-minute override. The entry MUST still be live.
        {
            let mut w = reg.inner.write().await;
            w.get_mut("long-lived").unwrap().last_seen_ms -= REGISTRATION_TTL_MS + 5_000;
        }

        let live = reg.list_live().await;
        assert_eq!(
            live.len(),
            1,
            "entry with 5min keep_alive must still be live after default TTL elapses"
        );
        assert_eq!(live[0].app.app_id, "long-lived");
        assert_eq!(live[0].keep_alive_ms, Some(300_000));

        // Sweep must NOT evict the entry either.
        let evicted = reg.sweep().await;
        assert_eq!(evicted, 0, "sweep must respect per-entry keep_alive_ms");
        assert!(reg.get("long-lived").await.is_some());
    }

    #[tokio::test]
    async fn keep_alive_still_evicted_once_its_own_window_elapses() {
        let reg = AppRegistry::new();
        // Short 2s keep-alive.
        reg.upsert(
            sample_app("brief"),
            None,
            AppTransport::Http,
            None,
            Some(2_000),
        )
        .await;

        // Push past 2s.
        {
            let mut w = reg.inner.write().await;
            w.get_mut("brief").unwrap().last_seen_ms -= 2_500;
        }

        assert!(
            reg.list_live().await.is_empty(),
            "entry past its own keep_alive_ms must be filtered"
        );
        let evicted = reg.sweep().await;
        assert_eq!(evicted, 1, "sweep must evict per-entry-stale entries");
    }

    #[tokio::test]
    async fn keep_alive_none_uses_global_default() {
        let reg = AppRegistry::new();
        reg.upsert(sample_app("default"), None, AppTransport::Http, None, None)
            .await;

        // Backdate past the global TTL.
        {
            let mut w = reg.inner.write().await;
            w.get_mut("default").unwrap().last_seen_ms -= REGISTRATION_TTL_MS + 1;
        }

        assert!(
            reg.list_live().await.is_empty(),
            "None keep_alive_ms must fall through to global default"
        );
    }
}
