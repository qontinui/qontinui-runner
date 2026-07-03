//! Background task that keeps the device-JWT fresh (Phase 2 of the
//! runner unified-devices migration).
//!
//! Coord mints 4-hour device-JWTs. To avoid the user being signed out
//! while the runner is idle, this loop checks the stored JWT's `exp`
//! every [`REFRESH_CHECK_INTERVAL`] and re-pairs (via
//! `qontinui_runner_lib::pair::pair_with_auth_token`) once we're within
//! TTL/3 of expiry. The new JWT is persisted to the same encrypted
//! `auth_tokens.enc` slot the backend relay reads, and the relay is
//! kicked so it reconnects with the fresh credential.
//!
//! ## Lifecycle parallel to backend_relay
//!
//! The refresher mirrors `mcp::backend_relay`'s shape:
//!
//! - `RefresherState { shutdown_tx, kick_tx, task_handle }` — same
//!   `tokio::sync::watch` channels for shutdown + kick.
//! - `auto_start_device_jwt_refresher(api_state)` — idempotent start;
//!   re-kicks instead of spawning a duplicate if a live task already
//!   exists.
//! - `commands::kick_device_jwt_refresher()` — public API consumed from
//!   `commands::auth::set_runner_tier` (and any future code that needs
//!   to wake the refresher; e.g. apply_web_integration_settings).
//!
//! ## Why is the decision predicate factored out?
//!
//! The async loop body is wrapped around blocking-thread spawns, watch
//! channels, and tracing — none of which fit a pure-function unit test.
//! [`next_action`] is the inner predicate (tier + token + needs-refresh
//! → [`Decision`]) so the spec-check tests can exercise the branching
//! without spinning a tokio runtime.

use std::sync::Arc;
use std::time::Duration;

use tauri::Emitter;
use tokio::sync::{watch, Mutex};
use tracing::{info, warn};

use crate::mcp::types::ApiState;
use crate::settings::{self, RunnerTier};

/// Frontend event fired when the runner goes credential-dark on a HARD Cognito
/// refresh failure (the refresh token is expired/revoked — no headless
/// recovery). Carries `{ dark: bool, message: String }`: `dark:true` on the
/// dark transition (the operator must sign in again to resume autonomy),
/// `dark:false` when a later refresh succeeds and autonomy resumes. The UI can
/// surface this as a banner/toast; fired once per transition (deduped via
/// [`RefreshBackoff::dark_notified`]).
pub const AUTONOMY_CREDENTIAL_DARK_EVENT: &str = "autonomy-credential-dark";

/// Outcome of a single refresh attempt. Tests assert on this variant
/// directly; the runtime loop in [`refresher_loop`] consumes it via
/// pattern-match (Replaced → kick the relay; KeptExisting/PersistFailed →
/// log + back off).
///
/// CRITICAL INVARIANT (Phase 5.2): a non-2xx coord response MUST map to
/// [`RefreshOutcome::KeptExisting`], NEVER to a code path that clears the
/// JWT. A 401 from coord means "this runner_token is stale"; if we
/// cleared the access_token slot in response, the relay would lose its
/// valid (just-not-yet-expired) device-JWT and the next user-flow would
/// be forced into a fresh browser-pair. The refresher's job is to
/// REPLACE, not REVOKE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RefreshOutcome {
    /// Coord returned 2xx with a fresh JWT, and we persisted it to the
    /// access_token slot. Carries the new JWT so the runtime loop can
    /// log its jti/exp without re-reading from disk.
    Replaced { new_jwt: String },
    /// Coord returned a non-2xx (401, 503, anything else), OR the
    /// network call failed, OR the spawn_blocking handle joined with
    /// an error. The existing JWT in the access_token slot is left
    /// untouched.
    KeptExisting,
    /// Coord returned a fresh JWT but persistence to AuthManager
    /// failed. The existing JWT in the access_token slot is left
    /// untouched (store_tokens is atomic; a failure aborts before
    /// rewriting the slot).
    PersistFailed(String),
}

/// How often the loop wakes to check whether the JWT is approaching
/// expiry. 5 minutes is plenty given the refresh threshold is 80 min.
const REFRESH_CHECK_INTERVAL: Duration = Duration::from_secs(300);

/// Floor of the transient-failure backoff. When a Cognito refresh fails
/// transiently (network/5xx/429) we retry far sooner than the 5m steady
/// cadence so parked autonomy self-recovers within seconds of connectivity
/// returning — not up to 5 minutes later.
const TRANSIENT_BACKOFF_MIN: Duration = Duration::from_secs(15);

/// Ceiling of the transient-failure backoff. Kept well under
/// [`REFRESH_CHECK_INTERVAL`] so even a long outage retries every 2 min (fast
/// recovery) without hot-looping on a persistent error.
const TRANSIENT_BACKOFF_MAX: Duration = Duration::from_secs(120);

/// Classification of a Cognito-bearer refresh attempt, used to drive the loop's
/// backoff + credential-dark notification. Produced by [`refresh_cognito_bearer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshClass {
    /// No refresh was needed, or the refresh succeeded — healthy.
    Ok,
    /// Refresh failed transiently (network, timeout, 5xx, 429) OR with an
    /// unclassified error. Retry promptly with capped-exponential backoff;
    /// autonomy self-recovers once connectivity returns.
    Transient,
    /// Refresh failed because the refresh token is expired/revoked
    /// (`invalid_grant`). No headless recovery — fire the credential-dark
    /// notification and fall to the steady cadence (don't hot-retry a grant
    /// that will keep failing identically).
    Hard,
    /// No Cognito session at all (no refresh token stored) — legacy/local-login
    /// install or a full sign-out. Not a Cognito-refresh episode; the caller
    /// falls back to the device-JWT slot bearer.
    NoSession,
}

/// Carried across loop iterations so the backoff can grow then reset and the
/// credential-dark notification fires exactly once per dark transition.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct RefreshBackoff {
    /// Consecutive transient failures so far — drives the exponential growth.
    consecutive_transient: u32,
    /// True once we've emitted the credential-dark notification for the current
    /// dark episode. Reset on recovery so we notify once per transition, not
    /// every 5m tick.
    dark_notified: bool,
}

/// What the loop should do after a Cognito-bearer attempt this tick: how long
/// to wait, and which (if any) notification to emit. Pure output of
/// [`plan_refresh_wait`] so the decision is unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefreshLoopAction {
    pub wait: Duration,
    /// Emit the credential-dark notification this tick (deduped to once per
    /// dark transition).
    pub notify_dark: bool,
    /// Emit the autonomy-resumed notification this tick (fired once, when a
    /// previously-dark runner recovers).
    pub notify_recovered: bool,
}

/// Capped exponential backoff for transient failures: 15s, 30s, 60s, 120s, then
/// pinned at the 120s ceiling. `consecutive` is the count of prior consecutive
/// transient failures (0 on the first failure).
fn transient_backoff(consecutive: u32) -> Duration {
    let min = TRANSIENT_BACKOFF_MIN.as_secs();
    let max = TRANSIENT_BACKOFF_MAX.as_secs();
    // min * 2^consecutive, saturating, clamped to [min, max].
    let scaled = min.saturating_mul(1u64 << consecutive.min(20));
    Duration::from_secs(scaled.clamp(min, max))
}

/// Pure transition: fold this tick's [`RefreshClass`] into the backoff state and
/// decide the wait duration + which notification to fire. Mutates `state` so the
/// loop carries the backoff + dark-dedup across iterations.
///
/// - `Transient` → shortened [`transient_backoff`] that grows each consecutive
///   failure; the counter resets on any non-transient outcome.
/// - `Hard` → steady [`REFRESH_CHECK_INTERVAL`] + the credential-dark
///   notification (once per dark transition).
/// - `Ok` → steady cadence; if we were dark, emit the recovered notification.
/// - `NoSession` → steady cadence; not a Cognito-refresh episode (no dark/
///   recovered signal — a full sign-out shouldn't claim "resumed").
pub(crate) fn plan_refresh_wait(
    state: &mut RefreshBackoff,
    class: RefreshClass,
) -> RefreshLoopAction {
    match class {
        RefreshClass::Transient => {
            let wait = transient_backoff(state.consecutive_transient);
            state.consecutive_transient = state.consecutive_transient.saturating_add(1);
            RefreshLoopAction {
                wait,
                notify_dark: false,
                notify_recovered: false,
            }
        }
        RefreshClass::Hard => {
            state.consecutive_transient = 0;
            let first = !state.dark_notified;
            state.dark_notified = true;
            RefreshLoopAction {
                wait: REFRESH_CHECK_INTERVAL,
                notify_dark: first,
                notify_recovered: false,
            }
        }
        RefreshClass::Ok => {
            let was_dark = state.dark_notified;
            state.consecutive_transient = 0;
            state.dark_notified = false;
            RefreshLoopAction {
                wait: REFRESH_CHECK_INTERVAL,
                notify_dark: false,
                notify_recovered: was_dark,
            }
        }
        RefreshClass::NoSession => {
            state.consecutive_transient = 0;
            RefreshLoopAction {
                wait: REFRESH_CHECK_INTERVAL,
                notify_dark: false,
                notify_recovered: false,
            }
        }
    }
}

/// Emit the credential-dark / recovered notification to the frontend. Best-
/// effort: an emit failure only `warn!`s — telemetry must never break the loop.
fn emit_credential_dark(app: &tauri::AppHandle, dark: bool) {
    let message = if dark {
        "Autonomous sessions paused — sign in again to resume."
    } else {
        "Autonomous sessions resumed — credentials refreshed."
    };
    let payload = serde_json::json!({ "dark": dark, "message": message });
    if let Err(e) = app.emit(AUTONOMY_CREDENTIAL_DARK_EVENT, &payload) {
        warn!("device_jwt_refresher: failed to emit {AUTONOMY_CREDENTIAL_DARK_EVENT}: {e}");
    }
}

/// What action the loop should take this iteration. Factored out of
/// [`refresher_loop`] so unit tests can exercise the branching without
/// running the async loop body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// JWT is fresh enough — sleep until next check or kick.
    Idle,
    /// Tier is not `QontinuiAccount` — refresher has nothing to do.
    /// Wait for a tier-change kick before re-checking.
    IdleWrongTier,
    /// JWT needs refresh — resolve a bearer in the `Pair` arm (Cognito
    /// access token, falling back to the device-JWT slot) and re-mint.
    /// The arm self-idles (with periodic re-check) if no bearer exists yet,
    /// so we no longer gate this decision on a `runner_token` being present
    /// (Cognito- and pair-code-paired runners have an empty `runner_token`
    /// but a valid Cognito/device bearer — gating on `runner_token` here
    /// stranded them and let their device JWT expire).
    Pair,
}

/// Pure decision predicate: given the current tier + "does the JWT need
/// refresh?" answer, what should the loop do?
///
/// Deliberately does NOT consult `web_integration.runner_token`. Post-Cognito
/// unification the bearer is the Cognito access token (or the device-JWT slot)
/// — neither populates `runner_token`, so gating on it here left every
/// Cognito-/pair-code-paired runner in a permanent "idle, no token" state and
/// let its device JWT silently expire. The `Pair` arm resolves the real bearer
/// and idles gracefully (with periodic re-check) when none is available yet.
pub(crate) fn next_action(tier: RunnerTier, needs_refresh: bool) -> Decision {
    if tier != RunnerTier::QontinuiAccount {
        return Decision::IdleWrongTier;
    }
    if !needs_refresh {
        return Decision::Idle;
    }
    Decision::Pair
}

// ===========================================================================
// Phase 1b — coord-credential health signal (runner publishes; coord derives
// the fleet alert in `evaluate()`).
// ===========================================================================

/// Outcome of the `Pair` arm as seen by the health-mapping fn. The arm can
/// either bail before re-minting (no bearer / no tenant / pair-cli failure
/// leaving an EXPIRED jwt behind) or complete (a fresh `Replaced`, or a
/// `KeptExisting`/`PersistFailed` whose existing JWT is still valid). Factored
/// out so [`coord_credential_health`] is a pure fn over an enum the loop can
/// construct at each arm without re-deriving any state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PairProgress {
    /// `Decision::Pair` reached but the loop bailed before re-minting because
    /// no Cognito/device bearer was resolvable. The string is the specific
    /// failing source for the operator-facing `reason`.
    BailNoBearer,
    /// `Decision::Pair` reached but no `tenant_id` resolved from any source
    /// (OAuth claim → outgoing device-JWT → machine.json). Gate-blocking.
    BailNoTenant,
    /// `Decision::Pair` ran `try_refresh_once`, which returned a non-2xx
    /// (`KeptExisting`) or a persist failure (`PersistFailed`), AND the JWT
    /// still in the slot is EXPIRED (or absent) — the runner is now
    /// credential-dark even though it tried. Degraded, not gate-blocking-by-
    /// config: the tenant resolved, the mint just failed.
    BailRefreshFailedExpired,
    /// The `Pair` arm completed with a usable JWT — either a fresh `Replaced`
    /// or a `KeptExisting`/`PersistFailed` whose existing JWT is still valid.
    Healthy,
}

/// Compact coord-credential health the runner stamps into its
/// `coord.device_status.details` on every heartbeat (plan 2026-06-13 Phase 1b).
/// Coord's `fleet_health::evaluate()` reads this to derive the device-scoped
/// `coord_credentials_missing` alert; the runner itself never touches coord's
/// alert machinery.
///
/// Wire shape (under `details.coord_credential`):
/// `{ "ok": <bool>, "reason": <string|null> }`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct CoordCredentialHealth {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl CoordCredentialHealth {
    fn ok() -> Self {
        Self {
            ok: true,
            reason: None,
        }
    }
    fn bad(reason: impl Into<String>) -> Self {
        Self {
            ok: false,
            reason: Some(reason.into()),
        }
    }
}

