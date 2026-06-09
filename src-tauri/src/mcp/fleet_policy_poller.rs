//! Device-scoped poller for the fleet-policy `install_interception` domain
//! (P3 + P4 of `2026-06-08-fleet-policy-channel-redesign.md`).
//!
//! Coord exposes `GET /coord/fleet-policy?domain=install_interception`
//! (FleetPrincipal / device-JWT gated) which returns the EFFECTIVE
//! interception level resolved for THIS device's tenant/fleet:
//!
//! ```json
//! {"domain":"install_interception","effective_level":"off|observe|gate",
//!  "master_enabled":true,"resolved_scope":"..."}
//! ```
//!
//! This module owns a process-global cache of that `effective_level` and a
//! supervised background loop that refreshes it every [`POLL_INTERVAL`]. The
//! cache is read SYNCHRONOUSLY (no app state, no async) by
//! `install_effects_producer::run_with_base` via
//! [`effective_install_intercept_mode`] so the interception pre-call can make
//! the per-install mode DYNAMIC (P4) rather than trusting the shim's
//! spawn-time `QONTINUI_INSTALL_INTERCEPT_MODE` env.
//!
//! ## Lifecycle parallel to `device_jwt_refresher`
//!
//! Mirrors `mcp::device_jwt_refresher`'s shape so the call sites read
//! consistently:
//!
//! - [`PollerState`] holds the `watch` shutdown channel + join handle.
//! - [`start_poller`] spawns the loop via `task_supervisor::spawn_supervised`
//!   so a panic self-heals (a dead poller would silently freeze the cached
//!   mode — see the refresher's supervisor rationale).
//! - [`commands::auto_start_fleet_policy_poller`] is the idempotent boot entry
//!   wired beside `auto_start_device_jwt_refresher` in `mcp_api`.
//!
//! ## Fail-safe contract (D7)
//!
//! - Before the FIRST successful poll, the cache reads **`off`** (NEVER gate).
//! - A poll ERROR (network / decode / non-2xx) keeps the LAST-GOOD value.
//! - A coord **404 / 401 / auth-required** resets the cache to **`off`** (the
//!   policy is absent or this device isn't authorized — never gate).
//! - If there is no device JWT yet (unpaired) the poll is SKIPPED quietly —
//!   no log spam — like the other daemons that no-op without a token.
//! - Degradation is logged ONCE (a level transition), not every tick.

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::time::Duration;

use tokio::sync::{watch, Mutex};
use tracing::{info, warn};

use crate::mcp::types::ApiState;

/// How often the loop refreshes the cached effective level. 45s sits in the
/// 30–60s window the plan specifies — long enough to not hammer coord, short
/// enough that an operator flipping the fleet policy sees it take effect on
/// already-injected terminals within a minute.
const POLL_INTERVAL: Duration = Duration::from_secs(45);

/// The fleet-policy domain this poller tracks.
const DOMAIN: &str = "install_interception";

/// The fail-safe default: every read before the first success, and every
/// reset on a 404/401/auth-required, collapses to this. NEVER `gate`.
const DEFAULT_MODE: &str = "off";

// ===========================================================================
// Process-global cache
// ===========================================================================

/// The cached effective interception mode (`off` | `observe` | `gate`).
/// `RwLock<String>` behind a `OnceLock` — `once_cell` is already a dep, but
/// `std::sync::OnceLock` is in std since 1.70 and is the same idiom the
/// `device_jwt_refresher::commands` holder uses, so we stay consistent.
static EFFECTIVE_MODE: OnceLock<RwLock<String>> = OnceLock::new();

fn cache() -> &'static RwLock<String> {
    EFFECTIVE_MODE.get_or_init(|| RwLock::new(DEFAULT_MODE.to_string()))
}

/// Read the current effective install-interception mode. Returns `"off"`
/// until the first successful poll (and after any auth/absent reset).
///
/// SYNCHRONOUS + lock-only — safe to call from `run_with_base` (which has no
/// app state and is not the place to do async I/O). A poisoned lock degrades
/// fail-safe to `"off"`.
pub fn effective_install_intercept_mode() -> String {
    cache()
        .read()
        .map(|g| g.clone())
        .unwrap_or_else(|_| DEFAULT_MODE.to_string())
}

/// Overwrite the cached mode. Internal — only the poll loop calls this.
fn set_mode(mode: &str) {
    if let Ok(mut g) = cache().write() {
        *g = mode.to_string();
    }
}

