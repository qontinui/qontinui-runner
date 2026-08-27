//! Server-side relay ROUTABILITY read-back — the other half of `ws_connected`.
//!
//! # Why this exists
//!
//! [`crate::mcp::backend_relay::WebIntegrationStatus::ws_connected`] reflects
//! the runner's LOCAL socket state only: "my WebSocket is open and I am
//! heartbeating on it". That is not the same question as "will the server
//! actually route relay traffic to me", and during a live production outage
//! the two disagreed in exactly the direction that hides the fault:
//! `coord.devices.ws_session_id` had been NULLed while the socket stayed up
//! and kept heartbeating, so `/web-integration/status` reported
//! `ws_connected: true, last_error: null` while every mobile cloud-relay call
//! 503'd (`POST /api/v1/devices/{id}/dispatch` gates on
//! `ws_session_id IS NOT NULL`). An operator staring at the status endpoint
//! saw green through a total relay outage; that blind spot is why the fault
//! went unnoticed for hours.
//!
//! So `relay_routable` is surfaced BESIDE `ws_connected`, never in place of
//! it. The two mean different things and both are worth seeing:
//!
//! | field | question it answers | source of truth |
//! |---|---|---|
//! | `ws_connected` | is my socket up? | this process |
//! | `relay_routable` | will the server route to me? | the server's device row |
//!
//! # How it is derived
//!
//! A background task ([`poll_loop`], spawned by
//! [`crate::mcp::backend_relay::start_relay`]) reads back
//! `GET {backend}/api/v1/devices/{device_id}` every
//! [`POLL_INTERVAL`] and takes the response's `wsConnected` field — which the
//! web backend computes as `ws_session_id is not None`, i.e. the SAME column
//! the dispatch route gates on. The result is cached; the HTTP handler only
//! ever reads the cache, because `/web-integration/status` is polled by
//! dashboards and must not fan out one upstream request per poll.
//!
//! # `unknown` is not `false`
//!
//! A read-back that fails — network error, non-200, a 200 whose body carries
//! no `wsConnected` — leaves `relay_routable` as `null` (UNKNOWN) with
//! [`RelayRoutableSnapshot::relay_routable_error`] naming the fault. It is
//! never reported as `false`. Absence of evidence is not evidence of absence
//! (served policy `verification-and-evidence` `silent-empty-is-unknown`);
//! reporting `false` on a failed probe would swap one lie (green through an
//! outage) for another (red through a credential hiccup).
//!
//! For the same reason a SUCCESSFUL value does not live forever: past
//! [`MAX_AGE`] the cached value decays back to `null`, because a `true` from
//! twenty minutes ago is a claim about the past, not evidence about now.
//! `relay_routable_checked_at_ms` is emitted regardless so a stale value is
//! visibly stale rather than silently stale.
//!
//! # Credential
//!
//! `GET /api/v1/devices/{id}` is gated on `get_current_active_user_async`
//! (a Cognito USER session) — it 401s a coord device-JWT, verified against
//! production on 2026-08-27. So the bearer cascade mirrors the one
//! `mcp::sdk_client` already uses: prefer a fresh Cognito bearer
//! ([`crate::mcp::device_jwt_refresher::ensure_fresh_cognito_bearer`]), fall
//! back to the device-JWT. On a headless/fleet runner with no Cognito session
//! the fallback 401s, and the honest answer there is `unknown` with the 401
//! recorded — not a fabricated `true`, and not a `false`.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use once_cell::sync::Lazy;
use tokio::sync::watch;
use tracing::{debug, warn};

/// How often the background task reads back the server's view of this
/// device. Sized against the brief's ~30–60s window: frequent enough that a
/// routing outage surfaces within a dashboard refresh or two, cheap enough
/// that it is one request per runner per minute-ish.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(45);

/// How long a SUCCESSFUL read-back stays authoritative before it decays to
/// UNKNOWN. Four poll intervals: long enough to ride out a couple of
/// transient failures without flapping the field, short enough that a value
/// reported as `true` was observed within the last three minutes.
pub(crate) const MAX_AGE: Duration = Duration::from_secs(180);