/// Pure map: refresher decision (plus, for `Pair`, how far the arm got) →
/// the `coord_credential` health the heartbeat publishes. No I/O — the loop
/// constructs the `PairProgress` at each arm and calls this so the mapping is
/// unit-testable in the same style as [`next_action`].
///
/// - [`Decision::Idle`] (JWT fresh)            → `{ok:true,  reason:null}`.
/// - [`Decision::IdleWrongTier`]               → `{ok:false, reason:"runner tier is not Qontinui account"}`.
/// - [`Decision::Pair`] + [`PairProgress::Healthy`]               → `{ok:true}`.
/// - [`Decision::Pair`] + bail variants        → `{ok:false, reason:<specific failing source>}`.
///
/// `pair_progress` is only consulted for [`Decision::Pair`]; callers pass
/// `None` for the idle arms.
pub(crate) fn coord_credential_health(
    decision: Decision,
    pair_progress: Option<PairProgress>,
) -> CoordCredentialHealth {
    match decision {
        Decision::Idle => CoordCredentialHealth::ok(),
        Decision::IdleWrongTier => {
            CoordCredentialHealth::bad("runner tier is not Qontinui account")
        }
        Decision::Pair => match pair_progress {
            Some(PairProgress::Healthy) | None => CoordCredentialHealth::ok(),
            Some(PairProgress::BailNoBearer) => CoordCredentialHealth::bad(
                "no Cognito session and access_token slot empty — user must sign in",
            ),
            Some(PairProgress::BailNoTenant) => CoordCredentialHealth::bad(
                "no resolvable tenant_id (OAuth claim, outgoing device-JWT, or \
                 machine.json::active_tenant_id all absent)",
            ),
            Some(PairProgress::BailRefreshFailedExpired) => CoordCredentialHealth::bad(
                "device-JWT re-mint failed (coord non-2xx or persist error) and the \
                 existing JWT is expired — runner is credential-dark",
            ),
        },
    }
}

/// True iff the device-JWT in the `access_token` slot is absent, opaque
/// (no decodable `exp`), or its `exp` is already in the past. Used to decide
/// whether a failed re-mint left the runner credential-DARK
/// ([`PairProgress::BailRefreshFailedExpired`]) vs. merely hit a transient
/// coord error while still holding a valid (not-yet-expired) JWT.
fn slot_jwt_is_expired_or_absent(auth_manager: &crate::auth::AuthManager) -> bool {
    match auth_manager.access_token_exp() {
        Some(exp) => chrono::Utc::now().timestamp() >= exp,
        None => true, // absent or opaque/undecodable → treat as not-live
    }
}

/// Best-effort publish of the coord-credential health into the runner's
/// `coord.device_status.details.coord_credential` via the existing
/// `POST {coord}/coord/status` upsert path (the same endpoint
/// `qontinui_profile`/coord-sync use; we reuse it rather than invent a new
/// route). Resolves `device_id`/`tenant_id`/coord-base exactly like the rest of
/// the runner:
///   - coord base: `COORD_HTTP_URL` env → active profile (`coord_base_url`),
///   - device_id: `QONTINUI_MACHINE_ID` env → `machine.json::device_id`,
///   - tenant_id: `resolve_active_tenant_id()` (machine.json::active_tenant_id),
///     falling back to the outgoing device-JWT's `tenant_id` claim.
///
/// A publish failure ONLY `warn!`s — it must never break the refresher loop
/// (the loop's job is keeping the JWT fresh; telemetry is strictly best-effort).
async fn publish_coord_credential_status(
    auth_manager: &crate::auth::AuthManager,
    health: &CoordCredentialHealth,
) {
    // device_id: env override first (multi-instance / test), else machine.json.
    let device_id = std::env::var("QONTINUI_MACHINE_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .or_else(|| qontinui_runner_lib::pair::read_device_id_from_disk().ok());
    let Some(device_id) = device_id else {
        // No stable identity → coord can't key the row; skip silently-ish.
        warn!(
            "device_jwt_refresher: cannot publish coord_credential status — \
             no device_id (QONTINUI_MACHINE_ID unset, machine.json unreadable)"
        );
        return;
    };
    let Ok(device_uuid) = uuid::Uuid::parse_str(device_id.trim()) else {
        warn!(
            "device_jwt_refresher: device_id {device_id} is not a UUID — skipping status publish"
        );
        return;
    };

    // tenant_id: machine.json::active_tenant_id (Phase 8b semantics: the
    // DEVICE-LEVEL DEFAULT binding — this publish is a device-scoped surface,
    // so the default is the correct attribution; session-scoped writes get
    // their tenant from the owning session instead), then the outgoing
    // device-JWT claim (still parseable when expired). NULL is acceptable on
    // the wire (coord's StatusUpsert.tenant_id is Option), but we send it
    // when known so the row is tenant-scoped — the explicit-tenant_id
    // publisher posture D2 site 16b/Phase 8 item 7 asks for.
    let tenant_id = crate::session::dual_write::resolve_active_tenant_id().or_else(|| {
        auth_manager
            .get_access_token()
            .ok()
            .as_deref()
            .and_then(qontinui_runner_lib::pair::tenant_id_from_oauth_claim)
            .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok())
    });

    let base = crate::coord_mcp::coord_base_url();
    let url = format!("{base}/coord/status");
    let mut body = serde_json::json!({
        "device_id": device_uuid,
        "details": { "coord_credential": health },
    });
    if let Some(t) = tenant_id {
        body["tenant_id"] = serde_json::json!(t);
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("device_jwt_refresher: status-publish client build failed: {e}");
            return;
        }
    };
    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => {
            warn!(
                "device_jwt_refresher: coord_credential status publish got HTTP {} \
                 (best-effort; loop continues)",
                resp.status()
            );
        }
        Err(e) => {
            warn!(
                "device_jwt_refresher: coord_credential status publish failed: {e} (best-effort)"
            );
        }
    }
}

/// State for the device-JWT refresher task. Owns the watch channels for
/// shutdown + kick and the join handle so callers can stop / re-kick.
pub struct RefresherState {
    shutdown_tx: watch::Sender<bool>,
    kick_tx: watch::Sender<u64>,
    task_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl RefresherState {
    /// Stop the refresher task, giving it a chance to shut down
    /// gracefully (up to 3 seconds before we drop the handle).
    pub async fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(handle) = self.task_handle.lock().await.take() {
            match tokio::time::timeout(Duration::from_secs(3), handle).await {
                Ok(_) => info!("Device-JWT refresher stopped gracefully"),
                Err(_) => warn!(
                    "Device-JWT refresher did not stop in 3s; shutdown signal sent, moving on"
                ),
            }
        }
    }

    /// Kick the refresher: interrupt any in-progress sleep so the next
    /// iteration runs immediately, re-reading settings + tokens.
    pub fn kick(&self) {
        let current = *self.kick_tx.borrow();
        let _ = self.kick_tx.send(current.wrapping_add(1));
    }
}

/// Spawn the refresher task. Returns the state handle so the caller can
/// stop / kick it. Used by `auto_start_device_jwt_refresher`.
pub fn start_refresher(api_state: Arc<ApiState>) -> Arc<RefresherState> {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (kick_tx, kick_rx) = watch::channel(0u64);

    // SUPERVISOR. `refresher_loop` is a long-lived task that should only RETURN
    // on a shutdown signal. Spawned bare it had the same fatal flaw the relay
    // had: a single panic in its directly-awaited path would unwind the task
    // and PERMANENTLY stop JWT refresh — and a dead refresher is especially
    // pernicious because the device-JWT then silently expires, the relay starts
    // getting 1008-rejected, and even with the relay's own respawn it can never
    // recover (no fresh token to present). Supervise it with the shared respawn
    // idiom so a panic/wedge self-heals instead of requiring a runner restart.
    // The factory clones a fresh `kick_rx`/`shutdown_rx` per respawn so
    // `kick_device_jwt_refresher` keeps working across respawns.
    let mut kick_rx_loop = kick_rx;
    let shutdown_rx_loop = shutdown_rx.clone();
    let task_handle = crate::mcp::task_supervisor::spawn_supervised(
        "Device-JWT refresher",
        shutdown_rx,
        move || {
            // Consume any kick delivered between respawns so the fresh loop
            // doesn't immediately fire on an already-handled kick.
            kick_rx_loop.borrow_and_update();
            refresher_loop(
                api_state.clone(),
                shutdown_rx_loop.clone(),
                kick_rx_loop.clone(),
            )
        },
    );

    Arc::new(RefresherState {
        shutdown_tx,
        kick_tx,
        task_handle: Mutex::new(Some(task_handle)),
    })
}

/// Which source supplied the `tenant_id` forwarded to pair-cli. Drives the
/// observability info-log (fallbacks 2 + 3 are logged so the eventual
/// cognito-home-tenant-attribute-sync fix has a signal to watch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TenantSource {
    /// The OAuth/runner bearer token's own `tenant_id` claim (priority 1,
    /// the historical happy path).
    OAuthClaim,
    /// The outgoing persisted device-JWT's `tenant_id` claim (priority 2).
    OutgoingDeviceJwt,
    /// `~/.qontinui/machine.json::active_tenant_id` (priority 3).
    MachineJson,
}

impl TenantSource {
    fn label(self) -> &'static str {
        match self {
            TenantSource::OAuthClaim => "OAuth claim",
            TenantSource::OutgoingDeviceJwt => "outgoing device-JWT",
            TenantSource::MachineJson => "machine.json",
        }
    }
}

/// Pure tenant-id resolution: walk the ordered fallback chain and return the
/// first usable `(tenant_id, source)`, or `None` if every source is
/// absent/malformed.
///
/// Order (see the call site in [`try_refresh_once`] for the prod-breakage
/// rationale):
///   1. `oauth_token`'s `tenant_id` JWT claim,
///   2. `outgoing_jwt`'s `tenant_id` JWT claim (the device-JWT we're about to
///      replace — still parseable even when expired),
///   3. `machine_tenant` (pre-resolved `machine.json::active_tenant_id`).
///
/// Factored out as a pure fn (no disk / no AuthManager) so the ordering is
/// unit-testable without faking `~/.qontinui` or spinning a tokio runtime.
pub(crate) fn resolve_pair_tenant_id(
    oauth_token: &str,
    outgoing_jwt: Option<&str>,
    machine_tenant: Option<uuid::Uuid>,
) -> Option<(uuid::Uuid, TenantSource)> {
    let from_claim = |token: &str| {
        qontinui_runner_lib::pair::tenant_id_from_oauth_claim(token)
            .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok())
    };

    if let Some(t) = from_claim(oauth_token) {
        return Some((t, TenantSource::OAuthClaim));
    }
    if let Some(t) = outgoing_jwt.and_then(from_claim) {
        return Some((t, TenantSource::OutgoingDeviceJwt));
    }
    if let Some(t) = machine_tenant {
        return Some((t, TenantSource::MachineJson));
    }
    None
}

/// Attempt one refresh against `pair_base` using `runner_token` as the
/// bearer + `device_id` / `user_id` as the wire body / header fields.
/// Factored out of the inline Pair-arm body so the Phase 5.2 tests can
/// drive it directly against an in-process mock backend.
///
/// `pair_base` is the web-backend URL (e.g. `http://127.0.0.1:8000`);
/// the underlying [`pair_with_auth_token_with_ids`] hits
/// `{pair_base}/api/v1/devices/pair-cli`, which the backend proxies to
/// coord with `tenant_id` resolved from the authenticated user.
///
/// The `tenant_id` forwarded to pair-cli is resolved via the
/// [`resolve_pair_tenant_id`] fallback chain (OAuth claim → outgoing
/// device-JWT claim → machine.json) — see that function + the call site for
/// the prod-breakage rationale.
///
/// Invariant: a non-2xx HTTP response, a network error, or a
/// spawn_blocking join failure all collapse to
/// [`RefreshOutcome::KeptExisting`]. The caller MUST NOT clear the JWT
/// in response to that variant — see the doc-comment on
/// [`RefreshOutcome`].
pub(crate) async fn try_refresh_once(
    auth_manager: &crate::auth::AuthManager,
    pair_base: &str,
    runner_token: &str,
    device_id: &str,
    user_id: &str,
    machine_tenant: Option<uuid::Uuid>,
) -> RefreshOutcome {
    let base = pair_base.to_string();
    let token = runner_token.to_string();
    let did = device_id.to_string();
    let uid = user_id.to_string();

    // Resolve tenant_id for `POST /api/v1/devices/pair-cli` (the web-backend
    // proxy that fronts coord since PR #224); the refresher reuses the same
    // endpoint to re-mint the device JWT, so it must forward a tenant_id.
    //
    // FALLBACK CHAIN (the OAuth claim alone broke prod: the operator's Cognito
    // token carries NO tenant_id claim, so every tick bailed with
    // "keeping existing JWT" → the device-JWT expired → coord's fleet-auth
    // gate 403'd the fleet panel + the relay flapped). We try, in order:
    //   1. the OAuth/runner token's `tenant_id` claim (original behavior),
    //   2. the OUTGOING persisted device-JWT's own `tenant_id` claim — coord
    //      verified that tenant at the last mint and the web backend
    //      re-validates server-side on pair-cli, so the runner is only
    //      forwarding a hint ("coord is the authority on tenant_id"),
    //   3. `machine_tenant` — `~/.qontinui/machine.json::active_tenant_id`,
    //      resolved by the CALLER (the refresher loop passes
    //      `session::dual_write::resolve_active_tenant_id()`; tests inject
    //      `Some`/`None` directly so they stay hermetic on any host).
    // Only if ALL THREE are absent/malformed do we keep the existing JWT.
    let outgoing_jwt = auth_manager.get_access_token().ok();
    let (tenant_id, tenant_source) =
        match resolve_pair_tenant_id(&token, outgoing_jwt.as_deref(), machine_tenant) {
            Some(resolved) => resolved,
            None => {
                warn!(
                    "device_jwt_refresher: no tenant_id from OAuth claim, outgoing \
                     device-JWT, or machine.json::active_tenant_id; keeping existing JWT"
                );
                return RefreshOutcome::KeptExisting;
            }
        };
    if tenant_source != TenantSource::OAuthClaim {
        // Observability for the cognito-home-tenant-attribute-sync plan that
        // will eventually fix the OAuth claim at the source. Once the Cognito
        // token carries tenant_id again, this info line stops firing.
        info!(
            "device_jwt_refresher: tenant_id resolved from {} (OAuth claim absent)",
            tenant_source.label()
        );
    }

    // pair_with_auth_token_with_ids is reqwest::blocking — must run via
    // spawn_blocking or it stalls the tokio runtime.
    let pair_join = tokio::task::spawn_blocking(move || {
        qontinui_runner_lib::pair::pair_with_auth_token_with_ids(
            &base, &token, &did, &uid, tenant_id,
        )
    })
    .await;

    let pair_result = match pair_join {
        Ok(inner) => inner,
        Err(join_err) => {
            warn!("device_jwt_refresher: pair task join failed: {join_err}");
            return RefreshOutcome::KeptExisting;
        }
    };

    let resp = match pair_result {
        Ok(r) => r,
        Err(e) => {
            // Non-2xx, network error, decode error — ALL leave the JWT
            // slot alone. The relay keeps presenting the existing (not-
            // yet-expired) JWT until coord's exp ticks past or the
            // operator pairs again.
            warn!("device_jwt_refresher: pair_with_auth_token_with_ids failed: {e}");
            return RefreshOutcome::KeptExisting;
        }
    };

    // Coord returned 2xx → persist the new JWT into the access_token
    // slot. The refresh-token slot stays empty (device-JWT lifecycle is
    // owned by coord, not by an OAuth refresh chain).
    match auth_manager.store_tokens(&resp.token, "") {
        Ok(()) => RefreshOutcome::Replaced {
            new_jwt: resp.token,
        },
        Err(e) => {
            warn!("device_jwt_refresher: persist new JWT failed: {e}");
            RefreshOutcome::PersistFailed(e.to_string())
        }
    }
}