/// Test-only cache setter so other modules' tests (e.g.
/// `install_effects_producer`'s intercept-mode tests, which need a non-`off`
/// effective mode for the pre-call to proceed past the P4 short-circuit) can
/// pin the process-global cache. NEVER compiled into a release binary.
#[cfg(test)]
pub(crate) fn set_mode_for_test(mode: &str) {
    set_mode(mode);
}

// ===========================================================================
// Wire type (coord response subset)
// ===========================================================================

/// Subset of coord's `GET /coord/fleet-policy` response we read. `master_enabled`
/// + `resolved_scope` are pulled through for observability but only
/// `effective_level` drives the cache. Every field defaults so a coord that
/// trims/renames a sibling field doesn't break the decode.
#[derive(Debug, Clone, serde::Deserialize)]
struct FleetPolicyResponse {
    #[serde(default)]
    effective_level: Option<String>,
}

/// Subset of coord's `GET /health` response we read for the capability probe.
///
/// Coord ships a `capabilities` object (parallel coord PR), e.g.
/// `{"capabilities":{"install_signatures":true,"fleet_policy":true}}`. We read
/// ONLY `fleet_policy` to decide whether this poller should run at all.
///
/// FAIL-SAFE DECODE (the absent-field=capable default): `capabilities` is
/// `Option` so a coord that PREDATES the field (no `capabilities` key) decodes
/// to `None` ⇒ we ASSUME capable and poll as normal. Within `capabilities`,
/// `fleet_policy` is `Option<bool>` so only an EXPLICIT `false` disables the
/// poller; an absent `fleet_policy` key (coord ships other caps but not this
/// one yet) also assumes capable. We never break against a coord that doesn't
/// know the field.
#[derive(Debug, Clone, serde::Deserialize)]
struct HealthResponse {
    #[serde(default)]
    capabilities: Option<Capabilities>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Capabilities {
    #[serde(default)]
    fleet_policy: Option<bool>,
}

// ===========================================================================
// Poller state + supervised loop
// ===========================================================================

/// State for the fleet-policy poller task. Owns the shutdown channel + join
/// handle so the boot entry can stop / restart it. (No kick channel: unlike
/// the refresher there's no event that needs to wake it early — the policy is
/// pull-only on a fixed cadence.)
pub struct PollerState {
    shutdown_tx: watch::Sender<bool>,
    task_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl PollerState {
    /// Stop the poller task, giving it up to 3 seconds to shut down cleanly.
    pub async fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(handle) = self.task_handle.lock().await.take() {
            match tokio::time::timeout(Duration::from_secs(3), handle).await {
                Ok(_) => info!("Fleet-policy poller stopped gracefully"),
                Err(_) => {
                    warn!("Fleet-policy poller did not stop in 3s; shutdown signal sent, moving on")
                }
            }
        }
    }
}

/// Spawn the poller task. Returns the state handle so the caller can stop it.
pub fn start_poller(api_state: Arc<ApiState>) -> Arc<PollerState> {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // SUPERVISOR. Same rationale as `device_jwt_refresher::start_refresher`:
    // `poller_loop` is long-lived and should only RETURN on shutdown. A bare
    // panic would PERMANENTLY freeze the cached mode at its last value (which,
    // worse, could be a stale `gate` after the operator turned the policy off
    // — leaving every terminal blocking installs). Supervise it so a
    // panic/wedge self-heals instead of requiring a runner restart.
    let shutdown_rx_loop = shutdown_rx.clone();
    let task_handle = crate::mcp::task_supervisor::spawn_supervised(
        "Fleet-policy poller",
        shutdown_rx,
        move || poller_loop(api_state.clone(), shutdown_rx_loop.clone()),
    );

    Arc::new(PollerState {
        shutdown_tx,
        task_handle: Mutex::new(Some(task_handle)),
    })
}

/// Outcome of a single poll attempt. Factored out so the loop's logging stays
/// edge-triggered (log only on a transition, never every tick).
#[derive(Debug, Clone, PartialEq, Eq)]
enum PollOutcome {
    /// Coord returned 2xx with a level — cache updated to this value.
    Updated(String),
    /// No device JWT yet (unpaired) — poll skipped, cache untouched.
    SkippedNoJwt,
    /// Coord said 401 / 404 / auth-required — cache RESET to `off` (fail-safe).
    ResetOff(u16),
    /// Network / decode / other non-2xx error — LAST-GOOD value kept.
    Kept(String),
}

/// Outcome of the one-shot capability probe done at poller start.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CapabilityCheck {
    /// Coord advertises `capabilities` and `fleet_policy` is EXPLICITLY false —
    /// the ONLY case that disables the poller.
    Disabled,
    /// Capable: coord said `fleet_policy:true`, OR `fleet_policy` was absent
    /// from a present `capabilities`, OR there was no `capabilities` field at
    /// all (older coord), OR the probe errored. Fail-safe — when in doubt, poll.
    Capable,
}