/// Timeout for one read-back request. The poller must never outlive its own
/// interval, or a hung backend would stall the loop instead of recording a
/// fault.
const READBACK_TIMEOUT: Duration = Duration::from_secs(10);

/// The outcome of one read-back attempt.
///
/// Deliberately three-valued. `Unknown` is what every FAILURE maps to; only a
/// 200 that actually carries `wsConnected` produces `Routable` /
/// `NotRoutable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Readback {
    /// The server holds an open WS session for this device — relay traffic
    /// will be routed.
    Routable,
    /// The server holds NO WS session for this device — relay calls will
    /// 503. This is the outage signature the incident had no way to show.
    NotRoutable,
    /// The read-back could not establish either, and why.
    Unknown(String),
}

/// What `/web-integration/status` reports for the routability half.
///
/// `relay_routable == None` means UNKNOWN in every case — never "not
/// routable". Pair it with [`Self::relay_routable_error`] to see why.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct RelayRoutableSnapshot {
    /// Whether the SERVER will route relay traffic to this device.
    ///
    /// `Some(true)` / `Some(false)` come only from a successful read-back
    /// within [`MAX_AGE`]. `None` is UNKNOWN — a failed read-back, no
    /// read-back yet, or a successful one that has since gone stale.
    pub relay_routable: Option<bool>,
    /// Unix epoch milliseconds of the last SUCCESSFUL read-back, whether or
    /// not that read-back is still within [`MAX_AGE`]. Emitted even once the
    /// value has decayed to `None`, so a stale reading is visibly stale
    /// instead of silently absent.
    pub relay_routable_checked_at_ms: Option<u64>,
    /// Why the freshest attempt did not yield a value: the fault from the
    /// most recent FAILED read-back, or the staleness note when a past
    /// success has aged out. `None` when the reported value is fresh.
    pub relay_routable_error: Option<String>,
}

/// The poller's accumulated state. Split out from the global so the
/// staleness / unknown-semantics decisions are exercisable without a clock,
/// a network, or a runner.
#[derive(Debug, Default)]
pub(crate) struct RoutableState {
    /// `(value, unix_ms, monotonic instant)` of the last SUCCESSFUL
    /// read-back. The wall-clock stamp is what operators read; the monotonic
    /// instant is what staleness is measured with (a wall-clock jump must not
    /// resurrect or condemn a reading).
    last_success: Option<(bool, u64, Instant)>,
    /// The fault from the most recent attempt, when that attempt FAILED.
    /// Cleared by a success, so it never explains away a fresh value.
    last_error: Option<String>,
}

impl RoutableState {
    /// Fold one read-back outcome into the state.
    pub(crate) fn record(&mut self, outcome: Readback, now: Instant, now_ms: u64) {
        match outcome {
            Readback::Routable => {
                self.last_success = Some((true, now_ms, now));
                self.last_error = None;
            }
            Readback::NotRoutable => {
                self.last_success = Some((false, now_ms, now));
                self.last_error = None;
            }
            // A failed attempt does NOT overwrite the last known-good value —
            // it only records why we could not refresh it. Whether the old
            // value still counts is `snapshot`'s call (via `MAX_AGE`), so a
            // brief blip degrades gracefully instead of erasing the reading.
            Readback::Unknown(reason) => self.last_error = Some(reason),
        }
    }

    /// Render the wire snapshot as of `now`.
    pub(crate) fn snapshot(&self, now: Instant, max_age: Duration) -> RelayRoutableSnapshot {
        match self.last_success {
            Some((value, at_ms, at)) => {
                let age = now.saturating_duration_since(at);
                if age <= max_age {
                    RelayRoutableSnapshot {
                        relay_routable: Some(value),
                        relay_routable_checked_at_ms: Some(at_ms),
                        // A fresh value needs no excuse; a failure recorded
                        // AFTER the last success is impossible here because
                        // `record` clears `last_error` on success and only a
                        // later failure could set it — that failure is
                        // reported below once the value ages out, and until
                        // then the value is still within its stated validity.
                        relay_routable_error: self.last_error.clone(),
                    }
                } else {
                    RelayRoutableSnapshot {
                        relay_routable: None,
                        relay_routable_checked_at_ms: Some(at_ms),
                        relay_routable_error: Some(match &self.last_error {
                            Some(e) => format!(
                                "last successful read-back is {}s old (max {}s) — routability is \
                                 UNKNOWN, not false; latest attempt failed: {e}",
                                age.as_secs(),
                                max_age.as_secs(),
                            ),
                            None => format!(
                                "last successful read-back is {}s old (max {}s) — routability is \
                                 UNKNOWN, not false",
                                age.as_secs(),
                                max_age.as_secs(),
                            ),
                        }),
                    }
                }
            }
            None => RelayRoutableSnapshot {
                relay_routable: None,
                relay_routable_checked_at_ms: None,
                relay_routable_error: Some(
                    self.last_error
                        .clone()
                        .unwrap_or_else(|| "no read-back has completed yet".to_string()),
                ),
            },
        }
    }
}