/// Coord's `POST /devices/:device_id/refresh-token` response. Only the
/// re-minted `token` matters to the runner (coord owns jti/exp; the runner
/// re-derives exp from the JWT claim when it next checks freshness). Extra
/// fields are ignored, so an evolving coord response stays compatible.
#[derive(Debug, serde::Deserialize)]
struct DeviceRefreshResponse {
    token: String,
}

/// Phase 4a: attempt a login-INDEPENDENT device self-refresh — re-mint the
/// device-JWT by presenting the runner's CURRENT (still-valid) device-JWT as the
/// bearer to coord's `POST /devices/:device_id/refresh-token`. NO Cognito is
/// touched. This is what lets a continuously-running runner stay autonomous
/// INDEFINITELY: as long as it holds a currently-valid device-JWT it re-mints
/// from itself, never hitting the ~30-day Cognito refresh-token ceiling.
///
/// Returns `Some(new_jwt)` on success — the new JWT is ALREADY persisted to the
/// `access_token` slot, so the caller treats it exactly like a successful Cognito
/// re-mint (wake the relay, publish healthy, steady wait) and SKIPS the Cognito
/// path for the tick.
///
/// Returns `None` — so the caller FALLS BACK to the Cognito pair-cli path — when:
///   - no device-JWT is held, or it's opaque/undecodable, or already expired
///     (self-refresh needs a currently-valid bearer; coord would 401 an expired
///     one, and an absent/expired JWT is precisely the Cognito-recovery case),
///   - coord returns ANY non-2xx (incl. 404 when the route isn't deployed yet —
///     ORDERING SAFETY: a runner talking to an old coord without this route
///     simply falls back to Cognito, so the runner half can ship before/without
///     the coord half), OR
///   - the network call fails or the body fails to decode.
///
/// REPLACE-not-REVOKE: on ANY failure the existing JWT is left UNTOUCHED. A
/// self-refresh miss means "try Cognito", NEVER "go credential-dark".
pub(crate) async fn try_device_self_refresh(
    auth_manager: &crate::auth::AuthManager,
    coord_base: &str,
    device_id: &str,
) -> Option<String> {
    // Precondition: a currently-valid (future-exp) device-JWT must be held —
    // coord authenticates this request with THAT bearer. An absent/opaque/
    // already-expired JWT cannot self-refresh, so hand off to Cognito.
    let current = auth_manager
        .get_access_token()
        .ok()
        .filter(|t| !t.trim().is_empty())?;
    match auth_manager.access_token_exp() {
        // Future exp → self-refreshable (even if within the refresh threshold).
        Some(exp) if chrono::Utc::now().timestamp() < exp => {}
        // Absent/opaque exp, or already expired → Cognito fallback.
        _ => return None,
    }

    let url = format!(
        "{}/devices/{}/refresh-token",
        coord_base.trim_end_matches('/'),
        device_id
    );
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("device_jwt_refresher: device self-refresh client build failed: {e} — falling back to Cognito");
            return None;
        }
    };
    let resp = match client.post(&url).bearer_auth(current.trim()).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(
                "device_jwt_refresher: device self-refresh request failed: {e} \
                 — falling back to Cognito"
            );
            return None;
        }
    };
    if !resp.status().is_success() {
        // 404 (route undeployed) / 401 / 5xx — all fall through to Cognito.
        // REPLACE-not-REVOKE: the existing JWT is left untouched.
        info!(
            "device_jwt_refresher: device self-refresh got HTTP {} \
             — falling back to Cognito (existing JWT preserved)",
            resp.status()
        );
        return None;
    }
    let body: DeviceRefreshResponse = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            warn!(
                "device_jwt_refresher: device self-refresh body decode failed: {e} \
                 — falling back to Cognito"
            );
            return None;
        }
    };
    if body.token.trim().is_empty() {
        warn!(
            "device_jwt_refresher: device self-refresh returned an empty token \
             — falling back to Cognito"
        );
        return None;
    }
    // Persist into the access_token slot (refresh-token slot stays empty — the
    // device-JWT lifecycle is coord-owned, not an OAuth refresh chain).
    match auth_manager.store_tokens(&body.token, "") {
        Ok(()) => {
            info!(
                "device_jwt_refresher: device-JWT self-refreshed login-independently (len={})",
                body.token.len()
            );
            Some(body.token)
        }
        Err(e) => {
            warn!(
                "device_jwt_refresher: persist self-refreshed JWT failed: {e} \
                 — falling back to Cognito (existing JWT preserved)"
            );
            None
        }
    }
}

// ===========================================================================
// Session-scoped multi-tenant per-tenant slot refresh (plan 2026-07-02, D4).
// Shipped behavior as of Phase 8a (the Phase-1 `QONTINUI_MULTI_TENANT_JWT`
// flag gate is retired): every `device_jwt:<tenant_id>` slot self-refreshes
// with its own claim each refresher iteration, independent of the legacy
// `access_token` slot (which the pre-existing paths above keep owning).
// ===========================================================================

/// How the `IdleWrongTier` arm should wait (Phase 8a). With per-tenant
/// slots held, the arm must wake on the steady cadence so the slot pass —
/// which runs once per loop iteration — can't be starved by an unbounded
/// block-until-kick (a wrong-tier runner may never receive a kick). With
/// no slots, `None` preserves the historical block-until-kick. Pure, so
/// the starvation fix is pinned by a unit test.
pub(crate) fn idle_wrong_tier_wait(has_tenant_slots: bool) -> Option<Duration> {
    has_tenant_slots.then_some(REFRESH_CHECK_INTERVAL)
}

/// What to do with one per-tenant device-JWT slot this pass. Pure output of
/// [`plan_tenant_slot`] so the staleness branching is unit-testable without
/// storage or a tokio runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TenantSlotPlan {
    /// Future-exp JWT within [`crate::auth::REFRESH_BEFORE_EXPIRY_SECS`] of
    /// expiry — self-refresh it now (presenting THAT slot's token).
    Refresh,
    /// Future-exp JWT with plenty of TTL left — leave it alone this pass.
    SkipFresh,
    /// Absent/opaque exp or already expired — NOT self-refreshable (coord
    /// would 401 the bearer). Skipped; the slot heals at the next pair for
    /// that tenant. REPLACE-not-REVOKE: never cleared here.
    SkipNotRefreshable,
}

/// Pure per-slot staleness decision. `exp` is the slot JWT's decoded
/// (unverified) `exp` claim; `now` is unix seconds.
pub(crate) fn plan_tenant_slot(exp: Option<i64>, now: i64) -> TenantSlotPlan {
    match exp {
        // Opaque/undecodable — cannot judge or present it for self-refresh.
        None => TenantSlotPlan::SkipNotRefreshable,
        // Already expired — coord would 401 the presented bearer.
        Some(e) if now >= e => TenantSlotPlan::SkipNotRefreshable,
        // Within TTL/3 of expiry — refresh now.
        Some(e) if now + crate::auth::REFRESH_BEFORE_EXPIRY_SECS >= e => TenantSlotPlan::Refresh,
        // Comfortably fresh.
        Some(_) => TenantSlotPlan::SkipFresh,
    }
}

/// Outcome of one per-tenant slot in a [`refresh_tenant_slots`] pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TenantSlotOutcome {
    /// Coord 2xx'd and the new JWT was persisted into that tenant's slot.
    Refreshed,
    /// Slot JWT still comfortably fresh — nothing to do.
    SkippedFresh,
    /// Slot empty/opaque/expired — not self-refreshable this pass.
    SkippedNotRefreshable,
    /// Refresh was attempted but failed (coord non-2xx, network error,
    /// decode failure, empty token, or persist error). The existing slot
    /// value is left UNTOUCHED (REPLACE-not-REVOKE), and the pass CONTINUES
    /// with the remaining slots.
    KeptExisting,
}

/// Flag-gated multi-tenant slot pass: walk every per-tenant device-JWT slot
/// and self-refresh each stale one via coord's
/// `POST /devices/{device_id}/refresh-token`, presenting THAT slot's token as
/// the bearer (coord re-mints from the presented claim's tenant — verified
/// plan premise, `tokens.rs:341-348`). Each slot succeeds or fails
/// independently: a failure on one slot never aborts the others, and the
/// legacy `access_token` slot is never read or written here.
///
/// Returns the per-tenant outcomes (deterministic slot order) for logging and
/// hermetic tests.
pub(crate) async fn refresh_tenant_slots(
    auth_manager: &crate::auth::AuthManager,
    coord_base: &str,
    device_id: &str,
) -> Vec<(uuid::Uuid, TenantSlotOutcome)> {
    let tenants = auth_manager.list_tenant_device_jwt_tenants();
    let mut outcomes = Vec::with_capacity(tenants.len());
    if tenants.is_empty() {
        return outcomes;
    }
    let url = format!(
        "{}/devices/{}/refresh-token",
        coord_base.trim_end_matches('/'),
        device_id
    );
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => Some(c),
        Err(e) => {
            warn!("device_jwt_refresher: tenant-slot refresh client build failed: {e}");
            None
        }
    };
    let now = chrono::Utc::now().timestamp();
    for tenant in tenants {
        let token = match auth_manager.get_tenant_device_jwt(&tenant) {
            Ok(Some(t)) if !t.trim().is_empty() => t.trim().to_string(),
            _ => {
                outcomes.push((tenant, TenantSlotOutcome::SkippedNotRefreshable));
                continue;
            }
        };
        match plan_tenant_slot(crate::auth::decode_jwt_exp(&token), now) {
            TenantSlotPlan::SkipFresh => {
                outcomes.push((tenant, TenantSlotOutcome::SkippedFresh));
                continue;
            }
            TenantSlotPlan::SkipNotRefreshable => {
                warn!(
                    "device_jwt_refresher: tenant {tenant} slot JWT is expired/opaque — \
                     cannot self-refresh; re-pair for that tenant to heal the slot"
                );
                outcomes.push((tenant, TenantSlotOutcome::SkippedNotRefreshable));
                continue;
            }
            TenantSlotPlan::Refresh => {}
        }
        let Some(client) = client.as_ref() else {
            outcomes.push((tenant, TenantSlotOutcome::KeptExisting));
            continue;
        };
        // Present THIS slot's token — coord re-mints for the claim's tenant.
        let resp = match client.post(&url).bearer_auth(&token).send().await {
            Ok(r) => r,
            Err(e) => {
                warn!("device_jwt_refresher: tenant {tenant} slot refresh request failed: {e}");
                outcomes.push((tenant, TenantSlotOutcome::KeptExisting));
                continue;
            }
        };
        if !resp.status().is_success() {
            warn!(
                "device_jwt_refresher: tenant {tenant} slot refresh got HTTP {} \
                 (existing slot JWT preserved)",
                resp.status()
            );
            outcomes.push((tenant, TenantSlotOutcome::KeptExisting));
            continue;
        }
        let body: DeviceRefreshResponse = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                warn!("device_jwt_refresher: tenant {tenant} slot refresh body decode failed: {e}");
                outcomes.push((tenant, TenantSlotOutcome::KeptExisting));
                continue;
            }
        };
        if body.token.trim().is_empty() {
            warn!("device_jwt_refresher: tenant {tenant} slot refresh returned an empty token");
            outcomes.push((tenant, TenantSlotOutcome::KeptExisting));
            continue;
        }
        match auth_manager.store_tenant_device_jwt(&tenant, &body.token) {
            Ok(()) => {
                info!(
                    "device_jwt_refresher: tenant {tenant} device-JWT slot refreshed (len={})",
                    body.token.len()
                );
                outcomes.push((tenant, TenantSlotOutcome::Refreshed));
            }
            Err(e) => {
                warn!("device_jwt_refresher: tenant {tenant} slot persist failed: {e}");
                outcomes.push((tenant, TenantSlotOutcome::KeptExisting));
            }
        }
    }
    outcomes
}

/// Ensure the Cognito (oauth) access token is fresh before it's used as the
/// pair-cli bearer. If a Cognito refresh token is stored and the access token
/// is within the refresh threshold (or already expired), POST the
/// `refresh_token` grant to Cognito and persist the new access/id tokens.
///
/// Best-effort: a refresh failure is logged and we fall through with whatever
/// is stored — `try_refresh_once` will surface a 401 from the web backend and
/// keep the existing device JWT (the REPLACE-not-REVOKE invariant).
///
/// Returns the bearer the device pair should present: the (possibly
/// refreshed) Cognito access token when a Cognito session exists, else `None`
/// (the caller falls back to the legacy device-JWT-slot bearer for installs
/// paired via local-login).
///
/// Exposed at `pub(crate)` so the web-backend-authenticated Tauri commands in
/// `commands::auth` (`check_auth_status`, `get_user_projects`) reuse the exact
/// same "refresh-first, then read" Cognito-bearer derivation the refresher uses
/// before a device re-bind. Single source of truth for "what user bearer do we
/// present to the web backend?".
pub(crate) async fn ensure_fresh_cognito_bearer(
    auth_manager: &crate::auth::AuthManager,
) -> Option<String> {
    refresh_cognito_bearer(auth_manager).await.0
}