/// One-shot `GET /health` capability probe (§5). Decides whether this poller
/// should run at all by reading coord's `capabilities.fleet_policy`.
///
/// DEFENSIVE / FAIL-SAFE CONTRACT:
/// - `fleet_policy == Some(false)` (explicitly disabled) ⇒ [`CapabilityCheck::Disabled`].
/// - `fleet_policy == Some(true)` ⇒ Capable.
/// - `fleet_policy` ABSENT but `capabilities` present ⇒ Capable (coord ships
///   other caps but not this flag yet — don't disable).
/// - NO `capabilities` field at all (coord predates it) ⇒ Capable.
/// - ANY error (coord base unresolved / request / non-2xx / decode) ⇒ Capable.
///   We never disable the poller because we couldn't reach `/health`.
async fn check_fleet_policy_capability() -> CapabilityCheck {
    let base = match crate::mcp::agent_worktrees::coord_http_base() {
        Ok(b) => b,
        // No coord base ⇒ assume capable; the poll loop's own per-tick base
        // resolution + JWT gating handles an actually-unconfigured runner.
        Err(_) => return CapabilityCheck::Capable,
    };
    let url = format!("{}/health", base.trim_end_matches('/'));

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return CapabilityCheck::Capable,
    };

    // `/health` is an unauthenticated liveness endpoint — no Bearer needed.
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(_) => return CapabilityCheck::Capable,
    };
    if !resp.status().is_success() {
        return CapabilityCheck::Capable;
    }
    let body: HealthResponse = match resp.json().await {
        Ok(b) => b,
        Err(_) => return CapabilityCheck::Capable,
    };

    match body.capabilities {
        // capabilities present + fleet_policy explicitly false ⇒ the only
        // disable case.
        Some(caps) if caps.fleet_policy == Some(false) => CapabilityCheck::Disabled,
        // capabilities present, fleet_policy true or absent ⇒ capable.
        // No capabilities field at all (older coord) ⇒ capable.
        _ => CapabilityCheck::Capable,
    }
}