static STATE: Lazy<std::sync::RwLock<RoutableState>> =
    Lazy::new(|| std::sync::RwLock::new(RoutableState::default()));

/// Current unix epoch milliseconds.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// The cached routability snapshot. This is the ONLY thing the status
/// handler calls — it never issues a read-back, so a dashboard hammering
/// `/web-integration/status` costs nothing upstream.
pub(crate) fn snapshot() -> RelayRoutableSnapshot {
    match STATE.read() {
        Ok(s) => s.snapshot(Instant::now(), MAX_AGE),
        // A poisoned lock means the poller panicked mid-write. That is a
        // failed read-back like any other: UNKNOWN with the reason, never a
        // fabricated value.
        Err(_) => RelayRoutableSnapshot {
            relay_routable: None,
            relay_routable_checked_at_ms: None,
            relay_routable_error: Some(
                "routability cache lock is poisoned (the poller panicked) — UNKNOWN".to_string(),
            ),
        },
    }
}

/// Fold an outcome into the global cache.
fn record(outcome: Readback) {
    match STATE.write() {
        Ok(mut s) => s.record(outcome, Instant::now(), now_ms()),
        Err(_) => warn!("relay_routable: cache lock poisoned; dropping read-back result"),
    }
}

/// Classify a read-back response. Pure — the whole point is that the
/// `unknown`-vs-`false` decision is testable without a server.
///
/// Only a 200 whose body carries a boolean `wsConnected` yields a verdict.
/// Everything else — including a 404 ("no such owned device"), which is
/// tempting to read as `false` — is UNKNOWN: a status code says something
/// about the REQUEST, and inferring device state from it is how a
/// misconfigured `backend_url` would come to report a healthy runner as
/// unroutable.
pub(crate) fn classify(status: u16, body: &str) -> Readback {
    if status != 200 {
        let excerpt: String = body.chars().take(200).collect();
        return Readback::Unknown(format!(
            "read-back GET /api/v1/devices/{{id}} returned HTTP {status}: {excerpt}"
        ));
    }
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return Readback::Unknown(format!("read-back 200 but body is not JSON: {e}"));
        }
    };
    match parsed.get("wsConnected").and_then(|v| v.as_bool()) {
        Some(true) => Readback::Routable,
        Some(false) => Readback::NotRoutable,
        None => Readback::Unknown(
            "read-back 200 but response carries no boolean `wsConnected` field".to_string(),
        ),
    }
}

/// Resolve the bearer for the read-back, preferring the Cognito user session
/// the route actually accepts. See the module docs on why the device-JWT is
/// only a fallback.
async fn readback_bearer(auth_manager: &crate::auth::AuthManager) -> Option<String> {
    if let Some(t) =
        crate::mcp::device_jwt_refresher::ensure_fresh_cognito_bearer(auth_manager).await
    {
        if !t.trim().is_empty() {
            return Some(t);
        }
    }
    match auth_manager.get_access_token() {
        Ok(t) if crate::auth::looks_like_jwt(&t) && !crate::auth::jwt_is_expired(&t) => Some(t),
        _ => None,
    }
}