/// The classifying core behind [`ensure_fresh_cognito_bearer`]. Returns the
/// bearer to present (same value the wrapper yields) AND a [`RefreshClass`] the
/// refresher loop uses to drive backoff + the credential-dark notification.
///
/// Phase 2: a failed Cognito refresh is no longer an opaque `warn!`-and-fall-
/// through. We classify the failure so the loop can:
///   - retry transient failures (network/5xx/429/unclassified) on a tightened
///     backoff (autonomy self-recovers fast), and
///   - treat a dead refresh token (`invalid_grant`) as terminal-headless:
///     notify the operator instead of silently retrying a grant that will keep
///     failing.
///
/// The bearer-returning contract is identical to the historical
/// `ensure_fresh_cognito_bearer`: a fresh token on success, the stored
/// (possibly stale) token on a refresh failure, `None` when there is no Cognito
/// session — so the REPLACE-not-REVOKE invariant holds (we never clear a stored
/// token on a transient error; `try_refresh_once` independently preserves the
/// device JWT).
pub(crate) async fn refresh_cognito_bearer(
    auth_manager: &crate::auth::AuthManager,
) -> (Option<String>, RefreshClass) {
    // No Cognito session → legacy/local-login install; let the caller use its
    // existing bearer source.
    let refresh_token = match auth_manager.get_oauth_refresh_token() {
        Ok(t) if !t.trim().is_empty() => t,
        _ => return (None, RefreshClass::NoSession),
    };

    if auth_manager.cognito_token_needs_refresh() {
        info!("device_jwt_refresher: Cognito access token stale — refreshing first");
        let rt = refresh_token.clone();
        let refreshed =
            tokio::task::spawn_blocking(move || qontinui_runner_lib::cognito::refresh_tokens(&rt))
                .await;
        match refreshed {
            Ok(Ok(resp)) => {
                let expires_at = chrono::Utc::now().timestamp() + resp.expires_in;
                // Cognito omits refresh_token on the refresh grant — keep the
                // existing one.
                let new_refresh = resp.refresh_token.unwrap_or(refresh_token);
                if let Err(e) = auth_manager.store_oauth_tokens(
                    &resp.access_token,
                    &resp.id_token,
                    &new_refresh,
                    expires_at,
                ) {
                    warn!("device_jwt_refresher: persist refreshed Cognito tokens failed: {e}");
                } else {
                    info!("device_jwt_refresher: Cognito access token refreshed");
                }
                return (Some(resp.access_token), RefreshClass::Ok);
            }
            Ok(Err(e)) => {
                // Classify: a dead refresh token is terminal-headless; anything
                // else is retryable. We still fall through with the stored
                // token (REPLACE-not-REVOKE).
                let class = if e.is_invalid_grant() {
                    warn!(
                        "device_jwt_refresher: Cognito refresh token expired/revoked \
                         (invalid_grant) — autonomy is credential-dark until the operator \
                         signs in again: {e}"
                    );
                    RefreshClass::Hard
                } else {
                    warn!(
                        "device_jwt_refresher: Cognito token refresh failed transiently: {e} \
                         — using stored token, will retry on backoff"
                    );
                    RefreshClass::Transient
                };
                return (auth_manager.get_oauth_access_token().ok(), class);
            }
            Err(join_err) => {
                // A spawn_blocking join failure is an internal/transient fault.
                warn!("device_jwt_refresher: Cognito refresh task join failed: {join_err}");
                return (
                    auth_manager.get_oauth_access_token().ok(),
                    RefreshClass::Transient,
                );
            }
        }
    }

    // Fresh enough — use whatever access token is stored.
    (auth_manager.get_oauth_access_token().ok(), RefreshClass::Ok)
}

async fn refresher_loop(
    api_state: Arc<ApiState>,
    mut shutdown_rx: watch::Receiver<bool>,
    mut kick_rx: watch::Receiver<u64>,
) {
    let auth_manager = crate::auth::AuthManager::new();
    info!("Device-JWT refresher started (check interval = 5m, threshold = 80m)");

    // Phase 2: carries the transient-failure backoff + the credential-dark
    // notify-once dedup across iterations.
    let mut backoff = RefreshBackoff::default();

    loop {
        if *shutdown_rx.borrow() {
            info!("Device-JWT refresher shutting down");
            return;
        }

        // Snapshot settings + needs-refresh decision once per iteration.
        let settings_snapshot = settings::load_settings();
        let needs_refresh_result = auth_manager.device_jwt_needs_refresh();
        let needs_refresh = match needs_refresh_result {
            Ok(b) => b,
            Err(e) => {
                warn!("device_jwt_refresher: device_jwt_needs_refresh failed: {e}");
                // Sleep before retrying to avoid a hot-loop on persistent error.
                if wait_with_signals(REFRESH_CHECK_INTERVAL, &mut shutdown_rx, &mut kick_rx).await {
                    return;
                }
                continue;
            }
        };

        let decision = next_action(settings_snapshot.tier, needs_refresh);

        // Session-scoped multi-tenant Phase 8a (plan 2026-07-02, D4): the
        // per-tenant device-JWT slot pass is the SHIPPED behavior (the
        // Phase-1 `QONTINUI_MULTI_TENANT_JWT` flag is retired). Every slot
        // self-refreshes with ITS OWN claim, independently of the legacy
        // `access_token` slot's own staleness/decision below; the
        // machine.json tenant-precedence chain (`resolve_pair_tenant_id`)
        // only ever feeds the DEFAULT slot's Cognito pair path. Placed
        // before the decision match so the Idle arm (legacy JWT fresh)
        // still ticks the tenant slots each cadence/kick. Runners with no
        // tenant slots (never paired post-8a) skip in one storage read.
        let has_tenant_slots = !auth_manager.list_tenant_device_jwt_tenants().is_empty();
        if has_tenant_slots {
            let mt_device_id = std::env::var("QONTINUI_MACHINE_ID")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .or_else(|| qontinui_runner_lib::pair::read_device_id_from_disk().ok());
            match mt_device_id {
                Some(did) => {
                    let coord_base = crate::coord_mcp::coord_base_url();
                    let outcomes = refresh_tenant_slots(&auth_manager, &coord_base, &did).await;
                    if !outcomes.is_empty() {
                        let refreshed = outcomes
                            .iter()
                            .filter(|(_, o)| *o == TenantSlotOutcome::Refreshed)
                            .count();
                        info!(
                            "device_jwt_refresher: multi-tenant slot pass — {refreshed}/{} \
                             refreshed ({outcomes:?})",
                            outcomes.len()
                        );
                    }
                }
                None => warn!(
                    "device_jwt_refresher: tenant device-JWT slots present but no device_id \
                     (QONTINUI_MACHINE_ID unset, machine.json unreadable) — skipping \
                     tenant-slot pass"
                ),
            }
        }

        match decision {
            Decision::IdleWrongTier => {
                // Phase 1b: publish the credential-dark signal so coord's
                // fleet `evaluate()` lights up a device-scoped alert. Best-
                // effort — never blocks the idle wait below.
                publish_coord_credential_status(
                    &auth_manager,
                    &coord_credential_health(decision, None),
                )
                .await;
                // Phase 8a: with tenant slots held, a wrong-tier idle must
                // NOT park indefinitely — the slot pass above only runs per
                // loop iteration, so an unbounded block would starve slot
                // refreshes until a kick that may never come. Bounded wait
                // instead (kick/shutdown still wake early). Slot-less
                // runners keep the historical block-until-kick behavior.
                if let Some(wait) = idle_wrong_tier_wait(has_tenant_slots) {
                    if wait_with_signals(wait, &mut shutdown_rx, &mut kick_rx).await {
                        info!("Device-JWT refresher shutting down (was idle on non-Tier2)");
                        return;
                    }
                    continue;
                }
                // Nothing to do until tier changes. Block on shutdown or
                // kick (set_runner_tier kicks us on every transition).
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        info!("Device-JWT refresher shutting down (was idle on non-Tier2)");
                        return;
                    }
                    _ = kick_rx.changed() => continue,
                }
            }
            Decision::Idle => {
                // Phase 1b: a fresh JWT → publish ok so any stale alert self-
                // clears on coord's next firing-set reconcile.
                publish_coord_credential_status(
                    &auth_manager,
                    &coord_credential_health(decision, None),
                )
                .await;
                // JWT is fresh — sleep until next check or wake on kick.
                if wait_with_signals(REFRESH_CHECK_INTERVAL, &mut shutdown_rx, &mut kick_rx).await {
                    return;
                }
                continue;
            }
            Decision::Pair => {
                // Phase 4a: PREFER a login-independent DEVICE self-refresh. When
                // the runner still holds a currently-valid device-JWT, re-mint it
                // by presenting that JWT to coord's
                // `POST /devices/:id/refresh-token` (NO Cognito touched) — this
                // keeps a continuously-running runner autonomous past the ~30-day
                // Cognito refresh-token ceiling. Only when no valid device-JWT is
                // held (expired/absent), OR coord doesn't support the route yet
                // (404 → fall through, ordering-safe), do we run the Cognito
                // pair-cli path below. device_id: env override first (multi-
                // instance / test), else machine.json — same resolution as the
                // status publisher.
                let self_refresh_device_id = std::env::var("QONTINUI_MACHINE_ID")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().to_string())
                    .or_else(|| qontinui_runner_lib::pair::read_device_id_from_disk().ok());
                if let Some(did) = self_refresh_device_id {
                    let coord_base = crate::coord_mcp::coord_base_url();
                    if let Some(new_jwt) =
                        try_device_self_refresh(&auth_manager, &coord_base, &did).await
                    {
                        info!(
                            "device_jwt_refresher: device-JWT self-refreshed (len={}) \
                             — Cognito path skipped this tick",
                            new_jwt.len()
                        );
                        // Same downstream handling as a successful Cognito re-mint:
                        // wake the relay so it reconnects with the new JWT, publish
                        // healthy, reset backoff, and fall to the steady cadence.
                        crate::mcp::backend_relay::commands::kick_cloud_relay().await;
                        publish_coord_credential_status(
                            &auth_manager,
                            &coord_credential_health(decision, Some(PairProgress::Healthy)),
                        )
                        .await;
                        // A healthy tick: reset transient backoff + emit "resumed"
                        // if we were previously credential-dark.
                        let action = plan_refresh_wait(&mut backoff, RefreshClass::Ok);
                        if action.notify_recovered {
                            emit_credential_dark(&api_state.app_handle, false);
                        }
                        if wait_with_signals(action.wait, &mut shutdown_rx, &mut kick_rx).await {
                            return;
                        }
                        continue;
                    }
                }

                // The web backend's pair-cli endpoint gates on the user
                // bearer (`Authorization: Bearer <user-token>`). Phase 5
                // (unified-Cognito-identity): when the runner was signed in
                // via Cognito, the bearer is the Cognito **access token** —
                // refreshed first if it's stale (so the re-bind presents a
                // valid user token). For legacy/local-login installs there's
                // no Cognito session, so we fall back to the device-JWT slot
                // bearer (the historical Phase-2 source).
                // Phase 2: classify the Cognito refresh so we can back off on a
                // transient blip and fire the credential-dark notification on a
                // dead refresh token (`invalid_grant`) instead of silently
                // stalling. `refresh_class` drives the wait + notification at
                // each exit below.
                let (cognito_bearer, refresh_class) = refresh_cognito_bearer(&auth_manager).await;
                let bearer_token = match cognito_bearer {
                    Some(t) if !t.trim().is_empty() => t.trim().to_string(),
                    _ => match auth_manager.get_access_token() {
                        Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
                        _ => {
                            warn!(
                                "device_jwt_refresher: no Cognito session and access_token slot \
                                 empty — user must sign in to Qontinui before the refresher can pair"
                            );
                            // Phase 1b: credential-dark (not signed in).
                            publish_coord_credential_status(
                                &auth_manager,
                                &coord_credential_health(
                                    decision,
                                    Some(PairProgress::BailNoBearer),
                                ),
                            )
                            .await;
                            // Phase 2: if the refresh token is dead
                            // (`invalid_grant`) and we also have no device-JWT
                            // bearer, fire the credential-dark notification +
                            // back off per classification; otherwise (no session)
                            // fall to the steady cadence.
                            let action = plan_refresh_wait(&mut backoff, refresh_class);
                            if action.notify_dark {
                                emit_credential_dark(&api_state.app_handle, true);
                            }
                            if wait_with_signals(action.wait, &mut shutdown_rx, &mut kick_rx).await
                            {
                                return;
                            }
                            continue;
                        }
                    },
                };
                let pair_base = settings_snapshot
                    .web_integration
                    .backend_url
                    .trim()
                    .trim_end_matches('/')
                    .to_string();
                if pair_base.is_empty() {
                    warn!("device_jwt_refresher: backend_url empty — cannot pair");
                    if wait_with_signals(REFRESH_CHECK_INTERVAL, &mut shutdown_rx, &mut kick_rx)
                        .await
                    {
                        return;
                    }
                    continue;
                }

                // Resolve device_id + user_id from disk. Phase 5 split:
                // these reads live here (operator-facing files) so the
                // factored `try_refresh_once` stays hermetic + testable.
                let device_id = match qontinui_runner_lib::pair::read_device_id_from_disk() {
                    Ok(d) => d,
                    Err(e) => {
                        warn!(
                            "device_jwt_refresher: machine.json unreadable: {e} \
                             — run `qontinui_profile device init` to create it"
                        );
                        if wait_with_signals(REFRESH_CHECK_INTERVAL, &mut shutdown_rx, &mut kick_rx)
                            .await
                        {
                            return;
                        }
                        continue;
                    }
                };
                let user_id = match qontinui_runner_lib::pair::read_paired_user_id_from_disk() {
                    Some(u) => u,
                    None => {
                        // No paired-user record yet — this runner hasn't
                        // completed a pairing (Cognito sign-in or pair-code),
                        // so there's nothing to re-mint. Log + back off; the
                        // next pairing writes the file and a kick wakes us.
                        warn!(
                            "device_jwt_refresher: paired_user.json missing — \
                             runner not paired yet (refresher idling until kick)"
                        );
                        // Phase 1b: not paired → credential-dark (not signed in).
                        publish_coord_credential_status(
                            &auth_manager,
                            &coord_credential_health(decision, Some(PairProgress::BailNoBearer)),
                        )
                        .await;
                        if wait_with_signals(REFRESH_CHECK_INTERVAL, &mut shutdown_rx, &mut kick_rx)
                            .await
                        {
                            return;
                        }
                        continue;
                    }
                };

                // Phase 1b: resolve the tenant ONCE up front so the post-outcome
                // health mapping can distinguish "no tenant resolved" (gate-
                // blocking config) from "tenant fine, the mint just failed"
                // (degraded). `try_refresh_once` re-resolves internally; the two
                // resolutions agree because both call `resolve_active_tenant_id`
                // + the same OAuth/outgoing-JWT fallback chain.
                let machine_tenant = crate::session::dual_write::resolve_active_tenant_id();
                let outgoing_jwt = auth_manager.get_access_token().ok();
                let tenant_resolved =
                    resolve_pair_tenant_id(&bearer_token, outgoing_jwt.as_deref(), machine_tenant)
                        .is_some();

                // Phase 5.2: try_refresh_once encapsulates the
                // pair-cli HTTP call + JWT persistence. It preserves
                // the existing JWT on any non-2xx outcome.
                let outcome = try_refresh_once(
                    &auth_manager,
                    &pair_base,
                    &bearer_token,
                    &device_id,
                    &user_id,
                    machine_tenant,
                )
                .await;
                let progress = match &outcome {
                    RefreshOutcome::Replaced { new_jwt } => {
                        info!(
                            "device_jwt_refresher: device-JWT refreshed (len={})",
                            new_jwt.len()
                        );
                        // Wake the relay so it reconnects with the new JWT.
                        crate::mcp::backend_relay::commands::kick_cloud_relay().await;
                        PairProgress::Healthy
                    }
                    RefreshOutcome::KeptExisting | RefreshOutcome::PersistFailed(_) => {
                        if let RefreshOutcome::PersistFailed(e) = &outcome {
                            warn!("device_jwt_refresher: persist new JWT failed: {e}");
                        }
                        // The mint did NOT advance the slot. Classify why:
                        //   - no tenant resolvable → gate-blocking config red,
                        //   - tenant fine but the slot JWT is now expired/absent
                        //     → degraded "credential-dark" red,
                        //   - tenant fine and the slot JWT is still valid →
                        //     healthy (a transient coord 5xx we'll retry).
                        if !tenant_resolved {
                            PairProgress::BailNoTenant
                        } else if slot_jwt_is_expired_or_absent(&auth_manager) {
                            PairProgress::BailRefreshFailedExpired
                        } else {
                            PairProgress::Healthy
                        }
                    }
                };
                publish_coord_credential_status(
                    &auth_manager,
                    &coord_credential_health(decision, Some(progress)),
                )
                .await;

                // Phase 2: drive the wait + credential-dark notification off the
                // Cognito-refresh classification. Transient → shortened backoff
                // (autonomy self-recovers fast once connectivity returns); Hard
                // (`invalid_grant`) → steady cadence + notify-once; Ok → steady
                // cadence, emitting "resumed" if we were previously dark.
                let action = plan_refresh_wait(&mut backoff, refresh_class);
                if action.notify_dark {
                    emit_credential_dark(&api_state.app_handle, true);
                }
                if action.notify_recovered {
                    emit_credential_dark(&api_state.app_handle, false);
                }
                if wait_with_signals(action.wait, &mut shutdown_rx, &mut kick_rx).await {
                    return;
                }
                continue;
            }
        }
    }
}