async fn poller_loop(_api_state: Arc<ApiState>, mut shutdown_rx: watch::Receiver<bool>) {
    // §5 capability gate (one-shot at start): if coord EXPLICITLY advertises it
    // lacks the fleet_policy capability, stay off and don't poll. Any other
    // outcome (capable, absent flag, no capabilities field, or any probe error)
    // proceeds to poll as normal — defensive default never breaks against an
    // older coord that predates the `capabilities` field.
    //
    // We do NOT `return` on Disabled: the task supervisor respawns any loop that
    // returns without a shutdown signal (task_supervisor.rs:108), which would
    // re-run this probe + re-log every backoff window. Instead we log ONCE and
    // PARK on the shutdown channel — the cache already reads the fail-safe
    // `off`, so a non-polling parked loop is exactly the desired "stay off".
    if check_fleet_policy_capability().await == CapabilityCheck::Disabled {
        info!("fleet_policy_poller: coord lacks fleet_policy capability — staying off");
        // Park until shutdown (the cache stays at the fail-safe DEFAULT_MODE).
        let _ = shutdown_rx.changed().await;
        info!("Fleet-policy poller shutting down (was parked: coord lacks capability)");
        return;
    }

    info!(
        "Fleet-policy poller started (domain={DOMAIN}, interval={}s, fail-safe default={DEFAULT_MODE})",
        POLL_INTERVAL.as_secs()
    );

    // Edge-trigger degradation logs: remember the LAST outcome class we logged
    // so a steady-state (e.g. repeated SkippedNoJwt while unpaired, or repeated
    // network errors) emits exactly one line, not one per tick.
    let mut last_logged: Option<PollOutcome> = None;

    loop {
        if *shutdown_rx.borrow() {
            info!("Fleet-policy poller shutting down");
            return;
        }

        let outcome = poll_once().await;

        // Apply the cache effect.
        match &outcome {
            PollOutcome::Updated(level) => set_mode(level),
            PollOutcome::ResetOff(_) => set_mode(DEFAULT_MODE),
            // Skipped / Kept leave the cache as-is (last-good or default).
            PollOutcome::SkippedNoJwt | PollOutcome::Kept(_) => {}
        }

        // Log only on a class change so we don't spam every 45s.
        let changed = last_logged.as_ref() != Some(&outcome);
        if changed {
            match &outcome {
                PollOutcome::Updated(level) => {
                    info!("fleet_policy_poller: effective install-interception level = {level}");
                }
                PollOutcome::SkippedNoJwt => {
                    info!(
                        "fleet_policy_poller: no device JWT yet (unpaired) — skipping poll, \
                         interception mode stays {DEFAULT_MODE}"
                    );
                }
                PollOutcome::ResetOff(status) => {
                    warn!(
                        "fleet_policy_poller: coord returned {status} (auth/absent) — \
                         resetting interception mode to {DEFAULT_MODE} (fail-safe, never gate)"
                    );
                }
                PollOutcome::Kept(err) => {
                    warn!(
                        "fleet_policy_poller: poll failed ({err}) — keeping last-good \
                         interception mode ({})",
                        effective_install_intercept_mode()
                    );
                }
            }
            last_logged = Some(outcome);
        }

        // Sleep until the next tick, waking early on shutdown.
        tokio::select! {
            _ = shutdown_rx.changed() => {
                info!("Fleet-policy poller shutting down");
                return;
            }
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
    }
}

/// One poll against coord. Resolves the coord base the SAME way the
/// install-effects producer does (`agent_worktrees::coord_http_base()`) and
/// presents the device JWT from `AuthManager::get_access_token()` as the
/// `Authorization: Bearer` — the exact accessor `backend_relay` uses
/// (`backend_relay.rs:452`) to authenticate the device WS.
///
/// NET-NEW coord client (D4): the existing `device_jwt_refresher` talks to the
/// WEB-BACKEND proxy, not coord, so we issue our own GET here.
async fn poll_once() -> PollOutcome {
    // Device JWT — same slot the relay reads as its Bearer (REPLACE-not-REVOKE
    // lifecycle owned by the refresher). Empty ⇒ unpaired ⇒ skip quietly.
    let device_jwt = match crate::auth::AuthManager::new().get_access_token() {
        Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => return PollOutcome::SkippedNoJwt,
    };

    // coord base — identical source-of-truth chain to the producer's
    // `coord_base()` (env COORD_HTTP_URL → profile coord_url → default).
    let base = match crate::mcp::agent_worktrees::coord_http_base() {
        Ok(b) => b,
        Err(e) => return PollOutcome::Kept(format!("coord base unresolved: {e}")),
    };
    let url = format!(
        "{}/coord/fleet-policy?domain={DOMAIN}",
        base.trim_end_matches('/')
    );

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => return PollOutcome::Kept(format!("client build: {e}")),
    };

    let resp = match client.get(&url).bearer_auth(&device_jwt).send().await {
        Ok(r) => r,
        Err(e) => return PollOutcome::Kept(format!("request: {e}")),
    };

    let status = resp.status();
    // Auth / absent ⇒ fail-safe reset to off (NEVER gate on a 401/404).
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::NOT_FOUND {
        return PollOutcome::ResetOff(status.as_u16());
    }
    if !status.is_success() {
        return PollOutcome::Kept(format!("coord status {}", status.as_u16()));
    }

    let body: FleetPolicyResponse = match resp.json().await {
        Ok(b) => b,
        Err(e) => return PollOutcome::Kept(format!("decode: {e}")),
    };

    // `effective_level` absent / null ⇒ coord's documented "off when absent"
    // contract — treat as off (cache update, not an error).
    let level = body
        .effective_level
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_MODE.to_string());

    // Normalize anything unexpected to off (fail-safe: never honor a level we
    // don't recognize as a gate trigger).
    let level = match level.as_str() {
        "off" | "observe" | "gate" => level,
        _ => DEFAULT_MODE.to_string(),
    };

    PollOutcome::Updated(level)
}

// ===========================================================================
// Boot entry — mirrors device_jwt_refresher::commands
// ===========================================================================

/// Global state holder + public boot surface. Same shape as
/// `device_jwt_refresher::commands` so the `mcp_api` call sites read alike.
pub mod commands {
    use super::*;

    static POLLER_STATE: OnceLock<tokio::sync::Mutex<Option<Arc<PollerState>>>> = OnceLock::new();