/// Perform one read-back attempt and classify it.
async fn read_back_once() -> Readback {
    let loaded = crate::settings::load_settings_full();
    // A relay that is deliberately idle (local tier, integration switched
    // off) has no server-side routing to report. Say so explicitly rather
    // than issuing a doomed request or, worse, leaving a stale value in
    // place — but still as UNKNOWN, because "not applicable" is not "not
    // routable".
    if loaded.settings.tier != crate::settings::RunnerTier::QontinuiAccount {
        return Readback::Unknown(format!(
            "relay is idle: tier is {:?}, not qontinui_account — no server-side routing expected",
            loaded.settings.tier
        ));
    }
    if !loaded.settings.web_integration.enabled {
        return Readback::Unknown(
            "relay is idle: web_integration.enabled is false — no server-side routing expected"
                .to_string(),
        );
    }

    let auth_manager = crate::auth::AuthManager::new();
    let device_id = match auth_manager.get_device_id() {
        Ok(id) if !id.trim().is_empty() => id.trim().to_string(),
        Ok(_) => return Readback::Unknown("device id is empty — cannot read back".to_string()),
        Err(e) => return Readback::Unknown(format!("device id unreadable: {e}")),
    };
    let Some(bearer) = readback_bearer(&auth_manager).await else {
        return Readback::Unknown(
            "no usable bearer for the read-back (no Cognito session, no unexpired device-JWT)"
                .to_string(),
        );
    };

    let base = crate::api_config::get_api_base_url();
    read_back_at(&base, &device_id, &bearer).await
}

/// The HTTP leg, with every input passed in. Split from
/// [`read_back_once`] — which resolves them from settings, the credential
/// store and the api-config precedence chain — so the wire behaviour is
/// exercisable against an ephemeral server, the same idiom
/// `backend_relay::relay_http_to_base` uses.
async fn read_back_at(base: &str, device_id: &str, bearer: &str) -> Readback {
    let url = format!("{base}/api/v1/devices/{device_id}");
    let client = match reqwest::Client::builder().timeout(READBACK_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => return Readback::Unknown(format!("read-back client build failed: {e}")),
    };
    // coord-auth-exempt(not-coord): `qontinui-web`'s device registry route,
    // authenticated by this operator's own Cognito session (device-JWT
    // fallback).
    let resp = match client.get(&url).bearer_auth(bearer).send().await {
        Ok(r) => r,
        Err(e) => return Readback::Unknown(format!("read-back request to {url} failed: {e}")),
    };
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    classify(status, &body)
}