/// Sleep `dur`, but wake early on shutdown or kick. Returns `true` iff
/// the loop should `return` (shutdown received).
async fn wait_with_signals(
    dur: Duration,
    shutdown_rx: &mut watch::Receiver<bool>,
    kick_rx: &mut watch::Receiver<u64>,
) -> bool {
    tokio::select! {
        _ = shutdown_rx.changed() => true,
        _ = kick_rx.changed() => false,
        _ = tokio::time::sleep(dur) => false,
    }
}

/// Global state holder + public surface. Same shape as
/// `backend_relay::commands` so the call sites read consistently.
pub mod commands {
    use super::*;
    use std::sync::OnceLock;

    static REFRESHER_STATE: OnceLock<tokio::sync::Mutex<Option<Arc<RefresherState>>>> =
        OnceLock::new();

    fn get_holder() -> &'static tokio::sync::Mutex<Option<Arc<RefresherState>>> {
        REFRESHER_STATE.get_or_init(|| tokio::sync::Mutex::new(None))
    }

    /// Idempotent start. If a live task already exists, kick it instead
    /// of spawning a duplicate. Called from `mcp_api::start_server` once
    /// `Arc<ApiState>` is available.
    pub async fn auto_start_device_jwt_refresher(api_state: Arc<ApiState>) {
        let mut guard = get_holder().lock().await;

        if let Some(ref existing) = *guard {
            let handle_guard = existing.task_handle.lock().await;
            let is_alive = handle_guard.as_ref().is_some_and(|h| !h.is_finished());
            drop(handle_guard);
            if is_alive {
                info!("Device-JWT refresher already running; kicking to re-read state");
                existing.kick();
                return;
            }
            info!("Device-JWT refresher task has ended, restarting...");
            existing.stop().await;
            *guard = None;
        }

        info!("Starting device-JWT refresher");
        let state = start_refresher(api_state);
        *guard = Some(state);
    }

    /// Kick the refresher: interrupt any in-progress sleep so the next
    /// iteration runs immediately. Used by `set_runner_tier` on
    /// transition into Tier 2, and by any future code that updates the
    /// runner_token or coord_url.
    pub async fn kick_device_jwt_refresher() {
        let guard = get_holder().lock().await;
        if let Some(ref state) = *guard {
            state.kick();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decides_no_refresh_when_jwt_fresh() {
        // Tier 2 + needs_refresh=false → Idle (no work).
        let d = next_action(RunnerTier::QontinuiAccount, false);
        assert_eq!(d, Decision::Idle);
    }

    #[test]
    fn decides_pair_when_jwt_stale() {
        let d = next_action(RunnerTier::QontinuiAccount, true);
        assert_eq!(d, Decision::Pair);
    }

    #[test]
    fn decides_pair_for_cognito_runner_with_empty_runner_token() {
        // Regression: a Cognito-/pair-code-paired runner has an EMPTY
        // `web_integration.runner_token` but a valid Cognito/device bearer.
        // The refresher must still attempt to re-mint (Pair) — it must NOT
        // idle on the missing runner_token (which let the device JWT expire
        // and silently dropped the runner off the cloud after ~one TTL).
        // `next_action` no longer consults runner_token at all; the `Pair`
        // arm resolves the real bearer and idles only if none exists.
        let d = next_action(RunnerTier::QontinuiAccount, true);
        assert_eq!(d, Decision::Pair);
    }

    #[test]
    fn decides_idle_when_tier_not_qontinui_account() {
        // LocalProvider: not Tier 2 — refresher idles regardless of
        // whether the JWT is stale.
        let d = next_action(RunnerTier::LocalProvider, true);
        assert_eq!(d, Decision::IdleWrongTier);
        let d2 = next_action(RunnerTier::Local, true);
        assert_eq!(d2, Decision::IdleWrongTier);
        // Even with no needs-refresh signal, wrong-tier still wins.
        let d3 = next_action(RunnerTier::LocalProvider, false);
        assert_eq!(d3, Decision::IdleWrongTier);
    }

    // ---- coord_credential_health mapping (pure, Phase 1b) ----

    #[test]
    fn health_idle_is_ok() {
        let h = coord_credential_health(Decision::Idle, None);
        assert!(h.ok);
        assert_eq!(h.reason, None);
    }

    #[test]
    fn health_wrong_tier_is_red_with_tier_reason() {
        let h = coord_credential_health(Decision::IdleWrongTier, None);
        assert!(!h.ok);
        assert_eq!(
            h.reason.as_deref(),
            Some("runner tier is not Qontinui account")
        );
    }

    #[test]
    fn health_pair_healthy_is_ok() {
        // A successful Replaced / still-valid KeptExisting → ok.
        let h = coord_credential_health(Decision::Pair, Some(PairProgress::Healthy));
        assert!(h.ok);
        assert_eq!(h.reason, None);
        // And `None` progress (defensive default) is also ok, never a false red.
        let h2 = coord_credential_health(Decision::Pair, None);
        assert!(h2.ok);
    }

    #[test]
    fn health_pair_bail_no_bearer_is_red_signin() {
        let h = coord_credential_health(Decision::Pair, Some(PairProgress::BailNoBearer));
        assert!(!h.ok);
        assert!(
            h.reason.as_deref().unwrap().contains("sign in"),
            "no-bearer reason must name the sign-in failing source, got {:?}",
            h.reason
        );
    }

    #[test]
    fn health_pair_bail_no_tenant_is_red_tenant() {
        let h = coord_credential_health(Decision::Pair, Some(PairProgress::BailNoTenant));
        assert!(!h.ok);
        assert!(
            h.reason.as_deref().unwrap().contains("tenant"),
            "no-tenant reason must name the tenant failing source, got {:?}",
            h.reason
        );
    }

    #[test]
    fn health_pair_bail_refresh_failed_expired_is_red_dark() {
        let h =
            coord_credential_health(Decision::Pair, Some(PairProgress::BailRefreshFailedExpired));
        assert!(!h.ok);
        assert!(
            h.reason.as_deref().unwrap().contains("credential-dark"),
            "failed-mint-expired reason must flag credential-dark, got {:?}",
            h.reason
        );
    }

    #[test]
    fn health_serializes_to_ok_reason_shape() {
        // Wire contract: `{ "ok": <bool>, "reason": <string|null> }`. The
        // `ok` case skips `reason` (Option::is_none), which deserializes back
        // to null on the coord side — the consumer treats absent == null.
        let ok = serde_json::to_value(coord_credential_health(Decision::Idle, None)).unwrap();
        assert_eq!(ok["ok"], serde_json::Value::Bool(true));
        assert!(ok.get("reason").is_none(), "ok health omits reason");

        let red =
            serde_json::to_value(coord_credential_health(Decision::IdleWrongTier, None)).unwrap();
        assert_eq!(red["ok"], serde_json::Value::Bool(false));
        assert!(
            red["reason"].is_string(),
            "red health carries a reason string"
        );
    }

    // ---- resolve_pair_tenant_id ordering (pure, no disk / no AuthManager) ----

    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

    /// Build a JWT-shaped token. When `tenant` is `Some`, embed it as the
    /// `tenant_id` claim; when `None`, omit the claim entirely (mirrors the
    /// operator's Cognito token that broke prod).
    fn jwt(tenant: Option<&str>) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = match tenant {
            Some(t) => URL_SAFE_NO_PAD.encode(format!(r#"{{"sub":"x","tenant_id":"{t}"}}"#)),
            None => URL_SAFE_NO_PAD.encode(br#"{"sub":"x"}"#),
        };
        format!("{header}.{payload}.sig")
    }

    const T_OAUTH: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const T_JWT: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const T_MACHINE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

    #[test]
    fn tenant_prefers_oauth_claim_over_all_fallbacks() {
        let machine = uuid::Uuid::parse_str(T_MACHINE).unwrap();
        let got =
            resolve_pair_tenant_id(&jwt(Some(T_OAUTH)), Some(&jwt(Some(T_JWT))), Some(machine))
                .expect("resolves");
        assert_eq!(got.0, uuid::Uuid::parse_str(T_OAUTH).unwrap());
        assert_eq!(got.1, TenantSource::OAuthClaim);
    }

    #[test]
    fn tenant_falls_back_to_outgoing_jwt_when_oauth_claim_absent() {
        // The prod scenario: Cognito bearer has NO tenant_id, but the device
        // JWT we're about to replace still carries one.
        let machine = uuid::Uuid::parse_str(T_MACHINE).unwrap();
        let got = resolve_pair_tenant_id(&jwt(None), Some(&jwt(Some(T_JWT))), Some(machine))
            .expect("resolves");
        assert_eq!(got.0, uuid::Uuid::parse_str(T_JWT).unwrap());
        assert_eq!(got.1, TenantSource::OutgoingDeviceJwt);
    }

    #[test]
    fn tenant_falls_back_to_machine_json_when_no_jwt_claims() {
        // OAuth claim absent AND the outgoing JWT has no tenant claim →
        // machine.json::active_tenant_id wins.
        let machine = uuid::Uuid::parse_str(T_MACHINE).unwrap();
        let got =
            resolve_pair_tenant_id(&jwt(None), Some(&jwt(None)), Some(machine)).expect("resolves");
        assert_eq!(got.0, machine);
        assert_eq!(got.1, TenantSource::MachineJson);
        // Also works when there's no outgoing JWT at all.
        let got2 =
            resolve_pair_tenant_id(&jwt(None), None, Some(machine)).expect("resolves w/o jwt");
        assert_eq!(got2.1, TenantSource::MachineJson);
    }

    #[test]
    fn tenant_none_when_all_sources_absent() {
        // No OAuth claim, no/claimless outgoing JWT, no machine.json → None
        // (caller keeps the existing JWT — the preserved historical behavior).
        assert!(resolve_pair_tenant_id(&jwt(None), Some(&jwt(None)), None).is_none());
        assert!(resolve_pair_tenant_id(&jwt(None), None, None).is_none());
        // A non-JWT opaque OAuth token + nothing else → None.
        assert!(resolve_pair_tenant_id("opaque-not-a-jwt", None, None).is_none());
    }

    #[test]
    fn refresh_check_interval_is_five_minutes() {
        // Pin the constant so a future refactor that "tunes" it has to
        // update this test (and explain why in review).
        assert_eq!(REFRESH_CHECK_INTERVAL, Duration::from_secs(300));
    }

    // ---- Phase 2: transient backoff + credential-dark notify decision ----

    #[test]
    fn transient_backoff_is_capped_exponential() {
        // 15s, 30s, 60s, 120s, then pinned at the 120s ceiling.
        assert_eq!(transient_backoff(0), Duration::from_secs(15));
        assert_eq!(transient_backoff(1), Duration::from_secs(30));
        assert_eq!(transient_backoff(2), Duration::from_secs(60));
        assert_eq!(transient_backoff(3), Duration::from_secs(120));
        assert_eq!(transient_backoff(4), Duration::from_secs(120));
        // Never panics / overflows for a large failure count, stays at ceiling.
        assert_eq!(transient_backoff(1000), Duration::from_secs(120));
        // And the ceiling is well under the steady cadence (fast recovery).
        assert!(TRANSIENT_BACKOFF_MAX < REFRESH_CHECK_INTERVAL);
    }

    #[test]
    fn transient_grows_then_resets_on_success() {
        let mut s = RefreshBackoff::default();
        // Each consecutive transient failure grows the wait...
        let a0 = plan_refresh_wait(&mut s, RefreshClass::Transient);
        assert_eq!(a0.wait, Duration::from_secs(15));
        assert!(
            !a0.notify_dark && !a0.notify_recovered,
            "transient never notifies"
        );
        let a1 = plan_refresh_wait(&mut s, RefreshClass::Transient);
        assert_eq!(a1.wait, Duration::from_secs(30));
        let a2 = plan_refresh_wait(&mut s, RefreshClass::Transient);
        assert_eq!(a2.wait, Duration::from_secs(60));
        // ...then a success resets to the steady cadence AND the counter.
        let ok = plan_refresh_wait(&mut s, RefreshClass::Ok);
        assert_eq!(ok.wait, REFRESH_CHECK_INTERVAL);
        assert!(
            !ok.notify_recovered,
            "Ok with no prior dark episode emits nothing"
        );
        // The next transient starts again from the floor (counter reset).
        let again = plan_refresh_wait(&mut s, RefreshClass::Transient);
        assert_eq!(again.wait, Duration::from_secs(15));
    }

    #[test]
    fn hard_failure_uses_steady_wait_and_notifies_once() {
        let mut s = RefreshBackoff::default();
        // First hard failure → steady cadence + the dark notification.
        let first = plan_refresh_wait(&mut s, RefreshClass::Hard);
        assert_eq!(
            first.wait, REFRESH_CHECK_INTERVAL,
            "hard must NOT hot-retry"
        );
        assert!(
            first.notify_dark,
            "first dark transition fires the notification"
        );
        assert!(!first.notify_recovered);
        // Subsequent hard ticks must NOT re-notify (deduped per dark episode).
        let second = plan_refresh_wait(&mut s, RefreshClass::Hard);
        assert!(
            !second.notify_dark,
            "credential-dark notify is once per transition"
        );
        assert_eq!(second.wait, REFRESH_CHECK_INTERVAL);
    }

    #[test]
    fn recovery_after_dark_emits_recovered_once() {
        let mut s = RefreshBackoff::default();
        let _ = plan_refresh_wait(&mut s, RefreshClass::Hard); // go dark
                                                               // A later success emits the recovered notification exactly once.
        let recovered = plan_refresh_wait(&mut s, RefreshClass::Ok);
        assert!(
            recovered.notify_recovered,
            "recovery from dark emits resumed"
        );
        assert!(!recovered.notify_dark);
        // A second consecutive Ok no longer re-emits recovered.
        let steady = plan_refresh_wait(&mut s, RefreshClass::Ok);
        assert!(
            !steady.notify_recovered,
            "recovered fires once per transition"
        );
        // And a fresh hard transition can fire dark again (state was cleared).
        let dark_again = plan_refresh_wait(&mut s, RefreshClass::Hard);
        assert!(dark_again.notify_dark);
    }

    #[test]
    fn no_session_is_steady_and_silent() {
        // A full sign-out / legacy install must not claim "resumed" and must
        // not fast-retry — there's no Cognito session to refresh.
        let mut s = RefreshBackoff::default();
        let _ = plan_refresh_wait(&mut s, RefreshClass::Hard); // was dark
        let ns = plan_refresh_wait(&mut s, RefreshClass::NoSession);
        assert_eq!(ns.wait, REFRESH_CHECK_INTERVAL);
        assert!(
            !ns.notify_dark && !ns.notify_recovered,
            "no-session is silent"
        );
    }

    #[test]
    fn transient_after_dark_does_not_emit_recovered() {
        // A transient blip while dark must NOT prematurely signal "resumed"
        // (we only recovered once a refresh actually succeeds → Ok).
        let mut s = RefreshBackoff::default();
        let _ = plan_refresh_wait(&mut s, RefreshClass::Hard);
        let t = plan_refresh_wait(&mut s, RefreshClass::Transient);
        assert!(!t.notify_recovered && !t.notify_dark);
        // Still dark → a subsequent success emits recovered.
        let ok = plan_refresh_wait(&mut s, RefreshClass::Ok);
        assert!(ok.notify_recovered);
    }
}

// ============================================================================
// PHASE 5 DEFERRED — live-stack scenarios outside this PR's reach
// ============================================================================
//
// PHASE 5 DEFERRED: the following acceptance-criteria scenarios are NOT
// exercised by the in-process tests above. They require a live web +
// live coord stack (or coord-side state coordination that the runner has
// no hooks into), and are out of scope for the calibrated Phase 5 PR.
// They MUST be exercised manually before the unified-devices migration
// is tagged as fully shipped:
//
//   1. Fresh-pair browser flow E2E. Operator opens the runner with no
//      paired_user.json on disk, clicks the Connection Wizard's
//      "browser pair" button, completes the web /connect-runner flow,
//      and verifies (a) the runner_token round-trips back through the
//      localhost callback, (b) coord mints a device-JWT, (c) the relay
//      reconnects with the new JWT, (d) the runner appears on the
//      qontinui-web "Connected runners" list. Needs a live web
//      /connect-runner and a live coord — driven by `manual-test-loop`.
//
//   2. Clock-skew across runner/web. The runner's local-exp check
//      (auth::device_jwt_needs_refresh) trusts the JWT's `exp` claim
//      without verifying signature, but web's JWKS verifier on the WS
//      handshake DOES verify the signature against coord's public key.
//      A skewed runner clock can produce a verdict mismatch (local says
//      "fresh," remote says "expired" → 401 spam). Operator changes
//      system clock by +/-15 minutes and verifies the relay still
//      reconnects cleanly via the refresher's 401-handler kick path.
//      Needs JWKS verifier on web; not a runner concern.
//
//   3. JWKS rotation. Coord rotates its signing key; in-flight JWTs
//      minted under the old key continue to verify for the JWKS TTL,
//      after which web rejects them with 401. Verifies coord-side
//      key-rotation deployment — out of scope for the runner.
//
//   4. runner_token revocation. Operator revokes the runner_token via
//      the web UI; the refresher's next pair-cli call must return
//      401 (verified by Phase 5.1 above), the relay must NOT clear the
//      existing JWT until it naturally expires (verified by Phase 5.2's
//      `refresher_handles_coord_401_without_clearing_jwt`), and after
//      JWT expiry the runner must surface "Re-pair required" in the
//      Settings UI instead of 401-spinning. Needs web + coord state
//      coordination.
//
// Do NOT delete this block — it's the migration acceptance criteria and
// the discoverable record of what live-stack work still owes the user.
//
// ============================================================================

#[cfg(test)]
mod try_refresh_once_tests {
    //! Phase 5.2 integration tests — `try_refresh_once` against an
    //! in-process mock web backend. These exercise the JWT-preservation
    //! invariant: a non-2xx coord response MUST NOT clear the existing
    //! access_token slot.
    //!
    //! Mock web backend: inline axum server on `127.0.0.1:0`, same pattern as
    //! `pair::pair_e2e_tests`. AuthManager: `with_storage(...)` +
    //! `SecureStorage::with_path(<temp_file>)` so each test has its own
    //! isolated tokens file.

    use super::*;
    use axum::{
        body::Bytes,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
        Router,
    };
    use std::sync::{Arc, Mutex};

    fn b64url(b: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
    }

    /// Mint a synthetic JWT carrying `exp` so `device_jwt_needs_refresh`
    /// can decode it. Signature isn't verified by the runner, so a
    /// placeholder is fine.
    fn synth_jwt(exp: i64) -> String {
        let header = b64url(b"{\"alg\":\"EdDSA\",\"typ\":\"JWT\"}");
        let payload = b64url(format!("{{\"exp\":{}}}", exp).as_bytes());
        let sig = b64url(b"fake-sig");
        format!("{header}.{payload}.{sig}")
    }

    /// Per-test isolated AuthManager. Each test name maps to its own
    /// `.enc` file under the OS temp dir.
    fn test_auth_manager(name: &str) -> crate::auth::AuthManager {
        let dir = std::env::temp_dir().join("qontinui_test_refresher");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{name}.enc"));
        let _ = std::fs::remove_file(&path);
        let storage = crate::secure_storage::SecureStorage::with_path(path).expect("storage");
        crate::auth::AuthManager::with_storage(storage)
    }

    #[derive(Clone)]
    struct MockState {
        status: StatusCode,
        body: String,
        hits: Arc<Mutex<u32>>,
        last_body: Arc<Mutex<Option<String>>>,
    }

    async fn handler(State(s): State<MockState>, _h: HeaderMap, b: Bytes) -> (StatusCode, String) {
        *s.hits.lock().unwrap() += 1;
        *s.last_body.lock().unwrap() = Some(String::from_utf8_lossy(&b).to_string());
        (s.status, s.body.clone())
    }

    /// Captured per-request server-side state: how many times pair-cli was
    /// hit and the body of the most recent request (so a test can assert the
    /// forwarded `tenant_id`).
    struct MockCapture {
        hits: Arc<Mutex<u32>>,
        last_body: Arc<Mutex<Option<String>>>,
    }

    fn spawn_mock(
        status: StatusCode,
        body: String,
    ) -> (String, MockCapture, tokio::sync::oneshot::Sender<()>) {
        let hits = Arc::new(Mutex::new(0u32));
        let last_body = Arc::new(Mutex::new(None));
        let hits_for_handler = hits.clone();
        let last_body_for_handler = last_body.clone();
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = std_listener.local_addr().expect("addr").port();
        std_listener.set_nonblocking(true).expect("nb");
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rt");
            rt.block_on(async move {
                let state = MockState {
                    status,
                    body,
                    hits: hits_for_handler,
                    last_body: last_body_for_handler,
                };
                let app: Router = Router::new()
                    // Mirror the live route — pair::pair_with_auth_token_with_ids
                    // POSTs to `{base}/api/v1/devices/pair-cli` (web-routed).
                    // Was registered as `/coord/devices/pair-cli` (legacy
                    // coord-direct) and 404'd the request, panicking both
                    // try_refresh_once_tests. Sibling of the pair.rs:980 fix.
                    .route("/api/v1/devices/pair-cli", post(handler))
                    .with_state(state);
                let listener =
                    tokio::net::TcpListener::from_std(std_listener).expect("tokio listener");
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = rx.await;
                    })
                    .await;
            });
        });
        std::thread::sleep(Duration::from_millis(50));
        (
            format!("http://127.0.0.1:{port}"),
            MockCapture { hits, last_body },
            tx,
        )
    }

    const DID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const UID: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

    /// JWT-shaped runner token whose payload carries a `tenant_id` claim.
    /// `try_refresh_once` extracts the tenant_id from the OAuth/runner
    /// token's payload (Phase 2 of the default-tenant-propagation plan),
    /// so the test fixture must look like a real JWT — not the prior
    /// opaque string. We use a fixed base64-encoded payload so each test
    /// gets the same tenant_id resolution path.
    fn tok() -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD
            .encode(br#"{"sub":"runner","tenant_id":"cccccccc-cccc-4ccc-8ccc-cccccccccccc"}"#);
        format!("{}.{}.test-signature", header, payload)
    }

    /// An OAuth/runner bearer with NO `tenant_id` claim — mirrors the
    /// operator's live Cognito token that broke prod (every refresh tick
    /// bailed on the missing claim until this fallback chain landed).
    fn tok_no_tenant() -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"runner"}"#);
        format!("{}.{}.test-signature", header, payload)
    }

    /// A device-JWT-shaped token carrying both `exp` (so
    /// `device_jwt_needs_refresh` can decode it) and a `tenant_id` claim (so
    /// the fallback-2 resolution can read it from the outgoing slot).
    fn synth_jwt_with_tenant(exp: i64, tenant: &str) -> String {
        let header = b64url(b"{\"alg\":\"EdDSA\",\"typ\":\"JWT\"}");
        let payload = b64url(format!("{{\"exp\":{exp},\"tenant_id\":\"{tenant}\"}}").as_bytes());
        let sig = b64url(b"fake-sig");
        format!("{header}.{payload}.{sig}")
    }

    #[tokio::test]
    async fn refresher_handles_coord_401_without_clearing_jwt() {
        // Setup: AuthManager holds a valid-shape (not-yet-expired) JWT.
        // Run: mock web backend returns 401 on pair-cli.
        // Assert: access_token slot STILL holds the original JWT (not
        // cleared) AND the outcome is KeptExisting.
        let mgr = test_auth_manager("handles_coord_401_without_clearing");
        let existing_jwt = synth_jwt(chrono::Utc::now().timestamp() + 30 * 60);
        mgr.store_tokens(&existing_jwt, "").expect("store");

        let (base, cap, _shutdown) = spawn_mock(
            StatusCode::UNAUTHORIZED,
            r#"{"error":"token expired"}"#.to_string(),
        );

        let outcome = try_refresh_once(&mgr, &base, &tok(), DID, UID, None).await;
        assert_eq!(
            outcome,
            RefreshOutcome::KeptExisting,
            "401 must yield KeptExisting (not Replaced, not PersistFailed)"
        );
        assert_eq!(
            *cap.hits.lock().unwrap(),
            1,
            "web backend pair-cli endpoint should be hit exactly once"
        );

        let still = mgr.get_access_token().expect("token still present");
        assert_eq!(
            still, existing_jwt,
            "JWT in access_token slot must be UNCHANGED after a 401"
        );
    }

    #[tokio::test]
    async fn refresher_handles_coord_503_without_clearing_jwt() {
        // Same as 401 but with a 503 — coord overloaded / down. We
        // MUST NOT punish the runner by clearing its JWT for a
        // transient server error.
        let mgr = test_auth_manager("handles_coord_503_without_clearing");
        let existing_jwt = synth_jwt(chrono::Utc::now().timestamp() + 30 * 60);
        mgr.store_tokens(&existing_jwt, "").expect("store");

        let (base, _cap, _shutdown) = spawn_mock(
            StatusCode::SERVICE_UNAVAILABLE,
            r#"{"error":"coord overloaded"}"#.to_string(),
        );

        let outcome = try_refresh_once(&mgr, &base, &tok(), DID, UID, None).await;
        assert_eq!(outcome, RefreshOutcome::KeptExisting);

        let still = mgr.get_access_token().expect("token still present");
        assert_eq!(
            still, existing_jwt,
            "JWT in access_token slot must be UNCHANGED after a 503"
        );
    }

    #[tokio::test]
    async fn refresher_handles_coord_200_replaces_jwt() {
        // Setup: AuthManager holds the OLD JWT.
        // Run: mock web backend returns canonical 200 with a NEW JWT.
        // Assert: outcome is Replaced with the NEW JWT, and the
        // access_token slot now holds the NEW JWT.
        let mgr = test_auth_manager("handles_coord_200_replaces");
        let old_jwt = synth_jwt(chrono::Utc::now().timestamp() + 30 * 60);
        mgr.store_tokens(&old_jwt, "").expect("store");

        let new_jwt = synth_jwt(chrono::Utc::now().timestamp() + 4 * 60 * 60);
        let body = serde_json::json!({
            "token": new_jwt,
            "device_id": "11111111-1111-4111-8111-111111111111",
            "user_id":   "22222222-2222-4222-8222-222222222222",
            "jti":       "33333333-3333-4333-8333-333333333333",
            "exp":       chrono::Utc::now().timestamp() + 4 * 60 * 60,
        })
        .to_string();

        let (base, _cap, _shutdown) = spawn_mock(StatusCode::OK, body);

        let outcome = try_refresh_once(&mgr, &base, &tok(), DID, UID, None).await;
        match outcome {
            RefreshOutcome::Replaced { new_jwt: got } => {
                assert_eq!(got, new_jwt, "Replaced must carry the new JWT");
            }
            other => panic!("expected Replaced, got {other:?}"),
        }
        let stored = mgr.get_access_token().expect("token present");
        assert_eq!(
            stored, new_jwt,
            "access_token slot must now hold the NEW JWT (not the old one)"
        );
        assert_ne!(
            stored, old_jwt,
            "access_token slot must NOT still hold the old JWT"
        );
    }

    /// Phase 5.4 migration guard: a legacy opaque token in the
    /// access_token slot must report `Ok(true)` from
    /// `device_jwt_needs_refresh` so the refresher heals it on the next
    /// tick. This re-verifies the Phase 2 invariant
    /// (`auth::device_jwt_tests::needs_refresh_when_legacy_opaque_token`)
    /// from the refresher-side perspective: if this ever silently
    /// flips, every pre-Phase-3 paired install will be wedged on the
    /// opaque bearer forever, the relay 401-spinning every reconnect.
    #[test]
    fn refresher_treats_legacy_opaque_token_as_needs_refresh() {
        let mgr = test_auth_manager("treats_legacy_opaque_token");
        mgr.store_tokens("qontinui_runner_legacy_abc123", "")
            .expect("store");
        let needs = mgr
            .device_jwt_needs_refresh()
            .expect("needs_refresh check should not error");
        assert!(
            needs,
            "MIGRATION GUARD: a legacy opaque `qontinui_runner_*` token in \
             the access_token slot MUST be treated as needs-refresh so the \
             refresher replaces it with a real device-JWT. Without this, \
             pre-Phase-3 paired installs are permanently wedged on the \
             opaque bearer and the relay 401-spins every reconnect."
        );
    }

    #[tokio::test]
    async fn refresher_uses_outgoing_jwt_tenant_when_oauth_claim_absent() {
        // PROD BUG REGRESSION: the OAuth/runner bearer carries NO tenant_id
        // (the operator's Cognito token) but the OUTGOING device-JWT — the one
        // we're about to replace — still carries its own tenant_id claim. The
        // refresh MUST proceed using the JWT-sourced tenant (fallback 2), NOT
        // bail. Before the fallback chain this case stranded the runner: the
        // device-JWT expired, coord's fleet-auth gate 403'd the panel, and the
        // relay flapped.
        //
        // The outgoing JWT carries the tenant, so fallback 2 short-circuits
        // before machine.json is ever consulted — fully hermetic regardless of
        // the host's ~/.qontinui.
        let mgr = test_auth_manager("uses_outgoing_jwt_tenant");
        let jwt_tenant = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
        let existing_jwt =
            synth_jwt_with_tenant(chrono::Utc::now().timestamp() + 30 * 60, jwt_tenant);
        mgr.store_tokens(&existing_jwt, "").expect("store");

        let new_jwt = synth_jwt(chrono::Utc::now().timestamp() + 4 * 60 * 60);
        let body = serde_json::json!({
            "token": new_jwt,
            "device_id": "11111111-1111-4111-8111-111111111111",
            "user_id":   "22222222-2222-4222-8222-222222222222",
            "jti":       "33333333-3333-4333-8333-333333333333",
            "exp":       chrono::Utc::now().timestamp() + 4 * 60 * 60,
        })
        .to_string();
        let (base, cap, _shutdown) = spawn_mock(StatusCode::OK, body);

        let outcome = try_refresh_once(&mgr, &base, &tok_no_tenant(), DID, UID, None).await;
        match outcome {
            RefreshOutcome::Replaced { new_jwt: got } => {
                assert_eq!(got, new_jwt, "Replaced must carry the new JWT");
            }
            other => panic!(
                "expected Replaced (refresh must proceed via the outgoing-JWT \
                 tenant fallback), got {other:?}"
            ),
        }
        // The mock must have received the tenant_id sourced from the OUTGOING
        // device-JWT (not the OAuth bearer, which had none).
        let sent = cap
            .last_body
            .lock()
            .unwrap()
            .clone()
            .expect("pair-cli received a request body");
        let sent_json: serde_json::Value =
            serde_json::from_str(&sent).expect("request body is JSON");
        assert_eq!(
            sent_json.get("tenant_id").and_then(|v| v.as_str()),
            Some(jwt_tenant),
            "pair-cli must receive the tenant_id resolved from the outgoing device-JWT"
        );
    }

    #[tokio::test]
    async fn refresher_keeps_existing_when_no_tenant_source_at_all() {
        // BEHAVIOR PRESERVED: OAuth bearer has no tenant claim, the persisted
        // device-JWT has no tenant claim, AND the caller-injected
        // `machine_tenant` is `None`. (`try_refresh_once` no longer reads
        // `~/.qontinui` itself — the prod loop passes
        // `resolve_active_tenant_id()`, tests inject directly — so this is
        // hermetic on any host.) All three sources absent → the refresher MUST
        // bail with KeptExisting: no HTTP call, JWT untouched — the same
        // warn-and-keep path as before the fallback chain.
        let mgr = test_auth_manager("keeps_existing_no_tenant_source");
        // Persisted device-JWT carries NO tenant claim → fallbacks 1 + 2 miss.
        let existing_jwt = synth_jwt(chrono::Utc::now().timestamp() + 30 * 60);
        mgr.store_tokens(&existing_jwt, "").expect("store");

        // Mock would 200 with a fresh JWT — it must NEVER be consulted.
        let new_jwt = synth_jwt(chrono::Utc::now().timestamp() + 4 * 60 * 60);
        let body = serde_json::json!({ "token": new_jwt }).to_string();
        let (base, cap, _shutdown) = spawn_mock(StatusCode::OK, body);

        let outcome = try_refresh_once(&mgr, &base, &tok_no_tenant(), DID, UID, None).await;

        assert_eq!(
            outcome,
            RefreshOutcome::KeptExisting,
            "no usable tenant from any source MUST yield KeptExisting"
        );
        assert_eq!(
            *cap.hits.lock().unwrap(),
            0,
            "pair-cli must NOT be hit when no tenant_id can be resolved"
        );
        let still = mgr.get_access_token().expect("token still present");
        assert_eq!(
            still, existing_jwt,
            "JWT in access_token slot must be UNCHANGED when no tenant resolves"
        );
    }

    #[tokio::test]
    async fn refresher_uses_machine_tenant_when_no_jwt_claims() {
        // Fallback 3: OAuth bearer + persisted device-JWT both lack a tenant
        // claim, but the caller-injected `machine_tenant`
        // (machine.json::active_tenant_id in prod) supplies one → the refresh
        // proceeds and pair-cli is hit exactly once.
        let mgr = test_auth_manager("uses_machine_tenant");
        let existing_jwt = synth_jwt(chrono::Utc::now().timestamp() + 30 * 60);
        mgr.store_tokens(&existing_jwt, "").expect("store");

        let new_jwt = synth_jwt(chrono::Utc::now().timestamp() + 4 * 60 * 60);
        // Full PairCompleteResponse shape (token alone fails the decode —
        // device_id/user_id/jti/exp are required fields).
        let body = serde_json::json!({
            "token": new_jwt,
            "device_id": "11111111-1111-4111-8111-111111111111",
            "user_id":   "22222222-2222-4222-8222-222222222222",
            "jti":       "33333333-3333-4333-8333-333333333333",
            "exp":       chrono::Utc::now().timestamp() + 4 * 60 * 60,
        })
        .to_string();
        let (base, cap, _shutdown) = spawn_mock(StatusCode::OK, body);

        let machine = uuid::Uuid::parse_str("cccccccc-cccc-4ccc-8ccc-cccccccccccc").unwrap();
        let outcome =
            try_refresh_once(&mgr, &base, &tok_no_tenant(), DID, UID, Some(machine)).await;

        match outcome {
            RefreshOutcome::Replaced { new_jwt: got } => {
                assert_eq!(got, new_jwt, "Replaced must carry the new JWT");
            }
            other => {
                panic!("machine tenant present → refresh must proceed (Replaced), got {other:?}")
            }
        }
        assert_eq!(
            *cap.hits.lock().unwrap(),
            1,
            "pair-cli should be hit exactly once when machine tenant supplies the id"
        );
    }
}