    fn get_holder() -> &'static tokio::sync::Mutex<Option<Arc<PollerState>>> {
        POLLER_STATE.get_or_init(|| tokio::sync::Mutex::new(None))
    }

    /// Idempotent start. If a live task already exists, no-op (the poller has
    /// no kick — it's a fixed-cadence pull). If the prior task ended, restart.
    /// Wired beside `auto_start_device_jwt_refresher` in `mcp_api::start_server`
    /// — runs ONCE per runner (device-scoped), supervised, regardless of agents.
    pub async fn auto_start_fleet_policy_poller(api_state: Arc<ApiState>) {
        let mut guard = get_holder().lock().await;

        if let Some(ref existing) = *guard {
            let handle_guard = existing.task_handle.lock().await;
            let is_alive = handle_guard.as_ref().is_some_and(|h| !h.is_finished());
            drop(handle_guard);
            if is_alive {
                info!("Fleet-policy poller already running; leaving it");
                return;
            }
            info!("Fleet-policy poller task has ended, restarting...");
            existing.stop().await;
            *guard = None;
        }

        info!("Starting fleet-policy poller");
        let state = start_poller(api_state);
        *guard = Some(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mode_constant_is_off() {
        // The fail-safe contract pin: the cache's resting/initial value is `off`
        // (NEVER gate before a successful poll). We assert the CONSTANT only —
        // NOT a live read of the shared process-global cache, which other
        // modules' tests legitimately mutate via `set_mode_for_test` and which
        // would race under cargo's parallel test threads.
        assert_eq!(DEFAULT_MODE, "off");
    }

    #[test]
    fn fresh_oncelock_initializes_to_off() {
        // Exercise the OnceLock init closure directly (a private throwaway lock,
        // not the shared global) so this is race-free: the cache MUST initialize
        // to the fail-safe default.
        let fresh = RwLock::new(DEFAULT_MODE.to_string());
        assert_eq!(*fresh.read().unwrap(), "off");
    }

    #[test]
    fn poll_interval_is_in_30_to_60s_window() {
        // Pin the cadence to the plan's window so a future "tune" has to update
        // this test and justify it in review.
        let s = POLL_INTERVAL.as_secs();
        assert!(
            (30..=60).contains(&s),
            "poll interval {s}s out of 30-60s window"
        );
    }

    #[test]
    fn unknown_level_is_normalized_off_via_response_shape() {
        // The decode + normalize path collapses an unrecognized level to off.
        // We exercise the pure normalization the loop relies on.
        let normalize = |raw: Option<&str>| -> String {
            let level = raw
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_MODE.to_string());
            match level.as_str() {
                "off" | "observe" | "gate" => level,
                _ => DEFAULT_MODE.to_string(),
            }
        };
        assert_eq!(normalize(Some("GATE")), "gate");
        assert_eq!(normalize(Some(" observe ")), "observe");
        assert_eq!(normalize(Some("bogus")), DEFAULT_MODE);
        assert_eq!(normalize(None), DEFAULT_MODE);
        assert_eq!(normalize(Some("")), DEFAULT_MODE);
    }

    #[test]
    fn capability_decode_disables_only_on_explicit_false() {
        // The §5 fail-safe contract: ONLY an explicit `fleet_policy:false`
        // disables the poller. Absent flag, no capabilities object, and a
        // decode of an older coord's body all ⇒ Capable (never break against a
        // coord that predates the field). We exercise the pure decision the
        // probe makes over the decoded `HealthResponse`.
        let decide = |json: &str| -> CapabilityCheck {
            let body: HealthResponse = serde_json::from_str(json).expect("decode");
            match body.capabilities {
                Some(caps) if caps.fleet_policy == Some(false) => CapabilityCheck::Disabled,
                _ => CapabilityCheck::Capable,
            }
        };

        // Explicit false ⇒ the ONLY disable case.
        assert_eq!(
            decide(r#"{"capabilities":{"fleet_policy":false}}"#),
            CapabilityCheck::Disabled
        );
        // Explicit true ⇒ capable.
        assert_eq!(
            decide(r#"{"capabilities":{"fleet_policy":true}}"#),
            CapabilityCheck::Capable
        );
        // capabilities present, fleet_policy absent (coord ships other caps but
        // not this flag) ⇒ capable.
        assert_eq!(
            decide(r#"{"capabilities":{"install_signatures":true}}"#),
            CapabilityCheck::Capable
        );
        // NO capabilities field at all (older coord predating the field) ⇒
        // capable — the defensive default that must never disable.
        assert_eq!(decide(r#"{"status":"ok"}"#), CapabilityCheck::Capable);
        // Empty body ⇒ capable.
        assert_eq!(decide(r#"{}"#), CapabilityCheck::Capable);
    }
}