/// Background read-back loop. Ticks every [`POLL_INTERVAL`] and folds each
/// outcome into the cache the status handler reads.
pub(crate) async fn poll_loop(mut shutdown_rx: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(POLL_INTERVAL);
    // Skip missed ticks rather than replaying them: a suspended laptop must
    // not wake up and fire a burst of read-backs.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    debug!("relay_routable: shutting down");
                    return;
                }
                continue;
            }
        }
        if *shutdown_rx.borrow() {
            return;
        }
        let outcome = read_back_once().await;
        match &outcome {
            Readback::NotRoutable => warn!(
                "relay_routable: the SERVER holds no WS session for this device — relay calls \
                 will 503 even though the local socket may be up"
            ),
            Readback::Unknown(reason) => {
                debug!("relay_routable: read-back UNKNOWN ({reason})")
            }
            Readback::Routable => debug!("relay_routable: server will route to this device"),
        }
        record(outcome);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn classify_200_ws_connected_true_is_routable() {
        assert_eq!(
            classify(200, r#"{"id":"x","wsConnected":true}"#),
            Readback::Routable
        );
    }

    /// The incident's exact signature: the socket is up locally, but the
    /// server's `ws_session_id` is NULL so `wsConnected` is false and every
    /// relay dispatch 503s.
    #[test]
    fn classify_200_ws_connected_false_is_not_routable() {
        assert_eq!(
            classify(200, r#"{"id":"x","wsConnected":false}"#),
            Readback::NotRoutable
        );
    }

    #[test]
    fn classify_non_200_is_unknown_never_false() {
        for status in [401u16, 403, 404, 500, 502, 503] {
            match classify(status, "{}") {
                Readback::Unknown(r) => assert!(
                    r.contains(&status.to_string()),
                    "reason should name the status: {r}"
                ),
                other => panic!("HTTP {status} must be UNKNOWN, got {other:?}"),
            }
        }
    }

    #[test]
    fn classify_200_without_field_is_unknown() {
        assert!(matches!(
            classify(200, r#"{"id":"x"}"#),
            Readback::Unknown(_)
        ));
        assert!(matches!(classify(200, "not json"), Readback::Unknown(_)));
        // A non-boolean `wsConnected` is a contract change, not a `false`.
        assert!(matches!(
            classify(200, r#"{"wsConnected":"yes"}"#),
            Readback::Unknown(_)
        ));
    }

    #[test]
    fn no_readback_yet_is_unknown_with_a_reason() {
        let s = RoutableState::default();
        let snap = s.snapshot(Instant::now(), MAX_AGE);
        assert_eq!(snap.relay_routable, None);
        assert_eq!(snap.relay_routable_checked_at_ms, None);
        assert!(snap.relay_routable_error.is_some());
    }

    #[test]
    fn a_failed_readback_is_unknown_not_false() {
        let mut s = RoutableState::default();
        let now = Instant::now();
        s.record(Readback::Unknown("HTTP 401".into()), now, 1_000);
        let snap = s.snapshot(now, MAX_AGE);
        assert_eq!(
            snap.relay_routable, None,
            "a failed read-back must never read as `false`"
        );
        assert_eq!(snap.relay_routable_error.as_deref(), Some("HTTP 401"));
    }

    #[test]
    fn success_is_reported_with_its_timestamp() {
        let mut s = RoutableState::default();
        let now = Instant::now();
        s.record(Readback::Routable, now, 1_700_000_000_000);
        let snap = s.snapshot(now, MAX_AGE);
        assert_eq!(snap.relay_routable, Some(true));
        assert_eq!(snap.relay_routable_checked_at_ms, Some(1_700_000_000_000));
        assert_eq!(snap.relay_routable_error, None);

        s.record(Readback::NotRoutable, now, 1_700_000_045_000);
        let snap = s.snapshot(now, MAX_AGE);
        assert_eq!(snap.relay_routable, Some(false));
        assert_eq!(snap.relay_routable_checked_at_ms, Some(1_700_000_045_000));
    }

    /// A blip must not erase a good reading, but the failure is still
    /// surfaced so the operator knows the value is not being refreshed.
    #[test]
    fn a_failure_after_a_success_keeps_the_value_and_names_the_fault() {
        let mut s = RoutableState::default();
        let now = Instant::now();
        s.record(Readback::Routable, now, 1_000);
        s.record(Readback::Unknown("connect timeout".into()), now, 2_000);
        let snap = s.snapshot(now, MAX_AGE);
        assert_eq!(snap.relay_routable, Some(true));
        assert_eq!(snap.relay_routable_checked_at_ms, Some(1_000));
        assert_eq!(
            snap.relay_routable_error.as_deref(),
            Some("connect timeout")
        );
    }

    /// A `true` from long ago is a claim about the past. Past `MAX_AGE` it
    /// decays to UNKNOWN — but the timestamp stays, so the staleness is
    /// visible rather than silent.
    #[test]
    fn a_stale_success_decays_to_unknown_and_keeps_its_timestamp() {
        let mut s = RoutableState::default();
        let then = Instant::now();
        s.record(Readback::Routable, then, 1_700_000_000_000);
        let later = then + MAX_AGE + Duration::from_secs(1);
        let snap = s.snapshot(later, MAX_AGE);
        assert_eq!(
            snap.relay_routable, None,
            "a value older than MAX_AGE is UNKNOWN, not still-true"
        );
        assert_eq!(snap.relay_routable_checked_at_ms, Some(1_700_000_000_000));
        let reason = snap.relay_routable_error.expect("staleness must be named");
        assert!(reason.contains("UNKNOWN"), "{reason}");
    }

    /// Staleness must not silently flip a `false` either — the decayed state
    /// is UNKNOWN in both directions.
    #[test]
    fn a_stale_not_routable_also_decays_to_unknown() {
        let mut s = RoutableState::default();
        let then = Instant::now();
        s.record(Readback::NotRoutable, then, 42);
        let snap = s.snapshot(then + MAX_AGE + Duration::from_secs(1), MAX_AGE);
        assert_eq!(snap.relay_routable, None);
        assert_eq!(snap.relay_routable_checked_at_ms, Some(42));
    }

    // ------------------------------------------------------------------
    // Wire behaviour, against a real ephemeral server. `classify` covers the
    // decision; these cover the leg around it — that the bearer is actually
    // attached, that the device id lands in the path, and that a dead
    // backend degrades to UNKNOWN rather than to `false`.
    // ------------------------------------------------------------------

    /// Serve one canned `GET /api/v1/devices/{id}` response and capture the
    /// Authorization header the read-back sent. Returns `(base_url, seen_auth)`.
    async fn spawn_device_route(
        body: &'static str,
        status: axum::http::StatusCode,
    ) -> (String, std::sync::Arc<Mutex<Option<String>>>) {
        use axum::{extract::Path, http::HeaderMap, routing::get, Router};
        let seen: std::sync::Arc<Mutex<Option<String>>> = std::sync::Arc::new(Mutex::new(None));
        let captured = seen.clone();
        let app = Router::new().route(
            "/api/v1/devices/{id}",
            get(move |Path(id): Path<String>, headers: HeaderMap| {
                let captured = captured.clone();
                async move {
                    *captured.lock().unwrap() = Some(format!(
                        "{}|{}",
                        id,
                        headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("<none>")
                    ));
                    (status, body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), seen)
    }

    #[tokio::test]
    async fn read_back_reports_the_servers_answer_and_sends_the_bearer() {
        let (base, seen) = spawn_device_route(
            r#"{"id":"d","wsConnected":true}"#,
            axum::http::StatusCode::OK,
        )
        .await;
        assert_eq!(
            read_back_at(&base, "dev-1", "tok-abc").await,
            Readback::Routable
        );
        assert_eq!(
            seen.lock().unwrap().clone(),
            Some("dev-1|Bearer tok-abc".to_string()),
            "the read-back must address THIS device and present the bearer",
        );
    }

    /// The incident end-to-end: the server says it holds no WS session, so
    /// the status endpoint must be able to say `relay_routable: false` even
    /// while the local socket is up.
    #[tokio::test]
    async fn read_back_surfaces_the_outage_state_over_the_wire() {
        let (base, _) = spawn_device_route(
            r#"{"id":"d","wsConnected":false}"#,
            axum::http::StatusCode::OK,
        )
        .await;
        assert_eq!(
            read_back_at(&base, "dev-1", "tok").await,
            Readback::NotRoutable
        );

        let mut state = RoutableState::default();
        let now = Instant::now();
        state.record(Readback::NotRoutable, now, 1_700_000_000_000);
        assert_eq!(state.snapshot(now, MAX_AGE).relay_routable, Some(false));
    }

    /// A 401 — what a device-JWT actually gets from this Cognito-gated route
    /// (verified against production 2026-08-27) — is UNKNOWN, never `false`.
    #[tokio::test]
    async fn a_rejected_read_back_is_unknown_over_the_wire() {
        let (base, _) = spawn_device_route(
            r#"{"error":"UNAUTHORIZED"}"#,
            axum::http::StatusCode::UNAUTHORIZED,
        )
        .await;
        assert!(matches!(
            read_back_at(&base, "dev-1", "tok").await,
            Readback::Unknown(_)
        ));
    }

    /// An unreachable backend is the purest "absence of evidence" case.
    #[tokio::test]
    async fn an_unreachable_backend_is_unknown_not_false() {
        // Port 1 has no listener -> connect error.
        match read_back_at("http://127.0.0.1:1", "dev-1", "tok").await {
            Readback::Unknown(reason) => {
                assert!(reason.contains("failed"), "reason should name it: {reason}")
            }
            other => panic!("an unreachable backend must be UNKNOWN, got {other:?}"),
        }
    }

    #[test]
    fn poll_interval_is_within_the_sane_window() {
        assert!(POLL_INTERVAL >= Duration::from_secs(30));
        assert!(POLL_INTERVAL <= Duration::from_secs(60));
        assert!(
            MAX_AGE > POLL_INTERVAL,
            "a value must survive at least one missed poll"
        );
    }
}