#[cfg(test)]
mod device_self_refresh_tests {
    //! Phase 4a tests — `try_device_self_refresh` against an in-process mock
    //! coord serving `POST /devices/:device_id/refresh-token`. Covers:
    //!   - preferred path: a valid (future-exp) device-JWT self-refreshes,
    //!     presenting itself as the bearer, stores the new token, returns
    //!     `Some(new_jwt)` (so the loop SKIPS the Cognito path),
    //!   - fallback: an expired/absent device-JWT returns `None` WITHOUT hitting
    //!     coord (the caller falls back to Cognito),
    //!   - REPLACE-not-REVOKE + ordering safety: a non-2xx (incl. 404 for an
    //!     undeployed coord route) returns `None` and leaves the existing JWT
    //!     untouched.

    use super::*;
    use axum::{
        extract::{Path, State},
        http::{HeaderMap, StatusCode},
        routing::post,
        Router,
    };
    use std::sync::{Arc, Mutex};

    fn b64url(b: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
    }

    /// Mint a synthetic device-JWT carrying `exp` so `access_token_exp` can
    /// decode it (signature isn't verified by the runner).
    fn synth_jwt(exp: i64) -> String {
        let header = b64url(b"{\"alg\":\"EdDSA\",\"typ\":\"JWT\"}");
        let payload = b64url(format!("{{\"exp\":{}}}", exp).as_bytes());
        let sig = b64url(b"fake-sig");
        format!("{header}.{payload}.{sig}")
    }

    fn test_auth_manager(name: &str) -> crate::auth::AuthManager {
        let dir = std::env::temp_dir().join("qontinui_test_self_refresh");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{name}.enc"));
        let _ = std::fs::remove_file(&path);
        let storage = crate::secure_storage::SecureStorage::with_path(path).expect("storage");
        crate::auth::AuthManager::with_storage(storage)
    }

    #[derive(Clone)]
    struct MockState {
        status: StatusCode,
        body: String,
        hits: Arc<Mutex<u32>>,
        last_auth: Arc<Mutex<Option<String>>>,
    }

    async fn handler(
        State(s): State<MockState>,
        Path(_device_id): Path<String>,
        h: HeaderMap,
    ) -> (StatusCode, String) {
        *s.hits.lock().unwrap() += 1;
        *s.last_auth.lock().unwrap() = h
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_string());
        (s.status, s.body.clone())
    }

    struct MockCapture {
        hits: Arc<Mutex<u32>>,
        last_auth: Arc<Mutex<Option<String>>>,
    }

    fn spawn_mock(
        status: StatusCode,
        body: String,
    ) -> (String, MockCapture, tokio::sync::oneshot::Sender<()>) {
        let hits = Arc::new(Mutex::new(0u32));
        let last_auth = Arc::new(Mutex::new(None));
        let hits_h = hits.clone();
        let last_auth_h = last_auth.clone();
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = std_listener.local_addr().expect("addr").port();
        std_listener.set_nonblocking(true).expect("nb");
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rt");
            rt.block_on(async move {
                let state = MockState {
                    status,
                    body,
                    hits: hits_h,
                    last_auth: last_auth_h,
                };
                // axum 0.8 path-param syntax: `{device_id}`.
                let app: Router = Router::new()
                    .route("/devices/{device_id}/refresh-token", post(handler))
                    .with_state(state);
                let listener =
                    tokio::net::TcpListener::from_std(std_listener).expect("tokio listener");
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = rx.await;
                    })
                    .await;
            });
        });
        std::thread::sleep(Duration::from_millis(50));
        (
            format!("http://127.0.0.1:{port}"),
            MockCapture { hits, last_auth },
            tx,
        )
    }

    const DID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    #[tokio::test]
    async fn self_refresh_preferred_when_valid_device_jwt_held() {
        // A currently-valid (future-exp) device-JWT is held → self-refresh
        // re-mints from it (presenting it as the bearer), stores the new token,
        // and returns Some. The caller uses this to SKIP the Cognito path
        // (Cognito is never consulted on this tick).
        let mgr = test_auth_manager("preferred_when_valid");
        let existing = synth_jwt(chrono::Utc::now().timestamp() + 30 * 60);
        mgr.store_tokens(&existing, "").expect("store");

        let new_jwt = synth_jwt(chrono::Utc::now().timestamp() + 4 * 60 * 60);
        let body = serde_json::json!({ "token": new_jwt }).to_string();
        let (base, cap, _shutdown) = spawn_mock(StatusCode::OK, body);

        let got = try_device_self_refresh(&mgr, &base, DID).await;
        assert_eq!(
            got.as_deref(),
            Some(new_jwt.as_str()),
            "self-refresh must return the new JWT"
        );
        assert_eq!(
            *cap.hits.lock().unwrap(),
            1,
            "coord self-refresh route should be hit exactly once"
        );
        // The CURRENT device-JWT was presented as the bearer (login-independent —
        // no Cognito token involved).
        assert_eq!(
            cap.last_auth.lock().unwrap().clone(),
            Some(format!("Bearer {existing}")),
            "self-refresh must present the CURRENT device-JWT as the bearer"
        );
        // Slot now holds the NEW token.
        assert_eq!(mgr.get_access_token().unwrap(), new_jwt);
    }

    #[tokio::test]
    async fn falls_back_to_cognito_when_device_jwt_expired() {
        // An already-expired device-JWT cannot self-refresh (coord would 401 the
        // bearer, and this IS the Cognito-recovery case). Return None WITHOUT
        // touching coord so the caller falls back to the Cognito path.
        let mgr = test_auth_manager("fallback_when_expired");
        let expired = synth_jwt(chrono::Utc::now().timestamp() - 60);
        mgr.store_tokens(&expired, "").expect("store");

        let (base, cap, _shutdown) = spawn_mock(
            StatusCode::OK,
            serde_json::json!({ "token": "x" }).to_string(),
        );

        let got = try_device_self_refresh(&mgr, &base, DID).await;
        assert!(got.is_none(), "expired JWT → None (Cognito fallback)");
        assert_eq!(
            *cap.hits.lock().unwrap(),
            0,
            "coord must NOT be hit for an expired JWT"
        );
        // Existing (expired) JWT untouched — REPLACE-not-REVOKE.
        assert_eq!(mgr.get_access_token().unwrap(), expired);
    }

    #[tokio::test]
    async fn falls_back_to_cognito_when_device_jwt_absent() {
        // No device-JWT held at all → None without hitting coord.
        let mgr = test_auth_manager("fallback_when_absent");
        let (base, cap, _shutdown) = spawn_mock(
            StatusCode::OK,
            serde_json::json!({ "token": "x" }).to_string(),
        );
        let got = try_device_self_refresh(&mgr, &base, DID).await;
        assert!(got.is_none(), "absent JWT → None (Cognito fallback)");
        assert_eq!(
            *cap.hits.lock().unwrap(),
            0,
            "coord must NOT be hit when no device-JWT is held"
        );
    }

    #[tokio::test]
    async fn self_refresh_404_returns_none_and_preserves_jwt() {
        // ORDERING SAFETY + REPLACE-not-REVOKE: a coord that hasn't deployed the
        // route yet 404s → None (fall back to Cognito) and the existing JWT is
        // left UNTOUCHED. This is what lets the runner half ship before the coord
        // half.
        let mgr = test_auth_manager("route_404_preserves");
        let existing = synth_jwt(chrono::Utc::now().timestamp() + 30 * 60);
        mgr.store_tokens(&existing, "").expect("store");

        let (base, cap, _shutdown) = spawn_mock(
            StatusCode::NOT_FOUND,
            r#"{"error":"not found"}"#.to_string(),
        );

        let got = try_device_self_refresh(&mgr, &base, DID).await;
        assert!(got.is_none(), "404 → None (Cognito fallback)");
        assert_eq!(
            *cap.hits.lock().unwrap(),
            1,
            "the route was attempted exactly once"
        );
        assert_eq!(
            mgr.get_access_token().unwrap(),
            existing,
            "REPLACE-not-REVOKE: existing JWT must be UNCHANGED after a 404"
        );
    }

    #[tokio::test]
    async fn self_refresh_401_returns_none_and_preserves_jwt() {
        // A 401 (stale/rejected bearer) → None + existing JWT preserved. The
        // caller falls back to Cognito; the runner is never punished by a cleared
        // slot for a single non-2xx.
        let mgr = test_auth_manager("resp_401_preserves");
        let existing = synth_jwt(chrono::Utc::now().timestamp() + 30 * 60);
        mgr.store_tokens(&existing, "").expect("store");

        let (base, _cap, _shutdown) = spawn_mock(
            StatusCode::UNAUTHORIZED,
            r#"{"error":"expired"}"#.to_string(),
        );

        let got = try_device_self_refresh(&mgr, &base, DID).await;
        assert!(got.is_none());
        assert_eq!(
            mgr.get_access_token().unwrap(),
            existing,
            "existing JWT must be UNCHANGED after a 401"
        );
    }
}

#[cfg(test)]
mod tenant_slot_refresh_tests {
    //! Session-scoped multi-tenant — hermetic tests for the per-tenant
    //! slot pass (`refresh_tenant_slots`, shipped un-gated as of Phase
    //! 8a) against an in-process mock coord, plus the pure
    //! `plan_tenant_slot` staleness decision and the `IdleWrongTier`
    //! bounded-wait rule. Injected tokens only (the established
    //! refresher test pattern) — no live coord, no `~/.qontinui`, no
    //! env-var mutation.

    use super::*;
    use axum::{
        extract::{Path, State},
        http::{HeaderMap, StatusCode},
        routing::post,
        Router,
    };
    use std::sync::{Arc, Mutex};

    fn b64url(b: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
    }

    /// Synthetic device-JWT with an `exp` claim and a distinguishing `sub`
    /// so each tenant's token is unique on the wire.
    fn synth_jwt(exp: i64, sub: &str) -> String {
        let header = b64url(b"{\"alg\":\"EdDSA\",\"typ\":\"JWT\"}");
        let payload = b64url(format!("{{\"exp\":{exp},\"sub\":\"{sub}\"}}").as_bytes());
        let sig = b64url(b"fake-sig");
        format!("{header}.{payload}.{sig}")
    }

    fn test_auth_manager(name: &str) -> crate::auth::AuthManager {
        let dir = std::env::temp_dir().join("qontinui_test_tenant_slots");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{name}.enc"));
        let _ = std::fs::remove_file(&path);
        let storage = crate::secure_storage::SecureStorage::with_path(path).expect("storage");
        crate::auth::AuthManager::with_storage(storage)
    }

    /// Mock coord: echoes each presented bearer back as `<bearer>.refreshed`
    /// (so per-slot assertions can prove EACH slot was refreshed with ITS OWN
    /// token), and 500s any bearer in `fail_bearers` (so failure-isolation is
    /// testable). Captures every presented bearer in order.
    #[derive(Clone)]
    struct MockState {
        fail_bearers: Arc<Mutex<Vec<String>>>,
        bearers_seen: Arc<Mutex<Vec<String>>>,
    }

    async fn handler(
        State(s): State<MockState>,
        Path(_device_id): Path<String>,
        h: HeaderMap,
    ) -> (StatusCode, String) {
        let bearer = h
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("")
            .to_string();
        s.bearers_seen.lock().unwrap().push(bearer.clone());
        if s.fail_bearers.lock().unwrap().contains(&bearer) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                r#"{"error":"boom"}"#.to_string(),
            );
        }
        (
            StatusCode::OK,
            serde_json::json!({ "token": format!("{bearer}.refreshed") }).to_string(),
        )
    }

    struct MockCapture {
        bearers_seen: Arc<Mutex<Vec<String>>>,
    }

    fn spawn_mock(
        fail_bearers: Vec<String>,
    ) -> (String, MockCapture, tokio::sync::oneshot::Sender<()>) {
        let bearers_seen = Arc::new(Mutex::new(Vec::new()));
        let bearers_h = bearers_seen.clone();
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = std_listener.local_addr().expect("addr").port();
        std_listener.set_nonblocking(true).expect("nb");
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rt");
            rt.block_on(async move {
                let state = MockState {
                    fail_bearers: Arc::new(Mutex::new(fail_bearers)),
                    bearers_seen: bearers_h,
                };
                // axum 0.8 path-param syntax: `{device_id}`.
                let app: Router = Router::new()
                    .route("/devices/{device_id}/refresh-token", post(handler))
                    .with_state(state);
                let listener =
                    tokio::net::TcpListener::from_std(std_listener).expect("tokio listener");
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = rx.await;
                    })
                    .await;
            });
        });
        std::thread::sleep(Duration::from_millis(50));
        (
            format!("http://127.0.0.1:{port}"),
            MockCapture { bearers_seen },
            tx,
        )
    }

    const DID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    fn tenant(n: u8) -> uuid::Uuid {
        uuid::Uuid::parse_str(&format!(
            "{c}{c}{c}{c}{c}{c}{c}{c}-{c}{c}{c}{c}-4{c}{c}{c}-8{c}{c}{c}-{c}{c}{c}{c}{c}{c}{c}{c}{c}{c}{c}{c}",
            c = (b'a' + n) as char
        ))
        .expect("valid uuid")
    }

    // ---- plan_tenant_slot: pure staleness decision ----

    #[test]
    fn plan_refreshes_stale_but_valid_and_skips_fresh_or_dead() {
        let now = 1_000_000_000i64;
        // Opaque/undecodable → not self-refreshable.
        assert_eq!(
            plan_tenant_slot(None, now),
            TenantSlotPlan::SkipNotRefreshable
        );
        // Already expired (or exactly at exp) → not self-refreshable.
        assert_eq!(
            plan_tenant_slot(Some(now - 1), now),
            TenantSlotPlan::SkipNotRefreshable
        );
        assert_eq!(
            plan_tenant_slot(Some(now), now),
            TenantSlotPlan::SkipNotRefreshable
        );
        // Future exp within TTL/3 → refresh.
        assert_eq!(
            plan_tenant_slot(Some(now + 30 * 60), now),
            TenantSlotPlan::Refresh
        );
        assert_eq!(
            plan_tenant_slot(Some(now + crate::auth::REFRESH_BEFORE_EXPIRY_SECS), now),
            TenantSlotPlan::Refresh
        );
        // Comfortably fresh → skip.
        assert_eq!(
            plan_tenant_slot(Some(now + 3 * 60 * 60), now),
            TenantSlotPlan::SkipFresh
        );
    }

    // ---- refresh_tenant_slots: hermetic multi-slot iteration ----

    #[tokio::test]
    async fn multi_slot_pass_refreshes_each_slot_with_its_own_token() {
        // Two tenant slots, both stale-but-valid, plus a legacy access_token.
        // Each slot must be refreshed by presenting ITS OWN token, persisted
        // into ITS OWN slot — and the legacy slot must be byte-identical after.
        let mgr = test_auth_manager("multi_slot_own_tokens");
        let (ta, tb) = (tenant(0), tenant(1));
        let now = chrono::Utc::now().timestamp();
        let jwt_a = synth_jwt(now + 30 * 60, "tenant-a");
        let jwt_b = synth_jwt(now + 30 * 60, "tenant-b");
        mgr.store_tokens("legacy.default.jwt", "")
            .expect("store legacy");
        mgr.store_tenant_device_jwt(&ta, &jwt_a).expect("slot a");
        mgr.store_tenant_device_jwt(&tb, &jwt_b).expect("slot b");

        let (base, cap, _shutdown) = spawn_mock(vec![]);
        let outcomes = refresh_tenant_slots(&mgr, &base, DID).await;

        assert_eq!(
            outcomes,
            vec![
                (ta, TenantSlotOutcome::Refreshed),
                (tb, TenantSlotOutcome::Refreshed)
            ],
            "both slots must refresh"
        );
        // Each slot presented ITS OWN token as the bearer.
        let seen = cap.bearers_seen.lock().unwrap().clone();
        assert_eq!(seen, vec![jwt_a.clone(), jwt_b.clone()]);
        // Each slot now holds ITS OWN re-minted token (echo-mint = bearer +
        // ".refreshed"), proving no cross-slot mixing.
        assert_eq!(
            mgr.get_tenant_device_jwt(&ta).unwrap().as_deref(),
            Some(format!("{jwt_a}.refreshed").as_str())
        );
        assert_eq!(
            mgr.get_tenant_device_jwt(&tb).unwrap().as_deref(),
            Some(format!("{jwt_b}.refreshed").as_str())
        );
        // LEGACY-SLOT PRESERVATION: the pass never touches access_token.
        assert_eq!(mgr.get_access_token().unwrap(), "legacy.default.jwt");
    }

    #[tokio::test]
    async fn failure_on_one_slot_does_not_abort_the_others() {
        // Coord 500s tenant A's bearer; tenant B must still refresh. A keeps
        // its existing token (REPLACE-not-REVOKE).
        let mgr = test_auth_manager("multi_slot_failure_isolation");
        let (ta, tb) = (tenant(0), tenant(1));
        let now = chrono::Utc::now().timestamp();
        let jwt_a = synth_jwt(now + 30 * 60, "tenant-a");
        let jwt_b = synth_jwt(now + 30 * 60, "tenant-b");
        mgr.store_tenant_device_jwt(&ta, &jwt_a).expect("slot a");
        mgr.store_tenant_device_jwt(&tb, &jwt_b).expect("slot b");

        let (base, cap, _shutdown) = spawn_mock(vec![jwt_a.clone()]);
        let outcomes = refresh_tenant_slots(&mgr, &base, DID).await;

        assert_eq!(
            outcomes,
            vec![
                (ta, TenantSlotOutcome::KeptExisting),
                (tb, TenantSlotOutcome::Refreshed)
            ],
            "A fails, B still refreshes — no abort"
        );
        // Both were attempted (B was not skipped because of A's failure).
        assert_eq!(cap.bearers_seen.lock().unwrap().len(), 2);
        // A's slot is UNCHANGED; B's slot advanced.
        assert_eq!(
            mgr.get_tenant_device_jwt(&ta).unwrap().as_deref(),
            Some(jwt_a.as_str())
        );
        assert_eq!(
            mgr.get_tenant_device_jwt(&tb).unwrap().as_deref(),
            Some(format!("{jwt_b}.refreshed").as_str())
        );
    }

    #[tokio::test]
    async fn fresh_and_dead_slots_are_skipped_without_http() {
        // A comfortably-fresh slot and an already-expired slot: neither hits
        // coord, neither slot value changes.
        let mgr = test_auth_manager("multi_slot_skip_fresh_dead");
        let (ta, tb) = (tenant(0), tenant(1));
        let now = chrono::Utc::now().timestamp();
        let fresh = synth_jwt(now + 3 * 60 * 60, "fresh");
        let expired = synth_jwt(now - 60, "expired");
        mgr.store_tenant_device_jwt(&ta, &fresh).expect("slot a");
        mgr.store_tenant_device_jwt(&tb, &expired).expect("slot b");

        let (base, cap, _shutdown) = spawn_mock(vec![]);
        let outcomes = refresh_tenant_slots(&mgr, &base, DID).await;

        assert_eq!(
            outcomes,
            vec![
                (ta, TenantSlotOutcome::SkippedFresh),
                (tb, TenantSlotOutcome::SkippedNotRefreshable)
            ]
        );
        assert!(
            cap.bearers_seen.lock().unwrap().is_empty(),
            "no HTTP call for fresh/expired slots"
        );
        assert_eq!(
            mgr.get_tenant_device_jwt(&ta).unwrap().as_deref(),
            Some(fresh.as_str())
        );
        assert_eq!(
            mgr.get_tenant_device_jwt(&tb).unwrap().as_deref(),
            Some(expired.as_str()),
            "REPLACE-not-REVOKE: an expired slot is preserved, never cleared"
        );
    }

    #[tokio::test]
    async fn empty_slot_set_is_a_no_op() {
        let mgr = test_auth_manager("multi_slot_empty_noop");
        let (base, cap, _shutdown) = spawn_mock(vec![]);
        let outcomes = refresh_tenant_slots(&mgr, &base, DID).await;
        assert!(outcomes.is_empty());
        assert!(cap.bearers_seen.lock().unwrap().is_empty());
    }

    /// Phase 8a starvation fix: with tenant slots held, the wrong-tier
    /// idle arm must wake on the steady cadence (so the per-slot pass at
    /// the top of the loop keeps running); with no slots it keeps the
    /// historical block-until-kick (`None`).
    #[test]
    fn idle_wrong_tier_waits_bounded_only_when_slots_exist() {
        assert_eq!(idle_wrong_tier_wait(true), Some(REFRESH_CHECK_INTERVAL));
        assert_eq!(idle_wrong_tier_wait(false), None);
    }
}
