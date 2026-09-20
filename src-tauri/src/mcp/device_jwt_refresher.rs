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
use tracing::{debug, info, warn};

use crate::mcp::types::ApiState;
use crate::settings::{self, RunnerTier};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

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
    /// Coord returned a fresh JWT, but its OWN `tenant_id` claim named a
    /// DIFFERENT tenant than the one this refresh asked for — refused at
    /// [`crate::auth::AuthManager::store_tokens_expecting`] and never
    /// persisted. The existing JWT in the access_token slot is left
    /// untouched.
    ///
    /// This is the exact defect plan
    /// `2026-09-17-device-jwt-refresh-drops-the-requested-tenant-and-coord-mints-the-home-tenant`
    /// names: coord fell back to the operator's home tenant and this runner
    /// silently adopted it as its default. D2/D3: defence in depth against an
    /// older web backend, or a coord bug, that mints for the wrong tenant.
    TenantMismatch {
        expected: uuid::Uuid,
        returned: Option<uuid::Uuid>,
    },
}

/// Cumulative count, since process start, of mint-stores refused by
/// [`crate::auth::AuthManager::store_tokens_expecting`] across all three
/// refresher mint paths (Cognito pair-cli, device self-refresh,
/// device-machine-key exchange). A number that climbs is a live instance of
/// the defect plan
/// `2026-09-17-device-jwt-refresh-drops-the-requested-tenant-and-coord-mints-the-home-tenant`
/// fixes — read via [`device_jwt_refresh_tenant_mismatch_total`].
static DEVICE_JWT_REFRESH_TENANT_MISMATCH_TOTAL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Snapshot of [`DEVICE_JWT_REFRESH_TENANT_MISMATCH_TOTAL`]. `allow(dead_code)`:
/// wiring this into a `/health` surface is a later phase; the tests exercise
/// it directly.
#[allow(dead_code)]
pub(crate) fn device_jwt_refresh_tenant_mismatch_total() -> u64 {
    DEVICE_JWT_REFRESH_TENANT_MISMATCH_TOTAL.load(std::sync::atomic::Ordering::Relaxed)
}

/// (expected, returned) pairs already `warn!`ed this process — D3: "One
/// `warn!` per (expected, returned) pair per process", so a mismatch that
/// keeps recurring every 5-minute tick does not spam the log the way the
/// Phase 2c doc-comment on [`TenantSlotHealth`] describes for an unrelated
/// condition (15,826 identical lines in six weeks, unnoticed).
static WARNED_TENANT_MISMATCHES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashSet<(uuid::Uuid, Option<uuid::Uuid>)>>,
> = std::sync::OnceLock::new();

/// Emit the operator-facing tenant-mismatch `warn!` at most once per
/// (expected, returned) pair per process. Naming both tenants and the remedy
/// (upgrade qontinui-web, or re-pair the runner to the expected tenant) is
/// the whole reason this exists as a distinct message rather than reusing
/// the generic persist-failure log line.
fn warn_tenant_mismatch_once(expected: uuid::Uuid, returned: Option<uuid::Uuid>) {
    let cell = WARNED_TENANT_MISMATCHES
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    let mut seen = cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if seen.insert((expected, returned)) {
        warn!(
            "device_jwt_refresher: device-JWT re-mint returned tenant {returned:?} but \
             {expected} was requested — refusing to persist. If this recurs: upgrade \
             qontinui-web (older backends silently drop the requested tenant_id), or \
             re-pair this runner to tenant {expected} if it should now be bound elsewhere."
        );
    }
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

/// Which AUTHORITY published a credential-dark signal.
///
/// M5 — two independent authorities publish onto this one event: the Cognito
/// refresh loop and the coord-credential posture. Without a source they
/// last-writer-win into a single frontend slot, and the banner can be WRONGLY
/// CLEARED: Cognito goes dark, the user signs in, Cognito's `notify_recovered`
/// fires `dark:false`, the banner clears — while the tenant slot is still
/// `unrefreshable`, and the posture arm will not re-fire because the posture
/// did not change. The frontend holds one signal PER SOURCE and reduces them.
pub(crate) const DARK_SOURCE_COGNITO: &str = "cognito";
pub(crate) const DARK_SOURCE_POSTURE: &str = "posture";

/// Emit the widened credential-dark / recovered notification to the frontend.
///
/// Payload (DD3 of the coord-credential-posture plan, plus M5's `source`):
/// `{source, dark, cause, message, cta, since}`. ONE event and ONE banner for
/// every terminal credential cause — a second banner component would be the
/// "wrong layer, two authorities" shape — but the two authorities are now
/// distinguishable within it.
///
/// Best-effort: an emit failure only `warn!`s — telemetry must never break the
/// loop.
fn emit_credential_dark_event(
    app: &tauri::AppHandle,
    source: &str,
    dark: bool,
    cause: &str,
    message: &str,
    cta: Option<&str>,
    since: Option<i64>,
) {
    let payload = serde_json::json!({
        "source": source,
        "dark": dark,
        "cause": cause,
        "message": message,
        "cta": cta,
        "since": since,
    });
    if let Err(e) = app.emit(AUTONOMY_CREDENTIAL_DARK_EVENT, &payload) {
        warn!("device_jwt_refresher: failed to emit {AUTONOMY_CREDENTIAL_DARK_EVENT}: {e}");
    }
}

/// The HARD-Cognito arm, unchanged in meaning and now one CAUSE among several
/// on the shared event. `invalid_grant` means the refresh token is
/// expired/revoked: no headless recovery exists, so the CTA is an interactive
/// sign-in.
fn emit_credential_dark(app: &tauri::AppHandle, dark: bool) {
    if dark {
        emit_credential_dark_event(
            app,
            DARK_SOURCE_COGNITO,
            true,
            "cognito_hard",
            "Autonomous sessions paused — sign in again to resume.",
            Some("sign_in"),
            None,
        );
    } else {
        emit_credential_dark_event(
            app,
            DARK_SOURCE_COGNITO,
            false,
            "recovered",
            "Autonomous sessions resumed — credentials refreshed.",
            None,
            None,
        );
    }
}

/// Fire the banner for a coord-credential POSTURE transition (Phase 2).
///
/// Called for every transition the publisher reports, INCLUDING the boot pass
/// — DD2: today a bad boot state is the one moment no transition happens, so
/// nothing fires and the runner is silent while every session it spawns has no
/// coord access.
///
/// `app: None` (hermetic tests, headless callers) derives and publishes the
/// posture but shows nothing — the state is still readable on `/health`.
fn notify_posture_transition(app: Option<&tauri::AppHandle>, transition: PostureTransition) {
    if !should_notify_posture(transition.from, transition.to) {
        return;
    }
    let since = coord_credential_posture().map(|s| s.since);
    let from = transition
        .from
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    warn!(
        "device_jwt_refresher: coord-credential posture {from} -> {} ({})",
        transition.to.as_str(),
        transition.to.message()
    );
    let Some(app) = app else { return };
    if transition.to.can_answer() {
        emit_credential_dark_event(
            app,
            DARK_SOURCE_POSTURE,
            false,
            "recovered",
            "Coord access restored — this runner's credential is live again.",
            None,
            since,
        );
    } else {
        emit_credential_dark_event(
            app,
            DARK_SOURCE_POSTURE,
            true,
            transition
                .to
                .cause()
                .unwrap_or_else(|| transition.to.as_str()),
            transition.to.message(),
            transition.to.cta(),
            since,
        );
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
    /// `try_refresh_once` returned [`RefreshOutcome::TenantMismatch`]: coord
    /// minted a JWT for a DIFFERENT tenant than the one requested, and it was
    /// refused rather than persisted. Terminal for the tick — D3 of plan
    /// `2026-09-17-device-jwt-refresh-drops-the-requested-tenant-and-coord-mints-the-home-tenant`:
    /// this must NOT fall through to the device-machine-key exchange, which
    /// would mint from the same (possibly still-wrong) tenant coord has on
    /// file for this device.
    BailTenantMismatch {
        expected: uuid::Uuid,
        returned: Option<uuid::Uuid>,
    },
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
            Some(PairProgress::BailTenantMismatch { expected, returned }) => {
                CoordCredentialHealth::bad(format!(
                    "device-JWT re-mint returned tenant {returned:?} but {expected} was \
                     requested — refused rather than persisted; upgrade qontinui-web or \
                     re-pair this runner to tenant {expected}"
                ))
            }
        },
    }
}

/// The `details.coord_credential` bag the heartbeat publishes — THE
/// cross-repo wire contract qontinui-web's console reads.
///
/// ## Why it is no longer `{ok, reason}` derived from [`Decision`] alone
///
/// `coord_credential_health(Decision::Idle, None)` answers `ok: true`, and
/// `Decision::Idle` means only *"the legacy `access_token` slot is fresh
/// enough"*. In the 2026-09-12 incident's own shape — a fresh legacy token
/// beside an EXPIRED per-tenant slot — the heartbeat therefore published
/// `ok: true` and the console rendered a credential-dark machine as healthy,
/// while the posture that knew better went only to the loopback `/health`.
///
/// So the posture is the authority here, and `ok` is DERIVED from it:
/// `false` for every posture that is not `live`/`expiring`. `ok` stays present
/// and stays a bool because coord's dark scan selects on it
/// (`fleet_health.rs`, `details #>> '{coord_credential,ok}' = 'false'`) —
/// deriving it from the posture is what finally makes that scan select these
/// devices.
///
/// `since` is an ISO-8601 STRING, not unix seconds: that is what the consuming
/// console parses, and a number would break it silently.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct CoordCredentialBag {
    /// Derived from `posture`; the field coord's dark scan selects on.
    pub ok: bool,
    /// Operator-facing sentence, or `null` when `ok`. Explicitly serialized as
    /// `null` rather than omitted — the contract names the key.
    pub reason: Option<String>,
    /// `live | expiring | expired | absent | unrefreshable | dark | unknown`.
    pub posture: String,
    /// ISO-8601 UTC instant the runner ENTERED this posture.
    pub since: String,
    pub tenant_id: Option<String>,
    /// Decoded `exp` of the credential the posture is about, unix seconds.
    pub exp: Option<i64>,
    /// How old this report may get before a reader must stop trusting it:
    /// [`COORD_CREDENTIAL_STALE_AFTER_PASSES`] × [`REFRESH_CHECK_INTERVAL`], in
    /// seconds. Coord's status upsert replaces `details` wholesale and stamps
    /// `updated_at`, so the row's age IS this bag's age — and a runner that went
    /// offline while `ok: true` would otherwise read `live` forever. Declared
    /// here, beside the cadence it is derived from, so the console keeps no
    /// constant of its own (the fleet-health `sample_stale_after_secs` pattern).
    pub stale_after_secs: u64,
}

/// How many refresher passes a published credential report outlives before it
/// is stale. Three tolerates one skipped pass plus transient backoff jitter.
const COORD_CREDENTIAL_STALE_AFTER_PASSES: u64 = 3;

/// The staleness bound every published bag declares — see
/// [`CoordCredentialBag::stale_after_secs`].
fn coord_credential_stale_after_secs() -> u64 {
    COORD_CREDENTIAL_STALE_AFTER_PASSES * REFRESH_CHECK_INTERVAL.as_secs()
}

/// Render unix seconds as the ISO-8601 string the bag's `since` must carry.
pub(crate) fn iso8601(unix_seconds: i64) -> String {
    chrono::DateTime::from_timestamp(unix_seconds, 0)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Build the published bag: the POSTURE decides, with the [`Decision`]-derived
/// health as the fallback for the one state the posture cannot speak to —
/// UNKNOWN, i.e. no refresher pass has concluded in this process yet.
///
/// UNKNOWN deliberately does NOT force `ok: false`. An absent measurement is
/// not a fault (`verification-and-evidence` `silent-empty-is-unknown`), and
/// forcing it would alert every runner for the first seconds of its life; the
/// pre-existing decision-derived answer is the honest continuation, and
/// `posture: "unknown"` says outright that the posture is not the source.
pub(crate) fn coord_credential_bag(
    fallback: &CoordCredentialHealth,
    posture: Option<&CoordCredentialStatus>,
) -> CoordCredentialBag {
    match posture {
        Some(p) => CoordCredentialBag {
            ok: p.posture.can_answer(),
            reason: (!p.posture.can_answer()).then(|| p.posture.message().to_string()),
            posture: p.posture.as_str().to_string(),
            since: iso8601(p.since),
            tenant_id: p.tenant_id.clone(),
            exp: p.exp,
            stale_after_secs: coord_credential_stale_after_secs(),
        },
        None => CoordCredentialBag {
            ok: fallback.ok,
            reason: fallback.reason.clone(),
            posture: "unknown".to_string(),
            since: iso8601(chrono::Utc::now().timestamp()),
            tenant_id: None,
            exp: None,
            stale_after_secs: coord_credential_stale_after_secs(),
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

    let (base, _coord_base_source) = crate::coord_mcp::coord_base_url_with_source();
    let url = format!("{base}/coord/status");
    // C4 — the cross-repo contract. The bag carries the POSTURE and derives
    // `ok` from it, so coord's dark scan and qontinui-web's console finally see
    // what `/health` sees.
    let bag = coord_credential_bag(health, coord_credential_posture().as_ref());
    let mut body = serde_json::json!({
        "device_id": device_uuid,
        "details": { "coord_credential": bag },
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
    // Tenant-scoped: when `tenant_id` made it onto the body the status row is
    // written under that tenant, so the bearer comes from that tenant's slot.
    // Safe on this surface because the tenant IS the device default
    // (`resolve_active_tenant_id`, falling back to the JWT's own claim), so the
    // two agree by construction — which is exactly why the absent case is
    // `TenantScope::Device` and not `Unresolved`: nothing failed to resolve, so
    // the D2 degrade must not arm on a device-status publish. A non-default
    // slot miss then sends unauthenticated rather than presenting another
    // tenant's JWT (`auth::select_device_bearer`).
    match crate::auth::attach_device_auth_for(
        client.post(&url).json(&body),
        crate::auth::TenantScope::for_device_default(tenant_id),
    )
    .send()
    .await
    {
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
    let pair_join = spawn_blocking_tracked(move || {
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
            // The full context chain (`{e:#}`), not the outermost layer: a
            // pair failure is most often a TRANSPORT fault, and `os error
            // 10053` / `operation timed out` lived one hop below the line this
            // used to print.
            crate::util::egress_context::record_failure(
                crate::util::egress_context::EgressClient::DeviceJwtRefresher,
            );
            warn!(
                "device_jwt_refresher: pair_with_auth_token_with_ids failed: {e:#} {}",
                crate::util::egress_context::snapshot_line(
                    crate::util::egress_context::EgressClient::DeviceJwtRefresher
                )
            );
            return RefreshOutcome::KeptExisting;
        }
    };

    // Coord returned 2xx → persist the new JWT into the access_token
    // slot. The refresh-token slot stays empty (device-JWT lifecycle is
    // owned by coord, not by an OAuth refresh chain). Guarded by the
    // tenant we just asked pair-cli to mint for — see `RefreshOutcome::TenantMismatch`.
    match auth_manager.store_tokens_expecting(&resp.token, "", Some(tenant_id)) {
        Ok(()) => {
            // M1: a NEW credential is in the slot — and, via the mirror, in
            // its tenant's slot — so every rejection coord recorded against
            // the old one is spent. Without this the streak survives
            // replacement and `dark` latches on a working token.
            retire_rejection_streaks_after_legacy_mint(&resp.token);
            RefreshOutcome::Replaced {
                new_jwt: resp.token,
            }
        }
        Err(e) => {
            if let Some(mismatch) = e.downcast_ref::<crate::auth::TenantMismatch>() {
                DEVICE_JWT_REFRESH_TENANT_MISMATCH_TOTAL
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                RefreshOutcome::TenantMismatch {
                    expected: mismatch.expected,
                    returned: mismatch.returned,
                }
            } else {
                warn!("device_jwt_refresher: persist new JWT failed: {e}");
                RefreshOutcome::PersistFailed(e.to_string())
            }
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
            warn!(
                "device_jwt_refresher: device self-refresh client build failed: {} \
                 — falling back to Cognito",
                crate::util::error_chain::error_chain(&e)
            );
            return None;
        }
    };
    let resp = {
        let _in_flight = crate::util::egress_context::in_flight(
            crate::util::egress_context::EgressClient::DeviceJwtRefresher,
        );
        // coord-auth-exempt(self-refresh): presents the EXPIRING device JWT itself —
        // coord authenticates the refresh against the very token being replaced.
        // Re-resolving through the helper would read the same slot back and buy
        // nothing but a counter tick.
        client.post(&url).bearer_auth(current.trim()).send().await
    };
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            crate::util::egress_context::record_failure(
                crate::util::egress_context::EgressClient::DeviceJwtRefresher,
            );
            warn!(
                "device_jwt_refresher: device self-refresh request to {url} failed: {} \
                 — falling back to Cognito {}",
                crate::util::error_chain::error_chain(&e),
                crate::util::egress_context::snapshot_line(
                    crate::util::egress_context::EgressClient::DeviceJwtRefresher
                )
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
    // device-JWT lifecycle is coord-owned, not an OAuth refresh chain). Guarded
    // by the OUTGOING token's own tenant claim: coord authenticated this
    // request against `current`, so a re-mint for any other tenant is a coord
    // bug or a stale slot, never a legitimate answer to THIS request.
    let expected_tenant = crate::auth::jwt_tenant_claim(&current);
    match auth_manager.store_tokens_expecting(&body.token, "", expected_tenant) {
        Ok(()) => {
            // M1: the old credential's rejections are spent evidence — for the
            // default bucket and for the tenant slot the mirror just wrote.
            retire_rejection_streaks_after_legacy_mint(&body.token);
            info!(
                "device_jwt_refresher: device-JWT self-refreshed login-independently (len={})",
                body.token.len()
            );
            Some(body.token)
        }
        Err(e) => {
            if let Some(mismatch) = e.downcast_ref::<crate::auth::TenantMismatch>() {
                DEVICE_JWT_REFRESH_TENANT_MISMATCH_TOTAL
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                warn!(
                    "device_jwt_refresher: device self-refresh re-minted for tenant \
                     {:?}, not the requested {} — refusing to persist and falling back \
                     to Cognito (existing JWT preserved)",
                    mismatch.returned, mismatch.expected
                );
            } else {
                warn!(
                    "device_jwt_refresher: persist self-refreshed JWT failed: {e} \
                     — falling back to Cognito (existing JWT preserved)"
                );
            }
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
    /// Absent/opaque `exp`, or already expired — this token can never be
    /// presented (coord would 401 it) and can never self-refresh.
    ///
    /// **This used to be `SkipNotRefreshable`, and it was an ABSORBING STATE**
    /// (plan `2026-08-31-coord-mcp-credential-selection-by-binding-provenance`
    /// Phase 2a). "REPLACE-not-REVOKE: never cleared here" left the slot
    /// holding a dead credential with no path out of the arm: every pass
    /// re-derived the same verdict, warned, and continued. Measured on the
    /// operator box: two slots expired 2026-07-14 and 2026-08-07, re-warned
    /// ~2x every 5 minutes for the full 14-day log retention window — 15,826
    /// passes, 0 refreshed, 0 healed. Meanwhile `select_device_bearer` kept
    /// serving those same dead tokens as slot HITS.
    ///
    /// The exit is to CLEAR the slot and re-derive through the
    /// device-machine-key exchange. REPLACE-not-REVOKE is the right rule for a
    /// TRANSIENT failure — it is the wrong rule for a credential we have
    /// locally decoded as dead.
    ClearAndRederive,
}

/// Pure per-slot staleness decision. `exp` is the slot JWT's decoded
/// (unverified) `exp` claim; `now` is unix seconds.
pub(crate) fn plan_tenant_slot(exp: Option<i64>, now: i64) -> TenantSlotPlan {
    match exp {
        // Opaque/undecodable — cannot judge or present it for self-refresh.
        None => TenantSlotPlan::ClearAndRederive,
        // Already expired — coord would 401 the presented bearer.
        Some(e) if now >= e => TenantSlotPlan::ClearAndRederive,
        // Within TTL/3 of expiry — refresh now.
        Some(e) if now + crate::auth::REFRESH_BEFORE_EXPIRY_SECS >= e => TenantSlotPlan::Refresh,
        // Comfortably fresh.
        Some(_) => TenantSlotPlan::SkipFresh,
    }
}

/// The ONLY two things that may ever reach a slot CLEAR.
///
/// Phase 2's mandatory safeguard, expressed in the type rather than in a
/// comment: a clear is constructible only from one of these, and neither can
/// be built out of a transport failure. A timeout or a connection error is
/// UNKNOWN — it says nothing about the credential — and must leave the slot
/// exactly as it found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotClearCause {
    /// LOCAL evidence: the slot JWT's own decoded `exp` is in the past, or the
    /// value is opaque and can never be presented nor judged. No network was
    /// involved in reaching this verdict.
    DecodedExpiry,
    /// COORD-ATTRIBUTED evidence: coord answered the refresh with an
    /// authentication rejection (see [`slot_refresh_is_credential_rejection`])
    /// for the credential we presented. Never a 5xx, never a 404, never a
    /// transport error.
    CoordRejection,
}

impl SlotClearCause {
    /// Short stable token for the rotation-forensics row and the health signal.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SlotClearCause::DecodedExpiry => "decoded-expiry",
            SlotClearCause::CoordRejection => "coord-rejection",
        }
    }
}

/// Outcome of one per-tenant slot in a [`refresh_tenant_slots`] pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TenantSlotOutcome {
    /// Coord 2xx'd and the new JWT was persisted into that tenant's slot.
    Refreshed,
    /// Slot JWT still comfortably fresh — nothing to do.
    SkippedFresh,
    /// The slot holds nothing to work with (empty), or the store could not be
    /// read. Nothing to refresh and — critically — nothing to clear: an
    /// unreadable store is UNKNOWN, not a dead credential.
    SkippedNoToken,
    /// The slot was CLEARED, on the evidence named by `cause`. `rederived`
    /// says whether the device-machine-key exchange then put a working
    /// credential back into it.
    ///
    /// Phase 2a/2b: this variant is the EXIT from the two absorbing states the
    /// pass used to have. Reaching it always emits a rotation-forensics row
    /// naming the evidence.
    Cleared {
        cause: SlotClearCause,
        rederived: bool,
    },
    /// Refresh was attempted and failed in a way that says NOTHING about the
    /// credential: a transport error, a client-build failure, a non-rejection
    /// HTTP status (5xx, 404, 429), an undecodable body, an empty token, or a
    /// persist error. The existing slot value is left UNTOUCHED
    /// (REPLACE-not-REVOKE), and the pass CONTINUES with the remaining slots.
    KeptExisting,
}

/// Does this refresh status mean coord REJECTED the credential we presented?
///
/// Pure and deliberately narrow, because it is the gate on a destructive
/// action. `401`/`403` are coord saying "this bearer is not acceptable" — the
/// slot is dead and keeping it only perpetuates the absorbing state.
/// Everything else is excluded on purpose:
///
/// * `5xx` — coord is unwell; the credential is unjudged. The fleet already
///   pins this posture for the legacy path
///   (`refresher_handles_coord_503_without_clearing_jwt`).
/// * `404` — the device row is unknown to THIS coord (a mis-pointed base URL
///   is the common cause); that is a routing fault, not a verdict on the key.
/// * `429` and everything else — no statement about the credential at all.
pub(crate) fn slot_refresh_is_credential_rejection(status: u16) -> bool {
    matches!(status, 401 | 403)
}

/// One row of [`TenantSlotHealth`] — the state of a single
/// `device_jwt:<tenant>` slot as of the last pass.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TenantSlotHealthRow {
    pub tenant_id: String,
    /// Stable machine-readable outcome token (`refreshed`, `skipped-fresh`,
    /// `skipped-no-token`, `cleared`, `kept-existing`).
    pub outcome: String,
    /// For a clear: which evidence class authorised it
    /// ([`SlotClearCause::as_str`]). `None` otherwise.
    pub clear_cause: Option<String>,
    /// For a clear: whether a working credential was put back.
    pub rederived: Option<bool>,
    /// Human-readable evidence, e.g. the decoded expiry or the coord status.
    pub detail: String,
}

/// The per-tenant slot pass's structured health, published on every pass.
///
/// Phase 2c. This replaces a 5-minutely `warn!` that nobody greps — the
/// operator-box investigation found 15,826 of them in the retained window and
/// the condition had still gone unnoticed for six weeks. A log line is not a
/// health signal; it is a hope that somebody runs the right `grep`.
///
/// Deliberately just STATE: a later phase wires it into the `/health` surface.
/// Nothing here builds an endpoint.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TenantSlotHealth {
    /// Unix seconds when this snapshot was taken.
    pub observed_at_unix: i64,
    pub slots: Vec<TenantSlotHealthRow>,
    /// How many slots are currently in a state that needs attention — a clear
    /// that could not re-derive, or a refresh that keeps failing.
    pub degraded_slots: usize,
    /// Cumulative clears since process start, by evidence class. A number that
    /// climbs steadily is a real signal; one that climbs once and stops is the
    /// self-heal working.
    pub cleared_on_expiry_total: u64,
    pub cleared_on_rejection_total: u64,
    /// Tenants coord says this device is bound to that hold NO credential
    /// here — or why that is UNKNOWN. See [`BindingGapReport`].
    ///
    /// Snapshotted from [`binding_gaps`] at publish time rather than computed
    /// here: the pass walks the slots that EXIST and structurally cannot see a
    /// tenant with none, so the report is composed by the loop from the same
    /// blocking-pool read the eviction sweep uses, and lands here so the pass
    /// and the gap answer are one snapshot instead of two surfaces.
    pub binding_gaps: BindingGapReport,
}

static TENANT_SLOT_HEALTH: std::sync::OnceLock<std::sync::Mutex<Option<TenantSlotHealth>>> =
    std::sync::OnceLock::new();
static CLEARED_ON_EXPIRY_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CLEARED_ON_REJECTION_TOTAL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn tenant_slot_health_cell() -> &'static std::sync::Mutex<Option<TenantSlotHealth>> {
    TENANT_SLOT_HEALTH.get_or_init(|| std::sync::Mutex::new(None))
}

/// The most recent per-tenant slot health snapshot, or `None` when no pass has
/// run yet in this process.
///
/// `None` is UNKNOWN, never "healthy" — a runner that has never held a tenant
/// slot and a runner whose refresher never started are indistinguishable here,
/// and neither is evidence of health.
///
/// `allow(dead_code)`: this is the READ side of Phase 2c. The pass publishes
/// unconditionally; the `/health` surface that consumes it is explicitly a
/// later phase, and building the endpoint here was out of scope. The tests
/// exercise it.
#[allow(dead_code)]
pub fn tenant_slot_health() -> Option<TenantSlotHealth> {
    tenant_slot_health_cell()
        .lock()
        .expect("tenant slot health poisoned")
        .clone()
}

fn publish_tenant_slot_health(rows: Vec<TenantSlotHealthRow>) {
    use std::sync::atomic::Ordering;
    let degraded = rows
        .iter()
        .filter(|r| {
            r.outcome == "kept-existing" || (r.outcome == "cleared" && r.rederived == Some(false))
        })
        .count();
    let snapshot = TenantSlotHealth {
        observed_at_unix: chrono::Utc::now().timestamp(),
        slots: rows,
        degraded_slots: degraded,
        cleared_on_expiry_total: CLEARED_ON_EXPIRY_TOTAL.load(Ordering::Relaxed),
        cleared_on_rejection_total: CLEARED_ON_REJECTION_TOTAL.load(Ordering::Relaxed),
        binding_gaps: binding_gaps(),
    };
    *tenant_slot_health_cell()
        .lock()
        .expect("tenant slot health poisoned") = Some(snapshot);
}

fn health_row(
    tenant: &uuid::Uuid,
    outcome: TenantSlotOutcome,
    detail: String,
) -> TenantSlotHealthRow {
    let (cause, rederived) = match outcome {
        TenantSlotOutcome::Cleared { cause, rederived } => {
            (Some(cause.as_str().to_string()), Some(rederived))
        }
        _ => (None, None),
    };
    TenantSlotHealthRow {
        tenant_id: tenant.to_string(),
        // ONE spelling of the outcome tokens, shared with the posture's
        // `last_refresh_outcome` so the two surfaces cannot drift apart.
        outcome: tenant_slot_outcome_token(outcome),
        clear_cause: cause,
        rederived,
        detail,
    }
}

// ===========================================================================
// Coord-credential POSTURE (plan
// `2026-09-12-runner-loads-with-an-expired-coord-credential-and-tells-nobody`,
// Phase 1).
//
// The incident this closes: a runner restored an ALREADY-EXPIRED coord
// device-JWT slot at boot, `/health` read `healthy` with a hard-coded
// `credentialDoors.coordMcpForwarder.canAnswer: true`, no banner fired, and
// every session it spawned had no coord access and did not know it.
//
// The posture is DERIVED — per refresher pass, out of the per-tenant slot
// pass's own material ([`TenantSlotOutcome`] / [`TenantSlotHealth`]) plus the
// verdicts coord actually returned — and never stored as a flag. A stored flag
// is the value that goes stale while the file that carries it does not.
//
// NOTE ON INPUTS. [`RefreshOutcome`] above is the **Cognito access-token**
// refresh outcome and is NOT an input here: in the incident Cognito refreshed
// normally throughout while the coord device-JWT slot was dead. Wiring the
// posture to it would report the one credential that was healthy.
// ===========================================================================

/// Why the runner is credential-dark while the credential it holds still looks
/// alive to a local `exp` check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DarkCause {
    /// Coord REPEATEDLY rejected the credential we presented
    /// (`token_expired` / `invalid token`) even though its own decoded `exp`
    /// is in the future — a revoked `jti`, a rotated signing key, a token
    /// bound to another tenant. `exp` alone reads this as `live`, which is
    /// exactly the silent case the posture exists to catch.
    UpstreamRejected,
}

impl DarkCause {
    /// Stable machine-readable token, used on `/health`, in the banner event
    /// payload, and by the fleet's readers.
    pub fn as_str(self) -> &'static str {
        match self {
            DarkCause::UpstreamRejected => "upstream_401",
        }
    }
}

/// The runner's coord-credential posture: one typed value, derived, served.
///
/// The ladder is ordered by how specific the evidence is, not by severity —
/// see [`derive_coord_credential_posture`], which is the authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordCredentialPosture {
    /// A credential with comfortable TTL. Forwarded calls should reach coord.
    Live,
    /// Future `exp`, but inside the refresh window. Still usable; the
    /// refresher is expected to replace it this pass or next.
    Expiring,
    /// The runner HOLDS a credential whose decoded `exp` is in the past, or
    /// whose value is opaque (undecodable, so it can never be presented).
    Expired,
    /// The runner holds NO coord credential at all.
    Absent,
    /// The runner held a dead credential, the automatic exit RAN
    /// (clear + device-machine-key re-derive), and it did not put a working
    /// credential back. This is the terminal state the operator must act on —
    /// no further automatic rung exists.
    Unrefreshable,
    /// The credential looks alive locally but coord will not accept it.
    Dark(DarkCause),
}

impl CoordCredentialPosture {
    /// Stable machine-readable token. `Dark` renders as `dark`; its cause is
    /// carried separately by [`CoordCredentialPosture::cause`] so a reader
    /// never has to parse a compound string.
    pub fn as_str(self) -> &'static str {
        match self {
            CoordCredentialPosture::Live => "live",
            CoordCredentialPosture::Expiring => "expiring",
            CoordCredentialPosture::Expired => "expired",
            CoordCredentialPosture::Absent => "absent",
            CoordCredentialPosture::Unrefreshable => "unrefreshable",
            CoordCredentialPosture::Dark(_) => "dark",
        }
    }

    /// The dark cause, for `Dark` only.
    pub fn cause(self) -> Option<&'static str> {
        match self {
            CoordCredentialPosture::Dark(c) => Some(c.as_str()),
            _ => None,
        }
    }

    /// Can a call forwarded with this credential be expected to reach coord?
    ///
    /// This is what `/health`'s `credentialDoors.coordMcpForwarder.canAnswer`
    /// reports. `live`/`expiring` only — every other posture means the
    /// forwarder is alive but its answer will be coord's 401.
    pub fn can_answer(self) -> bool {
        matches!(
            self,
            CoordCredentialPosture::Live | CoordCredentialPosture::Expiring
        )
    }

    /// Display precedence when several slots disagree: the posture that most
    /// needs an operator wins the one value `/health` and the banner carry.
    /// `Unrefreshable` outranks `Dark` because its automatic recovery has
    /// already been tried and refused.
    fn severity(self) -> u8 {
        match self {
            CoordCredentialPosture::Unrefreshable => 5,
            CoordCredentialPosture::Dark(_) => 4,
            CoordCredentialPosture::Expired => 3,
            CoordCredentialPosture::Absent => 2,
            CoordCredentialPosture::Expiring => 1,
            CoordCredentialPosture::Live => 0,
        }
    }

    /// One sentence naming the state and what it means for sessions — the
    /// banner body and the `/health` `reason`. `since` is rendered by the
    /// caller, which owns the clock.
    pub fn message(self) -> &'static str {
        match self {
            CoordCredentialPosture::Live => "coord credential is live",
            CoordCredentialPosture::Expiring => {
                "coord credential is inside its refresh window; the refresher is replacing it"
            }
            CoordCredentialPosture::Expired => {
                "the coord credential this runner holds has EXPIRED. Sessions spawned now \
                 have no coord access."
            }
            CoordCredentialPosture::Absent => {
                "this runner holds NO coord credential. Sessions spawned now have no coord \
                 access."
            }
            CoordCredentialPosture::Unrefreshable => {
                "the coord credential expired and automatic refresh FAILED (unrefreshable). \
                 Sessions spawned now have no coord access."
            }
            CoordCredentialPosture::Dark(DarkCause::UpstreamRejected) => {
                "coord is REJECTING this runner's credential even though it has not expired \
                 locally. Sessions spawned now have no coord access."
            }
        }
    }

    /// Which call to action this posture needs. `retry_refresh` needs no new
    /// command — `kick_device_jwt_refresher` already exists.
    pub fn cta(self) -> Option<&'static str> {
        match self {
            CoordCredentialPosture::Live | CoordCredentialPosture::Expiring => None,
            // A held-but-expired credential is exactly what the (shipped)
            // clear + re-derive exit heals; kicking the refresher retries it
            // now rather than up to 5 minutes from now.
            CoordCredentialPosture::Expired => Some("retry_refresh"),
            // These three have already exhausted the automatic rungs.
            CoordCredentialPosture::Absent
            | CoordCredentialPosture::Unrefreshable
            | CoordCredentialPosture::Dark(_) => Some("re_pair"),
        }
    }
}

/// What one `device_jwt:<tenant>` slot contributed to the posture in a pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlotObservation {
    pub tenant_id: Option<String>,
    /// Decoded (unverified) `exp` of the credential the slot HELD when the
    /// pass looked at it. `None` = absent, or present but opaque.
    pub exp: Option<i64>,
    /// Did the slot hold any value at all when the pass looked at it?
    pub present: bool,
    /// The slot could not be READ (unreadable store). This observation says
    /// NOTHING about the credential — absence-is-not-zero — so it is dropped
    /// before the derivation rather than lowering the posture to `absent`.
    pub unknown: bool,
    /// What the pass then DID with it. `None` = the pass has not concluded for
    /// this slot — which is the BOOT observation (DD2, below).
    pub outcome: Option<TenantSlotOutcome>,
}

impl SlotObservation {
    /// A slot observed at the top of a pass, before any recovery ran.
    pub(crate) fn observed(tenant_id: Option<String>, token: Option<&str>) -> Self {
        match token.map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => Self {
                tenant_id,
                exp: crate::auth::decode_jwt_exp(t),
                present: true,
                unknown: false,
                outcome: None,
            },
            None => Self {
                tenant_id,
                exp: None,
                present: false,
                unknown: false,
                outcome: None,
            },
        }
    }

    /// An UNREADABLE slot — a statement of UNKNOWN, not of absence.
    pub(crate) fn unreadable(tenant_id: Option<String>) -> Self {
        Self {
            tenant_id,
            exp: None,
            present: false,
            unknown: true,
            outcome: None,
        }
    }
}

/// The upstream half of the derivation: what COORD said about the credential
/// this runner presented, **per credential slot**.
///
/// It used to be one process-global counter. That was wrong in the dangerous
/// direction: the credential is PER TENANT (`device_jwt:<tenant>` slots, plus
/// the legacy default slot), so tenant B's healthy 2xx traffic continuously
/// reset tenant A's rejection streak and A read `live` while dark. Keyed by
/// slot, A's rejections stay A's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct UpstreamSignal {
    /// CONSECUTIVE coord answers that attributed a rejection to the
    /// credential. Reset to 0 by any 2xx.
    pub consecutive_rejections: u32,
    pub last_rejection_at: Option<i64>,
    pub last_ok_at: Option<i64>,
}

/// How many CONSECUTIVE credential-attributed coord rejections it takes before
/// the posture calls a locally-valid credential `dark`.
///
/// This threshold is the whole answer to the plan's named risk: *"a transient
/// 401 from coord during a coord deploy must not flip the posture to dark"*.
/// The input is a RATE — consecutive rejections, reset by any success — never
/// a single answer. Pinned by
/// `a_single_transient_401_never_flips_the_posture_to_dark`.
pub(crate) const UPSTREAM_DARK_THRESHOLD: u32 = 3;

/// Did coord's answer say something about the CREDENTIAL we presented?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpstreamVerdict {
    /// 2xx — the credential worked. Resets the streak.
    Accepted,
    /// 401/403 whose body attributes the refusal to the token itself.
    CredentialRejected,
    /// Everything else: a 5xx, a 404, a 429, and — importantly — a 401 that
    /// names something OTHER than the token (a stale proxy NONCE, a missing
    /// `user_id` claim). None of those is a verdict on the device JWT, so
    /// none of them may move the streak in either direction.
    Indeterminate,
}

/// The closed set of coord `code` values that are a verdict on the CREDENTIAL
/// this runner presented — every `RejectCode` coord can answer with plus the
/// revocation refusal, and nothing else.
///
/// Enumerated against coord's own sources rather than guessed:
/// `crates/coord/src/jwt.rs` `RejectCode::as_str` (`token_expired`,
/// `token_foreign_issuer`, `token_ambiguous_legacy_kid`,
/// `token_invalid_signature`, `token_invalid`) and
/// `crates/coord/src/auth.rs` (`token_revoked`).
///
/// Deliberately NOT here, because none of them says the token is bad:
/// `missing_token` (we sent none), `keys_not_initialized` (coord is unwell),
/// `attach_grant_not_a_principal` / `create_grant_not_a_principal` (a verified
/// bearer of the wrong KIND), `auth_required`, `tenant_not_resolved` (the
/// token verified and the ROUTE wanted an operator), and
/// `invalid operator token` (a device JWT on an operator-only route — the
/// credential is fine, the door is not).
pub(crate) const CREDENTIAL_REJECTION_CODES: &[&str] = &[
    "token_expired",
    "token_invalid",
    "token_invalid_signature",
    "token_foreign_issuer",
    "token_ambiguous_legacy_kid",
    "token_revoked",
];

/// Pull coord's machine-readable refusal `code` out of a response body.
///
/// Coord's refusal shape is `{"error": <message>, "code": <code>, …}`
/// (`crates/coord/src/auth.rs` `Refusal::body`). We read THAT FIELD rather
/// than substring-matching the body, for two reasons that both bit the first
/// implementation:
///
/// * substring matching MISSED four of coord's six credential refusals
///   (`token_revoked`, `token_invalid_signature`, `token_foreign_issuer`,
///   `token_ambiguous_legacy_kid`) — including all three the
///   [`DarkCause::UpstreamRejected`] doc comment names; and
/// * it has an inverse hazard: `contains("invalid token")` matches any body
///   that merely QUOTES the phrase, e.g. an error explaining that a *nonce*
///   is not an invalid token.
///
/// Parsing the body is safe on this path: nothing between the runner and coord
/// rewrites coord's bodies (the only ALB `fixed_response` is path-scoped to
/// `/metrics`).
///
/// A truncated body (the scan is bounded) will not parse as JSON, so there is
/// a second, still-targeted rung: find the FIRST `"code"` key and read its
/// quoted value. `serde_json` sorts object keys, so coord's top-level `code`
/// precedes any nested one; this is a key lookup, never a body-wide
/// `contains`.
pub(crate) fn upstream_refusal_code(body: &str) -> Option<String> {
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(serde_json::Value::String(code)) = map.get("code") {
            return Some(code.trim().to_ascii_lowercase());
        }
        // Parsed, and there is no `code` — that IS the answer. Falling through
        // to the text scan here would re-open the substring hazard on a body
        // that was fully readable.
        return None;
    }
    let key = body.find("\"code\"")?;
    let rest = &body[key + 6..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let value = after.strip_prefix('"')?;
    let end = value.find('"')?;
    Some(value[..end].trim().to_ascii_lowercase())
}

/// Pure classifier for [`note_coord_upstream_verdict`], split out so the
/// narrow "which 401s count" rule is directly testable.
pub(crate) fn classify_upstream_verdict(status: u16, body: &str) -> UpstreamVerdict {
    if (200..300).contains(&status) {
        return UpstreamVerdict::Accepted;
    }
    if !matches!(status, 401 | 403) {
        return UpstreamVerdict::Indeterminate;
    }
    match upstream_refusal_code(body) {
        Some(code) if CREDENTIAL_REJECTION_CODES.contains(&code.as_str()) => {
            UpstreamVerdict::CredentialRejected
        }
        _ => UpstreamVerdict::Indeterminate,
    }
}

/// The key a slot's upstream signal is filed under. `None` — an UNPINNED
/// single-tenant session — presents the legacy default `access_token` slot,
/// which is a distinct credential from any tenant-keyed one, so it gets its
/// own bucket rather than being folded into an arbitrary tenant's.
pub(crate) const DEFAULT_SLOT_KEY: &str = "";

fn upstream_key(tenant: Option<uuid::Uuid>) -> String {
    tenant
        .map(|t| t.to_string())
        .unwrap_or_else(|| DEFAULT_SLOT_KEY.to_string())
}

/// Per-slot upstream signals. A `BTreeMap` because `BTreeMap::new()` is a
/// `const fn` and this is a `static` — no `OnceLock` dance needed.
static UPSTREAM_SIGNALS: std::sync::Mutex<std::collections::BTreeMap<String, UpstreamSignal>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

fn with_upstream_signals<T>(
    f: impl FnOnce(&mut std::collections::BTreeMap<String, UpstreamSignal>) -> T,
) -> T {
    let mut guard = UPSTREAM_SIGNALS.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// How much of a coord response body the classifier scans. Coord's refusal
/// body carries a `credential_free_doors` hint that can run long, but
/// `serde_json` sorts keys, so `code` is at the FRONT — this budget holds the
/// whole of it in practice and the truncation rung in
/// [`upstream_refusal_code`] covers the rest.
const UPSTREAM_BODY_SCAN_BYTES: usize = 4096;

/// Record what coord answered a call this runner forwarded with the device
/// credential for `tenant`. Called from every device-bearer forwarder; cheap
/// and never fails.
///
/// This is the input that makes a WRONG credential visible even when its `exp`
/// is in the future — the case `exp` alone calls `live`.
///
/// `authenticating` is the caller's statement about the UPSTREAM ROUTE: does
/// coord actually verify the bearer there? A `false` here moves the streak in
/// NEITHER direction, because a 200 from a route that never looked at the
/// credential is not evidence the credential works. That is not hypothetical:
/// coord's claims reads sit behind an INFALLIBLE extractor
/// (`claims_read_observe.rs`, `type Rejection = Infallible`) whose enforcement
/// arm is gated on `COORD_CLAIMS_READ_AUTH_REQUIRED`, a variable absent from
/// `deploy/taskdef.json` — so in production an expired, revoked or
/// bad-signature JWT gets a cheerful 200 there. With those 200s counted, a box
/// that polls claims could never reach `dark` and `lastOkAt` claimed coord had
/// accepted a credential coord never checked.
pub fn note_coord_upstream_verdict(
    tenant: Option<uuid::Uuid>,
    authenticating: bool,
    status: u16,
    body: &[u8],
) {
    if !authenticating {
        return;
    }
    let scan = &body[..body.len().min(UPSTREAM_BODY_SCAN_BYTES)];
    let text = String::from_utf8_lossy(scan);
    let verdict = classify_upstream_verdict(status, &text);
    if matches!(verdict, UpstreamVerdict::Indeterminate) {
        // UNKNOWN says nothing — it must not move the streak in EITHER
        // direction, or a coord outage would both create and erase dark.
        return;
    }
    let key = upstream_key(tenant);
    let now = chrono::Utc::now().timestamp();
    let mut spent = false;
    with_upstream_signals(|m| {
        let slot = m.entry(key.clone()).or_default();
        match verdict {
            UpstreamVerdict::Accepted => {
                // CONTRADICTION — the honest retirement, and the reason a
                // claimed slot has never needed a clock.
                slot.consecutive_rejections = 0;
                slot.last_ok_at = Some(now);
                spent = true;
            }
            UpstreamVerdict::CredentialRejected => {
                slot.consecutive_rejections = slot.consecutive_rejections.saturating_add(1);
                slot.last_rejection_at = Some(now);
            }
            UpstreamVerdict::Indeterminate => unreachable!("returned above"),
        }
    });
    if spent {
        clear_orphan_warning(&key);
    }
}

/// A NEW credential was persisted into `tenant`'s slot — so every rejection
/// recorded against the OLD one is spent evidence.
///
/// Without this the streak survived credential replacement and `dark` LATCHED
/// on a working token: revoked → 3 × 401 → `dark`; the re-derive succeeds and
/// rung 1 publishes `live`; the next pass is `SkippedFresh`, rung 5 sees the
/// stale streak, and the runner goes `dark` again on a credential coord has
/// just minted — behind a banner the user cannot dismiss.
pub(crate) fn reset_upstream_rejections_for(tenant: Option<uuid::Uuid>) {
    let key = upstream_key(tenant);
    with_upstream_signals(|m| {
        let slot = m.entry(key.clone()).or_default();
        // The timestamps stay: `last401At` is forensics about a real past
        // event. Only the STREAK — the live predicate — is spent.
        slot.consecutive_rejections = 0;
    });
    clear_orphan_warning(&key);
}

/// Read one slot's aggregated upstream signal. `tenant_id` is the slot key as
/// the posture observations carry it (a tenant UUID string), or `None` for the
/// legacy default slot.
pub(crate) fn upstream_signal_for(tenant_id: Option<&str>) -> UpstreamSignal {
    let key = tenant_id.unwrap_or(DEFAULT_SLOT_KEY);
    with_upstream_signals(|m| m.get(key).copied().unwrap_or_default())
}

/// The default slot's signal — the process-wide reading that used to be the
/// only one. Kept for the pure-derivation tests.
#[cfg(test)]
pub(crate) fn upstream_signal() -> UpstreamSignal {
    upstream_signal_for(None)
}

/// When is one bucket's evidence newer than another's?
fn latest_upstream_event(s: &UpstreamSignal) -> i64 {
    s.last_rejection_at
        .unwrap_or(0)
        .max(s.last_ok_at.unwrap_or(0))
}

/// Evidence about an unclaimed bucket is retired by CONTRADICTION or by
/// IMPOSSIBILITY — never by age. This is the impossibility half's backstop,
/// and it is deliberately far past any plausible idle period.
///
/// The first cut of this used a 30-minute age bound as the PRIMARY guard, and
/// that was a healthy-while-dead defect in its own right. A legacy-only box
/// that coord is refusing goes QUIET — every call failing is exactly what
/// stops sessions calling — so at t+35min the bucket aged out, the orphan was
/// filtered, `worst` fell through to the legacy observation's future `exp`,
/// and the pass published `live` plus an explicit *"Coord access restored"*
/// banner. Nothing had healed. It was self-reinforcing: the darker a box, the
/// quieter it gets, and the quieter it gets, the healthier it reports.
///
/// The two honest retirements, both used here:
///
/// * **CONTRADICTION** — a 2xx on that key zeroes the streak
///   ([`note_coord_upstream_verdict`]). Needs no clock, which is precisely why
///   a CLAIMED slot has never needed one.
/// * **IMPOSSIBILITY** — the key can never be written again, because no
///   credential this runner can present resolves to it any more
///   ([`evict_unwritable_upstream_buckets`]). That is the re-pair/unpair case
///   the clock was actually written for, and it is CHECKABLE rather than
///   guessed.
///
/// What remains is a bucket that survived both — no success, still writable,
/// and silent for a week. That is not evidence about now either, but it is
/// also not a reason to announce health: it makes the pass ABSTAIN
/// ([`UnclaimedVerdict::StaleEvidence`]), leaving the previous posture
/// standing. Stuck-dark is the acceptable direction; false-healthy is not.
pub(crate) const UPSTREAM_ORPHAN_STALE_AFTER_SECS: i64 = 7 * 24 * 60 * 60;

/// Slot keys whose orphan warning has already been emitted this process, so
/// the `warn!` beside the unattributable publish fires ONCE per key instead of
/// every pass forever (the same latching the `warn_once_per_tenant_*` idiom in
/// `crate::auth` uses). Cleared whenever the bucket's streak is spent or the
/// bucket is evicted, so a genuinely new episode warns again.
static ORPHAN_WARNED_KEYS: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

fn with_orphan_warned<T>(f: impl FnOnce(&mut std::collections::BTreeSet<String>) -> T) -> T {
    let mut g = ORPHAN_WARNED_KEYS.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

/// Latch the orphan warning for `key`. `true` the first time, `false` after.
fn latch_orphan_warning(key: &str) -> bool {
    with_orphan_warned(|s| s.insert(key.to_string()))
}

fn clear_orphan_warning(key: &str) {
    with_orphan_warned(|s| {
        s.remove(key);
    });
}

/// Has the orphan warning for `key` been emitted (and not re-armed since)?
#[cfg(test)]
pub(crate) fn orphan_warning_latched(key: &str) -> bool {
    with_orphan_warned(|s| s.contains(key))
}

/// Log, LATCHED per key, that coord is refusing a credential filed under a
/// slot key no observation in this pass describes.
///
/// The KEY is the one actionable fact the published posture cannot carry (the
/// unattributable arm withholds `tenant_id`; a non-answering slot or pin that
/// wins the publish names a different tenant), so it goes to the log. Latched,
/// because the condition holds across every pass and an unlatched line would
/// repeat every five minutes forever. A tenant UUID, never a credential.
///
/// `published` says which arm the pass took: `true` when this orphan IS the
/// published nameless `dark`, `false` when a per-slot or pinned verdict that is
/// already non-answering won the publish and the orphan would otherwise be
/// silent. One latch serves both arms: a key first logged as NOT published
/// stays latched if it later becomes the published nameless `dark`, so the log
/// names each key once per episode rather than once per arm.
fn warn_unclaimed_orphan(orphan_key: &str, orphan_signal: &UpstreamSignal, published: bool) {
    if !latch_orphan_warning(orphan_key) {
        return;
    }
    let shown = if orphan_key.is_empty() {
        "<default>"
    } else {
        orphan_key
    };
    let action = if published {
        "publishing dark with no tenant"
    } else {
        "NOT published — a slot or the machine pin this pass observed is already \
         non-answering and holds the posture — but coord is refusing this key too"
    };
    warn!(
        "device_jwt_refresher: coord has refused the credential filed under slot key {:?} \
         {} consecutive times, and NO slot this pass observed describes it — {action} \
         (legacy-only install, a pin with no slot, or a slot this pass could not read)",
        shown, orphan_signal.consecutive_rejections,
    );
}

/// Drop every bucket that NOTHING CAN EVER WRITE AGAIN.
///
/// A tenant-keyed bucket is only ever written by a forwarder that resolved
/// that tenant and presented a credential for it, and a credential exists for
/// exactly two routes: the per-tenant slots
/// ([`crate::auth::AuthManager::list_tenant_device_jwt_tenants`]) and the
/// device's DEFAULT binding ([`crate::auth::default_binding_tenant`], which
/// `select_device_bearer` serves out of the legacy slot). See
/// [`resolve_writable_slot_keys`] for the invariant, with its call sites, and
/// for why the machine pin is not a third route. A key in neither of them
/// belongs to a tenant this runner has been re-paired or unpaired away from:
/// its streak can never be contradicted by a 2xx, because no call will ever
/// present that credential again.
///
/// That — not a clock — is what stops an orphan bucket pinning a healthy
/// re-paired runner dark forever.
///
/// The DEFAULT key is NEVER evicted: an unpinned session presents the legacy
/// slot, so that bucket is always writable.
///
/// `writable` is passed in rather than resolved here so the sweep is
/// hermetically testable without touching `$HOME`, the credential store or
/// `machine.json`. Returns the keys it dropped, for the log and the tests.
pub(crate) fn evict_unwritable_upstream_buckets(
    writable: &std::collections::HashSet<String>,
) -> Vec<String> {
    let dropped: Vec<String> = with_upstream_signals(|m| {
        let doomed: Vec<String> = m
            .keys()
            .filter(|k| k.as_str() != DEFAULT_SLOT_KEY && !writable.contains(k.as_str()))
            .cloned()
            .collect();
        for k in &doomed {
            m.remove(k);
        }
        doomed
    });
    for k in &dropped {
        clear_orphan_warning(k);
    }
    dropped
}

/// The writable-key set, or the reason it could not be established.
///
/// Eviction is DESTRUCTIVE and its two inputs both fail OPEN — each one
/// silently degrades to "absent" on failure — so the set must be FULLY
/// MEASURED before anything is deleted. A shrunken set does not mean "fewer
/// tenants are reachable"; it means the question was not answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WritableSlotKeys {
    Measured(std::collections::HashSet<String>),
    /// Nothing established. Names the input that failed, for the log.
    Unknown(&'static str),
}

// ===========================================================================
// BINDING GAPS — the one residual of D2 (plan
// `2026-09-20-per-tenant-coord-credentials-and-a-workspace-tenant-pin`,
// Phase 3). The warm-keeping loop itself shipped with Phase 8a of
// `2026-07-02-session-scoped-multi-tenant-device-binding`; nothing here
// rebuilds it.
// ===========================================================================

/// Tenants coord says this device is bound to for which this box holds NO
/// credential at all — or why that question could not be answered.
///
/// # Why this is a tri-state and not a `Vec`
///
/// The warm-keeping pass walks the slots that EXIST
/// ([`crate::auth::AuthManager::list_tenant_device_jwt_tenants`]). A tenant
/// bound with no slot never enters it: there is nothing to refresh, nothing
/// to clear and nothing to re-derive, so nothing ever asks for it. Making
/// that visible is this phase's entire job — and the trap is that every
/// input to the answer fails OPEN.
///
/// The bound set comes from the heartbeat sidecar `coord_bound_tenants.json`
/// ([`qontinui_runner_lib::pair::coord_bound_tenants`]), NOT from
/// `paired_user.json` `bindings`: that file deliberately holds only tenants
/// this runner already has a credential for and never fabricates an entry,
/// so `bindings ⊆ slots ∪ default` by construction and the difference is
/// always empty. Measured 2026-09-20 on the dev box: the sidecar is ABSENT
/// there, so a reader that took absence for zero would have printed
/// *"no gaps"* on a box with a known gap.
///
/// `Gaps(vec![])` is a MEASURED zero. `Unknown` is silence. The two must
/// never render the same way.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingGapReport {
    /// MEASURED. Every bound tenant was compared against every credential
    /// this box holds; these are the ones with none. Sorted, distinct.
    Gaps(Vec<String>),
    /// NOTHING was established — an absent or stale sidecar, an unreadable
    /// slot store, or an unreadable `paired_user.json`. Never "no gaps".
    Unknown(String),
}

/// Pure: which bound tenants have no credential on this box, or why that is
/// UNKNOWN.
///
/// Every UNKNOWN arm aborts rather than contributing nothing, for the same
/// reason [`resolve_writable_slot_keys`] does: each input collapses its
/// failure into an absence, and an absence on the COVERED side manufactures a
/// gap while an absence on the BOUND side erases one. Only a fully measured
/// pair is a report.
///
/// The covered set is slots ∪ the default binding — the default tenant is
/// served by the legacy `access_token` slot, so it is not a gap even with no
/// `device_jwt:<t>` entry (see [`crate::auth::select_device_bearer`]).
///
/// **No mint.** This function reports; it does not act. Seeding a gap is
/// Phase 4 and a separate PR.
pub(crate) fn resolve_binding_gaps(
    tenant_slots: Option<&[uuid::Uuid]>,
    default_binding: crate::auth::BindingTenantRead,
    bound: &qontinui_runner_lib::pair::CoordBoundTenantsRead,
) -> BindingGapReport {
    let bound = match bound {
        qontinui_runner_lib::pair::CoordBoundTenantsRead::Unknown(why) => {
            return BindingGapReport::Unknown((*why).to_string())
        }
        qontinui_runner_lib::pair::CoordBoundTenantsRead::Known(ids) => ids,
    };
    let Some(slots) = tenant_slots else {
        return BindingGapReport::Unknown(
            "the tenant device-JWT slot store could not be enumerated (an undecryptable \
             store reads as EMPTY) — the covered set is UNKNOWN, so a gap cannot be \
             distinguished from a slot this pass simply failed to see"
                .to_string(),
        );
    };
    let mut covered: std::collections::HashSet<uuid::Uuid> = slots.iter().copied().collect();
    match default_binding {
        // The legacy `access_token` slot serves the default binding, so a
        // default tenant with no per-tenant slot is not a gap.
        crate::auth::BindingTenantRead::Bound(t) => {
            covered.insert(t);
        }
        // MEASURED: no default binding. Contributes nothing, and that is a
        // fact rather than a gap.
        crate::auth::BindingTenantRead::Unbound => {}
        crate::auth::BindingTenantRead::Unknown => {
            return BindingGapReport::Unknown(
                "paired_user.json is unreadable or malformed — this box's DEFAULT binding \
                 is UNKNOWN, so a bound tenant covered by the legacy slot cannot be told \
                 apart from one with no credential at all"
                    .to_string(),
            );
        }
    }
    BindingGapReport::Gaps(
        bound
            .iter()
            .filter(|t| !covered.contains(t))
            .map(|t| t.to_string())
            .collect(),
    )
}

static BINDING_GAPS: std::sync::OnceLock<std::sync::Mutex<BindingGapReport>> =
    std::sync::OnceLock::new();

/// Before any pass has read the sidecar, the answer is UNKNOWN — a process
/// that has not looked has not found nothing.
fn binding_gaps_cell() -> &'static std::sync::Mutex<BindingGapReport> {
    BINDING_GAPS.get_or_init(|| {
        std::sync::Mutex::new(BindingGapReport::Unknown(
            "no refresher pass has read coord_bound_tenants.json yet in this process".to_string(),
        ))
    })
}

/// Record this pass's binding-gap report. ONE cell, read by
/// [`publish_tenant_slot_health`] when it builds a snapshot, so the doctor and
/// the loop cannot grow separate answers.
pub(crate) fn publish_binding_gaps(report: BindingGapReport) {
    *binding_gaps_cell().lock().expect("binding gaps poisoned") = report;
}

/// The most recent binding-gap report.
pub(crate) fn binding_gaps() -> BindingGapReport {
    binding_gaps_cell()
        .lock()
        .expect("binding gaps poisoned")
        .clone()
}

/// Compose the two reads into a writable-key set, ABORTING on any UNKNOWN.
///
/// # Why every input aborts rather than contributing nothing
///
/// Each read collapses a failure into an absence:
///
/// * `list_tenant_device_jwt_tenants()` is `load_tokens().map(…).unwrap_or_default()`
///   — a present-but-undecryptable store, a partial write, an I/O blip or
///   contention with a concurrent writer all return an EMPTY Vec,
///   indistinguishable from "this runner has no tenant slots". The caller now
///   passes [`crate::auth::AuthManager::try_list_tenant_device_jwt_tenants`]'s
///   verdict instead, and `None` here is its `Err`.
/// * `default_binding_tenant()` was `.ok()?` on both the file read and the
///   JSON parse; [`crate::auth::BindingTenantRead`] now separates a MEASURED
///   `Unbound` (no pairing file — a legitimately unbound device, safe) from
///   `Unknown`.
///
/// With either of them unmeasured, a bucket could be evicted for a tenant that is
/// in fact still reachable — and eviction DELETES the streak rather than
/// filtering it, so the posture does not merely pause: it collapses to `live`
/// on the next pass and fires *"Coord access restored"* at a runner that never
/// healed, with recovery gated on three fresh rejections that a quiet box may
/// take arbitrarily long to produce. Deferring an eviction costs nothing — the
/// abstain arm holds the posture meanwhile — while evicting on unmeasured
/// inputs destroys evidence. Fail-closed is free here; fail-open is not.
///
/// This is the standard this module already applies twice: the legacy posture
/// branch probes `probe_access_token()` precisely so an undecryptable store
/// does not read as "no credential", and `refresh_tenant_slots` has an
/// explicit *"slot unreadable — UNKNOWN, not cleared"* arm. The sweep had
/// reintroduced the collapse and attached a destructive action to it.
///
/// # The bearer set, and why the machine pin can only SPARE
///
/// A bucket is keyed by the tenant a forwarder presented a device BEARER for,
/// and a bearer exists only for a slot tenant or the default binding:
///
/// * Every production writer of a bucket keys it on the tenant that
///   [`crate::auth::device_bearer_for`] returned a non-empty bearer for, or on
///   `None` (the default key). The coord-mcp proxy binds that tenant
///   (`mcp_api.rs:5223`), degrades to `COORD_MCP_PROXY_CREDENTIAL_REFRESHING`
///   with no bearer before any upstream call (`mcp_api.rs:5238-5262`), and
///   records under the first selection's tenant (`mcp_api.rs:5742`). The
///   claims-read and write proxies pass `proxy_request_gate`, which refuses a
///   missing bearer (`coord_mcp.rs:5138-5146`), before they record
///   (`mcp_api.rs:6537`, `mcp_api.rs:7544`). Nothing outside `mcp_api.rs`
///   records a verdict.
/// * [`crate::auth::select_device_bearer`] (`auth.rs:1512-1540`) returns a
///   bearer for tenant `t` only when `device_jwt:<t>` holds a usable JWT — the
///   same map [`crate::auth::AuthManager::try_list_tenant_device_jwt_tenants`]
///   enumerates — or when `t` is the default binding (the legacy slot). It has
///   NO legacy fallback for any other tenant, and `crate::auth`'s test
///   `a_slotless_non_default_tenant_gets_no_bearer_even_beside_a_live_legacy_slot`
///   fails if one is added.
///
/// So bucket keys ⊆ slots ∪ default binding ∪ `""`. The machine pin and the
/// device-JWT claim only choose WHICH tenant the forwarder asks a bearer for; a
/// tenant outside that union gets none, is refused or degraded, and writes no
/// bucket. The claim is therefore not read here at all. Reading the pin and the
/// claim as ABORTING inputs added no writable key and one failure mode: a box
/// with no `machine.json` and an unreadable or claimless legacy slot aborted the
/// sweep on every pass, so an orphan bucket pinned the posture dark forever.
///
/// A key that WAS reachable when coord answered and is not now (its slot was
/// cleared, or the device re-paired) can no longer be written: nothing can
/// present that credential again, so nothing can contradict its streak.
///
/// # The machine pin is a SPARE-only input
///
/// `machine_pin` can only KEEP a key; it never aborts the sweep.
///
/// * `Pinned(t)` adds `t`. Coord revokes `t`, `clear_and_rederive_tenant_slot`
///   clears `t`'s slot and the re-derive fails: `t` is now neither a slot nor
///   the default, so by the bearer set alone its streak would be evicted.
///   Nothing would then observe `t`, the posture could publish `live` and
///   *"Coord access restored"*, and every `t`-pinned session would keep getting
///   `COORD_MCP_PROXY_CREDENTIAL_REFRESHING`. Sparing the pinned tenant's
///   bucket leaves it for [`UnclaimedVerdict`] to read as dark — which is true:
///   this machine still asks for `t` and cannot get a bearer for it.
/// * `Unpinned` and `Unresolvable` add NOTHING and never abort. Omitting a key
///   only PERMITS evicting a bucket that, by the invariant above, can no longer
///   be written, so an unmeasured pin cannot destroy live evidence and a box
///   with no `machine.json` still retires its orphans.
///
/// An extra kept key can only hold the posture dark (the acceptable direction);
/// it can never announce a recovery.
pub(crate) fn resolve_writable_slot_keys(
    tenant_slots: Option<&[uuid::Uuid]>,
    default_binding: crate::auth::BindingTenantRead,
    machine_pin: crate::session::tenant_pin::TenantPin,
) -> WritableSlotKeys {
    let Some(slots) = tenant_slots else {
        return WritableSlotKeys::Unknown("tenant device-JWT slot store unreadable");
    };
    let mut writable: std::collections::HashSet<String> =
        slots.iter().map(|t| t.to_string()).collect();
    match default_binding {
        crate::auth::BindingTenantRead::Bound(t) => {
            writable.insert(t.to_string());
        }
        // MEASURED: this device has no default binding. Contributes nothing,
        // and that is a fact rather than a gap.
        crate::auth::BindingTenantRead::Unbound => {}
        crate::auth::BindingTenantRead::Unknown => {
            return WritableSlotKeys::Unknown("paired_user.json unreadable or malformed");
        }
    }
    // SPARE-only (see above): a pinned tenant is kept; no pin state aborts.
    if let crate::session::tenant_pin::TenantPin::Pinned(t) = machine_pin {
        writable.insert(t.to_string());
    }
    WritableSlotKeys::Measured(writable)
}

/// Every input the eviction sweep reads from disk, gathered in ONE hop to the
/// blocking pool so none of it runs on the async executor.
struct SweepInputs {
    /// The UN-COLLAPSED slot enumeration. The sweep reads its `Err` as
    /// UNKNOWN; the refresher's branch choice keeps the historical collapse
    /// (unreadable → no slots → the legacy arm).
    tenant_slots: anyhow::Result<Vec<uuid::Uuid>>,
    default_binding: crate::auth::BindingTenantRead,
    /// SPARE-only — see [`resolve_writable_slot_keys`].
    machine_pin: crate::session::tenant_pin::TenantPin,
    /// Coord's authoritative bound-tenant set, or why it is UNKNOWN. Read in
    /// THIS hop rather than a new one: it is the third small file in the same
    /// directory as `paired_user.json`, and the eviction sweep's inputs are
    /// already gathered here so none of it runs on the async executor. Feeds
    /// [`resolve_binding_gaps`] only — nothing destructive reads it.
    coord_bound_tenants: qontinui_runner_lib::pair::CoordBoundTenantsRead,
}

/// Blocking: reads the slot store, `paired_user.json`, `machine.json` and the
/// heartbeat's `coord_bound_tenants.json` sidecar.
fn read_sweep_inputs(auth_manager: &crate::auth::AuthManager) -> SweepInputs {
    SweepInputs {
        tenant_slots: auth_manager.try_list_tenant_device_jwt_tenants(),
        default_binding: crate::auth::default_binding_tenant_probe(),
        machine_pin: crate::session::tenant_pin::resolve_tenant_pin(),
        coord_bound_tenants: qontinui_runner_lib::pair::coord_bound_tenants(),
    }
}

impl SweepInputs {
    /// The half of these inputs the POSTURE reads, too — or
    /// [`PosturePinInputs::UNPINNED`] when the slot store could not be read.
    ///
    /// An unreadable store takes the LEGACY arm (the branch choice collapses
    /// `Err` to "no slots"), whose only observation is the `access_token` slot
    /// with `tenant_id: None`. That observation can never name the pinned
    /// tenant, so feeding the real pin through would call a pin `unserved` on a
    /// multi-tenant box whose `device_jwt:<pin>` slot may be perfectly healthy —
    /// one failed read publishes `absent(T)`, and the next good read fires
    /// *"Coord access restored"*. An unreadable input is not evidence, so the
    /// posture is derived from the observations alone, exactly as if no pin
    /// existed. `UNPINNED` rather than a doctored `Unresolvable` pin because the
    /// pin itself WAS read; what is missing is any measurement of whether a slot
    /// serves it. A readable but EMPTY store (`Ok(vec![])`, a legacy-only
    /// install) is a measured absence and keeps the real pins.
    fn posture_pin_inputs(&self) -> PosturePinInputs {
        if self.tenant_slots.is_err() {
            return PosturePinInputs::UNPINNED;
        }
        PosturePinInputs {
            machine_pin: self.machine_pin,
            default_binding: self.default_binding,
        }
    }
}

/// What the posture needs to know about the tenant this machine ASKS for,
/// beyond the slots a pass observed: `machine.json`'s pin and
/// `paired_user.json`'s default binding, already read by
/// [`read_sweep_inputs`] and passed in rather than re-read.
///
/// Without it, a pinned tenant whose slot was cleared BEFORE any rejection
/// streak formed was observed by nothing and remembered by no bucket, so a
/// healthy sibling slot published `live` and *"Coord access restored"* while
/// every session pinned to that tenant was refused. See
/// [`derive_and_publish_posture`]'s unserved-pin arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PosturePinInputs {
    pub machine_pin: crate::session::tenant_pin::TenantPin,
    pub default_binding: crate::auth::BindingTenantRead,
}

impl PosturePinInputs {
    /// No pin: the posture is derived from the observations alone, exactly as
    /// before the pin was an input. For callers (and tests) with no
    /// `machine.json` to speak of.
    pub(crate) const UNPINNED: Self = Self {
        machine_pin: crate::session::tenant_pin::TenantPin::Unpinned,
        default_binding: crate::auth::BindingTenantRead::Unknown,
    };

    /// The tenant `machine.json` pins, iff that pin is POSITIVELY unserved by
    /// this pass: no observation — an unreadable one included — names it, and
    /// the default binding is MEASURED to be something else.
    ///
    /// Every unmeasured input abstains. `Unpinned`/`Unresolvable` name no
    /// tenant; an unreadable binding might be the pin itself (a legacy-only
    /// install serves its pinned default from the `access_token` slot, which no
    /// tenant observation names); an unreadable slot might hold a working
    /// credential. An unmeasured input is not evidence, and the unclaimed-bucket
    /// rung still covers a pin with a streak.
    fn unserved_pin(self, observations: &[SlotObservation]) -> Option<uuid::Uuid> {
        let crate::session::tenant_pin::TenantPin::Pinned(pinned) = self.machine_pin else {
            return None;
        };
        let binding_is_elsewhere = match self.default_binding {
            crate::auth::BindingTenantRead::Bound(b) => b != pinned,
            crate::auth::BindingTenantRead::Unbound => true,
            crate::auth::BindingTenantRead::Unknown => false,
        };
        let key = pinned.to_string();
        let observed = observations
            .iter()
            .any(|o| o.tenant_id.as_deref() == Some(key.as_str()));
        (binding_is_elsewhere && !observed).then_some(pinned)
    }
}

/// Pure: compose already-read [`SweepInputs`] into the writable-key set.
fn writable_slot_keys_from(inputs: &SweepInputs) -> WritableSlotKeys {
    resolve_writable_slot_keys(
        inputs.tenant_slots.as_ref().ok().map(|v| v.as_slice()),
        inputs.default_binding,
        inputs.machine_pin,
    )
}

/// Pure: compose the same already-read [`SweepInputs`] into the binding-gap
/// report. The machine pin is deliberately NOT an input — a pin is what this
/// box ASKS for, not what coord says it is bound to.
fn binding_gaps_from(inputs: &SweepInputs) -> BindingGapReport {
    resolve_binding_gaps(
        inputs.tenant_slots.as_ref().ok().map(|v| v.as_slice()),
        inputs.default_binding,
        &inputs.coord_bound_tenants,
    )
}

/// How often a REPEATING blocking-pool read failure is re-announced: on the
/// first failure, then every this-many consecutive failures — hourly at the
/// 5-minute refresher cadence. A panic that recurs every pass must neither
/// flood the log nor be announced once and then go silent.
const SWEEP_JOIN_FAILURE_REWARN_EVERY: u32 = 12;

/// Whether the `consecutive`-th consecutive read failure is announced.
fn should_warn_sweep_join_failure(consecutive: u32) -> bool {
    // `0.is_multiple_of(n)` is true, and zero failures is not a failure.
    consecutive == 1
        || (consecutive > 0 && consecutive.is_multiple_of(SWEEP_JOIN_FAILURE_REWARN_EVERY))
}

/// A fresh PAIRING (an interactive sign-in, a pair-code redeem) replaced the
/// credential for `tenant`, so every rejection coord recorded against the OLD
/// one is spent evidence — the same M1 rule the refresher's own mint paths
/// apply through [`reset_upstream_rejections_for`].
///
/// Without this, "Sign in to re-pair" on `dark(upstream_401)` stores a working
/// credential, the next pass observes it fresh, and rung 5 re-reads the stale
/// streak and republishes `dark` behind a banner that cannot be dismissed.
///
/// `default_binding` is `paired_user.json`'s default AFTER the pairing was
/// persisted. `pair::persist_pairing_with` writes the legacy `access_token` slot
/// exactly when the paired tenant is (or becomes) the default, so equality is
/// the "the credential also landed in the legacy slot" test, and only then is
/// the DEFAULT bucket spent too. An unreadable default (`None`) leaves that
/// bucket standing — stuck-dark, the acceptable direction.
pub(crate) fn retire_rejection_streaks_after_pairing(
    tenant: uuid::Uuid,
    default_binding: Option<uuid::Uuid>,
) {
    reset_upstream_rejections_for(Some(tenant));
    if default_binding == Some(tenant) {
        reset_upstream_rejections_for(None);
    }
}

/// The refresher's OWN legacy-slot mints — the Cognito pair re-mint, the
/// device self-refresh and the device-machine-key exchange — all persist
/// through [`crate::auth::AuthManager::store_tokens`], which writes the legacy
/// `access_token` slot AND mirrors the token into `device_jwt:<claim>` for the
/// tenant coord stamped into it. So a fresh credential lands in TWO slots, and
/// the M1 rule ([`reset_upstream_rejections_for`]) has to spend the evidence
/// filed under both keys. Resetting only the default bucket left the mirrored
/// tenant's streak standing: a pinned tenant's three 401s survived a legacy
/// re-mint, the next slot pass observed the mirrored slot `SkippedFresh` as
/// the sole slot, rung 5 read the stale streak, and the runner published
/// `dark` on a credential coord had just minted — a transient false-dark plus
/// a spurious *"Coord access restored"* pair once the first forwarded 2xx
/// healed it. Same rule as [`retire_rejection_streaks_after_pairing`], keyed
/// off the token itself because that is what the mirror keys off.
///
/// A token with no decodable `tenant_id` claim is mirrored nowhere, so only
/// the default bucket is spent.
pub(crate) fn retire_rejection_streaks_after_legacy_mint(token: &str) {
    reset_upstream_rejections_for(None);
    if let Some(tenant) = crate::auth::jwt_tenant_claim(token) {
        reset_upstream_rejections_for(Some(tenant));
    }
}

/// Prefix for the latch entries that keep a skipped-sweep warning from
/// repeating every pass. Namespaced so it cannot collide with a slot key.
const SWEEP_SKIP_LATCH: &str = "\u{0}sweep-skip:";

/// Run the sweep over an already-resolved writable set — or SKIP it, leaving
/// every bucket intact, when the set is UNKNOWN.
///
/// Does no I/O: the inputs were read on the blocking pool by
/// [`read_sweep_inputs`] and composed by [`resolve_writable_slot_keys`].
fn sweep_unwritable_upstream_buckets(writable: WritableSlotKeys) {
    let writable = match writable {
        WritableSlotKeys::Measured(w) => w,
        WritableSlotKeys::Unknown(reason) => {
            // Latched: a persistently undecryptable store would otherwise warn
            // every five minutes forever.
            if latch_orphan_warning(&format!("{SWEEP_SKIP_LATCH}{reason}")) {
                warn!(
                    "device_jwt_refresher: SKIPPING the upstream-bucket eviction sweep — \
                     {reason}. Eviction deletes evidence, so it runs only on a fully \
                     measured writable set; every bucket is left intact and the posture \
                     stands until the read succeeds."
                );
            }
            return;
        }
    };
    // The set was measured, so any earlier skip is over — re-arm its warning.
    with_orphan_warned(|s| s.retain(|k| !k.starts_with(SWEEP_SKIP_LATCH)));
    let dropped = evict_unwritable_upstream_buckets(&writable);
    if !dropped.is_empty() {
        info!(
            "device_jwt_refresher: evicted {} upstream-verdict bucket(s) for tenants this \
             runner can no longer present a credential for ({dropped:?}) — a re-pair or \
             unpair makes their rejections unfalsifiable, so they are retired rather than \
             left to darken the posture forever",
            dropped.len()
        );
    }
}

/// The worst UNCLAIMED bucket: a slot key that no observation in this pass
/// describes, whose streak is at or over the threshold and whose last
/// rejection is recent.
///
/// # Why this is a MAP DIFF and not another special case
///
/// `derive_and_publish_posture` reads exactly two things per pass — each
/// observed slot's own bucket, and (when the fold applies) the default one. It
/// never enumerates [`UPSTREAM_SIGNALS`]. So **every bucket keyed by something
/// no observation claims is read by nobody**, and the default key was only the
/// first way to produce one. Two more, both reachable today:
///
/// * **A legacy-only install**, which is a supported shape.
///   `paired_user.json` names `default_tenant_id: X`, `machine.json` pins
///   `active_tenant_id: X`, the credential lives in the legacy `access_token`
///   slot and there is NO `device_jwt:X` slot. The forwarder resolves
///   `Ok(Some(X))`, `select_device_bearer` misses the tenant slot and falls
///   back to `legacy_slot_bearer` because `default_tenant == Some(t)`
///   (`crate::auth`, `select_device_bearer`'s default-binding arm), so coord's
///   refusals file under key **`X`**. Meanwhile `list_tenant_device_jwt_tenants()`
///   is empty, so the refresher takes its legacy branch and publishes ONE
///   observation with `tenant_id: None` — which reads the DEFAULT bucket.
///   Bucket `X` climbs forever, unread; the legacy `exp` is in the future, so
///   rung 6 says `live` while coord refuses every call.
/// * **A pinned tenant whose slot was cleared**: tenant slots for Y and Z,
///   `machine.json` pinned to X, and X's slot cleared after coord revoked it
///   (a failed re-derive). X's bucket was written while its slot existed; no
///   bearer can refresh it now, but the eviction sweep SPARES it because the
///   pin still names X (see [`resolve_writable_slot_keys`]). No observation
///   claims X, so without this diff the bucket would be read by nobody while
///   every X-pinned session is refused. Same shape, same silence.
///
/// Diffing the map against what the observations claim closes the CLASS, so
/// the next key source — whatever it turns out to be — is covered without a
/// fourth narrowing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UnclaimedVerdict {
    /// No unclaimed bucket carries a verdict.
    Nothing,
    /// An unclaimed bucket coord is refusing NOW.
    Dark(String, UpstreamSignal),
    /// An unclaimed bucket over the threshold whose last rejection is older
    /// than [`UPSTREAM_ORPHAN_STALE_AFTER_SECS`]. Neither a verdict nor a
    /// clean bill: the pass ABSTAINS rather than letting `worst` fall through
    /// to `live` and fire a false recovery.
    StaleEvidence(String),
}

/// Is a bucket's last rejection recent enough to be a verdict about NOW? The
/// one staleness rule for every bucket no observation describes — the
/// unclaimed-bucket rung and the unserved-pin posture both read it, so the two
/// cannot drift onto different clocks.
fn upstream_rejection_is_fresh(s: &UpstreamSignal, now: i64) -> bool {
    s.last_rejection_at
        .is_some_and(|t| now - t <= UPSTREAM_ORPHAN_STALE_AFTER_SECS)
}

/// `unserved_pin_key` is the key of the pinned tenant the unserved-pin arm has
/// already entered into the `worst` fold (see [`derive_and_publish_posture`]),
/// or `None`. That arm reads its bucket under the tenant's own name, so the key
/// counts as CLAIMED here — otherwise T's own streak would be reported a second
/// time, as a nameless orphan.
fn unclaimed_upstream_verdict(
    observations: &[SlotObservation],
    fold_default: bool,
    unserved_pin_key: Option<&str>,
    now: i64,
) -> UnclaimedVerdict {
    let mut claimed: std::collections::HashSet<&str> = observations
        .iter()
        .filter(|o| !o.unknown)
        .map(|o| o.tenant_id.as_deref().unwrap_or(DEFAULT_SLOT_KEY))
        .collect();
    if fold_default {
        // The sole observation already folded this bucket in.
        claimed.insert(DEFAULT_SLOT_KEY);
    }
    if let Some(key) = unserved_pin_key {
        claimed.insert(key);
    }
    let worst = with_upstream_signals(|m| {
        m.iter()
            .filter(|(k, _)| !claimed.contains(k.as_str()))
            .filter(|(_, s)| s.consecutive_rejections >= UPSTREAM_DARK_THRESHOLD)
            .map(|(k, s)| (k.clone(), *s))
            .max_by_key(|(_, s)| s.consecutive_rejections)
    });
    match worst {
        None => UnclaimedVerdict::Nothing,
        Some((k, s)) => {
            if upstream_rejection_is_fresh(&s, now) {
                UnclaimedVerdict::Dark(k, s)
            } else {
                UnclaimedVerdict::StaleEvidence(k)
            }
        }
    }
}

/// The signal for ONE observed slot.
///
/// `fold_default` is true only when this pass observed exactly one usable
/// slot. In that shape — a single-tenant machine, which is also the only shape
/// in which `session_tenant_or_refuse` returns `Unpinned`/`None` and a
/// forwarder therefore files its verdicts under the default-slot key — the two
/// buckets describe the SAME credential (`device_bearer_for(Some(t))` serves
/// the legacy slot when `t` is the default binding), so keeping them apart
/// would lose evidence. They are not merged field-by-field (a `max` of the
/// streaks would manufacture a dark out of a bucket that has since seen a
/// 2xx): the bucket holding the MORE RECENT event wins whole, ties to the
/// tenant's own.
///
/// ⚠ DO NOT "simplify" this into a field-wise merge. A `max` of the two
/// streaks resurrects a rejection the other bucket has already answered with a
/// 2xx, which is a manufactured `dark` on a working credential. The whole
/// bucket moves or neither does; a pre-PR review called this out explicitly.
fn upstream_signal_for_observation(tenant_id: Option<&str>, fold_default: bool) -> UpstreamSignal {
    let own = upstream_signal_for(tenant_id);
    if !fold_default || tenant_id.is_none() {
        return own;
    }
    let default = upstream_signal_for(None);
    if latest_upstream_event(&default) > latest_upstream_event(&own) {
        default
    } else {
        own
    }
}

/// THE derivation. Pure, total, and the only place the ladder is written down.
///
/// Rungs, first match wins:
///
/// 1. **The pass put a WORKING credential in the slot** (`Refreshed`, or a
///    clear that re-derived) → `live`. The posture describes what the runner
///    holds NOW, and that is a credential coord just minted.
/// 2. **The automatic exit ran and FAILED** (`Cleared { rederived: false }`) →
///    `unrefreshable`. The slot is empty and the one recovery rung that needs
///    no user session has already refused.
/// 3. **The runner HOLDS a dead credential** (decoded `exp` in the past, or an
///    opaque value) → `expired`. This is the boot case: the observation is
///    published before the pass's recovery concludes, so a runner that
///    restored a dead credential says so immediately rather than one recovery
///    round-trip later (DD2 — *"Boot is a transition"*).
/// 4. **Nothing held at all** → `absent`.
/// 5. **Coord keeps refusing a locally-valid credential**
///    (`>= UPSTREAM_DARK_THRESHOLD` consecutive credential-attributed
///    rejections, on a slot this pass did not refresh) → `dark(upstream_401)`.
/// 6. Future `exp` inside the refresh window → `expiring`; otherwise `live`.
pub(crate) fn derive_coord_credential_posture(
    obs: &SlotObservation,
    upstream: UpstreamSignal,
    now: i64,
) -> CoordCredentialPosture {
    let healed = matches!(
        obs.outcome,
        Some(TenantSlotOutcome::Refreshed)
            | Some(TenantSlotOutcome::Cleared {
                rederived: true,
                ..
            })
    );
    if healed {
        return CoordCredentialPosture::Live;
    }
    if matches!(
        obs.outcome,
        Some(TenantSlotOutcome::Cleared {
            rederived: false,
            ..
        })
    ) {
        return CoordCredentialPosture::Unrefreshable;
    }
    if obs.present {
        let dead = match obs.exp {
            None => true,        // opaque — can never be presented
            Some(e) => now >= e, // already expired
        };
        if dead {
            return CoordCredentialPosture::Expired;
        }
    } else {
        return CoordCredentialPosture::Absent;
    }
    if upstream.consecutive_rejections >= UPSTREAM_DARK_THRESHOLD {
        return CoordCredentialPosture::Dark(DarkCause::UpstreamRejected);
    }
    match obs.exp {
        Some(e) if now + crate::auth::REFRESH_BEFORE_EXPIRY_SECS >= e => {
            CoordCredentialPosture::Expiring
        }
        Some(_) => CoordCredentialPosture::Live,
        // Unreachable in practice (an opaque present slot is `expired` above,
        // an absent one is `absent`), but the function stays total rather than
        // panicking on a shape a future caller invents.
        None => CoordCredentialPosture::Absent,
    }
}

/// The published posture plus the evidence a reader needs to act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordCredentialStatus {
    pub posture: CoordCredentialPosture,
    pub tenant_id: Option<String>,
    /// Decoded `exp` of the credential the posture is about.
    pub exp: Option<i64>,
    pub last_ok_at: Option<i64>,
    pub last_401_at: Option<i64>,
    /// The slot-pass outcome token this posture was derived from
    /// (`refreshed`, `cleared`, `kept-existing`, …), or `None` for a
    /// pre-recovery boot observation.
    pub last_refresh_outcome: Option<String>,
    /// Unix seconds at which this POSTURE was first observed — i.e. when it
    /// last changed. Carried across publishes so the banner can say
    /// "expired since 03:54".
    pub since: i64,
    pub observed_at_unix: i64,
    /// Does [`Self::tenant_id`] actually NAME the slot this posture is about?
    ///
    /// **`tenant_id: None` means two different things and a reader could not
    /// tell them apart.** A slot candidate for the LEGACY default
    /// (`access_token`) slot carries `None` because that slot has no tenant —
    /// and it precisely describes the credential an UNPINNED session presents.
    /// The unattributable orphan arm in [`derive_and_publish_posture`] also
    /// carries `None`, but for the opposite reason: it deliberately withholds
    /// the field because nothing in the pass describes which slot coord is
    /// refusing (its own doc comment says so).
    ///
    /// Advisory readers never had to care — a banner says the same words
    /// either way. The coord-mcp forwarder's per-request gate does: on the
    /// first shape `None` is a MATCH against an unpinned session and refusing
    /// is correct, on the second it is UNKNOWN and refusing would take out a
    /// session whose own credential works. `true` on every slot-derived
    /// publish, `false` only on the orphan arm.
    pub attributable: bool,
}

impl CoordCredentialStatus {
    /// The `/health` `coordCredential` wire shape.
    ///
    /// The key is `posture` — the same name the heartbeat bag carries
    /// (`details.coord_credential.posture`), so one word means one thing on
    /// both wires. `state` is emitted alongside it as a deprecated alias for
    /// anything written against the first cut of this shape.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "posture": self.posture.as_str(),
            "state": self.posture.as_str(),
            "cause": self.posture.cause(),
            "canAnswer": self.posture.can_answer(),
            "reason": self.posture.message(),
            "cta": self.posture.cta(),
            "tenantId": self.tenant_id,
            "exp": self.exp,
            "lastOkAt": self.last_ok_at,
            "last401At": self.last_401_at,
            "lastRefreshOutcome": self.last_refresh_outcome,
            "since": self.since,
            "observedAtUnix": self.observed_at_unix,
            // `false` = "we could not say WHICH slot", not "no tenant". A
            // reader joining `tenantId` to a slot must check this first.
            "attributable": self.attributable,
        })
    }
}

/// A posture CHANGE, as seen by the publisher. `from: None` is the boot
/// publish — the first observation this process made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PostureTransition {
    pub from: Option<CoordCredentialPosture>,
    pub to: CoordCredentialPosture,
}

/// Should this transition fire the credential banner?
///
/// The dedup the plan asks for, generalised from the Cognito path's
/// `RefreshBackoff::dark_notified` boolean to a POSTURE-keyed rule: the state
/// itself is the last-notified value, so "once per transition" is structural
/// rather than a flag that can drift from what was actually shown.
///
/// - Entering a non-answering posture fires — **including at boot**
///   (`from: None`), which is DD2: today a bad boot state is the one moment no
///   transition happens, so nothing fires and the runner is silent.
/// - A CHANGE OF CAUSE while already dark fires again: `expired` becoming
///   `unrefreshable` is new information the operator needs.
/// - Recovery fires `dark:false`, but only if we actually reported dark —
///   a healthy boot must not announce that autonomy "resumed".
pub(crate) fn should_notify_posture(
    from: Option<CoordCredentialPosture>,
    to: CoordCredentialPosture,
) -> bool {
    let was_dark = from.is_some_and(|p| !p.can_answer());
    if !to.can_answer() {
        return !was_dark || from != Some(to);
    }
    was_dark
}

static COORD_CREDENTIAL_POSTURE: std::sync::OnceLock<
    std::sync::Mutex<Option<CoordCredentialStatus>>,
> = std::sync::OnceLock::new();

/// Every posture CHANGE this process has published, in order. Test-only: the
/// BOOT publish is overwritten by the same pass's concluded publish, so
/// without a record there is no way to assert what the runner said at boot —
/// which is the one moment DD2 is about.
#[cfg(test)]
static POSTURE_TRANSITIONS: std::sync::Mutex<Vec<PostureTransition>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn recorded_posture_transitions() -> Vec<PostureTransition> {
    POSTURE_TRANSITIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn coord_credential_posture_cell() -> &'static std::sync::Mutex<Option<CoordCredentialStatus>> {
    COORD_CREDENTIAL_POSTURE.get_or_init(|| std::sync::Mutex::new(None))
}

/// The most recent posture, or `None` when no refresher pass has run yet in
/// this process.
///
/// `None` is UNKNOWN, never "healthy" — the same discipline
/// [`tenant_slot_health`] states. `/health` renders it as an explicit
/// `state: "unknown"` with a null `canAnswer` rather than defaulting either
/// way.
pub fn coord_credential_posture() -> Option<CoordCredentialStatus> {
    // L1: `/health` calls this twice per request. `.expect()` here meant one
    // poisoned mutex — from any panic anywhere that touched the cell — turned
    // the degradation-reporting endpoint into a permanent 500.
    coord_credential_posture_cell()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Return the posture cell and the upstream counters to their process-start
/// state. The posture is a PROCESS-GLOBAL derived from process-global
/// counters, and `cargo test` runs this binary's tests on parallel threads —
/// so a test that asserts on the BOOT publish (which is keyed on "nothing
/// published yet") has to start from a known zero. Callers hold the same
/// serialising lock the slot-health tests use.
/// THE serialising lock for every test that touches the process-global
/// posture cell, the per-slot upstream signals or the tenant-slot health
/// snapshot. `cargo test` runs this binary's tests on parallel threads, so any
/// assertion about those globals is racy without it.
///
/// Crate-visible because the WIRING tests live in `mcp_api` (the forwarders
/// are there) while the state lives here — both must take the same lock or
/// they serialise against nothing.
///
/// Poisoning is recovered rather than propagated: one failing test must not
/// convert every sibling into a panic-on-lock and hide the real failure.
#[cfg(test)]
pub(crate) fn posture_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
pub(crate) fn reset_coord_credential_posture_for_test() {
    *coord_credential_posture_cell()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    with_upstream_signals(|m| m.clear());
    with_orphan_warned(|s| s.clear());
    POSTURE_TRANSITIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Publish a freshly derived posture and report whether it CHANGED.
///
/// `since` is carried forward when the posture is unchanged, so it keeps
/// naming when the runner entered this state rather than when it was last
/// observed in it.
pub(crate) fn publish_coord_credential_posture(
    posture: CoordCredentialPosture,
    tenant_id: Option<String>,
    exp: Option<i64>,
    last_refresh_outcome: Option<String>,
) -> Option<PostureTransition> {
    // The signal for THIS slot, not a process-wide aggregate: `lastOkAt` /
    // `last401At` must describe the credential the posture is about.
    let signal = upstream_signal_for(tenant_id.as_deref());
    // ATTRIBUTABLE: this entry point publishes a posture derived from a named
    // slot. The one arm that cannot name a slot goes through
    // `publish_coord_credential_posture_with` directly with `false`.
    publish_coord_credential_posture_with(
        posture,
        tenant_id,
        exp,
        last_refresh_outcome,
        signal,
        true,
    )
}

/// [`publish_coord_credential_posture`] with the slot's signal supplied by the
/// caller, so the timestamps published are the ones the posture was DERIVED
/// from. That matters on a single-tenant box, where the derivation folds in
/// the default slot's bucket ([`upstream_signal_for_observation`]) and a
/// re-read of the tenant bucket alone would publish a null `last401At` beside
/// a `dark` posture.
fn publish_coord_credential_posture_with(
    posture: CoordCredentialPosture,
    tenant_id: Option<String>,
    exp: Option<i64>,
    last_refresh_outcome: Option<String>,
    signal: UpstreamSignal,
    attributable: bool,
) -> Option<PostureTransition> {
    let now = chrono::Utc::now().timestamp();
    // L1: a poisoned mutex must not turn `/health` — the endpoint whose job is
    // reporting degradation — into a permanent 500. Recover the inner value,
    // exactly as the test helper already does.
    let mut cell = coord_credential_posture_cell()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let previous = cell.as_ref().map(|s| s.posture);
    let since = match previous {
        Some(p) if p == posture => cell.as_ref().map(|s| s.since).unwrap_or(now),
        _ => now,
    };
    *cell = Some(CoordCredentialStatus {
        posture,
        tenant_id,
        exp,
        last_ok_at: signal.last_ok_at,
        last_401_at: signal.last_rejection_at,
        last_refresh_outcome,
        since,
        observed_at_unix: now,
        attributable,
    });
    let transition = (previous != Some(posture)).then_some(PostureTransition {
        from: previous,
        to: posture,
    });
    #[cfg(test)]
    if let Some(t) = transition {
        POSTURE_TRANSITIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(t);
    }
    transition
}

/// Derive the pass's posture from every slot it observed and publish it.
///
/// When slots disagree the most-attention-needing one wins
/// ([`CoordCredentialPosture::severity`]) and its tenant/exp are the ones
/// carried, because that is the slot an operator has to fix.
///
/// Observations marked [`SlotObservation::unknown`] are DROPPED first. If that
/// leaves nothing — every slot unreadable — nothing is published and the
/// previous posture stands: an unreadable store is UNKNOWN, and publishing
/// `absent` off it would manufacture a fault out of a missing measurement.
///
/// # The UNATTRIBUTABLE arm (N1)
///
/// Per-slot keying reads only the buckets the pass's own observations name, so
/// a bucket keyed by anything NO observation claims is read by nobody. That is
/// a CLASS, not one bug — the default key was merely the first instance, a
/// legacy-only install and a pin-with-no-slot are two more, and
/// [`unclaimed_upstream_verdict`] enumerates them by diffing
/// [`UPSTREAM_SIGNALS`] against what the observations claim.
///
/// Dropping those was a regression in the one direction that matters: the
/// slots hold a future `exp`, rung 6 calls them `live`, `canAnswer` reads
/// `true`, no banner fires — while coord refuses every call this runner makes.
/// The pre-keying global counter WOULD have surfaced it.
///
/// "Cannot attribute" therefore means *report it without a tenant*, never
/// *drop it*: the arm publishes `dark` with `tenant_id: None`. It defers to a
/// per-slot verdict that is ALREADY non-answering, because that one names a
/// slot an operator can actually fix.
///
/// An orphan's evidence is retired by CONTRADICTION (a 2xx zeroes the streak)
/// or by IMPOSSIBILITY ([`evict_unwritable_upstream_buckets`] drops a bucket
/// no credential can ever write again) — never by age. A bucket that survives
/// both and has merely gone silent makes the pass ABSTAIN rather than publish
/// `live`: see [`UPSTREAM_ORPHAN_STALE_AFTER_SECS`] for why the age-as-primary
/// version was itself a healthy-while-dead defect.
///
/// # The UNSERVED-PIN arm
///
/// `machine.json` pins tenant T, the pass observes no slot for T, and the
/// default binding is measured to be another tenant (see
/// [`PosturePinInputs::unserved_pin`]). Every T-pinned session is then refused,
/// whatever the other slots hold — so T enters the `worst` fold itself, under
/// its own name, rather than only when a rejection streak happens to exist for
/// the unclaimed-bucket rung to find. A slot cleared on a LOCAL expiry or a
/// refresh-token 401 records no streak at all, and without this arm a healthy
/// sibling slot published `live` and *"Coord access restored"* over it.
///
/// T's posture honours its own bucket: a fresh streak at or over
/// [`UPSTREAM_DARK_THRESHOLD`] is `dark(upstream_401)`, anything else is
/// `absent` (the pass that cleared the slot already published
/// `unrefreshable`; a later pass that finds no slot at all is exactly absent).
/// It is NOT a synthetic observation fed through the ladder: rung 4 answers
/// `absent` before rung 5 reads the streak, which would downgrade a dark pin.
///
/// The fold's `fold_default` is still computed from the REAL observations
/// only: an extra entry would change which credential the default-slot bucket
/// is folded onto.
pub(crate) fn derive_and_publish_posture(
    observations: &[SlotObservation],
    pins: PosturePinInputs,
    now: i64,
) -> Option<PostureTransition> {
    /// One entry in the `worst` fold, carrying everything the publish needs.
    struct Candidate {
        posture: CoordCredentialPosture,
        tenant_id: Option<String>,
        exp: Option<i64>,
        outcome: Option<TenantSlotOutcome>,
        signal: UpstreamSignal,
    }

    // One usable slot means the default-slot bucket describes the same
    // credential — see [`upstream_signal_for_observation`].
    let fold_default = observations.iter().filter(|o| !o.unknown).count() == 1;
    let unserved_pin_key = pins.unserved_pin(observations).map(|t| t.to_string());
    let unserved_pin = unserved_pin_key.clone().map(|key| {
        let signal = upstream_signal_for(Some(&key));
        let posture = if signal.consecutive_rejections >= UPSTREAM_DARK_THRESHOLD
            && upstream_rejection_is_fresh(&signal, now)
        {
            CoordCredentialPosture::Dark(DarkCause::UpstreamRejected)
        } else {
            CoordCredentialPosture::Absent
        };
        Candidate {
            posture,
            tenant_id: Some(key),
            exp: None,
            outcome: None,
            signal,
        }
    });
    // The unserved pin goes FIRST: `max_by_key` keeps the last of equal
    // maxima, so a real slot at the same severity still wins the tie, exactly
    // as it did before the pin was an input.
    let worst = unserved_pin
        .into_iter()
        .chain(observations.iter().filter(|o| !o.unknown).map(|o| {
            let signal = upstream_signal_for_observation(o.tenant_id.as_deref(), fold_default);
            Candidate {
                posture: derive_coord_credential_posture(o, signal, now),
                tenant_id: o.tenant_id.clone(),
                exp: o.exp,
                outcome: o.outcome,
                signal,
            }
        }))
        .max_by_key(|c| c.posture.severity());

    // N1 — the unattributable arm, as a MAP DIFF rather than a special case
    // for the default key. See [`unclaimed_upstream_verdict`] for the class
    // and the shapes beyond the default key that reach it.
    //
    // A per-slot verdict that is ALREADY non-answering wins over both arms
    // below: it names a slot an operator can actually fix. The unserved pin
    // counts as such a verdict — it names the tenant to re-pair — and its key
    // is CLAIMED for the diff, so T's own streak is never also an orphan.
    let slot_already_non_answering = worst.as_ref().is_some_and(|c| !c.posture.can_answer());
    // Computed UNCONDITIONALLY: when a slot or the pin wins the publish, a
    // DIFFERENT orphan coord is refusing right now must still reach the log,
    // or the actionable posture would silently hide it.
    let unclaimed =
        unclaimed_upstream_verdict(observations, fold_default, unserved_pin_key.as_deref(), now);
    if slot_already_non_answering {
        if let UnclaimedVerdict::Dark(orphan_key, orphan_signal) = &unclaimed {
            warn_unclaimed_orphan(orphan_key, orphan_signal, false);
        }
        // `StaleEvidence` abstains only where it would otherwise let `worst`
        // fall through to `live`; a non-answering `worst` cannot, so it is
        // published as before.
    } else {
        match unclaimed {
            UnclaimedVerdict::Dark(orphan_key, orphan_signal) => {
                warn_unclaimed_orphan(&orphan_key, &orphan_signal, true);
                return publish_coord_credential_posture_with(
                    CoordCredentialPosture::Dark(DarkCause::UpstreamRejected),
                    // No tenant — deliberately, even when the orphan key IS a
                    // tenant id. `tenant_id` on this wire names a
                    // `device_jwt:<tenant>` SLOT an operator can act on, and
                    // this arm cannot in general point at one: the shapes that
                    // reach it include a legacy-only install and an unpaired
                    // tenant (no slot exists at all) AND an observation this
                    // pass marked `unknown` (the slot exists, but the pass
                    // could not read it, so nothing here describes its state).
                    // Rather than sometimes-right, the field is withheld and
                    // the key goes to the log ([`warn_unclaimed_orphan`]),
                    // which is honest in every shape.
                    None,
                    // Likewise no `exp`: the answer to "which slot's?" is none.
                    None,
                    None,
                    orphan_signal,
                    // UNATTRIBUTABLE — and the field exists so a reader can
                    // tell this `None` from the legacy default slot's `None`.
                    // The coord-mcp forwarder's per-request gate refuses a
                    // session only when the posture describes the slot THAT
                    // session presents; without this flag it would read the
                    // withheld `tenant_id` as "the legacy slot" and refuse
                    // every unpinned session on a box where some OTHER,
                    // unreadable slot is the one coord is refusing.
                    false,
                );
            }
            UnclaimedVerdict::StaleEvidence(orphan_key) => {
                // ABSTAIN. The bucket is over the threshold, nothing has
                // contradicted it with a 2xx, and it is still writable (an
                // unwritable one was evicted) — but its last rejection is
                // ancient. That is not a verdict, and it is emphatically not a
                // clean bill: falling through to `worst` here would publish
                // `live` off a future `exp` and fire an explicit "Coord access
                // restored" banner at a runner that never healed, because a
                // box whose every coord call fails is a box whose sessions go
                // quiet. Publish NOTHING; the previous posture stands.
                debug!(
                    "device_jwt_refresher: unclaimed slot key {orphan_key:?} still holds a \
                     rejection streak but its evidence is stale — abstaining (the previous \
                     posture stands) rather than publishing `live` off an untested `exp`"
                );
                return None;
            }
            UnclaimedVerdict::Nothing => {}
        }
    }

    let worst = worst?;
    publish_coord_credential_posture_with(
        worst.posture,
        worst.tenant_id,
        worst.exp,
        worst.outcome.map(tenant_slot_outcome_token),
        worst.signal,
        // ATTRIBUTABLE: `worst` is a real slot candidate (or the unserved
        // pin), so `tenant_id` names the slot this posture is about —
        // including when it is `None`, which is the LEGACY default slot and
        // is exactly the credential an unpinned session presents.
        true,
    )
}

/// Stable token for a slot outcome, shared by the health row and the posture's
/// `last_refresh_outcome` so the two can never drift into different spellings.
pub(crate) fn tenant_slot_outcome_token(outcome: TenantSlotOutcome) -> String {
    match outcome {
        TenantSlotOutcome::Refreshed => "refreshed",
        TenantSlotOutcome::SkippedFresh => "skipped-fresh",
        TenantSlotOutcome::SkippedNoToken => "skipped-no-token",
        TenantSlotOutcome::Cleared { .. } => "cleared",
        TenantSlotOutcome::KeptExisting => "kept-existing",
    }
    .to_string()
}

/// Clear one dead per-tenant slot and try to re-derive a working credential
/// for it — the EXIT from Phase 2's two absorbing states.
///
/// ## The safeguard lives here
///
/// This is the only function in the refresher that removes a credential, and
/// it cannot be called without a [`SlotClearCause`], which cannot be
/// constructed from a transport signal. Read the two call sites in
/// [`refresh_tenant_slots`]: one is reached from a LOCALLY decoded expiry
/// before any network call happens at all, the other only from a status that
/// [`slot_refresh_is_credential_rejection`] accepts. Every other failure —
/// timeout, connection refused, 5xx, 404, undecodable body — returns
/// [`TenantSlotOutcome::KeptExisting`] without ever reaching here.
///
/// A failed clear is NOT a clear: if the store write fails the slot still
/// holds its old value, so the outcome is `KeptExisting` and the state is
/// honestly unchanged.
///
/// ## Re-derivation
///
/// [`try_device_machine_key_exchange`] is the one credential path that needs
/// neither a live device JWT nor a Cognito session, which is exactly the
/// situation a dead slot is in. It is called with `expected_tenant =
/// Some(*tenant)`, so — per plan
/// `2026-09-17-device-jwt-refresh-drops-the-requested-tenant-and-coord-mints-the-home-tenant`
/// D2 — it refuses to persist ANYTHING, including the legacy default slot,
/// when the re-minted JWT's own `tenant_id` claim names a different tenant:
/// coord alone decides which tenant a device-machine-key exchange mints for,
/// so a mismatch here means `coord.devices.tenant_id` is not (or no longer)
/// `tenant`, and seeding either slot with that credential would be the exact
/// cross-tenant substitution `select_device_bearer` refuses by design. A
/// `None` return (mismatch, or any other failure) leaves this slot cleared.
async fn clear_and_rederive_tenant_slot(
    auth_manager: &crate::auth::AuthManager,
    tenant: &uuid::Uuid,
    web_base: &str,
    device_id: &str,
    cause: SlotClearCause,
    evidence: &str,
) -> TenantSlotOutcome {
    use std::sync::atomic::Ordering;
    if let Err(e) = auth_manager.clear_tenant_device_jwt(tenant) {
        warn!(
            "device_jwt_refresher: tenant {tenant} slot clear FAILED ({e}) — slot left \
             as-is ({evidence})"
        );
        return TenantSlotOutcome::KeptExisting;
    }
    match cause {
        SlotClearCause::DecodedExpiry => CLEARED_ON_EXPIRY_TOTAL.fetch_add(1, Ordering::Relaxed),
        SlotClearCause::CoordRejection => {
            CLEARED_ON_REJECTION_TOTAL.fetch_add(1, Ordering::Relaxed)
        }
    };
    // Forensics: every clear names its evidence, on the same JSONL stream the
    // nonce lifecycle uses. Without this a slot that vanishes between two
    // observations is unattributable.
    crate::coord_mcp::log_device_jwt_slot_clear(tenant, cause.as_str(), evidence);
    warn!(
        "device_jwt_refresher: CLEARED tenant {tenant} device-JWT slot \
         (cause={}, evidence: {evidence}) — attempting device-machine-key re-derive",
        cause.as_str()
    );

    if web_base.trim().is_empty() {
        warn!(
            "device_jwt_refresher: tenant {tenant} slot cleared but no web backend URL \
             is configured — cannot re-derive this pass"
        );
        return TenantSlotOutcome::Cleared {
            cause,
            rederived: false,
        };
    }
    // expected_tenant = Some(*tenant): a mismatched mint is already refused
    // (both slots) inside try_device_machine_key_exchange, so a `Some` here
    // is guaranteed to be tenant's own credential — see the doc comment above.
    let Some(jwt) =
        try_device_machine_key_exchange(auth_manager, web_base, device_id, Some(*tenant)).await
    else {
        return TenantSlotOutcome::Cleared {
            cause,
            rederived: false,
        };
    };
    match auth_manager.store_tenant_device_jwt(tenant, &jwt) {
        Ok(()) => {
            info!(
                "device_jwt_refresher: tenant {tenant} device-JWT slot RE-DERIVED via \
                 device-machine-key exchange (len={})",
                jwt.len()
            );
            // M1 — see [`reset_upstream_rejections_for`]. This is the arm that
            // LATCHED: re-derive succeeds, rung 1 says `live`, and the next
            // pass's rung 5 read the dead credential's streak back.
            reset_upstream_rejections_for(Some(*tenant));
            TenantSlotOutcome::Cleared {
                cause,
                rederived: true,
            }
        }
        Err(e) => {
            warn!("device_jwt_refresher: tenant {tenant} re-derived slot persist failed: {e}");
            TenantSlotOutcome::Cleared {
                cause,
                rederived: false,
            }
        }
    }
}

/// Multi-tenant slot pass: walk every per-tenant device-JWT slot and
/// self-refresh each stale one via coord's
/// `POST /devices/{device_id}/refresh-token`, presenting THAT slot's token as
/// the bearer (coord re-mints from the presented claim's tenant — verified
/// plan premise, `tokens.rs:341-348`). Each slot succeeds or fails
/// independently: a failure on one slot never aborts the others, and the
/// legacy `access_token` slot is never read or written here.
///
/// Shipped behavior since Phase 8a (the Phase-1 `QONTINUI_MULTI_TENANT_JWT`
/// flag gate is retired).
///
/// ## Phase 2: no absorbing states
///
/// A dead slot no longer sits here forever. Both routes out are destructive,
/// so both are gated on evidence that actually names the credential:
///
/// | Signal | Action |
/// |---|---|
/// | decoded `exp` in the past, or an opaque value | CLEAR + re-derive |
/// | coord answers 401/403 to the presented slot token | CLEAR + re-derive |
/// | transport error, 5xx, 404, undecodable body, persist error | **untouched** |
///
/// The last row is the safeguard, and it is load-bearing: a timeout is UNKNOWN,
/// not a rejection. `slot_refresh_never_clears_on_transport_error` pins it.
///
/// `web_base` is the qontinui-web backend URL (the caller's `pair_base`) —
/// where the device-machine-key exchange lives. It is NOT the coord URL, and
/// an empty value simply disables re-derivation for the pass.
///
/// Returns the per-tenant outcomes (deterministic slot order) for logging and
/// hermetic tests, and publishes [`tenant_slot_health`] plus the derived
/// [`coord_credential_posture`].
///
/// `app` is the Tauri handle the credential banner is emitted on. `None`
/// (every hermetic test, and any caller with no window) still derives and
/// publishes the posture — it simply cannot show it to a user.
///
/// `pins` is the machine pin and default binding the loop already read on the
/// blocking pool; both posture publishes read it, so a pinned tenant with no
/// slot is named rather than silently skipped (see
/// [`derive_and_publish_posture`]).
pub(crate) async fn refresh_tenant_slots(
    auth_manager: &crate::auth::AuthManager,
    coord_base: &str,
    web_base: &str,
    device_id: &str,
    app: Option<&tauri::AppHandle>,
    pins: PosturePinInputs,
) -> Vec<(uuid::Uuid, TenantSlotOutcome)> {
    let tenants = auth_manager.list_tenant_device_jwt_tenants();
    let mut outcomes = Vec::with_capacity(tenants.len());
    if tenants.is_empty() {
        return outcomes;
    }
    let mut health: Vec<TenantSlotHealthRow> = Vec::with_capacity(tenants.len());
    // Phase 1 of the coord-credential-posture plan: what each slot HELD when
    // this pass looked at it — `(tenant, exp, present, unknown)`, pushed
    // exactly once per slot in the same order as `outcomes`, so the two zip
    // into the posture's observations at the end of the pass.
    let mut slot_seen: Vec<(uuid::Uuid, Option<i64>, bool, bool)> =
        Vec::with_capacity(tenants.len());
    // DD2 — "Boot is a transition". On the FIRST pass of the process, publish
    // the posture from the restored slots BEFORE attempting recovery, so a
    // runner that came up holding a dead credential says so immediately rather
    // than one recovery round-trip later. Subsequent passes publish once, at
    // the end, so the steady state never flaps.
    let boot_pass = coord_credential_posture().is_none();
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
    if boot_pass {
        let boot_obs: Vec<SlotObservation> = tenants
            .iter()
            .map(|t| match auth_manager.get_tenant_device_jwt(t) {
                Ok(v) => SlotObservation::observed(Some(t.to_string()), v.as_deref()),
                Err(_) => SlotObservation::unreadable(Some(t.to_string())),
            })
            .collect();
        if let Some(transition) = derive_and_publish_posture(&boot_obs, pins, now) {
            notify_posture_transition(app, transition);
        }
    }
    for tenant in tenants {
        let token = match auth_manager.get_tenant_device_jwt(&tenant) {
            Ok(Some(t)) if !t.trim().is_empty() => t.trim().to_string(),
            Ok(_) => {
                // Empty slot: nothing to refresh, and nothing to clear.
                let o = TenantSlotOutcome::SkippedNoToken;
                health.push(health_row(&tenant, o, "slot empty".to_string()));
                slot_seen.push((tenant, None, false, false));
                outcomes.push((tenant, o));
                continue;
            }
            Err(e) => {
                // UNREADABLE STORE — the absence-is-not-zero case. This says
                // nothing about the credential, so it must never reach a clear.
                warn!("device_jwt_refresher: tenant {tenant} slot read failed: {e}");
                let o = TenantSlotOutcome::SkippedNoToken;
                health.push(health_row(
                    &tenant,
                    o,
                    format!("slot unreadable ({e}) — UNKNOWN, not cleared"),
                ));
                // UNKNOWN, so it contributes NOTHING to the posture: an
                // unreadable store must never render as `absent`.
                slot_seen.push((tenant, None, false, true));
                outcomes.push((tenant, o));
                continue;
            }
        };
        let exp = crate::auth::decode_jwt_exp(&token);
        slot_seen.push((tenant, exp, true, false));
        match plan_tenant_slot(exp, now) {
            TenantSlotPlan::SkipFresh => {
                let o = TenantSlotOutcome::SkippedFresh;
                health.push(health_row(
                    &tenant,
                    o,
                    format!("fresh (exp={})", exp.unwrap_or_default()),
                ));
                outcomes.push((tenant, o));
                continue;
            }
            TenantSlotPlan::ClearAndRederive => {
                // EVIDENCE (i) of the two the safeguard allows: decoded
                // LOCALLY, before any network call exists. No transport signal
                // can reach this arm.
                let evidence = match exp {
                    Some(e) => format!("decoded exp={e} is in the past (now={now})"),
                    None => {
                        "slot value is opaque — no decodable exp, cannot be presented".to_string()
                    }
                };
                let o = clear_and_rederive_tenant_slot(
                    auth_manager,
                    &tenant,
                    web_base,
                    device_id,
                    SlotClearCause::DecodedExpiry,
                    &evidence,
                )
                .await;
                health.push(health_row(&tenant, o, evidence));
                outcomes.push((tenant, o));
                continue;
            }
            TenantSlotPlan::Refresh => {}
        }
        let Some(client) = client.as_ref() else {
            let o = TenantSlotOutcome::KeptExisting;
            health.push(health_row(&tenant, o, "no HTTP client".to_string()));
            outcomes.push((tenant, o));
            continue;
        };
        // Present THIS slot's token — coord re-mints for the claim's tenant.
        // coord-auth-exempt(self-refresh): presents THIS tenant slot's own JWT so
        // coord re-mints for that slot's tenant. The default binding's token would
        // refresh the wrong slot.
        let resp = match client.post(&url).bearer_auth(&token).send().await {
            Ok(r) => r,
            Err(e) => {
                // TRANSPORT ERROR — timeout, DNS, connection refused. This is
                // UNKNOWN, never a rejection: the slot is left exactly as it
                // was. Pinned by
                // `slot_refresh_never_clears_on_transport_error`.
                warn!("device_jwt_refresher: tenant {tenant} slot refresh request failed: {e}");
                let o = TenantSlotOutcome::KeptExisting;
                health.push(health_row(
                    &tenant,
                    o,
                    format!("transport error ({e}) — UNKNOWN, slot untouched"),
                ));
                outcomes.push((tenant, o));
                continue;
            }
        };
        let status = resp.status();
        if !status.is_success() {
            if slot_refresh_is_credential_rejection(status.as_u16()) {
                // EVIDENCE (ii): coord ATTRIBUTED the rejection to the
                // credential we presented. REPLACE-not-REVOKE is right for a
                // transient failure and wrong for a 401 — the kept token is
                // already expired, so keeping it only re-enters the absorbing
                // state on the next pass.
                let evidence = format!("coord rejected the presented slot token: HTTP {status}");
                let o = clear_and_rederive_tenant_slot(
                    auth_manager,
                    &tenant,
                    web_base,
                    device_id,
                    SlotClearCause::CoordRejection,
                    &evidence,
                )
                .await;
                health.push(health_row(&tenant, o, evidence));
                outcomes.push((tenant, o));
                continue;
            }
            warn!(
                "device_jwt_refresher: tenant {tenant} slot refresh got HTTP {status} \
                 (not a credential rejection — existing slot JWT preserved)"
            );
            let o = TenantSlotOutcome::KeptExisting;
            health.push(health_row(
                &tenant,
                o,
                format!("HTTP {status} — not a rejection, slot untouched"),
            ));
            outcomes.push((tenant, o));
            continue;
        }
        let body: DeviceRefreshResponse = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                warn!("device_jwt_refresher: tenant {tenant} slot refresh body decode failed: {e}");
                let o = TenantSlotOutcome::KeptExisting;
                health.push(health_row(&tenant, o, format!("body decode failed ({e})")));
                outcomes.push((tenant, o));
                continue;
            }
        };
        if body.token.trim().is_empty() {
            warn!("device_jwt_refresher: tenant {tenant} slot refresh returned an empty token");
            let o = TenantSlotOutcome::KeptExisting;
            health.push(health_row(
                &tenant,
                o,
                "coord returned an empty token".to_string(),
            ));
            outcomes.push((tenant, o));
            continue;
        }
        match auth_manager.store_tenant_device_jwt(&tenant, &body.token) {
            Ok(()) => {
                info!(
                    "device_jwt_refresher: tenant {tenant} device-JWT slot refreshed (len={})",
                    body.token.len()
                );
                // M1 — see [`reset_upstream_rejections_for`].
                reset_upstream_rejections_for(Some(tenant));
                let o = TenantSlotOutcome::Refreshed;
                health.push(health_row(&tenant, o, "refreshed".to_string()));
                outcomes.push((tenant, o));
            }
            Err(e) => {
                warn!("device_jwt_refresher: tenant {tenant} slot persist failed: {e}");
                let o = TenantSlotOutcome::KeptExisting;
                health.push(health_row(&tenant, o, format!("persist failed ({e})")));
                outcomes.push((tenant, o));
            }
        }
    }
    publish_tenant_slot_health(health);

    // N2 — the DEFAULT-key half of M1, and the same latch.
    //
    // The heal paths reset the credential's own TENANT key. On a single-slot
    // box whose sessions are UNPINNED — the exact shape the fold exists for —
    // the forwarders file their verdicts under the DEFAULT key instead, so
    // after a successful re-derive the default bucket still held a streak of
    // 3 with a fresh `last_rejection_at`; the fold then picked it and the next
    // `SkippedFresh` pass went `dark` again on a credential coord had just
    // minted, behind a banner the user cannot dismiss. It self-heals on the
    // first successful forwarded call, but that is a latch until then.
    //
    // Scoped to the one-slot case on purpose: with two or more slots the
    // default bucket is NOT this tenant's credential (see the unattributable
    // arm in `derive_and_publish_posture`), so clearing it there would erase
    // another slot's evidence.
    let sole_usable_slot = slot_seen
        .iter()
        .filter(|(_, _, _, unknown)| !*unknown)
        .count()
        == 1;
    let pass_healed_a_slot = outcomes.iter().any(|(_, o)| {
        matches!(
            o,
            TenantSlotOutcome::Refreshed
                | TenantSlotOutcome::Cleared {
                    rederived: true,
                    ..
                }
        )
    });
    if sole_usable_slot && pass_healed_a_slot {
        reset_upstream_rejections_for(None);
    }

    // The pass's CONCLUDED posture: each slot as found, refined with what the
    // pass did about it. Published once per pass; a change fires the banner.
    debug_assert_eq!(slot_seen.len(), outcomes.len());
    let observations: Vec<SlotObservation> = slot_seen
        .iter()
        .zip(outcomes.iter())
        .map(|((tid, exp, present, unknown), (_, o))| {
            // M8 — `slot_seen` recorded the exp the slot held BEFORE this pass
            // acted on it. A HEALED slot publishes `live` beside that old,
            // already-past `exp`, which reads as a contradiction on `/health`
            // and in the heartbeat bag. Re-read the decoded exp for exactly
            // the two outcomes that replaced the credential; an unreadable
            // re-read nulls it (UNKNOWN) rather than keeping the stale value.
            let healed = matches!(
                o,
                TenantSlotOutcome::Refreshed
                    | TenantSlotOutcome::Cleared {
                        rederived: true,
                        ..
                    }
            );
            let (exp, present) = if healed {
                match auth_manager.get_tenant_device_jwt(tid) {
                    Ok(Some(t)) if !t.trim().is_empty() => {
                        (crate::auth::decode_jwt_exp(t.trim()), true)
                    }
                    _ => (None, *present),
                }
            } else {
                (*exp, *present)
            };
            SlotObservation {
                tenant_id: Some(tid.to_string()),
                exp,
                present,
                unknown: *unknown,
                outcome: Some(*o),
            }
        })
        .collect();
    if let Some(transition) = derive_and_publish_posture(&observations, pins, now) {
        notify_posture_transition(app, transition);
    }
    outcomes
}

/// `POST {web_base}/api/v1/devices/{device_id}/machine-credential/exchange`
/// (header `X-Device-Machine-Key: <dmk_>`, no body). This is what lets a
/// runner offline longer than BOTH the device-JWT TTL (so 4a self-refresh is
/// impossible) AND the Cognito refresh-token window (so the pair-cli path is
/// dead) recover a device JWT with NO user session.
///
/// `web_base` is the web-backend URL — the SAME base the Cognito pair path
/// forwards to (`settings.web_integration.backend_url`, i.e. the caller's
/// `pair_base`); it is NOT the coord URL. `device_id` is resolved by the caller
/// exactly like 4a's self-refresh (`QONTINUI_MACHINE_ID` env →
/// `read_device_id_from_disk`).
///
/// Returns `Some(new_jwt)` on success — the new JWT is ALREADY persisted to the
/// `access_token` slot, so the caller treats it like any other successful
/// re-mint (kick the relay, publish healthy, reset backoff, continue).
///
/// Returns `None` — so the caller falls through to the existing credential-dark
/// bail — when:
///   - no `dmk_` is stored (this device was never issued one),
///   - web returns ANY non-2xx (401/403 revoked/expired/mismatch, 503 when
///     web's `COORD_ADMIN_SECRET` is unset, anything else),
///   - the network call fails, the body fails to decode, or the token is empty, OR
///   - `expected_tenant` is `Some` and the minted token's own `tenant_id`
///     claim names a DIFFERENT tenant (D2/D3 of plan
///     `2026-09-17-device-jwt-refresh-drops-the-requested-tenant-and-coord-mints-the-home-tenant`;
///     the exchange authenticates by device-machine-key alone, so coord
///     alone decides which tenant to mint for — a mismatch means
///     `coord.devices.tenant_id` is not (or no longer) the tenant this
///     runner asked for, exactly the residue an earlier wrong pairing can
///     leave behind).
///
/// REPLACE-not-REVOKE: on ANY failure — including a tenant mismatch — the
/// existing JWT is left UNTOUCHED — a missed exchange means "stay
/// credential-dark", NEVER "clear the slot".
pub(crate) async fn try_device_machine_key_exchange(
    auth_manager: &crate::auth::AuthManager,
    web_base: &str,
    device_id: &str,
    expected_tenant: Option<uuid::Uuid>,
) -> Option<String> {
    // No dmk_ stored → this recovery path is unavailable for this device.
    let dmk = match auth_manager.get_device_machine_key() {
        Ok(Some(k)) if !k.trim().is_empty() => k,
        _ => return None,
    };

    let url = format!(
        "{}/api/v1/devices/{}/machine-credential/exchange",
        web_base.trim_end_matches('/'),
        device_id
    );
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("device_jwt_refresher: dmk exchange client build failed: {e}");
            return None;
        }
    };
    // coord-auth-exempt(not-coord): `qontinui-web`
    // `/api/v1/devices/{id}/machine-credential/exchange`, authenticated by the
    // device machine key. This is one of the paths that MINTS the device JWT.
    let resp = match client
        .post(&url)
        .header("X-Device-Machine-Key", dmk.trim())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!("device_jwt_refresher: dmk exchange request failed: {e}");
            return None;
        }
    };
    if !resp.status().is_success() {
        // 401/403 (revoked/expired/mismatch), 503 (web COORD_ADMIN_SECRET
        // unset), anything else → no recovery this path. REPLACE-not-REVOKE:
        // the existing JWT is left untouched.
        info!(
            "device_jwt_refresher: dmk exchange got HTTP {} — no recovery this path \
             (existing JWT preserved)",
            resp.status()
        );
        return None;
    }
    let body: DeviceRefreshResponse = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            warn!("device_jwt_refresher: dmk exchange body decode failed: {e}");
            return None;
        }
    };
    if body.token.trim().is_empty() {
        warn!("device_jwt_refresher: dmk exchange returned an empty token");
        return None;
    }
    // Persist into the access_token slot (refresh-token slot stays empty — the
    // device-JWT lifecycle is coord-owned). Guarded by `expected_tenant`: see
    // the doc comment above for why a mismatch here is exactly the incident
    // this exchange path was found to bypass.
    match auth_manager.store_tokens_expecting(&body.token, "", expected_tenant) {
        Ok(()) => {
            // M1: the old credential's rejections are spent evidence — for the
            // default bucket and for the tenant slot the mirror just wrote.
            retire_rejection_streaks_after_legacy_mint(&body.token);
            info!(
                "device_jwt_refresher: device JWT re-minted via device-machine-key \
                 exchange (len={})",
                body.token.len()
            );
            Some(body.token)
        }
        Err(e) => {
            if let Some(mismatch) = e.downcast_ref::<crate::auth::TenantMismatch>() {
                DEVICE_JWT_REFRESH_TENANT_MISMATCH_TOTAL
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                warn!(
                    "device_jwt_refresher: dmk exchange re-minted for tenant {:?}, not the \
                     requested {} — refusing to persist (existing JWT preserved)",
                    mismatch.returned, mismatch.expected
                );
            } else {
                warn!("device_jwt_refresher: persist dmk-exchanged JWT failed: {e}");
            }
            None
        }
    }
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
            spawn_blocking_tracked(move || qontinui_runner_lib::cognito::refresh_tokens(&rt)).await;
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

/// The qontinui-web backend base URL this refresher should talk to.
///
/// `api_config` refuses a LOOPBACK persisted `backend_url` on a RELEASE build
/// (see `api_config::resolve_api_base_url`), so on such a runner the relay
/// dials the release default. Reading the persisted field raw would leave the
/// loop minting against `127.0.0.1:8000` while the relay talks to production —
/// the prod/local device-JWT split the persisted rung was introduced to close
/// (plan 2026-07-08), re-opened pointing the other way.
///
/// So a refused value defers to `get_api_base_url()` — the one authority —
/// rather than to a second copy of rung 4 here. The two can then not disagree
/// by construction. A runner whose persisted value is honoured (every debug
/// build, and every release build pointed at a real remote backend) takes the
/// same path it always did.
///
/// Blank is NOT refused (it is unset, not loopback), so callers still have to
/// guard the unconfigured case — an empty return means "no web backend".
///
/// Extracted from the `Decision::Refresh` arm when Phase 2's per-tenant slot
/// pass gained its own need for the same base (the device-machine-key
/// exchange). Two hand-copied resolutions of this rule is exactly how the
/// 2026-07-08 split reopened.
fn resolve_pair_base(settings: &crate::settings::Settings) -> String {
    let persisted = settings
        .web_integration
        .backend_url
        .trim()
        .trim_end_matches('/')
        .to_string();
    if crate::api_config::persisted_backend_url_refused(&persisted, cfg!(debug_assertions)) {
        crate::api_config::get_api_base_url()
    } else {
        persisted
    }
}

async fn refresher_loop(
    api_state: Arc<ApiState>,
    mut shutdown_rx: watch::Receiver<bool>,
    mut kick_rx: watch::Receiver<u64>,
) {
    // `Arc` so the eviction sweep's input read can move a handle onto the
    // blocking pool; every `&auth_manager` below deref-coerces unchanged.
    let auth_manager = std::sync::Arc::new(crate::auth::AuthManager::new());
    info!("Device-JWT refresher started (check interval = 5m, threshold = 80m)");

    // Phase 2: carries the transient-failure backoff + the credential-dark
    // notify-once dedup across iterations.
    let mut backoff = RefreshBackoff::default();
    // Consecutive blocking-pool read failures, for the skipped-pass warning's
    // bounded re-announcement (`should_warn_sweep_join_failure`). Reset on a
    // successful read.
    let mut sweep_join_failures: u32 = 0;

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
        // Every input the eviction sweep needs — the slot store,
        // `paired_user.json` and `machine.json` — is read in ONE blocking-pool
        // hop, then composed on this side. The pin and the default binding
        // also feed the POSTURE (see [`PosturePinInputs`]): a pinned tenant
        // with no slot must be published under its own name, never skipped.
        //
        // The slot read is the UN-COLLAPSED one, because the sweep below is
        // destructive: an unreadable store must not read as "no tenant slots"
        // there. The BRANCH choice keeps its historical collapse — an
        // unreadable store takes the legacy arm, which does its own tri-state
        // `probe_access_token()` — so only the eviction's failure direction
        // differs.
        //
        // A JOIN failure is not that case: nothing at all was read, so neither
        // branch is honest — the legacy arm would publish a legacy-only posture
        // over a multi-tenant box. Skip the whole pass and try again next
        // cadence (or kick).
        let auth_for_sweep = std::sync::Arc::clone(&auth_manager);
        let sweep_inputs = match spawn_blocking_tracked(move || read_sweep_inputs(&auth_for_sweep))
            .await
        {
            Ok(inputs) => {
                sweep_join_failures = 0;
                inputs
            }
            Err(join_err) => {
                sweep_join_failures = sweep_join_failures.saturating_add(1);
                if should_warn_sweep_join_failure(sweep_join_failures) {
                    warn!(
                        "device_jwt_refresher: credential-store read task failed \
                         ({join_err}; {sweep_join_failures} consecutive) — skipping this \
                         pass rather than deriving a posture from nothing"
                    );
                }
                if wait_with_signals(REFRESH_CHECK_INTERVAL, &mut shutdown_rx, &mut kick_rx).await {
                    return;
                }
                continue;
            }
        };
        let has_tenant_slots = sweep_inputs
            .tenant_slots
            .as_ref()
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        let writable = writable_slot_keys_from(&sweep_inputs);
        // D2's residual (Phase 3): the pass below walks the slots that EXIST,
        // so a tenant bound with NO slot is structurally invisible to it.
        // Composed here, from the SAME blocking-pool read, and published
        // BEFORE the pass so the health snapshot it publishes carries this
        // pass's answer. Report only — no mint is attempted for a gap; that
        // is Phase 4 (`seed_a_bound_tenants_slot`), a separate change.
        publish_binding_gaps(binding_gaps_from(&sweep_inputs));
        // Retire every upstream-verdict bucket this runner can no longer write
        // (a re-pair or an unpair), BEFORE the posture is derived from what is
        // left. Runs in BOTH branches because a re-pair can happen from either
        // shape, and it is the IMPOSSIBILITY half of retiring an orphan's
        // evidence — the half that replaced a guessed clock.
        sweep_unwritable_upstream_buckets(writable);
        if has_tenant_slots {
            let mt_device_id = std::env::var("QONTINUI_MACHINE_ID")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .or_else(|| qontinui_runner_lib::pair::read_device_id_from_disk().ok());
            match mt_device_id {
                Some(did) => {
                    let (coord_base, _coord_base_source) =
                        crate::coord_mcp::coord_base_url_with_source();
                    // The web-backend base, resolved exactly as the Cognito
                    // pair path below resolves it — the device-machine-key
                    // exchange lives there, and Phase 2's re-derive needs it.
                    // Empty simply disables re-derivation for the pass.
                    let slot_web_base = resolve_pair_base(&settings_snapshot);
                    let outcomes = refresh_tenant_slots(
                        &auth_manager,
                        &coord_base,
                        &slot_web_base,
                        &did,
                        Some(&api_state.app_handle),
                        sweep_inputs.posture_pin_inputs(),
                    )
                    .await;
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
        } else {
            // No per-tenant slot exists on this runner, so the slot pass never
            // runs and never publishes. The coord credential is then the
            // LEGACY `access_token` slot, and a runner holding a dead one (or
            // none at all) is exactly the silent case this posture exists to
            // end — a never-paired runner must read `absent`, not UNKNOWN
            // forever. Derived from the same pure ladder; no pass outcome
            // exists, so the derivation sees `None` and speaks from `exp`.
            //
            // Read through the TRI-STATE probe, not `get_access_token`: that
            // returns `Err` both for a never-paired runner and for a
            // present-but-undecryptable store, and collapsing the two would
            // fire a "you have no coord credential" banner at a paired runner
            // whose store merely failed to decrypt this tick.
            let obs = match auth_manager.probe_access_token() {
                crate::secure_storage::StoredTokenRead::Present(t) => {
                    SlotObservation::observed(None, Some(t.as_str()))
                }
                crate::secure_storage::StoredTokenRead::Absent => {
                    SlotObservation::observed(None, None)
                }
                crate::secure_storage::StoredTokenRead::Unreadable(_) => {
                    SlotObservation::unreadable(None)
                }
            };
            if let Some(transition) = derive_and_publish_posture(
                &[obs],
                sweep_inputs.posture_pin_inputs(),
                chrono::Utc::now().timestamp(),
            ) {
                notify_posture_transition(Some(&api_state.app_handle), transition);
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
                    let (coord_base, _coord_base_source) =
                        crate::coord_mcp::coord_base_url_with_source();
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
                // The base a device JWT is MINTED against must be the base the
                // relay DIALS, or the runner presents a credential to a backend
                // that never issued it.
                //
                // `api_config` refuses a LOOPBACK persisted `backend_url` on a
                // RELEASE build (see `api_config::resolve_api_base_url`), so on
                // such a runner the relay dials the release default. Reading the
                // persisted field raw here would leave this loop minting against
                // `127.0.0.1:8000` while the relay talks to production — the
                // prod/local device-JWT split the persisted rung was introduced
                // to close (plan 2026-07-08), re-opened pointing the other way.
                //
                // So a refused value defers to `get_api_base_url()` — the one
                // authority — rather than to a second copy of rung 4 here. The
                // two can then not disagree by construction. A runner whose
                // persisted value is honoured (every debug build, and every
                // release build pointed at a real remote backend) takes the
                // same path it always did.
                let pair_base = resolve_pair_base(&settings_snapshot);
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
                let resolved_tenant =
                    resolve_pair_tenant_id(&bearer_token, outgoing_jwt.as_deref(), machine_tenant);
                let tenant_resolved = resolved_tenant.is_some();

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
                    // D3: terminal for the tick — see `PairProgress::BailTenantMismatch`.
                    // Deliberately NOT `PersistFailed`'s arm: that path still falls
                    // through to the dmk-exchange fallback below, which this must not.
                    RefreshOutcome::TenantMismatch { expected, returned } => {
                        warn_tenant_mismatch_once(*expected, *returned);
                        PairProgress::BailTenantMismatch {
                            expected: *expected,
                            returned: *returned,
                        }
                    }
                };

                // Phase 4b: FINAL cold-start fallback. Self-refresh (4a) AND the
                // Cognito pair-cli path have BOTH failed to advance the slot this
                // tick. If a device machine key (`dmk_`) is stored, exchange it
                // with web for a fresh device JWT — this recovers a runner offline
                // past both the device-JWT TTL and the Cognito refresh-token window
                // (>30d) with no user session. On success: same healthy-tick
                // handling as the other re-mints (kick relay, publish healthy,
                // reset backoff, emit "resumed" if we were dark, continue). On
                // None: fall through to the existing bail.
                // REPLACE-not-REVOKE: a miss never clears the existing JWT.
                //
                // D3: gated on the SPECIFIC bail reasons this fallback answers —
                // NOT a blanket "anything but Healthy". `BailTenantMismatch` is
                // deliberately excluded: coord already minted for the wrong
                // tenant once this tick, and a dmk exchange mints from the SAME
                // `coord.devices.tenant_id` column that wrong mint may have just
                // overwritten, so falling through here would repeat the incident
                // this plan closes rather than recover from it.
                if matches!(
                    progress,
                    PairProgress::BailNoTenant | PairProgress::BailRefreshFailedExpired
                ) {
                    let dmk_device_id = std::env::var("QONTINUI_MACHINE_ID")
                        .ok()
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| s.trim().to_string())
                        .or_else(|| qontinui_runner_lib::pair::read_device_id_from_disk().ok());
                    if let Some(did) = dmk_device_id {
                        if let Some(new_jwt) = try_device_machine_key_exchange(
                            &auth_manager,
                            &pair_base,
                            &did,
                            resolved_tenant.map(|(t, _)| t),
                        )
                        .await
                        {
                            info!(
                                "device_jwt_refresher: device JWT recovered via \
                                 device-machine-key exchange (len={}) — self-refresh + \
                                 Cognito both failed this tick",
                                new_jwt.len()
                            );
                            crate::mcp::backend_relay::commands::kick_cloud_relay().await;
                            publish_coord_credential_status(
                                &auth_manager,
                                &coord_credential_health(decision, Some(PairProgress::Healthy)),
                            )
                            .await;
                            let action = plan_refresh_wait(&mut backoff, RefreshClass::Ok);
                            if action.notify_recovered {
                                emit_credential_dark(&api_state.app_handle, false);
                            }
                            if wait_with_signals(action.wait, &mut shutdown_rx, &mut kick_rx).await
                            {
                                return;
                            }
                            continue;
                        }
                    }
                }

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

    /// D2/D3: coord mints a 2xx JWT, but its OWN `tenant_id` claim names a
    /// DIFFERENT tenant than the one `tok()` asked for. Must be refused, not
    /// persisted — the exact defect plan
    /// `2026-09-17-device-jwt-refresh-drops-the-requested-tenant-and-coord-mints-the-home-tenant`
    /// fixes: the old behaviour stored whatever coord returned.
    #[tokio::test]
    async fn refresher_refuses_a_mismatched_tenant_and_preserves_jwt() {
        let mgr = test_auth_manager("refuses_mismatched_tenant");
        let old_jwt = synth_jwt_with_tenant(
            chrono::Utc::now().timestamp() + 30 * 60,
            "cccccccc-cccc-4ccc-8ccc-cccccccccccc", // matches tok()'s claim
        );
        mgr.store_tokens(&old_jwt, "").expect("store");

        // Minted for a tenant `tok()` never asked for — the home-tenant
        // fallback this plan closes.
        let wrong_tenant_jwt = synth_jwt_with_tenant(
            chrono::Utc::now().timestamp() + 4 * 60 * 60,
            "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
        );
        // `PairCompleteResponse::user_id` has no `#[serde(default)]` — omitting
        // it would fail decode and collapse to KeptExisting before the tenant
        // guard is ever reached, masking this test's actual assertion.
        let body = serde_json::json!({ "token": wrong_tenant_jwt, "user_id": UID }).to_string();
        let (base, _cap, _shutdown) = spawn_mock(StatusCode::OK, body);

        let outcome = try_refresh_once(&mgr, &base, &tok(), DID, UID, None).await;
        match outcome {
            RefreshOutcome::TenantMismatch { expected, returned } => {
                assert_eq!(
                    expected.to_string(),
                    "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
                    "expected must be the tenant tok() asked for"
                );
                assert_eq!(
                    returned.map(|u| u.to_string()),
                    Some("dddddddd-dddd-4ddd-8ddd-dddddddddddd".to_string()),
                    "returned must be the tenant coord actually minted for"
                );
            }
            other => panic!("expected TenantMismatch, got {other:?}"),
        }
        assert_eq!(
            mgr.get_access_token().expect("token present"),
            old_jwt,
            "a tenant mismatch must leave the existing JWT untouched"
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

    /// As [`synth_jwt`], but also carrying a `tenant_id` claim.
    fn synth_jwt_with_tenant(tenant: &str, exp: i64) -> String {
        let header = b64url(b"{\"alg\":\"EdDSA\",\"typ\":\"JWT\"}");
        let payload = b64url(format!("{{\"exp\":{exp},\"tenant_id\":\"{tenant}\"}}").as_bytes());
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

    /// D2/D3: coord authenticated this self-refresh against `existing`'s own
    /// tenant, but the re-minted JWT names a DIFFERENT one. Refused, not
    /// persisted — the existing (still-valid) JWT is left untouched and the
    /// caller falls back to Cognito exactly as any other self-refresh miss.
    #[tokio::test]
    async fn self_refresh_refuses_a_mismatched_tenant_and_preserves_jwt() {
        let mgr = test_auth_manager("refuses_mismatched_tenant");
        let existing = synth_jwt_with_tenant(
            "11111111-2222-4333-8444-555555555561",
            chrono::Utc::now().timestamp() + 30 * 60,
        );
        mgr.store_tokens(&existing, "").expect("store");

        let wrong_tenant_jwt = synth_jwt_with_tenant(
            "22222222-3333-4444-5555-666666666672",
            chrono::Utc::now().timestamp() + 4 * 60 * 60,
        );
        let body = serde_json::json!({ "token": wrong_tenant_jwt }).to_string();
        let (base, cap, _shutdown) = spawn_mock(StatusCode::OK, body);

        let got = try_device_self_refresh(&mgr, &base, DID).await;
        assert!(got.is_none(), "a mismatched tenant must be refused");
        assert_eq!(*cap.hits.lock().unwrap(), 1, "coord was still hit once");
        assert_eq!(
            mgr.get_access_token().unwrap(),
            existing,
            "a tenant mismatch must leave the existing JWT untouched"
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
    /// token), and answers any bearer listed in `failures` with the status
    /// paired with it — so failure isolation, a 401 rejection and a 503 outage
    /// are each testable against the same mock. Captures every presented
    /// bearer in order.
    #[derive(Clone)]
    struct MockState {
        failures: Arc<Mutex<Vec<(String, u16)>>>,
        bearers_seen: Arc<Mutex<Vec<String>>>,
        /// Every MINT attempt this mock saw. Phase 3 reports gaps and must
        /// never seed one — see [`mint_attempts`] on [`MockCapture`].
        mint_attempts: Arc<Mutex<Vec<String>>>,
    }

    /// The ONE door in this module that mints a device JWT:
    /// `try_device_machine_key_exchange` POSTs
    /// `{web_base}/api/v1/devices/{id}/machine-credential/exchange` and
    /// PERSISTS the result into this box's `access_token` slot. Wiring it into
    /// the shared mock makes "no mint was attempted" an assertion rather than a
    /// claim, for every test in this module that passes the mock as `web_base`.
    async fn mint_tripwire(
        State(s): State<MockState>,
        Path(device_id): Path<String>,
    ) -> (StatusCode, String) {
        s.mint_attempts.lock().unwrap().push(device_id);
        (
            StatusCode::SERVICE_UNAVAILABLE,
            r#"{"error":"the mint door must not be knocked on in phase 3"}"#.to_string(),
        )
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
        let forced = s
            .failures
            .lock()
            .unwrap()
            .iter()
            .find(|(b, _)| *b == bearer)
            .map(|(_, st)| *st);
        if let Some(code) = forced {
            return (
                StatusCode::from_u16(code).expect("valid status"),
                r#"{"error":"forced"}"#.to_string(),
            );
        }
        (
            StatusCode::OK,
            serde_json::json!({ "token": format!("{bearer}.refreshed") }).to_string(),
        )
    }

    struct MockCapture {
        bearers_seen: Arc<Mutex<Vec<String>>>,
        /// Non-empty iff something knocked on the mint door. Phase 3 asserts
        /// this stays empty.
        mint_attempts: Arc<Mutex<Vec<String>>>,
    }

    fn spawn_mock(
        failures: Vec<(String, u16)>,
    ) -> (String, MockCapture, tokio::sync::oneshot::Sender<()>) {
        let bearers_seen = Arc::new(Mutex::new(Vec::new()));
        let bearers_h = bearers_seen.clone();
        let mint_attempts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mint_h = mint_attempts.clone();
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
                    failures: Arc::new(Mutex::new(failures)),
                    bearers_seen: bearers_h,
                    mint_attempts: mint_h,
                };
                // axum 0.8 path-param syntax: `{device_id}`.
                let app: Router = Router::new()
                    .route("/devices/{device_id}/refresh-token", post(handler))
                    .route(
                        "/api/v1/devices/{device_id}/machine-credential/exchange",
                        post(mint_tripwire),
                    )
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
            MockCapture {
                bearers_seen,
                mint_attempts,
            },
            tx,
        )
    }

    const DID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    /// [`tenant_slot_health`] and the `CLEARED_*_TOTAL` counters are
    /// PROCESS-GLOBAL — one snapshot and two monotone counters shared by every
    /// test in this binary, which `cargo test` runs on parallel threads. Every
    /// test that calls [`refresh_tenant_slots`] publishes into them, so any
    /// assertion about them is racy unless the passes are serialised.
    ///
    /// Poisoning is recovered rather than propagated: one failing test must not
    /// convert every sibling into a panic-on-lock and hide the real failure.
    fn health_lock() -> std::sync::MutexGuard<'static, ()> {
        // THE crate-wide lock — the `mcp_api` forwarder-wiring tests take the
        // same one, so the two suites serialise against each other and not
        // merely within themselves.
        super::posture_test_lock()
    }

    /// The sidecar state of a box whose `coord_bound_tenants.json` this test
    /// is not about — UNKNOWN, which is what an absent one really is.
    fn unread_sidecar() -> qontinui_runner_lib::pair::CoordBoundTenantsRead {
        qontinui_runner_lib::pair::CoordBoundTenantsRead::Unknown("test: sidecar not read")
    }

    fn tenant(n: u8) -> uuid::Uuid {
        uuid::Uuid::parse_str(&format!(
            "{c}{c}{c}{c}{c}{c}{c}{c}-{c}{c}{c}{c}-4{c}{c}{c}-8{c}{c}{c}-{c}{c}{c}{c}{c}{c}{c}{c}{c}{c}{c}{c}",
            c = (b'a' + n) as char
        ))
        .expect("valid uuid")
    }

    // ---- plan_tenant_slot: pure staleness decision ----

    #[test]
    fn plan_refreshes_stale_but_valid_and_clears_fresh_or_dead() {
        let now = 1_000_000_000i64;
        // Opaque/undecodable → cannot be presented; clear and re-derive.
        assert_eq!(
            plan_tenant_slot(None, now),
            TenantSlotPlan::ClearAndRederive
        );
        // Already expired (or exactly at exp) → clear and re-derive.
        assert_eq!(
            plan_tenant_slot(Some(now - 1), now),
            TenantSlotPlan::ClearAndRederive
        );
        assert_eq!(
            plan_tenant_slot(Some(now), now),
            TenantSlotPlan::ClearAndRederive
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
        let _serialised = health_lock();
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
        // Empty `web_base`: this pass must never need the re-derive path.
        let outcomes =
            refresh_tenant_slots(&mgr, &base, "", DID, None, PosturePinInputs::UNPINNED).await;

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
        let _serialised = health_lock();
        // Coord 500s tenant A's bearer; tenant B must still refresh. A keeps
        // its existing token (REPLACE-not-REVOKE).
        let mgr = test_auth_manager("multi_slot_failure_isolation");
        let (ta, tb) = (tenant(0), tenant(1));
        let now = chrono::Utc::now().timestamp();
        let jwt_a = synth_jwt(now + 30 * 60, "tenant-a");
        let jwt_b = synth_jwt(now + 30 * 60, "tenant-b");
        mgr.store_tenant_device_jwt(&ta, &jwt_a).expect("slot a");
        mgr.store_tenant_device_jwt(&tb, &jwt_b).expect("slot b");

        let (base, cap, _shutdown) = spawn_mock(vec![(jwt_a.clone(), 500)]);
        let outcomes =
            refresh_tenant_slots(&mgr, &base, "", DID, None, PosturePinInputs::UNPINNED).await;

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
    async fn fresh_slot_is_skipped_and_dead_slot_is_cleared_without_http() {
        let _serialised = health_lock();
        // A comfortably-fresh slot and an already-expired one. Neither hits
        // coord — but the expired one is no longer PRESERVED: Phase 2a clears
        // it on the locally-decoded expiry, which is the exit from the
        // absorbing state. `web_base` is empty, so no re-derive is attempted.
        let mgr = test_auth_manager("multi_slot_skip_fresh_clear_dead");
        let (ta, tb) = (tenant(0), tenant(1));
        let now = chrono::Utc::now().timestamp();
        let fresh = synth_jwt(now + 3 * 60 * 60, "fresh");
        let expired = synth_jwt(now - 60, "expired");
        mgr.store_tenant_device_jwt(&ta, &fresh).expect("slot a");
        mgr.store_tenant_device_jwt(&tb, &expired).expect("slot b");

        let (base, cap, _shutdown) = spawn_mock(vec![]);
        let outcomes =
            refresh_tenant_slots(&mgr, &base, "", DID, None, PosturePinInputs::UNPINNED).await;

        assert_eq!(
            outcomes,
            vec![
                (ta, TenantSlotOutcome::SkippedFresh),
                (
                    tb,
                    TenantSlotOutcome::Cleared {
                        cause: SlotClearCause::DecodedExpiry,
                        rederived: false,
                    }
                )
            ]
        );
        assert!(
            cap.bearers_seen.lock().unwrap().is_empty(),
            "neither a fresh nor a decoded-dead slot may present a bearer"
        );
        assert_eq!(
            mgr.get_tenant_device_jwt(&ta).unwrap().as_deref(),
            Some(fresh.as_str()),
            "a fresh slot is untouched"
        );
        assert_eq!(
            mgr.get_tenant_device_jwt(&tb).unwrap(),
            None,
            "the expired slot must be CLEARED — REPLACE-not-REVOKE is for \
             transient failures, not for a credential we decoded as dead"
        );
    }

    /// An OPAQUE slot value takes the same exit: we cannot judge it and coord
    /// would reject it, so it is cleared on that (local) evidence.
    #[tokio::test]
    async fn opaque_slot_is_cleared_without_http() {
        let _serialised = health_lock();
        let mgr = test_auth_manager("multi_slot_clear_opaque");
        let ta = tenant(0);
        mgr.store_tenant_device_jwt(&ta, "qontinui_runner_legacy_abc123")
            .expect("slot a");

        let (base, cap, _shutdown) = spawn_mock(vec![]);
        let outcomes =
            refresh_tenant_slots(&mgr, &base, "", DID, None, PosturePinInputs::UNPINNED).await;

        assert_eq!(
            outcomes,
            vec![(
                ta,
                TenantSlotOutcome::Cleared {
                    cause: SlotClearCause::DecodedExpiry,
                    rederived: false,
                }
            )]
        );
        assert!(cap.bearers_seen.lock().unwrap().is_empty());
        assert_eq!(mgr.get_tenant_device_jwt(&ta).unwrap(), None);
    }

    /// Phase 2b: a coord REJECTION clears. The kept token is already expired,
    /// so `KeptExisting` here just re-entered the absorbing state next pass.
    #[tokio::test]
    async fn coord_rejection_clears_the_slot() {
        let _serialised = health_lock();
        let mgr = test_auth_manager("multi_slot_401_clears");
        let ta = tenant(0);
        let now = chrono::Utc::now().timestamp();
        // Stale-but-valid, so the pass actually presents it.
        let jwt = synth_jwt(now + 30 * 60, "rejected");
        mgr.store_tenant_device_jwt(&ta, &jwt).expect("slot a");

        let (base, cap, _shutdown) = spawn_mock(vec![(jwt.clone(), 401)]);
        let outcomes =
            refresh_tenant_slots(&mgr, &base, "", DID, None, PosturePinInputs::UNPINNED).await;

        assert_eq!(
            outcomes,
            vec![(
                ta,
                TenantSlotOutcome::Cleared {
                    cause: SlotClearCause::CoordRejection,
                    rederived: false,
                }
            )]
        );
        assert_eq!(
            cap.bearers_seen.lock().unwrap().len(),
            1,
            "it was presented"
        );
        assert_eq!(
            mgr.get_tenant_device_jwt(&ta).unwrap(),
            None,
            "a 401 is coord saying the credential is dead — clear it"
        );
    }

    /// …and a 403 is the same class of statement.
    #[tokio::test]
    async fn coord_403_clears_the_slot_too() {
        let _serialised = health_lock();
        let mgr = test_auth_manager("multi_slot_403_clears");
        let ta = tenant(0);
        let now = chrono::Utc::now().timestamp();
        let jwt = synth_jwt(now + 30 * 60, "forbidden");
        mgr.store_tenant_device_jwt(&ta, &jwt).expect("slot a");

        let (base, _cap, _shutdown) = spawn_mock(vec![(jwt.clone(), 403)]);
        let outcomes =
            refresh_tenant_slots(&mgr, &base, "", DID, None, PosturePinInputs::UNPINNED).await;

        assert_eq!(
            outcomes,
            vec![(
                ta,
                TenantSlotOutcome::Cleared {
                    cause: SlotClearCause::CoordRejection,
                    rederived: false,
                }
            )]
        );
        assert_eq!(mgr.get_tenant_device_jwt(&ta).unwrap(), None);
    }

    /// THE SAFEGUARD, half one: coord being UNWELL is not coord rejecting the
    /// credential. A 503 must leave the slot exactly as it found it — the same
    /// posture the legacy path already pins in
    /// `refresher_handles_coord_503_without_clearing_jwt`.
    #[tokio::test]
    async fn coord_503_never_clears_the_slot() {
        let _serialised = health_lock();
        let mgr = test_auth_manager("multi_slot_503_keeps");
        let ta = tenant(0);
        let now = chrono::Utc::now().timestamp();
        let jwt = synth_jwt(now + 30 * 60, "outage");
        mgr.store_tenant_device_jwt(&ta, &jwt).expect("slot a");

        let (base, _cap, _shutdown) = spawn_mock(vec![(jwt.clone(), 503)]);
        let outcomes =
            refresh_tenant_slots(&mgr, &base, "", DID, None, PosturePinInputs::UNPINNED).await;

        assert_eq!(outcomes, vec![(ta, TenantSlotOutcome::KeptExisting)]);
        assert_eq!(
            mgr.get_tenant_device_jwt(&ta).unwrap().as_deref(),
            Some(jwt.as_str()),
            "a 5xx says nothing about the credential — REPLACE-not-REVOKE holds"
        );
    }

    /// THE SAFEGUARD, half two — the mandated test. A TRANSPORT failure (here:
    /// nothing listening, so the connect fails; a timeout is the same class)
    /// is UNKNOWN, not a rejection. Clearing must be UNREACHABLE from it.
    ///
    /// This is the test that would fail if anyone ever "simplified" the
    /// rejection gate into `if !status.is_success()` plus a catch-all on the
    /// error arm — the exact shape the clear must never take.
    #[tokio::test]
    async fn slot_refresh_never_clears_on_transport_error() {
        let _serialised = health_lock();
        let mgr = test_auth_manager("multi_slot_transport_error_keeps");
        // The clear counters are cumulative for the whole process, so the only
        // honest assertion is that this pass did not MOVE them.
        let rejections_before = tenant_slot_health()
            .map(|h| h.cleared_on_rejection_total)
            .unwrap_or(0);
        let expiries_before = tenant_slot_health()
            .map(|h| h.cleared_on_expiry_total)
            .unwrap_or(0);
        let ta = tenant(0);
        let now = chrono::Utc::now().timestamp();
        // Stale-but-valid so the pass genuinely attempts the network call —
        // an expired slot would be cleared on LOCAL evidence before any
        // transport is involved, which would not exercise this path at all.
        let jwt = synth_jwt(now + 30 * 60, "unreachable");
        mgr.store_tenant_device_jwt(&ta, &jwt).expect("slot a");

        // A port with nothing bound: the connect fails at the transport layer,
        // with no HTTP status of any kind. `web_base` is deliberately the same
        // dead base, so even a stray re-derive attempt could not succeed.
        let dead_base = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            let port = l.local_addr().expect("addr").port();
            drop(l);
            format!("http://127.0.0.1:{port}")
        };
        let outcomes = refresh_tenant_slots(
            &mgr,
            &dead_base,
            &dead_base,
            DID,
            None,
            PosturePinInputs::UNPINNED,
        )
        .await;

        assert_eq!(
            outcomes,
            vec![(ta, TenantSlotOutcome::KeptExisting)],
            "a transport error is UNKNOWN — it may never be read as a rejection"
        );
        assert_eq!(
            mgr.get_tenant_device_jwt(&ta).unwrap().as_deref(),
            Some(jwt.as_str()),
            "a timeout/connection failure MUST NOT clear the slot"
        );
        // And the health signal must say UNKNOWN too, not "cleared".
        let health = tenant_slot_health().expect("a pass publishes health");
        assert_eq!(health.slots.len(), 1);
        assert_eq!(health.slots[0].outcome, "kept-existing");
        assert_eq!(health.slots[0].clear_cause, None);
        assert_eq!(
            health.cleared_on_rejection_total, rejections_before,
            "a transport error must not increment the rejection-clear counter"
        );
        assert_eq!(
            health.cleared_on_expiry_total, expiries_before,
            "…nor the expiry-clear counter"
        );
    }

    /// The gate on the destructive action, in isolation. Only an
    /// authentication rejection qualifies.
    #[test]
    fn only_401_and_403_count_as_a_credential_rejection() {
        assert!(slot_refresh_is_credential_rejection(401));
        assert!(slot_refresh_is_credential_rejection(403));
        for status in [400u16, 404, 408, 409, 418, 429, 500, 502, 503, 504] {
            assert!(
                !slot_refresh_is_credential_rejection(status),
                "HTTP {status} says nothing about the credential and must not clear"
            );
        }
    }

    /// Phase 2c: the pass publishes structured state a health surface can read,
    /// instead of relying on a 5-minutely `warn!` nobody greps.
    #[tokio::test]
    async fn a_pass_publishes_structured_slot_health() {
        let _serialised = health_lock();
        let mgr = test_auth_manager("multi_slot_health_signal");
        let (ta, tb) = (tenant(0), tenant(1));
        let now = chrono::Utc::now().timestamp();
        let fresh = synth_jwt(now + 3 * 60 * 60, "fresh");
        let expired = synth_jwt(now - 60, "expired");
        mgr.store_tenant_device_jwt(&ta, &fresh).expect("slot a");
        mgr.store_tenant_device_jwt(&tb, &expired).expect("slot b");

        let (base, _cap, _shutdown) = spawn_mock(vec![]);
        let _ = refresh_tenant_slots(&mgr, &base, "", DID, None, PosturePinInputs::UNPINNED).await;

        let health = tenant_slot_health().expect("a pass publishes health");
        assert_eq!(health.slots.len(), 2);
        assert_eq!(health.slots[0].outcome, "skipped-fresh");
        assert_eq!(health.slots[1].outcome, "cleared");
        assert_eq!(
            health.slots[1].clear_cause.as_deref(),
            Some("decoded-expiry")
        );
        assert_eq!(health.slots[1].rederived, Some(false));
        assert!(
            health.slots[1].detail.contains("decoded exp="),
            "the health row names its evidence: {}",
            health.slots[1].detail
        );
        assert_eq!(
            health.degraded_slots, 1,
            "a clear with no re-derive is a slot that still needs attention"
        );
    }

    #[tokio::test]
    async fn empty_slot_set_is_a_no_op() {
        let _serialised = health_lock();
        let mgr = test_auth_manager("multi_slot_empty_noop");
        let (base, cap, _shutdown) = spawn_mock(vec![]);
        let outcomes =
            refresh_tenant_slots(&mgr, &base, "", DID, None, PosturePinInputs::UNPINNED).await;
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

    // ---- Coord-credential posture (plan 2026-09-12, Phase 1) ----

    /// The posture cell, the upstream counters and the transition log are all
    /// PROCESS-GLOBAL. Every posture test takes [`health_lock`] (the same lock
    /// the slot-health tests take, because the same passes publish both) and
    /// starts from a known zero.
    fn reset_posture() {
        reset_coord_credential_posture_for_test();
    }

    /// DD2 — *"Boot is a transition"*. A runner that restores an ALREADY
    /// EXPIRED coord slot must say so on the FIRST pass, before its recovery
    /// attempt has concluded. Today that moment is the one moment nothing
    /// fires, which is why the 2026-09-12 runner was silent for ten hours.
    #[tokio::test]
    async fn an_expired_restored_slot_at_boot_is_expired_on_the_first_pass() {
        let _serialised = health_lock();
        reset_posture();
        let mgr = test_auth_manager("posture_boot_expired");
        let ta = tenant(0);
        let now = chrono::Utc::now().timestamp();
        // Restored from the encrypted store already dead — the incident's
        // exact shape.
        mgr.store_tenant_device_jwt(&ta, &synth_jwt(now - 600, "restored-dead"))
            .expect("slot a");

        let (base, _cap, _shutdown) = spawn_mock(vec![]);
        // Empty web_base: the device-machine-key re-derive is unavailable, so
        // the automatic exit cannot heal it — the terminal state.
        let _ = refresh_tenant_slots(&mgr, &base, "", DID, None, PosturePinInputs::UNPINNED).await;

        let transitions = recorded_posture_transitions();
        assert!(
            !transitions.is_empty(),
            "the boot pass must publish a posture"
        );
        assert_eq!(
            transitions[0],
            PostureTransition {
                from: None,
                to: CoordCredentialPosture::Expired,
            },
            "the FIRST thing a runner holding a dead restored credential says \
             must be `expired`, not silence: {transitions:?}"
        );
        // And it is a transition that FIRES — boot is not exempt.
        assert!(should_notify_posture(None, CoordCredentialPosture::Expired));

        // The same pass then concludes: the slot was cleared and nothing could
        // be re-derived, which is the terminal `unrefreshable`.
        let concluded = coord_credential_posture().expect("a pass publishes a posture");
        assert_eq!(concluded.posture, CoordCredentialPosture::Unrefreshable);
        assert_eq!(concluded.last_refresh_outcome.as_deref(), Some("cleared"));
        assert_eq!(
            concluded.tenant_id.as_deref(),
            Some(ta.to_string().as_str())
        );
        assert!(!concluded.posture.can_answer());
    }

    /// A token whose own `exp` is comfortably in the FUTURE, that coord keeps
    /// refusing. `exp` alone calls this `live` — which is the silent case the
    /// upstream input exists to catch.
    #[test]
    fn a_future_exp_slot_that_coord_401s_reads_dark_not_live() {
        let now = 1_700_000_000i64;
        let obs = SlotObservation {
            tenant_id: Some("t".into()),
            exp: Some(now + 3 * 60 * 60),
            present: true,
            unknown: false,
            outcome: Some(TenantSlotOutcome::SkippedFresh),
        };
        // No upstream evidence: `exp` says live, and live is what we report.
        assert_eq!(
            derive_coord_credential_posture(&obs, UpstreamSignal::default(), now),
            CoordCredentialPosture::Live
        );
        // Coord has refused it enough times to be a verdict rather than noise.
        let rejected = UpstreamSignal {
            consecutive_rejections: UPSTREAM_DARK_THRESHOLD,
            last_rejection_at: Some(now),
            last_ok_at: None,
        };
        assert_eq!(
            derive_coord_credential_posture(&obs, rejected, now),
            CoordCredentialPosture::Dark(DarkCause::UpstreamRejected)
        );
        assert_eq!(
            CoordCredentialPosture::Dark(DarkCause::UpstreamRejected).cause(),
            Some("upstream_401")
        );
    }

    /// THE named risk: *"a transient 401 from coord during a coord deploy must
    /// not flip the posture to dark"*. The input is a RATE — consecutive
    /// credential-attributed rejections, reset by any success — never a single
    /// answer.
    #[test]
    fn a_single_transient_401_never_flips_the_posture_to_dark() {
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let live_slot = SlotObservation {
            tenant_id: None,
            exp: Some(now + 3 * 60 * 60),
            present: true,
            unknown: false,
            outcome: Some(TenantSlotOutcome::SkippedFresh),
        };
        let posture = || derive_coord_credential_posture(&live_slot, upstream_signal(), now);

        // One, then two. A coord deploy that 401s a couple of calls is not a
        // statement about this runner's credential.
        for i in 1..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_expired"}"#);
            assert_eq!(
                posture(),
                CoordCredentialPosture::Live,
                "{i} consecutive 401s must NOT be dark"
            );
        }
        // The threshold'th consecutive rejection is the verdict.
        note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_expired"}"#);
        assert_eq!(
            posture(),
            CoordCredentialPosture::Dark(DarkCause::UpstreamRejected)
        );

        // …and any success clears it immediately: the deploy finished.
        note_coord_upstream_verdict(None, true, 200, br#"{"ok":true}"#);
        assert_eq!(posture(), CoordCredentialPosture::Live);
        assert!(upstream_signal().last_ok_at.is_some());

        // A 5xx storm says NOTHING about the credential and must move nothing.
        for _ in 0..10 {
            note_coord_upstream_verdict(None, true, 503, b"upstream unavailable");
        }
        assert_eq!(posture(), CoordCredentialPosture::Live);

        // Neither does a 401 that names the NONCE rather than the token — the
        // 2026-09-12 first diagnosis blamed exactly that.
        for _ in 0..10 {
            note_coord_upstream_verdict(
                None,
                true,
                401,
                br#"{"code":"COORD_MCP_PROXY_UNAUTHORIZED"}"#,
            );
        }
        assert_eq!(posture(), CoordCredentialPosture::Live);
        reset_posture();
    }

    /// The narrow classifier, in isolation — it is the gate on a state that
    /// tells a user their sessions are broken.
    #[test]
    fn only_a_credential_attributed_401_counts_as_an_upstream_rejection() {
        assert_eq!(
            classify_upstream_verdict(401, r#"{"code":"token_expired"}"#),
            UpstreamVerdict::CredentialRejected
        );
        // C2, the INVERSE hazard: a body that merely QUOTES the phrase is not
        // coord refusing the token. The old substring classifier called this a
        // credential rejection; the code field is the only thing that decides.
        assert_eq!(
            classify_upstream_verdict(403, "Invalid token is not what happened here"),
            UpstreamVerdict::Indeterminate
        );
        assert_eq!(
            classify_upstream_verdict(200, ""),
            UpstreamVerdict::Accepted
        );
        assert_eq!(
            classify_upstream_verdict(204, ""),
            UpstreamVerdict::Accepted
        );
        for (status, body) in [
            (401u16, r#"{"code":"COORD_MCP_PROXY_UNAUTHORIZED"}"#),
            (403, r#"{"detail":"forbidden for this tenant"}"#),
            (500, "token_expired"),
            (503, "token_expired"),
            (404, "token_expired"),
            (429, "token_expired"),
        ] {
            assert_eq!(
                classify_upstream_verdict(status, body),
                UpstreamVerdict::Indeterminate,
                "HTTP {status} / {body:?} is not a verdict on this runner's credential"
            );
        }
    }

    /// A transport error is UNKNOWN. It must leave the slot alone (the
    /// pre-existing `slot_refresh_never_clears_on_transport_error`
    /// discipline) — and now also leave the POSTURE alone: no clear, no dark,
    /// no second transition.
    #[tokio::test]
    async fn a_transport_error_leaves_the_posture_unchanged() {
        let _serialised = health_lock();
        reset_posture();
        let mgr = test_auth_manager("posture_transport_error");
        let ta = tenant(0);
        let now = chrono::Utc::now().timestamp();
        // Stale-but-valid, so the pass genuinely attempts the network call.
        let jwt = synth_jwt(now + 30 * 60, "unreachable");
        mgr.store_tenant_device_jwt(&ta, &jwt).expect("slot a");

        let dead_base = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            let port = l.local_addr().expect("addr").port();
            drop(l);
            format!("http://127.0.0.1:{port}")
        };
        let outcomes = refresh_tenant_slots(
            &mgr,
            &dead_base,
            &dead_base,
            DID,
            None,
            PosturePinInputs::UNPINNED,
        )
        .await;
        assert_eq!(outcomes, vec![(ta, TenantSlotOutcome::KeptExisting)]);

        let transitions = recorded_posture_transitions();
        assert_eq!(
            transitions,
            vec![PostureTransition {
                from: None,
                to: CoordCredentialPosture::Expiring,
            }],
            "a timeout may not produce a SECOND transition — the posture the \
             boot observation established still stands: {transitions:?}"
        );
        let after = coord_credential_posture().expect("a pass publishes a posture");
        assert_eq!(after.posture, CoordCredentialPosture::Expiring);
        assert!(
            after.posture.can_answer(),
            "an unreachable coord must not make this runner claim its own \
             credential is dead"
        );
        reset_posture();
    }

    /// `/health`'s `coordMcpForwarder.canAnswer` reads this and nothing else.
    #[test]
    fn can_answer_flips_with_the_posture() {
        for p in [
            CoordCredentialPosture::Live,
            CoordCredentialPosture::Expiring,
        ] {
            assert!(p.can_answer(), "{} must answer", p.as_str());
            assert_eq!(p.cta(), None);
        }
        for p in [
            CoordCredentialPosture::Expired,
            CoordCredentialPosture::Absent,
            CoordCredentialPosture::Unrefreshable,
            CoordCredentialPosture::Dark(DarkCause::UpstreamRejected),
        ] {
            assert!(!p.can_answer(), "{} must NOT answer", p.as_str());
            assert!(p.cta().is_some(), "{} must offer a CTA", p.as_str());
            assert!(
                p.message().contains("no coord access"),
                "{} must say what it costs the user: {}",
                p.as_str(),
                p.message()
            );
        }
    }

    /// The dedup, generalised from the Cognito path's `dark_notified` boolean
    /// to a posture-keyed rule.
    #[test]
    fn posture_notification_fires_once_per_transition_and_at_boot() {
        use CoordCredentialPosture as P;
        // Boot into a bad state IS a transition (DD2).
        assert!(should_notify_posture(None, P::Expired));
        assert!(should_notify_posture(None, P::Unrefreshable));
        // Boot into a good state announces nothing — a healthy runner must not
        // claim autonomy "resumed".
        assert!(!should_notify_posture(None, P::Live));
        assert!(!should_notify_posture(None, P::Expiring));
        // Steady state: no repeat every 5 minutes.
        assert!(!should_notify_posture(Some(P::Expired), P::Expired));
        assert!(!should_notify_posture(Some(P::Live), P::Live));
        // A change of CAUSE while dark is new information.
        assert!(should_notify_posture(Some(P::Expired), P::Unrefreshable));
        assert!(should_notify_posture(
            Some(P::Expired),
            P::Dark(DarkCause::UpstreamRejected)
        ));
        // Recovery clears the banner — a stale banner after recovery teaches
        // users to ignore it.
        assert!(should_notify_posture(Some(P::Unrefreshable), P::Live));
        assert!(should_notify_posture(Some(P::Expired), P::Expiring));
        // …but only when we actually reported dark.
        assert!(!should_notify_posture(Some(P::Expiring), P::Live));
    }

    /// An unreadable store says nothing. It must not render as `absent`, which
    /// would fire a banner off a missing measurement.
    #[test]
    fn an_unreadable_slot_is_unknown_and_publishes_nothing() {
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let unreadable = SlotObservation::unreadable(Some("t".into()));
        assert_eq!(
            derive_and_publish_posture(&[unreadable], PosturePinInputs::UNPINNED, now),
            None
        );
        assert!(
            coord_credential_posture().is_none(),
            "UNKNOWN must leave the posture UNKNOWN"
        );
        reset_posture();
    }

    /// A runner that holds no coord credential at all reads `absent` — the
    /// never-paired case, whose sessions also have no coord access.
    #[test]
    fn a_runner_holding_nothing_reads_absent() {
        let now = 1_700_000_000i64;
        assert_eq!(
            derive_coord_credential_posture(
                &SlotObservation::observed(None, None),
                UpstreamSignal::default(),
                now
            ),
            CoordCredentialPosture::Absent
        );
        assert_eq!(
            derive_coord_credential_posture(
                &SlotObservation::observed(None, Some("   ")),
                UpstreamSignal::default(),
                now
            ),
            CoordCredentialPosture::Absent
        );
    }

    /// A pass that PUT a working credential in the slot ends the story: the
    /// posture describes what the runner holds now.
    #[test]
    fn a_healed_slot_reads_live_whatever_it_held_before() {
        let now = 1_700_000_000i64;
        let dead_then_healed = |outcome: TenantSlotOutcome| SlotObservation {
            tenant_id: None,
            exp: Some(now - 600),
            present: true,
            unknown: false,
            outcome: Some(outcome),
        };
        assert_eq!(
            derive_coord_credential_posture(
                &dead_then_healed(TenantSlotOutcome::Refreshed),
                UpstreamSignal::default(),
                now
            ),
            CoordCredentialPosture::Live
        );
        assert_eq!(
            derive_coord_credential_posture(
                &dead_then_healed(TenantSlotOutcome::Cleared {
                    cause: SlotClearCause::DecodedExpiry,
                    rederived: true,
                }),
                UpstreamSignal::default(),
                now
            ),
            CoordCredentialPosture::Live
        );
        // A clear that could NOT re-derive is the terminal state instead.
        assert_eq!(
            derive_coord_credential_posture(
                &dead_then_healed(TenantSlotOutcome::Cleared {
                    cause: SlotClearCause::CoordRejection,
                    rederived: false,
                }),
                UpstreamSignal::default(),
                now
            ),
            CoordCredentialPosture::Unrefreshable
        );
    }

    /// An opaque slot value can never be presented and can never be judged by
    /// `exp` — it is dead, not fresh.
    #[test]
    fn an_opaque_slot_value_is_expired_not_live() {
        let now = 1_700_000_000i64;
        assert_eq!(
            derive_coord_credential_posture(
                &SlotObservation::observed(None, Some("not-a-jwt")),
                UpstreamSignal::default(),
                now
            ),
            CoordCredentialPosture::Expired
        );
    }

    /// `since` names when the runner ENTERED the posture, not when it was last
    /// observed in it — the banner says "expired since 03:54".
    #[test]
    fn since_is_carried_across_unchanged_publishes() {
        let _serialised = health_lock();
        reset_posture();
        let first = publish_coord_credential_posture(
            CoordCredentialPosture::Expired,
            Some("t".into()),
            Some(1),
            None,
        );
        assert!(first.is_some(), "the first publish is a transition");
        let since = coord_credential_posture().expect("published").since;
        // Same posture again: no transition, and `since` must not move.
        assert_eq!(
            publish_coord_credential_posture(
                CoordCredentialPosture::Expired,
                Some("t".into()),
                Some(1),
                None,
            ),
            None
        );
        assert_eq!(coord_credential_posture().expect("published").since, since);
        reset_posture();
    }

    /// The `/health` wire shape — the field names the fleet's readers key on.
    #[test]
    fn posture_json_names_state_cause_and_can_answer() {
        let status = CoordCredentialStatus {
            posture: CoordCredentialPosture::Dark(DarkCause::UpstreamRejected),
            tenant_id: Some("t".into()),
            exp: Some(42),
            last_ok_at: Some(1),
            last_401_at: Some(2),
            last_refresh_outcome: Some("kept-existing".into()),
            since: 7,
            observed_at_unix: 9,
            attributable: true,
        };
        let v = status.to_json();
        assert_eq!(v["state"], "dark");
        assert_eq!(v["cause"], "upstream_401");
        assert_eq!(v["canAnswer"], false);
        assert_eq!(v["cta"], "re_pair");
        assert_eq!(v["since"], 7);
        assert_eq!(v["lastRefreshOutcome"], "kept-existing");
        assert_eq!(v["last401At"], 2);
        // `posture` is the canonical key; `state` stays as a deprecated alias.
        assert_eq!(v["posture"], "dark");
    }

    // -----------------------------------------------------------------------
    // Regression tests for the pre-PR review's blocking findings (C1-C4, M1,
    // M2, M8). Each one FAILS against the behaviour at a2cf91992.
    // -----------------------------------------------------------------------

    /// **C2.** Coord has SIX credential refusals and the first classifier
    /// matched two of them by substring. Every one of the six must land as
    /// `CredentialRejected`, and every refusal that is NOT about the token
    /// must stay `Indeterminate` — including the three the
    /// [`DarkCause::UpstreamRejected`] doc comment names (a revoked jti, a
    /// rotated signing key, a foreign-issuer token), all of which used to be
    /// Indeterminate and so could never produce the `dark` rung they describe.
    ///
    /// The code spellings are coord's own: `RejectCode::as_str`
    /// (`crates/coord/src/jwt.rs`) and `crates/coord/src/auth.rs`.
    #[test]
    fn every_coord_credential_refusal_code_classifies_as_a_credential_rejection() {
        for code in [
            "token_expired",
            "token_invalid",
            "token_invalid_signature",
            "token_foreign_issuer",
            "token_ambiguous_legacy_kid",
            "token_revoked",
        ] {
            let body = format!(
                r#"{{"error":"…","code":"{code}","credential_free_doors":{{"guard_free":[]}}}}"#
            );
            assert_eq!(
                classify_upstream_verdict(401, &body),
                UpstreamVerdict::CredentialRejected,
                "coord code {code} IS a verdict on this runner's credential"
            );
        }
        // Not about the token: we sent none, coord is unwell, the bearer is a
        // verified principal of the wrong KIND, or the ROUTE wanted an
        // operator. None of these may move the streak.
        for code in [
            "missing_token",
            "keys_not_initialized",
            "attach_grant_not_a_principal",
            "create_grant_not_a_principal",
            "auth_required",
            "tenant_not_resolved",
        ] {
            let body = format!(r#"{{"error":"…","code":"{code}"}}"#);
            for status in [401u16, 403] {
                assert_eq!(
                    classify_upstream_verdict(status, &body),
                    UpstreamVerdict::Indeterminate,
                    "coord code {code} says nothing about the credential"
                );
            }
        }
        // The operator-route refusal carries no machine code at all.
        assert_eq!(
            classify_upstream_verdict(401, r#"{"detail":"invalid operator token"}"#),
            UpstreamVerdict::Indeterminate
        );
        // `invalid_token` was dead code in the first cut — coord spells it
        // `token_invalid` and `invalid_token` never appears in an HTTP body.
        // If coord ever DID answer it, it is still not in the closed set.
        assert_eq!(
            classify_upstream_verdict(401, r#"{"code":"invalid_token"}"#),
            UpstreamVerdict::Indeterminate
        );
        // A truncated body still yields the code: the scan is bounded, so the
        // JSON may not close.
        assert_eq!(
            classify_upstream_verdict(401, r#"{"code":"token_revoked","error":"tok"#),
            UpstreamVerdict::CredentialRejected
        );
    }

    /// **C1.** A 200 from a route coord does not gate is not evidence about the
    /// credential, so it must move the streak in NEITHER direction and must
    /// NOT stamp `lastOkAt`.
    ///
    /// This is not hypothetical: coord's claims reads sit behind an INFALLIBLE
    /// extractor whose enforcement arm is gated on an env var absent from the
    /// production task definition, so a dead JWT gets a 200 there. With those
    /// 200s counted, a box that polls claims could never reach `dark`.
    #[test]
    fn a_200_from_a_non_authenticating_route_moves_the_streak_in_neither_direction() {
        let _serialised = health_lock();
        reset_posture();
        let t = tenant(3);

        // Two real rejections from a real door.
        for _ in 0..2 {
            note_coord_upstream_verdict(Some(t), true, 401, br#"{"code":"token_expired"}"#);
        }
        assert_eq!(
            upstream_signal_for(Some(&t.to_string())).consecutive_rejections,
            2
        );

        // A hundred cheerful 200s from the claims door. None of them looked at
        // the credential.
        for _ in 0..100 {
            note_coord_upstream_verdict(Some(t), false, 200, br#"{"claims":[]}"#);
        }
        let sig = upstream_signal_for(Some(&t.to_string()));
        assert_eq!(
            sig.consecutive_rejections, 2,
            "a 200 from a route coord never gated must not reset the streak"
        );
        assert!(
            sig.last_ok_at.is_none(),
            "lastOkAt must not claim coord accepted a credential coord never checked"
        );

        // And it must not move it the other way either.
        for _ in 0..100 {
            note_coord_upstream_verdict(Some(t), false, 401, br#"{"code":"token_expired"}"#);
        }
        assert_eq!(
            upstream_signal_for(Some(&t.to_string())).consecutive_rejections,
            2,
            "a non-authenticating route's 401 is not a verdict either"
        );

        // The third REAL rejection is what reaches the threshold.
        note_coord_upstream_verdict(Some(t), true, 401, br#"{"code":"token_revoked"}"#);
        let now = chrono::Utc::now().timestamp();
        let obs = SlotObservation {
            tenant_id: Some(t.to_string()),
            exp: Some(now + 3 * 60 * 60),
            present: true,
            unknown: false,
            outcome: Some(TenantSlotOutcome::SkippedFresh),
        };
        assert_eq!(
            derive_coord_credential_posture(&obs, upstream_signal_for(Some(&t.to_string())), now),
            CoordCredentialPosture::Dark(DarkCause::UpstreamRejected),
            "a box that polls claims must still be able to reach dark"
        );
        reset_posture();
    }

    /// **M1.** The streak used to survive credential REPLACEMENT, so `dark`
    /// latched on a working token: revoked → 3 × 401 → `dark`; the re-mint
    /// succeeds and rung 1 says `live`; the next pass is `SkippedFresh` and
    /// rung 5 reads the dead credential's streak back — `dark` again, on a
    /// credential coord has just minted, behind a non-dismissable banner.
    #[tokio::test]
    async fn persisting_a_new_credential_resets_the_streak_so_dark_cannot_latch() {
        let _serialised = health_lock();
        reset_posture();
        let mgr = test_auth_manager("posture_streak_reset");
        let ta = tenant(4);
        let now = chrono::Utc::now().timestamp();
        // Stale-but-valid, so the pass refreshes rather than clears.
        mgr.store_tenant_device_jwt(&ta, &synth_jwt(now + 30 * 60, "stale"))
            .expect("slot a");
        // Coord has been refusing the OLD credential.
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(Some(ta), true, 401, br#"{"code":"token_revoked"}"#);
        }
        assert_eq!(
            upstream_signal_for(Some(&ta.to_string())).consecutive_rejections,
            UPSTREAM_DARK_THRESHOLD
        );

        let (base, _cap, _shutdown) = spawn_mock(vec![]);
        let outcomes =
            refresh_tenant_slots(&mgr, &base, "", DID, None, PosturePinInputs::UNPINNED).await;
        assert_eq!(outcomes, vec![(ta, TenantSlotOutcome::Refreshed)]);

        assert_eq!(
            upstream_signal_for(Some(&ta.to_string())).consecutive_rejections,
            0,
            "a NEW credential in the slot spends every rejection against the old one"
        );
        assert_eq!(
            coord_credential_posture()
                .expect("a pass publishes")
                .posture,
            CoordCredentialPosture::Live
        );

        // THE latch: the next pass touches nothing (the slot is fresh), so the
        // posture is decided by rung 5 — which must not read a spent streak.
        let fresh_obs = SlotObservation {
            tenant_id: Some(ta.to_string()),
            exp: Some(now + 3 * 60 * 60),
            present: true,
            unknown: false,
            outcome: Some(TenantSlotOutcome::SkippedFresh),
        };
        assert_eq!(
            derive_and_publish_posture(
                std::slice::from_ref(&fresh_obs),
                PosturePinInputs::UNPINNED,
                now
            ),
            None,
            "still live — no transition, and above all no relapse to dark"
        );
        assert_eq!(
            coord_credential_posture().expect("published").posture,
            CoordCredentialPosture::Live
        );
        reset_posture();
    }

    /// **N2** — the DEFAULT-key half of the same latch, and the shape the
    /// original M1 test did not cover.
    ///
    /// On a single-slot box whose sessions are UNPINNED, the forwarders file
    /// their verdicts under the DEFAULT key while the heal paths reset the
    /// TENANT key. So after a successful refresh the default bucket still held
    /// a streak of 3 with a fresh `last_rejection_at`, the fold picked it, and
    /// the next `SkippedFresh` pass went `dark` again on a credential coord
    /// had just minted.
    #[tokio::test]
    async fn a_heal_also_spends_the_default_slots_streak_on_a_single_slot_box() {
        let _serialised = health_lock();
        reset_posture();
        let mgr = test_auth_manager("posture_streak_reset_default");
        let ta = tenant(5);
        let now = chrono::Utc::now().timestamp();
        mgr.store_tenant_device_jwt(&ta, &synth_jwt(now + 30 * 60, "stale"))
            .expect("slot a");
        // THE difference from the sibling test: the rejections are filed under
        // the DEFAULT key, because an unpinned session presents the legacy
        // slot. `device_bearer_for(None)` and `device_bearer_for(Some(ta))`
        // are the same credential here — which is why the fold reads it.
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_revoked"}"#);
        }
        assert_eq!(
            upstream_signal_for(None).consecutive_rejections,
            UPSTREAM_DARK_THRESHOLD
        );

        let (base, _cap, _shutdown) = spawn_mock(vec![]);
        let outcomes =
            refresh_tenant_slots(&mgr, &base, "", DID, None, PosturePinInputs::UNPINNED).await;
        assert_eq!(outcomes, vec![(ta, TenantSlotOutcome::Refreshed)]);

        assert_eq!(
            upstream_signal_for(None).consecutive_rejections,
            0,
            "the heal must spend the DEFAULT bucket too, or the fold reads it back"
        );
        assert_eq!(
            coord_credential_posture()
                .expect("a pass publishes")
                .posture,
            CoordCredentialPosture::Live
        );

        // The latch: the next pass touches nothing, so rung 5 decides.
        let fresh_obs = SlotObservation {
            tenant_id: Some(ta.to_string()),
            exp: Some(now + 3 * 60 * 60),
            present: true,
            unknown: false,
            outcome: Some(TenantSlotOutcome::SkippedFresh),
        };
        assert_eq!(
            derive_and_publish_posture(
                std::slice::from_ref(&fresh_obs),
                PosturePinInputs::UNPINNED,
                now
            ),
            None,
            "no relapse to dark on a freshly minted credential"
        );
        assert_eq!(
            coord_credential_posture().expect("published").posture,
            CoordCredentialPosture::Live
        );
        reset_posture();
    }

    /// The N2 reset is SCOPED to the one-slot case: with two slots the default
    /// bucket is not this tenant's credential, so a heal on one slot must not
    /// erase evidence the unattributable arm is about to report.
    #[tokio::test]
    async fn a_heal_on_a_multi_slot_box_does_not_erase_the_default_buckets_evidence() {
        let _serialised = health_lock();
        reset_posture();
        let mgr = test_auth_manager("posture_streak_reset_multi");
        let ta = tenant(0);
        let tb = tenant(1);
        let now = chrono::Utc::now().timestamp();
        mgr.store_tenant_device_jwt(&ta, &synth_jwt(now + 30 * 60, "stale-a"))
            .expect("slot a");
        mgr.store_tenant_device_jwt(&tb, &synth_jwt(now + 3 * 60 * 60, "fresh-b"))
            .expect("slot b");
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_expired"}"#);
        }

        let (base, _cap, _shutdown) = spawn_mock(vec![]);
        let _ = refresh_tenant_slots(&mgr, &base, "", DID, None, PosturePinInputs::UNPINNED).await;

        assert_eq!(
            upstream_signal_for(None).consecutive_rejections,
            UPSTREAM_DARK_THRESHOLD,
            "healing ONE slot says nothing about a credential no slot owns"
        );
        let published = coord_credential_posture().expect("published");
        assert_eq!(
            published.posture,
            CoordCredentialPosture::Dark(DarkCause::UpstreamRejected)
        );
        assert_eq!(published.tenant_id, None);
        reset_posture();
    }

    /// **M2.** The streak was PROCESS-GLOBAL while the credential is PER
    /// TENANT. Tenant B's healthy traffic continuously reset tenant A's
    /// rejections, so A read `live` while dark — the dangerous direction.
    ///
    /// Also pins the reported identity: the worst slot's tenant is the one
    /// `/health` must name, because that is the slot an operator has to fix.
    #[test]
    fn one_tenants_rejections_are_not_erased_by_another_tenants_health() {
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let a = tenant(4);
        let b = tenant(5);

        let slot = |t: uuid::Uuid| SlotObservation {
            tenant_id: Some(t.to_string()),
            exp: Some(now + 3 * 60 * 60),
            present: true,
            unknown: false,
            outcome: Some(TenantSlotOutcome::SkippedFresh),
        };

        // Interleaved exactly as a live box would produce them: A refused, B
        // fine, over and over.
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(Some(a), true, 401, br#"{"code":"token_expired"}"#);
            note_coord_upstream_verdict(Some(b), true, 200, br#"{"ok":true}"#);
        }

        assert_eq!(
            derive_coord_credential_posture(
                &slot(a),
                upstream_signal_for(Some(&a.to_string())),
                now
            ),
            CoordCredentialPosture::Dark(DarkCause::UpstreamRejected),
            "tenant A is dark and B's health is not evidence about A"
        );
        assert_eq!(
            derive_coord_credential_posture(
                &slot(b),
                upstream_signal_for(Some(&b.to_string())),
                now
            ),
            CoordCredentialPosture::Live
        );

        // The published posture is the WORST slot, and it names A — not
        // whichever slot `max_by_key` happened to visit last.
        let transition =
            derive_and_publish_posture(&[slot(a), slot(b)], PosturePinInputs::UNPINNED, now);
        assert_eq!(
            transition.map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected))
        );
        let published = coord_credential_posture().expect("published");
        assert_eq!(
            published.tenant_id.as_deref(),
            Some(a.to_string().as_str()),
            "`/health` must name the tenant an operator has to fix, never the healthy one"
        );
        assert!(published.last_401_at.is_some());

        // Same evidence, slots presented in the other order — the verdict and
        // the named tenant must not depend on iteration order.
        reset_coord_credential_posture_for_test();
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(Some(a), true, 401, br#"{"code":"token_expired"}"#);
            note_coord_upstream_verdict(Some(b), true, 200, br#"{"ok":true}"#);
        }
        let _ = derive_and_publish_posture(&[slot(b), slot(a)], PosturePinInputs::UNPINNED, now);
        assert_eq!(
            coord_credential_posture()
                .expect("published")
                .tenant_id
                .as_deref(),
            Some(a.to_string().as_str())
        );
        reset_posture();
    }

    /// The single-tenant machine: an UNPINNED session presents the legacy
    /// default slot, so its verdicts are filed under the default key. With one
    /// usable slot the two buckets describe the SAME credential, and keeping
    /// them apart would lose the evidence that used to reach the posture when
    /// the counter was global.
    #[test]
    fn a_sole_slot_folds_in_the_default_slots_verdicts() {
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let a = tenant(2);
        let slot = SlotObservation {
            tenant_id: Some(a.to_string()),
            exp: Some(now + 3 * 60 * 60),
            present: true,
            unknown: false,
            outcome: Some(TenantSlotOutcome::SkippedFresh),
        };
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_expired"}"#);
        }
        let transition = derive_and_publish_posture(
            std::slice::from_ref(&slot),
            PosturePinInputs::UNPINNED,
            now,
        );
        assert_eq!(
            transition.map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected)),
            "the one slot on the box IS the credential the default door presented"
        );
        assert!(
            coord_credential_posture()
                .expect("published")
                .last_401_at
                .is_some(),
            "the timestamps published must be the ones the posture was DERIVED from,              not a re-read of a bucket that holds no evidence"
        );
        // …but with a SECOND tenant present the fold is off, because the
        // default bucket can no longer be ATTRIBUTED. It must still be
        // REPORTED — see the unattributable arm, pinned next door.
        reset_coord_credential_posture_for_test();
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_expired"}"#);
        }
        let other = SlotObservation {
            tenant_id: Some(tenant(3).to_string()),
            ..slot.clone()
        };
        let transition =
            derive_and_publish_posture(&[slot, other], PosturePinInputs::UNPINNED, now);
        assert_eq!(
            transition.map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected)),
            "unattributable is not the same as absent — this assertion USED to \
             read `Live`, which pinned the N1 defect as intended behaviour"
        );
        let published = coord_credential_posture().expect("published");
        assert_eq!(
            published.tenant_id, None,
            "no tenant owns these rejections, and saying so is the finding"
        );
        reset_posture();
    }

    /// **N1**, the whole scenario end to end: two `device_jwt:<tenant>` slots
    /// and a `machine.json` with no `active_tenant_id`.
    ///
    /// `resolve_session_tenant` answers `Ok(None)` for every session on such a
    /// box (both pins `Unpinned`), so every forwarded call presents
    /// `device_bearer_for(None)` — the LEGACY slot — and every rejection lands
    /// under the default key. With the fold scoped to one slot, nothing read
    /// that bucket: both slots hold a future `exp`, rung 6 called them `live`,
    /// `canAnswer` read `true`, and no banner fired while coord was refusing
    /// every call the runner made. The pre-keying global counter WOULD have
    /// surfaced it, so dropping the bucket was a regression in the one
    /// direction that matters.
    #[test]
    fn rejections_no_tenant_slot_owns_are_reported_without_a_tenant_not_dropped() {
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let healthy_slot = |t: uuid::Uuid| SlotObservation {
            tenant_id: Some(t.to_string()),
            // Comfortably fresh: `exp` alone says `live`, which is exactly why
            // the upstream signal has to be consulted.
            exp: Some(now + 3 * 60 * 60),
            present: true,
            unknown: false,
            outcome: Some(TenantSlotOutcome::SkippedFresh),
        };
        let slots = [healthy_slot(tenant(0)), healthy_slot(tenant(1))];

        // Below the threshold, nothing happens — the rate rule still governs
        // this arm, so a coord deploy cannot flip it either.
        for _ in 1..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_expired"}"#);
        }
        assert_eq!(
            derive_and_publish_posture(&slots, PosturePinInputs::UNPINNED, now).map(|t| t.to),
            Some(CoordCredentialPosture::Live),
            "two transient 401s on an unattributable credential are not a verdict"
        );

        // The threshold'th consecutive rejection IS the verdict.
        note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_revoked"}"#);
        let transition = derive_and_publish_posture(&slots, PosturePinInputs::UNPINNED, now);
        assert_eq!(
            transition.map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected))
        );
        let published = coord_credential_posture().expect("published");
        assert_eq!(published.tenant_id, None, "unattributable, and it says so");
        assert_eq!(published.exp, None, "\"which slot's exp?\" has no answer");
        assert!(published.last_401_at.is_some());
        assert!(!published.posture.can_answer());
        // And the banner fires: this is the state the whole plan exists for.
        assert!(should_notify_posture(
            Some(CoordCredentialPosture::Live),
            published.posture
        ));

        // A per-slot verdict that is ALREADY non-answering keeps the floor —
        // it names a slot an operator can actually fix, which the
        // unattributable arm cannot.
        reset_coord_credential_posture_for_test();
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_expired"}"#);
        }
        let dead = SlotObservation {
            exp: Some(now - 60),
            ..healthy_slot(tenant(1))
        };
        let _ = derive_and_publish_posture(
            &[healthy_slot(tenant(0)), dead],
            PosturePinInputs::UNPINNED,
            now,
        );
        let published = coord_credential_posture().expect("published");
        assert_eq!(published.posture, CoordCredentialPosture::Expired);
        assert_eq!(
            published.tenant_id.as_deref(),
            Some(tenant(1).to_string().as_str()),
            "a slot-attributed fault outranks the unattributable one"
        );

        // A 2xx on the default credential clears it, same as any other slot.
        reset_coord_credential_posture_for_test();
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_expired"}"#);
        }
        note_coord_upstream_verdict(None, true, 200, br#"{"ok":true}"#);
        assert_eq!(
            derive_and_publish_posture(&slots, PosturePinInputs::UNPINNED, now).map(|t| t.to),
            Some(CoordCredentialPosture::Live)
        );
        reset_posture();
    }

    /// **N1, the CLASS** — a LEGACY-ONLY INSTALL, which is a supported shape
    /// and the most reachable remaining instance.
    ///
    /// `paired_user.json` names `default_tenant_id: X`, `machine.json` pins
    /// `active_tenant_id: X`, the credential lives in the legacy
    /// `access_token` slot, and there is no `device_jwt:X` slot at all.
    ///
    /// * FORWARDER side: `session_tenant_or_refuse` answers `Ok(Some(X))`;
    ///   `select_device_bearer` misses the tenant slot and falls back to
    ///   `legacy_slot_bearer` because `default_tenant == Some(t)`
    ///   (`src-tauri/src/auth.rs:1527-1529`, reached from `device_bearer_for`
    ///   at `:1463`, which supplies `default_binding_tenant()`). So coord's
    ///   refusals file under key **`X`**.
    /// * POSTURE side: `list_tenant_device_jwt_tenants()` is empty, so the
    ///   refresher takes its legacy branch (`refresher_loop`, the
    ///   `has_tenant_slots == false` arm) and publishes ONE observation with
    ///   `tenant_id: None` — which reads the DEFAULT bucket.
    ///
    /// The fixture is the exact MIRROR of every other N1-adjacent test, and
    /// that asymmetry is the whole point: the others build TENANT-keyed
    /// observations and file under the DEFAULT key, so they stayed green while
    /// this defect was live. Here the observation carries `tenant_id: None`
    /// and the rejections are filed under a TENANT key.
    ///
    /// Bucket `X` therefore climbs past the threshold with nobody reading it,
    /// the default bucket stays empty so the first cut of the unattributable
    /// arm never fired, and the legacy `exp` is in the future → rung 6 →
    /// `live`, `canAnswer: true`, `derived_status: healthy`, no banner, while
    /// coord refuses every call the runner makes.
    #[test]
    fn a_legacy_only_install_refused_under_its_pinned_tenant_key_reads_dark() {
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let x = tenant(0);

        // EXACTLY what the legacy branch publishes, built through the SAME
        // constructor it uses (`SlotObservation::observed(None, …)`) rather
        // than a hand-copied literal, so this fixture follows the production
        // shape if that shape moves: one observation, NO tenant — because
        // `list_tenant_device_jwt_tenants()` is empty on this box — carrying a
        // live legacy credential.
        let legacy_jwt = synth_jwt(now + 3 * 60 * 60, "legacy-only-install");
        let legacy_obs = SlotObservation::observed(None, Some(legacy_jwt.as_str()));
        assert_eq!(legacy_obs.tenant_id, None);
        assert_eq!(legacy_obs.exp, Some(now + 3 * 60 * 60));
        assert!(legacy_obs.present && !legacy_obs.unknown);
        assert_eq!(
            legacy_obs.outcome, None,
            "no pass outcome on the legacy branch"
        );
        // …and exactly where the forwarder files coord's verdicts: under the
        // PINNED TENANT's key, because that is the tenant it resolved.
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(Some(x), true, 401, br#"{"code":"token_expired"}"#);
        }
        // The default bucket — the only one the first cut of this arm read —
        // is empty, and the observation's own key IS the default one.
        assert_eq!(upstream_signal_for(None).consecutive_rejections, 0);

        let transition = derive_and_publish_posture(
            std::slice::from_ref(&legacy_obs),
            PosturePinInputs::UNPINNED,
            now,
        );
        assert_eq!(
            transition.map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected)),
            "a legacy-only install whose credential coord refuses must not read `live`"
        );
        let published = coord_credential_posture().expect("published");
        assert_eq!(
            published.tenant_id, None,
            "`tenant_id` names the device_jwt slot to fix, and on this box there is none"
        );
        assert!(!published.posture.can_answer());
        assert!(published.last_401_at.is_some());
        reset_posture();
    }

    /// The second shape of the same class: tenant slots for Y and Z, but the
    /// machine is pinned to X, which has no slot. X's bucket is an orphan and
    /// the healthy slots must not hide it.
    #[test]
    fn an_orphan_tenant_key_is_reported_even_when_every_observed_slot_is_healthy() {
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let healthy = |t: uuid::Uuid| SlotObservation {
            tenant_id: Some(t.to_string()),
            exp: Some(now + 3 * 60 * 60),
            present: true,
            unknown: false,
            outcome: Some(TenantSlotOutcome::SkippedFresh),
        };
        let slots = [healthy(tenant(1)), healthy(tenant(2))];
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(Some(tenant(3)), true, 401, br#"{"code":"token_revoked"}"#);
        }
        // Neither observed slot's own bucket, nor the default one, holds
        // anything — the first cut read all three and saw nothing.
        assert_eq!(upstream_signal_for(None).consecutive_rejections, 0);
        assert_eq!(
            upstream_signal_for(Some(&tenant(1).to_string())).consecutive_rejections,
            0
        );

        let transition = derive_and_publish_posture(&slots, PosturePinInputs::UNPINNED, now);
        assert_eq!(
            transition.map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected))
        );
        assert_eq!(
            coord_credential_posture().expect("published").tenant_id,
            None
        );

        // A CLAIMED bucket is not an orphan: the same rejections filed under a
        // slot this pass observes go down the ordinary per-slot path and NAME
        // that slot, which is the more actionable answer.
        reset_coord_credential_posture_for_test();
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(Some(tenant(1)), true, 401, br#"{"code":"token_revoked"}"#);
        }
        let _ = derive_and_publish_posture(&slots, PosturePinInputs::UNPINNED, now);
        assert_eq!(
            coord_credential_posture()
                .expect("published")
                .tenant_id
                .as_deref(),
            Some(tenant(1).to_string().as_str()),
            "an observed slot's own rejections are attributable, and must be attributed"
        );
        reset_posture();
    }

    /// **THE ROUND-4 BLOCKER**: a dark box goes QUIET, and a quiet box used to
    /// report itself healthy.
    ///
    /// Legacy-only install, dark. t=0: three forwarded calls 401 under key
    /// `X`. The t+5min pass fires `dark(tenant_id: None)` — correct. Sessions
    /// then STOP calling coord, which is the normal consequence of every coord
    /// call failing, not an edge case. Under the age-as-primary-guard version
    /// the t+35min pass filtered the orphan, fell through to `worst` — the
    /// legacy observation with a future `exp` — and published `live`,
    /// `canAnswer: true`, `derived_status: healthy`, plus an explicit "Coord
    /// access restored — this runner's credential is live again." banner,
    /// because `should_notify_posture(Some(Dark), Live)` fires on `was_dark`.
    ///
    /// Nothing had healed. The box was exactly as dead at t+35min as at t+5min,
    /// and the defect was self-reinforcing: the darker a box, the quieter its
    /// sessions get, and the quieter it gets, the healthier it reports.
    #[test]
    fn a_dark_box_that_goes_quiet_does_not_report_itself_healthy() {
        let _serialised = health_lock();
        reset_posture();
        let t0 = chrono::Utc::now().timestamp();
        let x = tenant(0);
        // 30 days, so every probe below stays INSIDE the credential's own
        // validity: the only thing changing across them is how long the box
        // has been quiet. (A shorter exp made the t+24h probe cross it and
        // transition to `expired` — correct behaviour, but it tests the clock
        // rather than the silence.)
        let legacy_jwt = synth_jwt(t0 + 30 * 24 * 60 * 60, "legacy-only-install");
        let legacy_obs = SlotObservation::observed(None, Some(legacy_jwt.as_str()));

        // t=0 — three forwarded calls, all refused, filed under the PINNED
        // tenant's key because that is the tenant the forwarder resolved.
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(Some(x), true, 401, br#"{"code":"token_expired"}"#);
        }

        // t+5min — dark, correctly.
        let t5 = t0 + 5 * 60;
        assert_eq!(
            derive_and_publish_posture(
                std::slice::from_ref(&legacy_obs),
                PosturePinInputs::UNPINNED,
                t5
            )
            .map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected))
        );

        // …and then silence, because every call fails.
        let transitions_before = recorded_posture_transitions().len();
        for minutes in [35i64, 90, 6 * 60, 24 * 60] {
            let later = t0 + minutes * 60;
            let transition = derive_and_publish_posture(
                std::slice::from_ref(&legacy_obs),
                PosturePinInputs::UNPINNED,
                later,
            );
            assert_eq!(
                transition, None,
                "t+{minutes}min: nothing healed, so nothing may transition"
            );
            let published = coord_credential_posture().expect("published");
            assert_eq!(
                published.posture,
                CoordCredentialPosture::Dark(DarkCause::UpstreamRejected),
                "t+{minutes}min: a quiet dark box must stay dark, never read `live`"
            );
            // THE property, stated independently of which non-answering
            // posture we are in: silence must never buy `canAnswer: true`.
            assert!(!published.posture.can_answer());
        }

        // And above all: no "Coord access restored" was ever emitted.
        assert_eq!(
            recorded_posture_transitions().len(),
            transitions_before,
            "a recovery banner on a box that never healed is worse than silence"
        );
        assert!(
            !recorded_posture_transitions().iter().any(|t| t.from
                == Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected))
                && t.to.can_answer()),
            "no dark -> answering transition may be published without a 2xx"
        );

        // CONTRADICTION is what clears it — a real success on that key, at any
        // time, with no clock involved.
        note_coord_upstream_verdict(Some(x), true, 200, br#"{"ok":true}"#);
        assert_eq!(
            derive_and_publish_posture(
                std::slice::from_ref(&legacy_obs),
                PosturePinInputs::UNPINNED,
                t0 + 48 * 60 * 60
            )
            .map(|t| t.to),
            Some(CoordCredentialPosture::Live),
            "a 2xx on the orphan key retires its evidence the honest way"
        );
        reset_posture();
    }

    /// The IMPOSSIBILITY half, which replaced the clock: a bucket for a tenant
    /// this runner can no longer present a credential for is EVICTED, so it
    /// cannot pin a re-paired runner dark forever.
    ///
    /// This is what `a_stale_orphan_bucket_does_not_darken_a_healthy_runner`
    /// used to assert through a 30-minute age bound. The scenario it was
    /// written for is real; the guard was the wrong one, because age also
    /// retires the evidence of a box that is still dark and merely silent.
    #[test]
    fn an_evicted_bucket_does_not_darken_a_re_paired_runner() {
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let gone = tenant(0); // the tenant this box was re-paired AWAY from
        let y = tenant(1);
        let z = tenant(2);
        let healthy = |t: uuid::Uuid| SlotObservation {
            tenant_id: Some(t.to_string()),
            exp: Some(now + 3 * 60 * 60),
            present: true,
            unknown: false,
            outcome: Some(TenantSlotOutcome::SkippedFresh),
        };
        let slots = [healthy(y), healthy(z)];

        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(Some(gone), true, 401, br#"{"code":"token_expired"}"#);
        }
        // Before the sweep it is a live orphan and MUST darken: at this point
        // the box has not been re-paired, it is simply unattributable.
        assert_eq!(
            derive_and_publish_posture(&slots, PosturePinInputs::UNPINNED, now).map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected))
        );

        // The re-pair: `gone` is in no slot and is not the default binding, so
        // no bearer can ever be presented for it again — and `machine.json`
        // does not pin it, so nothing spares its bucket.
        let writable: std::collections::HashSet<String> =
            [y.to_string(), z.to_string()].into_iter().collect();
        assert_eq!(
            evict_unwritable_upstream_buckets(&writable),
            vec![gone.to_string()]
        );
        assert_eq!(
            upstream_signal_for(Some(&gone.to_string())).consecutive_rejections,
            0,
            "the bucket is gone, not merely ignored"
        );

        assert_eq!(
            derive_and_publish_posture(&slots, PosturePinInputs::UNPINNED, now).map(|t| t.to),
            Some(CoordCredentialPosture::Live),
            "a re-paired runner with two live slots is live"
        );
        reset_posture();
    }

    /// The eviction predicate's kept keys — the two BEARER routes (a tenant
    /// slot, the default binding), the tenant the SPARE-only machine pin names
    /// — and the one key that is never evicted.
    #[test]
    fn eviction_spares_every_key_a_credential_can_still_reach() {
        let _serialised = health_lock();
        reset_posture();
        let in_a_slot = tenant(0);
        let default_binding = tenant(1);
        let live_pin = tenant(2);
        let orphan = tenant(3);
        for t in [in_a_slot, default_binding, live_pin, orphan] {
            note_coord_upstream_verdict(Some(t), true, 401, br#"{"code":"token_expired"}"#);
        }
        // The DEFAULT key is always writable: an UNPINNED session presents the
        // legacy slot, so that bucket can always be contradicted.
        note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_expired"}"#);

        let writable: std::collections::HashSet<String> = [
            in_a_slot.to_string(),
            default_binding.to_string(),
            live_pin.to_string(),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            evict_unwritable_upstream_buckets(&writable),
            vec![orphan.to_string()],
            "only the key no credential can reach is retired"
        );
        for t in [in_a_slot, default_binding, live_pin] {
            assert_eq!(
                upstream_signal_for(Some(&t.to_string())).consecutive_rejections,
                1,
                "a reachable key keeps its evidence — a 2xx is what retires it"
            );
        }
        assert_eq!(
            upstream_signal_for(None).consecutive_rejections,
            1,
            "the DEFAULT key is never evicted"
        );
        reset_posture();
    }

    /// **THE ROUND-5 BLOCKER**: the sweep's three inputs all fail OPEN, and
    /// eviction is DESTRUCTIVE, so one transient read failure deleted live
    /// evidence and the posture collapsed to `live` with a false recovery.
    ///
    /// Slots A and B, both files naming A, coord refusing B. Pass N:
    /// `load_tokens()` hiccups once → the old sweep saw `writable = {A}` and
    /// REMOVED bucket B. Pass N+1 with the store readable again: both slots
    /// observed, both `exp` future, and no upstream evidence exists anywhere
    /// because the streak was DELETED rather than filtered → `Nothing` → falls
    /// through to `worst` → `live`, and `should_notify_posture(Some(Dark),
    /// Live)` fires *"Coord access restored"*. Nothing healed, and recovery
    /// needs three fresh rejections that a quiet box may take arbitrarily long
    /// to produce.
    #[test]
    fn an_unreadable_slot_store_defers_the_sweep_instead_of_deleting_evidence() {
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let a = tenant(0);
        let b = tenant(1);
        let slot = |t: uuid::Uuid| SlotObservation {
            tenant_id: Some(t.to_string()),
            exp: Some(now + 3 * 60 * 60),
            present: true,
            unknown: false,
            outcome: Some(TenantSlotOutcome::SkippedFresh),
        };
        let slots = [slot(a), slot(b)];
        for _ in 0..5 {
            note_coord_upstream_verdict(Some(b), true, 401, br#"{"code":"token_revoked"}"#);
        }
        assert_eq!(
            derive_and_publish_posture(&slots, PosturePinInputs::UNPINNED, now).map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected)),
            "B is refused, so the box is dark before the hiccup"
        );

        // PASS N — the store cannot be read. The sweep must DEFER, not shrink.
        // The default binding is healthy and names A, so the slot store is the
        // ONLY thing unmeasured.
        sweep_unwritable_upstream_buckets(resolve_writable_slot_keys(
            None,
            crate::auth::BindingTenantRead::Bound(a),
            crate::session::tenant_pin::TenantPin::Pinned(a),
        ));
        assert_eq!(
            upstream_signal_for(Some(&b.to_string())).consecutive_rejections,
            5,
            "an unreadable store is UNKNOWN, and UNKNOWN may not delete evidence"
        );

        // PASS N+1 — store readable again, nothing else changed.
        let transitions_before = recorded_posture_transitions().len();
        assert_eq!(
            derive_and_publish_posture(&slots, PosturePinInputs::UNPINNED, now + 5 * 60),
            None,
            "nothing healed between the two passes, so nothing may transition"
        );
        let published = coord_credential_posture().expect("published");
        assert_eq!(
            published.posture,
            CoordCredentialPosture::Dark(DarkCause::UpstreamRejected)
        );
        assert!(!published.posture.can_answer());
        assert_eq!(
            published.tenant_id.as_deref(),
            Some(b.to_string().as_str()),
            "and it still names B, the slot an operator has to fix"
        );
        assert_eq!(
            recorded_posture_transitions().len(),
            transitions_before,
            "no `Coord access restored` may be emitted by a failed READ"
        );
        reset_posture();
    }

    /// Each of the two ABORTING inputs, failing on its own and both at once —
    /// and the spare-only pin, which neither rescues an unmeasured set nor
    /// makes a measured one UNKNOWN. This is the composition where the
    /// fail-open actually lived — the pure eviction half was already covered
    /// with an injected `writable`, which is exactly why the defect passed 9616
    /// green.
    #[test]
    fn the_sweep_aborts_on_any_unmeasured_input_and_only_evicts_when_fully_measured() {
        use crate::auth::BindingTenantRead;
        use crate::session::tenant_pin::TenantPin;
        let a = tenant(0);
        let slots = [a];

        for pin in [
            TenantPin::Pinned(a),
            TenantPin::Unpinned,
            TenantPin::Unresolvable,
        ] {
            // (1) Slot store unreadable. Even a pin naming `a` cannot rescue it.
            assert!(matches!(
                resolve_writable_slot_keys(None, BindingTenantRead::Bound(a), pin),
                WritableSlotKeys::Unknown(_)
            ));
            // (2) paired_user.json unreadable or malformed.
            assert!(matches!(
                resolve_writable_slot_keys(Some(&slots), BindingTenantRead::Unknown, pin),
                WritableSlotKeys::Unknown(_)
            ));
            // Both at once — the pass on which EVERY tenant bucket used to go.
            assert!(matches!(
                resolve_writable_slot_keys(None, BindingTenantRead::Unknown, pin),
                WritableSlotKeys::Unknown(_)
            ));
        }

        // MEASURED absence is not a gap: an unpaired device is a fact, and so is
        // a machine that pins nothing or cannot state its pin — the pin never
        // aborts, it can only spare.
        let b = tenant(1);
        let c = tenant(2);
        for pin in [TenantPin::Unpinned, TenantPin::Unresolvable] {
            match resolve_writable_slot_keys(Some(&slots), BindingTenantRead::Unbound, pin) {
                WritableSlotKeys::Measured(w) => {
                    assert_eq!(w, [a.to_string()].into_iter().collect());
                }
                other => panic!("measured inputs must yield a set, got {other:?}"),
            }
        }
        // …and every kept route lands in the set: a slot, the default binding,
        // and the tenant the pin names.
        match resolve_writable_slot_keys(
            Some(&slots),
            BindingTenantRead::Bound(b),
            TenantPin::Pinned(c),
        ) {
            WritableSlotKeys::Measured(w) => {
                assert_eq!(
                    w,
                    [a.to_string(), b.to_string(), c.to_string()]
                        .into_iter()
                        .collect()
                );
            }
            other => panic!("expected a measured set, got {other:?}"),
        }
    }

    /// Re-review W-1. `machine.json` pins T; coord revokes T; the slot pass
    /// clears T's slot and the re-derive fails. T is now neither a slot nor the
    /// default binding, so by the bearer set alone its streak would be evicted,
    /// nothing would observe T, and the posture could publish `live` while
    /// every T-pinned session is refused. The SPARE-only pin keeps the bucket,
    /// and the unclaimed-bucket rung keeps the box dark.
    #[test]
    fn a_pinned_tenant_with_no_slot_keeps_its_dark_streak() {
        use crate::session::tenant_pin::TenantPin;
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let pinned = tenant(3);
        let other_slot = tenant(4);
        let slots = [SlotObservation {
            tenant_id: Some(other_slot.to_string()),
            exp: Some(now + 3 * 60 * 60),
            present: true,
            unknown: false,
            outcome: Some(TenantSlotOutcome::SkippedFresh),
        }];
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(Some(pinned), true, 401, br#"{"code":"token_revoked"}"#);
        }

        let writable = resolve_writable_slot_keys(
            Some(&[other_slot]),
            crate::auth::BindingTenantRead::Unbound,
            TenantPin::Pinned(pinned),
        );
        sweep_unwritable_upstream_buckets(writable);
        assert_eq!(
            upstream_signal_for(Some(&pinned.to_string())).consecutive_rejections,
            UPSTREAM_DARK_THRESHOLD,
            "the pinned tenant's streak is SPARED even though it has no slot"
        );
        assert_eq!(
            derive_and_publish_posture(&slots, PosturePinInputs::UNPINNED, now).map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected)),
            "and the box stays dark — never `Coord access restored` over a refused pin"
        );
        assert_eq!(
            coord_credential_posture().expect("published").tenant_id,
            None,
            "without the pin as an input, only the unclaimed-bucket rung sees it — nameless"
        );

        // Plan 2026-09-14 Phase 1: with the pin as an input, the SAME pass is
        // still dark — the unserved-pin arm honours the streak rather than
        // downgrading it to `absent` — and now names the tenant to re-pair.
        let pins = PosturePinInputs {
            machine_pin: TenantPin::Pinned(pinned),
            default_binding: crate::auth::BindingTenantRead::Unbound,
        };
        assert_eq!(
            derive_and_publish_posture(&slots, pins, now),
            None,
            "still dark, so no transition — and never a recovery"
        );
        let published = coord_credential_posture().expect("published");
        assert_eq!(
            published.posture,
            CoordCredentialPosture::Dark(DarkCause::UpstreamRejected)
        );
        assert_eq!(
            published.tenant_id.as_deref(),
            Some(pinned.to_string().as_str()),
            "the banner names T, the pin every refused session asks for"
        );
        assert_eq!(published.exp, None, "no slot, so no exp to report");
        assert!(
            published.last_401_at.is_some(),
            "the published timestamps are T's own bucket's"
        );
        reset_posture();
    }

    /// A live slot for tenant `Y` — a sibling slot, not the pin — beside the
    /// pinned-but-unserved tenant `T`.
    fn live_slot(t: uuid::Uuid, now: i64) -> SlotObservation {
        SlotObservation {
            tenant_id: Some(t.to_string()),
            exp: Some(now + 3 * 60 * 60),
            present: true,
            unknown: false,
            outcome: Some(TenantSlotOutcome::SkippedFresh),
        }
    }

    /// Plan 2026-09-14 Phase 1 (finding `7abcb51a` M-1) — THE false recovery.
    /// `machine.json` pins T; T's slot was cleared on a LOCAL expiry (or a
    /// refresh-token 401), neither of which records a bucket verdict, and the
    /// re-derive minted for the default tenant instead. So T has NO slot and NO
    /// streak, the unclaimed-bucket rung has nothing to read, and Y is live.
    /// The posture used to publish `live` with *"Coord access restored"* while
    /// every T-pinned session got `COORD_MCP_PROXY_CREDENTIAL_REFRESHING`.
    #[test]
    fn a_pinned_tenant_cleared_before_any_streak_publishes_absent_under_its_own_name() {
        use crate::session::tenant_pin::TenantPin;
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let pinned = tenant(3);
        let other_slot = tenant(4);
        assert_eq!(
            upstream_signal_for(Some(&pinned.to_string())).consecutive_rejections,
            0,
            "precondition: T has no streak at all"
        );
        let pins = PosturePinInputs {
            machine_pin: TenantPin::Pinned(pinned),
            default_binding: crate::auth::BindingTenantRead::Bound(other_slot),
        };

        let transition = derive_and_publish_posture(&[live_slot(other_slot, now)], pins, now);
        assert_eq!(
            transition.map(|t| t.to),
            Some(CoordCredentialPosture::Absent),
            "a pin no slot serves is absent — never `live` off the sibling slot"
        );
        let published = coord_credential_posture().expect("published");
        assert!(!published.posture.can_answer());
        assert_eq!(
            published.tenant_id.as_deref(),
            Some(pinned.to_string().as_str()),
            "and it names T, not the healthy Y"
        );
        assert_eq!(published.exp, None);
        assert_eq!(published.last_refresh_outcome, None);
        reset_posture();
    }

    /// Plan 2026-09-14 Phase 1: a pin the DEFAULT binding serves is not
    /// unserved — the legacy `access_token` slot answers for it (the
    /// legacy-only install), so no tenant observation naming it proves
    /// nothing. The posture is exactly what the observations alone derive.
    #[test]
    fn a_pin_served_by_the_default_binding_synthesizes_nothing() {
        use crate::session::tenant_pin::TenantPin;
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let pinned = tenant(3);
        let other_slot = tenant(4);
        let slots = [live_slot(other_slot, now)];
        let today = derive_coord_credential_posture(&slots[0], UpstreamSignal::default(), now);
        assert!(today.can_answer(), "precondition: Y alone answers");

        let pins = PosturePinInputs {
            machine_pin: TenantPin::Pinned(pinned),
            default_binding: crate::auth::BindingTenantRead::Bound(pinned),
        };
        assert_eq!(
            derive_and_publish_posture(&slots, pins, now).map(|t| t.to),
            Some(today)
        );
        assert_eq!(
            coord_credential_posture()
                .expect("published")
                .tenant_id
                .as_deref(),
            Some(other_slot.to_string().as_str())
        );
        reset_posture();
    }

    /// Plan 2026-09-14 Phase 1: every UNMEASURED pin input abstains — an
    /// unresolvable `machine.json`, no pin at all, an unreadable default
    /// binding, and a pinned tenant whose slot this pass could not read (it
    /// may hold a working credential). None of them is evidence the pin is
    /// unserved, so each publishes exactly what the observations derive.
    #[test]
    fn an_unresolvable_pin_or_an_unmeasured_input_synthesizes_nothing() {
        use crate::auth::BindingTenantRead;
        use crate::session::tenant_pin::TenantPin;
        let _serialised = health_lock();
        let now = chrono::Utc::now().timestamp();
        let pinned = tenant(3);
        let other_slot = tenant(4);
        let live = live_slot(other_slot, now);
        let today = derive_coord_credential_posture(&live, UpstreamSignal::default(), now);
        let unreadable_pin_slot = SlotObservation::unreadable(Some(pinned.to_string()));

        for (label, machine_pin, default_binding, slots) in [
            (
                "unresolvable pin",
                TenantPin::Unresolvable,
                BindingTenantRead::Unbound,
                vec![live.clone()],
            ),
            (
                "no pin",
                TenantPin::Unpinned,
                BindingTenantRead::Unbound,
                vec![live.clone()],
            ),
            (
                "unreadable binding",
                TenantPin::Pinned(pinned),
                BindingTenantRead::Unknown,
                vec![live.clone()],
            ),
            (
                "the pinned slot is unreadable",
                TenantPin::Pinned(pinned),
                BindingTenantRead::Unbound,
                vec![live.clone(), unreadable_pin_slot.clone()],
            ),
        ] {
            reset_posture();
            let pins = PosturePinInputs {
                machine_pin,
                default_binding,
            };
            assert_eq!(
                derive_and_publish_posture(&slots, pins, now).map(|t| t.to),
                Some(today),
                "{label}: no synthesis"
            );
            assert_eq!(
                coord_credential_posture()
                    .expect("published")
                    .tenant_id
                    .as_deref(),
                Some(other_slot.to_string().as_str()),
                "{label}: the published tenant is the observed one"
            );
        }
        reset_posture();
    }

    /// Plan 2026-09-14 Phase 1, step 4: `fold_default` counts the REAL
    /// observations only. One real slot Y folds the default-slot bucket onto
    /// itself; were the unserved pin counted, Y would stop folding it, read
    /// `live`, and the pin's `absent` would hide the dark default credential.
    #[test]
    fn the_unserved_pin_does_not_change_which_credential_the_default_bucket_folds_onto() {
        use crate::session::tenant_pin::TenantPin;
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let pinned = tenant(3);
        let other_slot = tenant(4);
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_revoked"}"#);
        }
        let pins = PosturePinInputs {
            machine_pin: TenantPin::Pinned(pinned),
            default_binding: crate::auth::BindingTenantRead::Unbound,
        };
        assert_eq!(
            derive_and_publish_posture(&[live_slot(other_slot, now)], pins, now).map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected)),
            "Y still folds the default bucket, so Y is dark and outranks the pin's absent"
        );
        assert_eq!(
            coord_credential_posture()
                .expect("published")
                .tenant_id
                .as_deref(),
            Some(other_slot.to_string().as_str())
        );
        reset_posture();
    }

    /// The legacy arm's observation: the `access_token` slot, holding a
    /// credential with a future `exp` — what `SlotObservation::observed(None,
    /// Some(jwt))` yields for a live token.
    fn live_legacy_observation(now: i64) -> SlotObservation {
        SlotObservation {
            tenant_id: None,
            exp: Some(now + 3 * 60 * 60),
            present: true,
            unknown: false,
            outcome: None,
        }
    }

    /// Review round 1, B-1. An UNREADABLE slot store collapses to the legacy
    /// arm, whose one observation (`tenant_id: None`) can never name the pin.
    /// Feeding the real pin through published `absent(T)` on a multi-tenant box
    /// pinned to T and bound to B — off one failed read — and the next good
    /// read fired "Coord access restored". The pins built from those inputs
    /// abstain, so the posture is what the observation alone derives.
    #[test]
    fn an_unreadable_slot_store_synthesizes_no_pin_posture_on_the_legacy_arm() {
        use crate::session::tenant_pin::TenantPin;
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let pinned = tenant(3);
        let bound = tenant(4);
        let inputs = SweepInputs {
            tenant_slots: Err(anyhow::anyhow!("undecryptable store")),
            default_binding: crate::auth::BindingTenantRead::Bound(bound),
            machine_pin: TenantPin::Pinned(pinned),
            coord_bound_tenants: unread_sidecar(),
        };
        let pins = inputs.posture_pin_inputs();
        assert_eq!(
            pins,
            PosturePinInputs::UNPINNED,
            "an unreadable input abstains"
        );

        let legacy = live_legacy_observation(now);
        let without_a_pin =
            derive_coord_credential_posture(&legacy, UpstreamSignal::default(), now);
        assert!(
            without_a_pin.can_answer(),
            "precondition: the legacy slot answers"
        );
        assert_eq!(
            derive_and_publish_posture(&[legacy], pins, now).map(|t| t.to),
            Some(without_a_pin),
            "no `absent(T)` off a store this pass could not read"
        );
        assert_eq!(
            coord_credential_posture().expect("published").tenant_id,
            None,
            "the published posture is the legacy observation's, not the pin's"
        );
        reset_posture();
    }

    /// Review round 1, B-1 — the other half. A READABLE but EMPTY store
    /// (`Ok(vec![])`, a legacy-only install) is a MEASURED absence of slots, so
    /// a pin the default binding does not serve is still published `absent`
    /// under its own name.
    #[test]
    fn an_empty_readable_slot_store_still_synthesizes_the_unserved_pin() {
        use crate::session::tenant_pin::TenantPin;
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let pinned = tenant(3);
        let bound = tenant(4);
        let inputs = SweepInputs {
            tenant_slots: Ok(vec![]),
            default_binding: crate::auth::BindingTenantRead::Bound(bound),
            machine_pin: TenantPin::Pinned(pinned),
            coord_bound_tenants: unread_sidecar(),
        };
        assert_eq!(
            derive_and_publish_posture(
                &[live_legacy_observation(now)],
                inputs.posture_pin_inputs(),
                now
            )
            .map(|t| t.to),
            Some(CoordCredentialPosture::Absent)
        );
        assert_eq!(
            coord_credential_posture()
                .expect("published")
                .tenant_id
                .as_deref(),
            Some(pinned.to_string().as_str())
        );
        reset_posture();
    }

    /// Review round 1, S-1. T pinned and unserved with no streak publishes
    /// `absent(T)` — the actionable verdict, and the documented precedence is
    /// unchanged. A DIFFERENT unclaimed key O that coord is refusing right now
    /// used to be skipped silently behind it; it must still reach the log.
    #[test]
    fn an_absent_pin_still_logs_a_different_fresh_orphan() {
        use crate::session::tenant_pin::TenantPin;
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let pinned = tenant(3);
        let other_slot = tenant(4);
        let orphan = tenant(5);
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(Some(orphan), true, 401, br#"{"code":"token_revoked"}"#);
        }
        let pins = PosturePinInputs {
            machine_pin: TenantPin::Pinned(pinned),
            default_binding: crate::auth::BindingTenantRead::Bound(other_slot),
        };
        assert_eq!(
            derive_and_publish_posture(&[live_slot(other_slot, now)], pins, now).map(|t| t.to),
            Some(CoordCredentialPosture::Absent),
            "the pin's actionable absent still wins the publish"
        );
        assert_eq!(
            coord_credential_posture()
                .expect("published")
                .tenant_id
                .as_deref(),
            Some(pinned.to_string().as_str())
        );
        assert!(
            orphan_warning_latched(&orphan.to_string()),
            "the hidden orphan's warning fired"
        );
        assert!(!orphan_warning_latched(&pinned.to_string()));
        reset_posture();
    }

    /// Review round 1, S-1 (i). T's own fresh streak is read by the
    /// unserved-pin arm under T's name, so the unclaimed diff counts T as
    /// CLAIMED: the posture is `dark` naming T, and T is never also warned
    /// about as a nameless orphan.
    #[test]
    fn a_dark_pin_is_not_also_reported_as_an_orphan() {
        use crate::session::tenant_pin::TenantPin;
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let pinned = tenant(3);
        let other_slot = tenant(4);
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(Some(pinned), true, 401, br#"{"code":"token_revoked"}"#);
        }
        let pins = PosturePinInputs {
            machine_pin: TenantPin::Pinned(pinned),
            default_binding: crate::auth::BindingTenantRead::Bound(other_slot),
        };
        assert_eq!(
            derive_and_publish_posture(&[live_slot(other_slot, now)], pins, now).map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected))
        );
        assert_eq!(
            coord_credential_posture()
                .expect("published")
                .tenant_id
                .as_deref(),
            Some(pinned.to_string().as_str())
        );
        assert!(
            !orphan_warning_latched(&pinned.to_string()),
            "T's streak is the pin's verdict, not an orphan"
        );
        reset_posture();
    }

    /// Review round 1, S-2. The unserved-pin arm's `dark` needs BOTH a streak
    /// at or over the threshold AND a fresh last rejection — the same clock the
    /// unclaimed-bucket rung reads. Each boundary is pinned here: a stale streak
    /// at the threshold is `absent`, a fresh streak one short is `absent`, and a
    /// fresh streak exactly at the threshold is `dark` naming T.
    #[test]
    fn the_unserved_pin_is_dark_only_on_a_fresh_streak_at_the_threshold() {
        use crate::session::tenant_pin::TenantPin;
        let _serialised = health_lock();
        let pinned = tenant(3);
        let other_slot = tenant(4);
        let pins = PosturePinInputs {
            machine_pin: TenantPin::Pinned(pinned),
            default_binding: crate::auth::BindingTenantRead::Bound(other_slot),
        };
        for (label, streak, stale, expected) in [
            (
                "stale streak at the threshold",
                UPSTREAM_DARK_THRESHOLD,
                true,
                CoordCredentialPosture::Absent,
            ),
            (
                "fresh streak one short of the threshold",
                UPSTREAM_DARK_THRESHOLD - 1,
                false,
                CoordCredentialPosture::Absent,
            ),
            (
                "fresh streak exactly at the threshold",
                UPSTREAM_DARK_THRESHOLD,
                false,
                CoordCredentialPosture::Dark(DarkCause::UpstreamRejected),
            ),
        ] {
            reset_posture();
            for _ in 0..streak {
                note_coord_upstream_verdict(
                    Some(pinned),
                    true,
                    401,
                    br#"{"code":"token_revoked"}"#,
                );
            }
            // Read the clock AFTER the rejections are stamped: every stamp is
            // then <= `recorded`, so the stale case is strictly past the bound
            // (and the fresh cases never read a negative age) however the
            // second boundary falls.
            let recorded = chrono::Utc::now().timestamp();
            let eval_now = if stale {
                recorded + UPSTREAM_ORPHAN_STALE_AFTER_SECS + 1
            } else {
                recorded
            };
            assert_eq!(
                derive_and_publish_posture(&[live_slot(other_slot, eval_now)], pins, eval_now)
                    .map(|t| t.to),
                Some(expected),
                "{label}"
            );
            assert_eq!(
                coord_credential_posture()
                    .expect("published")
                    .tenant_id
                    .as_deref(),
                Some(pinned.to_string().as_str()),
                "{label}: named T"
            );
        }
        reset_posture();
    }

    /// Re-review L-1: a read failure that repeats every pass is announced on
    /// the first failure and then at a bounded cadence — never every pass, and
    /// never once-then-silent.
    #[test]
    fn a_repeating_sweep_read_failure_is_re_announced_at_a_bounded_cadence() {
        assert!(!should_warn_sweep_join_failure(0));
        assert!(should_warn_sweep_join_failure(1), "the first failure warns");
        for n in 2..SWEEP_JOIN_FAILURE_REWARN_EVERY {
            assert!(!should_warn_sweep_join_failure(n), "pass {n} is silent");
        }
        assert!(should_warn_sweep_join_failure(
            SWEEP_JOIN_FAILURE_REWARN_EVERY
        ));
        assert!(!should_warn_sweep_join_failure(
            SWEEP_JOIN_FAILURE_REWARN_EVERY + 1
        ));
        assert!(should_warn_sweep_join_failure(
            3 * SWEEP_JOIN_FAILURE_REWARN_EVERY
        ));
    }

    /// Re-review W-3. Deleting the post-pairing streak retirement from either
    /// in-process re-pair path kept every behavioural test green, because a
    /// Tauri command that runs a browser login or a network redeem cannot be
    /// driven from a unit test. So this pins the wiring at the source — the
    /// same technique `mcp_api.rs`'s
    /// `the_coord_mcp_proxy_reports_its_upstream_verdict_to_the_posture` uses —
    /// and requires the call to come AFTER `persist_pairing`.
    #[test]
    fn every_in_process_re_pair_path_retires_the_stale_rejection_streak() {
        for (file, item) in [
            ("src/commands/auth.rs", "async fn finalize_signed_in("),
            (
                "src/commands/web_integration.rs",
                "pub async fn redeem_pair_code(",
            ),
        ] {
            // From CARGO_MANIFEST_DIR, never the CWD: a test binary can be run
            // from anywhere.
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
            let text =
                std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {file}: {e}"));
            let start = text
                .find(item)
                .unwrap_or_else(|| panic!("{item} exists in {file}"));
            // A top-level fn body ends at the first unindented closing brace.
            let end = text[start..]
                .find("\n}\n")
                .map(|i| start + i)
                .unwrap_or(text.len());
            // Line comments are cut before either needle is searched: each
            // line is truncated at its first `//` (which also covers `///` and
            // whole-line comments), so a `// retire_rejection_streaks_after_pairing(`
            // left behind after deleting the call — on its own line OR trailing
            // real code — cannot satisfy the scan. The body is sliced on the
            // RAW text above. Accepted limit of a source scan: it still cannot
            // see through string literals (a `"//"` truncates a line early, a
            // needle inside a string still matches) or `/* */` block comments.
            let body: String = text[start..end]
                .lines()
                .map(|line| line.find("//").map_or(line, |i| &line[..i]))
                .collect::<Vec<_>>()
                .join("\n");
            let persisted = body
                .find("persist_pairing(")
                .unwrap_or_else(|| panic!("{item} persists a pairing"));
            let retired = body
                .find("retire_rejection_streaks_after_pairing(")
                .unwrap_or_else(|| {
                    panic!(
                        "{item} in {file} must retire the old credential's rejection \
                         streak after persisting a pairing, or a re-pair on \
                         `dark(upstream_401)` is republished dark by the next pass"
                    )
                });
            assert!(
                retired > persisted,
                "{item}: the retirement must come AFTER persist_pairing succeeds"
            );
        }
    }

    /// Plan `2026-09-13-coord-credential-posture-residuals` Phase 1.
    ///
    /// A box with NO `machine.json` (pin `Unresolvable`) and a stale orphan
    /// bucket for a tenant that is neither a slot nor the default binding. The
    /// pin used to be an ABORTING sweep input, so this box aborted on every pass
    /// and the orphan pinned the posture dark forever. The writable set is now
    /// the BEARER set (slots ∪ default) plus a spare-only pin, which no
    /// machine-pin state can make UNKNOWN: the orphan is evicted, and the live
    /// slot tenant's bucket, the default binding's bucket and the `""` bucket
    /// are all spared.
    #[test]
    fn a_box_with_no_machine_json_evicts_an_orphan_and_spares_every_live_bucket() {
        let _serialised = health_lock();
        reset_posture();
        let live_slot = tenant(3);
        let default_binding = tenant(4);
        let orphan = tenant(5);
        for t in [Some(live_slot), Some(default_binding), Some(orphan), None] {
            for _ in 0..UPSTREAM_DARK_THRESHOLD {
                note_coord_upstream_verdict(t, true, 401, br#"{"code":"token_revoked"}"#);
            }
        }

        let writable = resolve_writable_slot_keys(
            Some(&[live_slot]),
            crate::auth::BindingTenantRead::Bound(default_binding),
            crate::session::tenant_pin::TenantPin::Unresolvable,
        );
        assert!(
            matches!(writable, WritableSlotKeys::Measured(_)),
            "an unresolvable pin must not abort the sweep: {writable:?}"
        );
        sweep_unwritable_upstream_buckets(writable);

        assert_eq!(
            upstream_signal_for(Some(&orphan.to_string())).consecutive_rejections,
            0,
            "the orphan is retired — no bearer can ever be presented for it again"
        );
        for t in [live_slot, default_binding] {
            assert_eq!(
                upstream_signal_for(Some(&t.to_string())).consecutive_rejections,
                UPSTREAM_DARK_THRESHOLD,
                "a tenant that still has a bearer keeps its evidence"
            );
        }
        assert_eq!(
            upstream_signal_for(None).consecutive_rejections,
            UPSTREAM_DARK_THRESHOLD,
            "the DEFAULT key is never evicted"
        );
        reset_posture();
    }

    /// Phase 2: the pure composition over already-read inputs keeps the slot
    /// store's `Err` as UNKNOWN for the sweep.
    #[test]
    fn composed_sweep_inputs_keep_an_unreadable_slot_store_unknown() {
        let a = tenant(0);
        let unreadable = SweepInputs {
            tenant_slots: Err(anyhow::anyhow!("undecryptable store")),
            default_binding: crate::auth::BindingTenantRead::Bound(a),
            machine_pin: crate::session::tenant_pin::TenantPin::Pinned(a),
            coord_bound_tenants: unread_sidecar(),
        };
        assert!(matches!(
            writable_slot_keys_from(&unreadable),
            WritableSlotKeys::Unknown(_)
        ));
        let readable = SweepInputs {
            tenant_slots: Ok(vec![a]),
            default_binding: crate::auth::BindingTenantRead::Unbound,
            machine_pin: crate::session::tenant_pin::TenantPin::Unresolvable,
            coord_bound_tenants: unread_sidecar(),
        };
        assert_eq!(
            writable_slot_keys_from(&readable),
            WritableSlotKeys::Measured([a.to_string()].into_iter().collect())
        );
    }

    /// Phase 2 WIRING, through the production reads: [`read_sweep_inputs`]
    /// against a real secure-storage directory holding one tenant slot and a
    /// real `paired_user.json` naming a different default — first with NO
    /// `machine.json` at all, then with one that pins a third tenant. Rewiring
    /// the read onto another store, collapsing it, dropping the pin read, or
    /// making a missing `machine.json` abort fails here, where the pure tests
    /// above cannot notice.
    #[test]
    fn read_sweep_inputs_measures_the_bearer_set_from_disk_without_machine_json() {
        use crate::session::tenant_pin::TenantPin;
        let slot = tenant(1);
        let default_binding = tenant(2);
        let pinned = tenant(3);
        // The ambient fixture is released before anything else is locked, so
        // this test never holds the env lock and the posture lock together.
        let (unpinned_inputs, pinned_inputs) = {
            let amb = crate::test_env::isolated_ambient();
            assert!(
                !amb.machine_json_path().exists(),
                "the fixture must start with no machine.json"
            );
            std::fs::write(
                amb.dir().join("paired_user.json"),
                format!(r#"{{"default_tenant_id":"{default_binding}"}}"#),
            )
            .expect("write paired_user.json");
            let am = crate::auth::AuthManager::new();
            am.store_tenant_device_jwt(&slot, "jwt.for.slot")
                .expect("store a tenant slot");
            let without_machine_json = read_sweep_inputs(&am);
            amb.write_active_tenant_id(pinned);
            (without_machine_json, read_sweep_inputs(&am))
        };

        assert_eq!(
            unpinned_inputs.default_binding,
            crate::auth::BindingTenantRead::Bound(default_binding)
        );
        assert_eq!(
            unpinned_inputs.tenant_slots.as_ref().ok().cloned(),
            Some(vec![slot])
        );
        assert_eq!(unpinned_inputs.machine_pin, TenantPin::Unresolvable);
        assert_eq!(
            writable_slot_keys_from(&unpinned_inputs),
            WritableSlotKeys::Measured(
                [slot.to_string(), default_binding.to_string()]
                    .into_iter()
                    .collect()
            ),
            "a missing machine.json neither aborts nor adds a key"
        );

        assert_eq!(pinned_inputs.machine_pin, TenantPin::Pinned(pinned));
        assert_eq!(
            writable_slot_keys_from(&pinned_inputs),
            WritableSlotKeys::Measured(
                [
                    slot.to_string(),
                    default_binding.to_string(),
                    pinned.to_string()
                ]
                .into_iter()
                .collect()
            ),
            "the pinned tenant is spared"
        );
    }

    /// Review W1. "Sign in to re-pair" on `dark(upstream_401)` persists a fresh
    /// pairing, but the streak recorded against the OLD credential survived
    /// it: the next pass observed a fresh slot, rung 5 re-read the stale streak
    /// and republished `dark`. The post-pairing retirement is what lets the
    /// posture heal.
    #[test]
    fn a_fresh_pairing_retires_the_stale_rejection_streak_so_the_posture_heals() {
        let _serialised = health_lock();
        reset_posture();
        let now = chrono::Utc::now().timestamp();
        let t = tenant(2);
        let slots = [SlotObservation {
            tenant_id: Some(t.to_string()),
            exp: Some(now + 3 * 60 * 60),
            present: true,
            unknown: false,
            outcome: Some(TenantSlotOutcome::SkippedFresh),
        }];
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(Some(t), true, 401, br#"{"code":"token_revoked"}"#);
            note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_revoked"}"#);
        }
        assert_eq!(
            derive_and_publish_posture(&slots, PosturePinInputs::UNPINNED, now).map(|x| x.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected)),
            "coord refused the old credential, so the box is dark before the sign-in"
        );

        // The sign-in persisted t's slot, and t is the default (legacy slot too).
        retire_rejection_streaks_after_pairing(t, Some(t));

        derive_and_publish_posture(&slots, PosturePinInputs::UNPINNED, now + 60);
        let published = coord_credential_posture().expect("published");
        assert!(
            published.posture.can_answer(),
            "a freshly paired credential must not be held dark by the old \
             credential's streak, got {:?}",
            published.posture
        );
        reset_posture();
    }

    /// The DEFAULT bucket is spent only when the pairing's credential also
    /// landed in the legacy slot — i.e. the paired tenant is the default. A
    /// pairing for a non-default tenant leaves the legacy slot holding the
    /// other default's credential, whose evidence is still live.
    #[test]
    fn a_fresh_pairing_retires_the_default_bucket_only_when_it_is_the_default() {
        let _serialised = health_lock();
        reset_posture();
        let paired = tenant(2);
        let other_default = tenant(3);
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(Some(paired), true, 401, br#"{"code":"token_revoked"}"#);
            note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_revoked"}"#);
        }

        retire_rejection_streaks_after_pairing(paired, Some(other_default));
        assert_eq!(
            upstream_signal_for(Some(&paired.to_string())).consecutive_rejections,
            0
        );
        assert_eq!(
            upstream_signal_for(None).consecutive_rejections,
            UPSTREAM_DARK_THRESHOLD,
            "the legacy slot still holds the OTHER default's credential"
        );

        retire_rejection_streaks_after_pairing(paired, None);
        assert_eq!(
            upstream_signal_for(None).consecutive_rejections,
            UPSTREAM_DARK_THRESHOLD,
            "an unreadable default is not proof the legacy slot was replaced"
        );

        retire_rejection_streaks_after_pairing(paired, Some(paired));
        assert_eq!(upstream_signal_for(None).consecutive_rejections, 0);
        reset_posture();
    }

    /// The `paired_user.json` read, against a REAL tempdir: an absent file is
    /// a measured `Unbound`, while an unreadable or malformed one is `Unknown`
    /// — the distinction the sweep's fail-closed arm is built on, and the one
    /// `.ok()?` erased.
    #[test]
    fn the_binding_read_separates_a_missing_file_from_an_unreadable_one() {
        use crate::auth::{default_binding_tenant_in, BindingTenantRead};
        let dir =
            std::env::temp_dir().join(format!("qontinui_binding_probe_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tempdir");
        let path = dir.join("paired_user.json");

        // MEASURED absence: an unpaired device. Safe to sweep on.
        assert_eq!(default_binding_tenant_in(&dir), BindingTenantRead::Unbound);

        let t = tenant(0);
        std::fs::write(&path, format!(r#"{{"default_tenant_id":"{t}"}}"#)).expect("write");
        assert_eq!(
            default_binding_tenant_in(&dir),
            BindingTenantRead::Bound(t),
            "the v2 key"
        );
        std::fs::write(&path, format!(r#"{{"tenant_id":"{t}"}}"#)).expect("write");
        assert_eq!(
            default_binding_tenant_in(&dir),
            BindingTenantRead::Bound(t),
            "the legacy key"
        );

        // Well-formed and names nobody — still MEASURED.
        std::fs::write(&path, r#"{"user":"someone"}"#).expect("write");
        assert_eq!(default_binding_tenant_in(&dir), BindingTenantRead::Unbound);

        // Present but unreadable: a partial write / corrupt file. NOT absence.
        std::fs::write(&path, b"{ this is not json").expect("write");
        assert_eq!(default_binding_tenant_in(&dir), BindingTenantRead::Unknown);
        // Names SOMETHING we cannot read — also not absence.
        std::fs::write(&path, r#"{"default_tenant_id":"not-a-uuid"}"#).expect("write");
        assert_eq!(default_binding_tenant_in(&dir), BindingTenantRead::Unknown);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The backstop, and the direction it fails in.
    ///
    /// A bucket that survived BOTH honest retirements — no 2xx, still writable
    /// — and has been silent for longer than
    /// [`UPSTREAM_ORPHAN_STALE_AFTER_SECS`] is not a verdict. It is also not a
    /// clean bill, so the pass ABSTAINS: it publishes nothing and the previous
    /// posture stands. Falling through to `worst` here is precisely the
    /// healthy-while-dead defect this round exists to close.
    #[test]
    fn ancient_unclaimed_evidence_makes_the_pass_abstain_never_publish_live() {
        let _serialised = health_lock();
        reset_posture();
        let real_now = chrono::Utc::now().timestamp();
        let healthy = |t: uuid::Uuid| SlotObservation {
            tenant_id: Some(t.to_string()),
            exp: Some(real_now + 30 * 24 * 60 * 60),
            present: true,
            unknown: false,
            outcome: Some(TenantSlotOutcome::SkippedFresh),
        };
        let slots = [healthy(tenant(4)), healthy(tenant(5))];
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_expired"}"#);
        }

        // Fresh: fires, and that is the posture on record.
        assert_eq!(
            derive_and_publish_posture(&slots, PosturePinInputs::UNPINNED, real_now).map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected))
        );

        // A week and a second later, with nothing else changed: ABSTAIN.
        let later = real_now + UPSTREAM_ORPHAN_STALE_AFTER_SECS + 1;
        assert_eq!(
            derive_and_publish_posture(&slots, PosturePinInputs::UNPINNED, later),
            None,
            "ancient evidence is not a verdict — and publishing `live` off it \
             would announce a recovery that never happened"
        );
        assert_eq!(
            coord_credential_posture().expect("published").posture,
            CoordCredentialPosture::Dark(DarkCause::UpstreamRejected),
            "the previous posture stands; stuck-dark is the acceptable direction"
        );

        // The evidence is not forgotten, and a fresh rejection under the same
        // key fires again immediately.
        assert_eq!(
            upstream_signal_for(None).consecutive_rejections,
            UPSTREAM_DARK_THRESHOLD
        );
        reset_coord_credential_posture_for_test();
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_expired"}"#);
        }
        assert_eq!(
            derive_and_publish_posture(
                &slots,
                PosturePinInputs::UNPINNED,
                chrono::Utc::now().timestamp()
            )
            .map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected))
        );
        reset_posture();
    }

    /// The orphan `warn!` is LATCHED. The condition holds across every pass —
    /// once per five minutes, indefinitely — so an unlatched line would be a
    /// log flood for as long as the box stays dark. It re-arms when the
    /// evidence is honestly retired, so a genuinely new episode warns again.
    #[test]
    fn the_orphan_warning_is_latched_per_key_and_re_arms_on_retirement() {
        let _serialised = health_lock();
        reset_posture();
        let x = tenant(0);
        assert!(latch_orphan_warning(&x.to_string()), "first is the warning");
        assert!(
            !latch_orphan_warning(&x.to_string()),
            "every later pass is silent"
        );
        // CONTRADICTION re-arms it.
        note_coord_upstream_verdict(Some(x), true, 200, br#"{"ok":true}"#);
        assert!(
            latch_orphan_warning(&x.to_string()),
            "a new episode after a real success warns again"
        );
        // So does eviction.
        let _ = evict_unwritable_upstream_buckets(&std::collections::HashSet::new());
        assert!(latch_orphan_warning(&x.to_string()));
        // …and so does spending the streak on a heal.
        reset_upstream_rejections_for(Some(x));
        assert!(latch_orphan_warning(&x.to_string()));
        reset_posture();
    }

    /// **C4.** The cross-repo contract qontinui-web consumes. `ok` is DERIVED
    /// from the posture — it used to come from [`Decision`] alone, and
    /// `Decision::Idle => ok()` published `ok: true` in the incident's own
    /// shape (a fresh legacy token beside an expired tenant slot), so the
    /// console rendered a dark machine as healthy and coord's dark scan
    /// (`details #>> '{coord_credential,ok}' = 'false'`) selected nothing.
    #[test]
    fn the_published_bag_derives_ok_from_the_posture_and_dates_since_in_iso8601() {
        let fallback = coord_credential_health(Decision::Idle, None);
        assert!(fallback.ok, "the old authority says healthy…");

        for (posture, expect_ok) in [
            (CoordCredentialPosture::Live, true),
            (CoordCredentialPosture::Expiring, true),
            (CoordCredentialPosture::Expired, false),
            (CoordCredentialPosture::Absent, false),
            (CoordCredentialPosture::Unrefreshable, false),
            (
                CoordCredentialPosture::Dark(DarkCause::UpstreamRejected),
                false,
            ),
        ] {
            let status = CoordCredentialStatus {
                posture,
                tenant_id: Some("11111111-1111-4111-8111-111111111111".into()),
                exp: Some(1_700_000_100),
                last_ok_at: None,
                last_401_at: None,
                last_refresh_outcome: None,
                since: 1_700_000_000,
                observed_at_unix: 1_700_000_050,
                attributable: true,
            };
            let bag = coord_credential_bag(&fallback, Some(&status));
            assert_eq!(
                bag.ok, expect_ok,
                "…but the POSTURE decides: {posture:?} must publish ok={expect_ok}"
            );
            assert_eq!(bag.posture, posture.as_str());
            assert_eq!(bag.exp, Some(1_700_000_100));
            assert_eq!(
                bag.tenant_id.as_deref(),
                Some("11111111-1111-4111-8111-111111111111")
            );
            assert_eq!(bag.reason.is_none(), expect_ok);

            let v = serde_json::to_value(&bag).expect("serializes");
            // The KEY is `posture`, not `state` — a rename that matters
            // because this is the wire qontinui-web reads.
            assert_eq!(v["posture"], posture.as_str());
            assert!(v.get("state").is_none(), "`state` is the /health spelling");
            // `since` is an ISO-8601 STRING. A number silently breaks the
            // consumer.
            assert_eq!(
                v["since"], "2023-11-14T22:13:20Z",
                "since must be ISO-8601, not unix seconds"
            );
            assert!(v["since"].is_string());
            // `ok` stays PRESENT and a bool: coord's dark scan selects on it.
            assert!(v["ok"].is_boolean());
            // `reason` is emitted as null rather than omitted.
            assert!(v.as_object().expect("object").contains_key("reason"));
            // Plan 2026-09-14 Phase 4: the report declares its own staleness
            // bound — three refresher passes, derived from the cadence, under
            // exactly the key qontinui-web reads.
            assert_eq!(bag.stale_after_secs, 3 * REFRESH_CHECK_INTERVAL.as_secs());
            assert_eq!(v["stale_after_secs"], 3 * REFRESH_CHECK_INTERVAL.as_secs());
        }

        // No pass has concluded: UNKNOWN. The posture is not the source and
        // says so, and an absent measurement does not manufacture a fault.
        let unknown = coord_credential_bag(&fallback, None);
        assert_eq!(
            unknown.stale_after_secs,
            3 * REFRESH_CHECK_INTERVAL.as_secs(),
            "an UNKNOWN report goes stale on the same bound"
        );
        assert_eq!(unknown.posture, "unknown");
        assert!(unknown.ok, "UNKNOWN keeps the decision-derived answer");
        let bad = coord_credential_health(Decision::IdleWrongTier, None);
        assert!(!coord_credential_bag(&bad, None).ok);
    }

    /// **M8.** `slot_seen` records the exp the slot held BEFORE the pass acted,
    /// so a HEALED slot used to publish `live` beside an already-past `exp`.
    #[tokio::test]
    async fn a_healed_slot_does_not_publish_the_old_tokens_exp() {
        let _serialised = health_lock();
        reset_posture();
        let mgr = test_auth_manager("posture_healed_exp");
        let ta = tenant(1);
        let now = chrono::Utc::now().timestamp();
        let stale_exp = now + 30 * 60;
        mgr.store_tenant_device_jwt(&ta, &synth_jwt(stale_exp, "stale"))
            .expect("slot a");

        let (base, _cap, _shutdown) = spawn_mock(vec![]);
        let outcomes =
            refresh_tenant_slots(&mgr, &base, "", DID, None, PosturePinInputs::UNPINNED).await;
        assert_eq!(outcomes, vec![(ta, TenantSlotOutcome::Refreshed)]);

        let published = coord_credential_posture().expect("published");
        assert_eq!(published.posture, CoordCredentialPosture::Live);
        assert_ne!(
            published.exp,
            Some(stale_exp),
            "a healed slot must not publish the exp of the credential it replaced"
        );
        reset_posture();
    }

    /// ISO-8601 rendering, in isolation — the one field a wrong type breaks
    /// silently on the other side of the wire.
    #[test]
    fn iso8601_renders_a_z_terminated_second_precision_instant() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    /// A device JWT of the shape coord issues: a `tenant_id` claim beside `exp`.
    /// Unsigned — `jwt_tenant_claim` reads the payload only.
    fn jwt_claiming(tenant: uuid::Uuid) -> String {
        use base64::Engine as _;
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"alg":"none","typ":"JWT"}"#);
        let exp = chrono::Utc::now().timestamp() + 3 * 60 * 60;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!(r#"{{"tenant_id":"{tenant}","exp":{exp}}}"#).as_bytes());
        format!("{header}.{payload}.sig")
    }

    /// Post-merge follow-up to #1532 (review finding on the landed diff). The
    /// refresher's legacy-slot mints persist through `store_tokens`, whose
    /// mirror also writes `device_jwt:<claim>`; spending only the DEFAULT
    /// bucket left the mirrored tenant's streak to re-darken a credential coord
    /// had just minted. Both buckets are spent; a third tenant's evidence is
    /// untouched.
    #[test]
    fn a_legacy_mint_retires_the_default_streak_and_the_mirrored_tenants() {
        let _serialised = health_lock();
        reset_posture();
        let claimed = tenant(2);
        let bystander = tenant(3);
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_revoked"}"#);
            note_coord_upstream_verdict(Some(claimed), true, 401, br#"{"code":"token_revoked"}"#);
            note_coord_upstream_verdict(Some(bystander), true, 401, br#"{"code":"token_revoked"}"#);
        }

        retire_rejection_streaks_after_legacy_mint(&jwt_claiming(claimed));

        assert_eq!(
            upstream_signal_for(None).consecutive_rejections,
            0,
            "default spent"
        );
        assert_eq!(
            upstream_signal_for(Some(&claimed.to_string())).consecutive_rejections,
            0,
            "the tenant the mirror wrote is spent too"
        );
        assert_eq!(
            upstream_signal_for(Some(&bystander.to_string())).consecutive_rejections,
            UPSTREAM_DARK_THRESHOLD,
            "a tenant the mint did not touch keeps its evidence"
        );
        reset_posture();
    }

    /// An opaque legacy bearer (no decodable `tenant_id`) is mirrored nowhere,
    /// so only the default bucket is spent — a tenant bucket must not be
    /// guessed at.
    #[test]
    fn a_legacy_mint_with_no_tenant_claim_retires_only_the_default_streak() {
        let _serialised = health_lock();
        reset_posture();
        let t = tenant(2);
        for _ in 0..UPSTREAM_DARK_THRESHOLD {
            note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_revoked"}"#);
            note_coord_upstream_verdict(Some(t), true, 401, br#"{"code":"token_revoked"}"#);
        }

        retire_rejection_streaks_after_legacy_mint("qontinui_runner_legacy_opaque");

        assert_eq!(upstream_signal_for(None).consecutive_rejections, 0);
        assert_eq!(
            upstream_signal_for(Some(&t.to_string())).consecutive_rejections,
            UPSTREAM_DARK_THRESHOLD,
            "no claim, no mirror, no reset"
        );
        reset_posture();
    }

    /// WIRING, pinned at the source like the re-pair scan above: every
    /// production `store_tokens_expecting(` write in this file (the three
    /// legacy mints) is paired with a `retire_rejection_streaks_after_legacy_mint(`
    /// call, so a fourth mint path cannot be added without spending the
    /// evidence, and deleting a call while keeping the write fails here.
    /// Comment lines are cut first, so a commented-out call does not count.
    ///
    /// Matches `store_tokens_expecting(`, not the bare `store_tokens(`: plan
    /// `2026-09-17-device-jwt-refresh-drops-the-requested-tenant-and-coord-mints-the-home-tenant`
    /// D2 routed all three refresher-owned mints through the tenant-guarded
    /// seam (`AuthManager::store_tokens_expecting`) instead of calling
    /// `store_tokens` directly — a STRICTER invariant than before, not a
    /// weaker one, and this scan must track wherever that seam moves to next.
    #[test]
    fn every_legacy_mint_in_this_file_retires_the_stale_rejection_streaks() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/mcp/device_jwt_refresher.rs");
        let text = std::fs::read_to_string(&path).expect("read this file");
        // Production code only: everything before the first test MODULE.
        let end = text
            .find("\n#[cfg(test)]\nmod ")
            .expect("this file has test modules");
        let production: String = text[..end]
            .lines()
            .map(|line| line.find("//").map_or(line, |i| &line[..i]))
            .collect::<Vec<_>>()
            .join("\n");
        let writes = production.matches(".store_tokens_expecting(").count();
        let retirements = production
            .matches("retire_rejection_streaks_after_legacy_mint(")
            .count()
            // The definition itself is not a call.
            .saturating_sub(1);
        assert_eq!(writes, 3, "the three refresher-owned legacy mints");
        assert_eq!(
            retirements, writes,
            "every legacy-slot write must retire the old credential's streaks"
        );
    }

    // =======================================================================
    // Phase 3 — binding-gap visibility (D2's residual). Plan
    // `2026-09-20-per-tenant-coord-credentials-and-a-workspace-tenant-pin`.
    //
    // The warm-keeping loop itself shipped with Phase 8a of
    // `2026-07-02-session-scoped-multi-tenant-device-binding`. The first test
    // below is therefore a REGRESSION test over behaviour that already exists,
    // not a test of anything this phase added.
    // =======================================================================

    use qontinui_runner_lib::pair::CoordBoundTenantsRead;

    /// A sidecar file seeded at `path`, stamped `observed_at`. Written as raw
    /// JSON on purpose: the heartbeat's writer is lib-private, and pinning the
    /// ON-DISK shape here is what proves the reader parses what the heartbeat
    /// actually lays down (`pair.rs` `CoordBoundTenantsFile`).
    fn seed_sidecar(path: &std::path::Path, ids: &[uuid::Uuid], observed_at: i64) {
        let ids = ids
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(",");
        std::fs::write(
            path,
            format!(r#"{{"tenant_ids":[{ids}],"observed_at":{observed_at}}}"#),
        )
        .expect("seed sidecar");
    }

    /// [`test_auth_manager`] with a device machine key already stored, so the
    /// mint door is genuinely AVAILABLE — "no mint" is then a choice this
    /// phase makes, not an accident of an unconfigured box
    /// (`try_device_machine_key_exchange` returns early with no `dmk_`).
    fn test_auth_manager_with_dmk(name: &str, dmk: &str) -> crate::auth::AuthManager {
        let dir = std::env::temp_dir().join("qontinui_test_tenant_slots");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{name}.enc"));
        let _ = std::fs::remove_file(&path);
        let seed =
            crate::secure_storage::SecureStorage::with_path(path.clone()).expect("seed storage");
        seed.store_device_machine_key(dmk).expect("seed dmk");
        let storage = crate::secure_storage::SecureStorage::with_path(path).expect("storage");
        crate::auth::AuthManager::with_storage(storage)
    }

    fn sidecar_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join("qontinui_test_binding_gaps")
            .join(format!("{name}_{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// GATE 1 — with two slots seeded and the clock pinned, the pass behaves
    /// EXACTLY as it does today: the fresh slot is skipped, the stale one is
    /// refreshed against `POST /devices/:id/refresh-token` presenting ITS OWN
    /// token, each slot ends holding the right value, and the health rows say
    /// so. Nothing in Phase 3 may move any of this.
    #[tokio::test]
    async fn two_slots_refresh_exactly_as_they_do_today() {
        let _serialised = health_lock();
        let mgr = test_auth_manager("p3_two_slots_unchanged");
        let (fresh_t, stale_t) = (tenant(0), tenant(1));
        // The pass reads `Utc::now()` itself, so "pinned" here means both
        // tokens are minted from ONE `now` and the assertions are stated in
        // terms of `plan_tenant_slot`'s thresholds rather than wall time.
        let now = chrono::Utc::now().timestamp();
        let fresh = synth_jwt(now + 3 * 60 * 60, "fresh-slot");
        assert_eq!(
            plan_tenant_slot(Some(now + 3 * 60 * 60), now),
            TenantSlotPlan::SkipFresh,
            "precondition: the clock makes this slot comfortably fresh"
        );
        let stale = synth_jwt(now + 30 * 60, "stale-slot");
        assert_eq!(
            plan_tenant_slot(Some(now + 30 * 60), now),
            TenantSlotPlan::Refresh,
            "precondition: the clock puts this slot inside the refresh window"
        );
        mgr.store_tenant_device_jwt(&fresh_t, &fresh)
            .expect("fresh");
        mgr.store_tenant_device_jwt(&stale_t, &stale)
            .expect("stale");

        let (base, cap, _shutdown) = spawn_mock(vec![]);
        let outcomes =
            refresh_tenant_slots(&mgr, &base, &base, DID, None, PosturePinInputs::UNPINNED).await;

        assert_eq!(
            outcomes,
            vec![
                (fresh_t, TenantSlotOutcome::SkippedFresh),
                (stale_t, TenantSlotOutcome::Refreshed),
            ],
            "the shipped per-slot decision is unchanged"
        );
        assert_eq!(
            cap.bearers_seen.lock().unwrap().clone(),
            vec![stale.clone()],
            "only the stale slot hits coord, and it presents ITS OWN token"
        );
        assert_eq!(
            mgr.get_tenant_device_jwt(&fresh_t).unwrap().as_deref(),
            Some(fresh.as_str()),
            "a fresh slot is left exactly as it was"
        );
        let expected_refreshed = format!("{stale}.refreshed");
        assert_eq!(
            mgr.get_tenant_device_jwt(&stale_t).unwrap().as_deref(),
            Some(expected_refreshed.as_str()),
            "the refreshed slot holds what coord returned for ITS token"
        );
        let health = tenant_slot_health().expect("a pass publishes health");
        assert_eq!(
            health
                .slots
                .iter()
                .map(|s| (s.tenant_id.as_str(), s.outcome.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (fresh_t.to_string().as_str(), "skipped-fresh"),
                (stale_t.to_string().as_str(), "refreshed"),
            ]
        );
        assert_eq!(health.degraded_slots, 0);
        // GATE 5 — no mint in this phase.
        assert!(
            cap.mint_attempts.lock().unwrap().is_empty(),
            "the pass must never knock on the machine-credential mint door"
        );
    }

    /// GATE 2 — a tenant coord says this device is bound to, with NO slot and
    /// not the default binding, is reported as a gap.
    #[test]
    fn a_bound_tenant_with_no_slot_is_a_gap() {
        let (held, default_t, bound_only) = (tenant(0), tenant(1), tenant(2));
        let report = resolve_binding_gaps(
            Some(&[held]),
            crate::auth::BindingTenantRead::Bound(default_t),
            &CoordBoundTenantsRead::Known(vec![held, default_t, bound_only]),
        );
        assert_eq!(
            report,
            BindingGapReport::Gaps(vec![bound_only.to_string()]),
            "the slotless, non-default bound tenant is the gap"
        );
        // The default binding is served by the LEGACY `access_token` slot, so
        // it is not a gap even with no `device_jwt:<t>` entry.
        assert_eq!(
            resolve_binding_gaps(
                Some(&[]),
                crate::auth::BindingTenantRead::Bound(default_t),
                &CoordBoundTenantsRead::Known(vec![default_t]),
            ),
            BindingGapReport::Gaps(vec![]),
            "a MEASURED zero — every bound tenant has a credential"
        );
    }

    /// GATE 3 — an ABSENT sidecar is UNKNOWN. This is the case measured on the
    /// dev box on 2026-09-20, where a naive read would have printed "no gaps"
    /// on a box with a known gap.
    #[test]
    fn an_absent_sidecar_reports_unknown_never_no_gaps() {
        let dir = sidecar_dir("absent");
        let read = qontinui_runner_lib::pair::coord_bound_tenants_at(
            &dir.join("coord_bound_tenants.json"),
            1_800_000_000,
        );
        assert!(
            matches!(read, CoordBoundTenantsRead::Unknown(_)),
            "an absent sidecar is UNKNOWN: {read:?}"
        );
        let report = resolve_binding_gaps(
            Some(&[tenant(0)]),
            crate::auth::BindingTenantRead::Bound(tenant(0)),
            &read,
        );
        match &report {
            BindingGapReport::Unknown(why) => assert!(
                why.contains("ABSENT"),
                "the report names WHICH silence it was: {why}"
            ),
            BindingGapReport::Gaps(g) => {
                panic!("an absent sidecar must NEVER render as gaps (got {g:?})")
            }
        }
        // And on the wire it is `{"unknown": …}` — not `{"gaps": []}`.
        let wire = serde_json::to_value(&report).expect("serialize");
        assert!(wire.get("unknown").is_some(), "wire shape: {wire}");
        assert!(wire.get("gaps").is_none(), "wire shape: {wire}");
    }

    /// GATE 4 — a sidecar older than `COORD_BOUND_TENANTS_MAX_AGE_SECS` (24h)
    /// is UNKNOWN too. A stale set would otherwise count an unpair forever,
    /// and, worse here, would report "no gaps" from evidence nobody refreshed.
    #[test]
    fn a_stale_sidecar_reports_unknown_never_no_gaps() {
        let dir = sidecar_dir("stale");
        let path = dir.join("coord_bound_tenants.json");
        let t0 = 1_800_000_000;
        let (held, bound_only) = (tenant(0), tenant(2));
        seed_sidecar(&path, &[held, bound_only], t0);

        // Inside the window the very same file DOES report the gap…
        let fresh_read = qontinui_runner_lib::pair::coord_bound_tenants_at(&path, t0 + 60);
        assert_eq!(
            resolve_binding_gaps(
                Some(&[held]),
                crate::auth::BindingTenantRead::Unbound,
                &fresh_read,
            ),
            BindingGapReport::Gaps(vec![bound_only.to_string()])
        );

        // …and one second past 24h it reports nothing at all.
        let stale_read = qontinui_runner_lib::pair::coord_bound_tenants_at(
            &path,
            t0 + qontinui_runner_lib::pair::COORD_BOUND_TENANTS_MAX_AGE_SECS + 1,
        );
        let report = resolve_binding_gaps(
            Some(&[held]),
            crate::auth::BindingTenantRead::Unbound,
            &stale_read,
        );
        match &report {
            BindingGapReport::Unknown(why) => {
                assert!(why.contains("24h"), "names the window: {why}")
            }
            BindingGapReport::Gaps(g) => {
                panic!("a stale sidecar must NEVER render as gaps (got {g:?})")
            }
        }
        let wire = serde_json::to_value(&report).expect("serialize");
        assert!(wire.get("unknown").is_some(), "wire shape: {wire}");
        assert!(wire.get("gaps").is_none(), "wire shape: {wire}");
    }

    /// The OTHER two UNKNOWN arms, which the covered side owns. Both must
    /// abort the report rather than manufacture a gap out of a failed read.
    #[test]
    fn an_unreadable_covered_side_is_unknown_not_a_gap() {
        let (held, bound_only) = (tenant(0), tenant(2));
        let bound = CoordBoundTenantsRead::Known(vec![held, bound_only]);
        // An undecryptable slot store reads as EMPTY — which would report
        // EVERY bound tenant as a gap.
        assert!(matches!(
            resolve_binding_gaps(None, crate::auth::BindingTenantRead::Unbound, &bound),
            BindingGapReport::Unknown(_)
        ));
        // An unreadable `paired_user.json` hides the default binding — which
        // would report the DEFAULT tenant as a gap.
        assert!(matches!(
            resolve_binding_gaps(
                Some(&[held]),
                crate::auth::BindingTenantRead::Unknown,
                &bound
            ),
            BindingGapReport::Unknown(_)
        ));
    }

    /// The composition over the real [`SweepInputs`] — the hop the sidecar
    /// read was added to, rather than a new one.
    #[test]
    fn the_sweep_hop_composes_the_gap_report() {
        let (held, bound_only) = (tenant(0), tenant(2));
        let inputs = SweepInputs {
            tenant_slots: Ok(vec![held]),
            default_binding: crate::auth::BindingTenantRead::Unbound,
            machine_pin: crate::session::tenant_pin::TenantPin::Pinned(held),
            coord_bound_tenants: CoordBoundTenantsRead::Known(vec![held, bound_only]),
        };
        assert_eq!(
            binding_gaps_from(&inputs),
            BindingGapReport::Gaps(vec![bound_only.to_string()]),
            "the machine pin is not an input — a pin is what this box ASKS for"
        );
    }

    /// GATE 6 — the gap report reaches `/coord-mcp/doctor`, BESIDE `slots`,
    /// and GATE 5 again end-to-end: running the pass with a gap published
    /// knocks on no mint door.
    #[tokio::test]
    async fn the_gap_report_reaches_the_doctor_and_mints_nothing() {
        let _serialised = health_lock();
        let mgr = test_auth_manager_with_dmk("p3_doctor_surface", "dmk_phase3_tripwire");
        let (held, bound_only) = (tenant(0), tenant(2));
        let now = chrono::Utc::now().timestamp();
        let fresh = synth_jwt(now + 3 * 60 * 60, "held");
        mgr.store_tenant_device_jwt(&held, &fresh).expect("slot");

        publish_binding_gaps(BindingGapReport::Gaps(vec![bound_only.to_string()]));
        let (base, cap, _shutdown) = spawn_mock(vec![]);
        let _ =
            refresh_tenant_slots(&mgr, &base, &base, DID, None, PosturePinInputs::UNPINNED).await;

        let report = crate::coord_mcp::doctor::report();
        let gaps = report
            .get("slot_health")
            .and_then(|h| h.get("binding_gaps"))
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        assert_eq!(
            gaps,
            serde_json::json!({ "gaps": [bound_only.to_string()] }),
            "the doctor carries the gap beside `slots`: {report}"
        );
        assert!(
            report
                .get("slot_health")
                .and_then(|h| h.get("slots"))
                .is_some(),
            "…beside, not instead of: {report}"
        );

        // GATE 5, end to end: a published gap must not cause a mint.
        assert!(
            cap.mint_attempts.lock().unwrap().is_empty(),
            "Phase 3 REPORTS a gap; seeding one is Phase 4 and a separate change"
        );

        // And the UNKNOWN arm survives the same round trip.
        publish_binding_gaps(BindingGapReport::Unknown("sidecar absent".to_string()));
        let _ =
            refresh_tenant_slots(&mgr, &base, &base, DID, None, PosturePinInputs::UNPINNED).await;
        let report = crate::coord_mcp::doctor::report();
        assert_eq!(
            report
                .get("slot_health")
                .and_then(|h| h.get("binding_gaps"))
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            serde_json::json!({ "unknown": "sidecar absent" }),
            "UNKNOWN must reach the doctor as UNKNOWN, never as an empty gap list"
        );
        assert!(cap.mint_attempts.lock().unwrap().is_empty());
    }
}

#[cfg(test)]
mod device_machine_key_exchange_tests {
    //! Phase 4b tests — `try_device_machine_key_exchange` against an in-process
    //! mock web backend serving
    //! `POST /api/v1/devices/{device_id}/machine-credential/exchange`. Covers:
    //!   - the last-resort recovery: a stored `dmk_` is exchanged for a fresh
    //!     device JWT (returned + stored), and the `X-Device-Machine-Key` header
    //!     carries the dmk_,
    //!   - no `dmk_` stored → `None` WITHOUT any HTTP call (so the loop falls
    //!     through to the credential-dark bail),
    //!   - REPLACE-not-REVOKE: a non-2xx (403 revoked/expired, 503 web
    //!     COORD_ADMIN_SECRET unset) → `None` and the existing JWT untouched.
    //!
    //! ORDERING NOTE: that the exchange is only ATTEMPTED after 4a self-refresh
    //! and the Cognito pair-cli path both fail is enforced structurally by its
    //! placement in `refresher_loop`'s `Pair` arm (guarded by
    //! `progress != Healthy`, after `try_device_self_refresh` and
    //! `try_refresh_once`). These unit tests exercise the leaf function in the
    //! same style as `device_self_refresh_tests`.

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

    /// Mint a synthetic device-JWT carrying `exp` (signature unverified).
    fn synth_jwt(exp: i64) -> String {
        let header = b64url(b"{\"alg\":\"EdDSA\",\"typ\":\"JWT\"}");
        let payload = b64url(format!("{{\"exp\":{}}}", exp).as_bytes());
        let sig = b64url(b"fake-sig");
        format!("{header}.{payload}.{sig}")
    }

    /// As [`synth_jwt`], but also carrying a `tenant_id` claim.
    fn synth_jwt_with_tenant(tenant: &uuid::Uuid, exp: i64) -> String {
        let header = b64url(b"{\"alg\":\"EdDSA\",\"typ\":\"JWT\"}");
        let payload = b64url(format!("{{\"exp\":{exp},\"tenant_id\":\"{tenant}\"}}").as_bytes());
        let sig = b64url(b"fake-sig");
        format!("{header}.{payload}.{sig}")
    }

    /// Build an isolated AuthManager over a temp `.enc`, optionally pre-seeding
    /// a stored `dmk_` and/or a device JWT. The seed writes go through a sibling
    /// `SecureStorage` at the SAME path (AuthManager exposes only a `dmk_`
    /// getter), so the manager reads them back from the encrypted file.
    fn setup(
        name: &str,
        dmk: Option<&str>,
        existing_jwt: Option<&str>,
    ) -> crate::auth::AuthManager {
        let dir = std::env::temp_dir().join("qontinui_test_dmk_exchange");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{name}.enc"));
        let _ = std::fs::remove_file(&path);
        let seed =
            crate::secure_storage::SecureStorage::with_path(path.clone()).expect("seed storage");
        if let Some(jwt) = existing_jwt {
            seed.store_tokens(jwt, "").expect("seed jwt");
        }
        if let Some(k) = dmk {
            seed.store_device_machine_key(k).expect("seed dmk");
        }
        let storage = crate::secure_storage::SecureStorage::with_path(path).expect("storage");
        crate::auth::AuthManager::with_storage(storage)
    }

    #[derive(Clone)]
    struct MockState {
        status: StatusCode,
        body: String,
        hits: Arc<Mutex<u32>>,
        last_dmk: Arc<Mutex<Option<String>>>,
    }

    async fn handler(
        State(s): State<MockState>,
        Path(_device_id): Path<String>,
        h: HeaderMap,
    ) -> (StatusCode, String) {
        *s.hits.lock().unwrap() += 1;
        *s.last_dmk.lock().unwrap() = h
            .get("x-device-machine-key")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_string());
        (s.status, s.body.clone())
    }

    struct MockCapture {
        hits: Arc<Mutex<u32>>,
        last_dmk: Arc<Mutex<Option<String>>>,
    }

    fn spawn_mock(
        status: StatusCode,
        body: String,
    ) -> (String, MockCapture, tokio::sync::oneshot::Sender<()>) {
        let hits = Arc::new(Mutex::new(0u32));
        let last_dmk = Arc::new(Mutex::new(None));
        let hits_h = hits.clone();
        let last_dmk_h = last_dmk.clone();
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
                    last_dmk: last_dmk_h,
                };
                let app: Router = Router::new()
                    .route(
                        "/api/v1/devices/{device_id}/machine-credential/exchange",
                        post(handler),
                    )
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
            MockCapture { hits, last_dmk },
            tx,
        )
    }

    const DID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    #[tokio::test]
    async fn exchange_recovers_device_jwt_from_stored_dmk() {
        // The >30-day cold-start: the slot holds an EXPIRED device JWT (4a +
        // Cognito already failed), but a dmk_ is stored → the exchange mints a
        // fresh device JWT, stores it, and returns it. The dmk_ must ride the
        // `X-Device-Machine-Key` header.
        let expired = synth_jwt(chrono::Utc::now().timestamp() - 60);
        let mgr = setup(
            "recovers_from_dmk",
            Some("dmk_unit_fixture_placeholder"),
            Some(&expired),
        );

        let new_jwt = synth_jwt(chrono::Utc::now().timestamp() + 4 * 60 * 60);
        let body = serde_json::json!({ "token": new_jwt }).to_string();
        let (base, cap, _shutdown) = spawn_mock(StatusCode::OK, body);

        let got = try_device_machine_key_exchange(&mgr, &base, DID, None).await;
        assert_eq!(
            got.as_deref(),
            Some(new_jwt.as_str()),
            "a stored dmk_ must be exchanged for the fresh device JWT"
        );
        assert_eq!(
            *cap.hits.lock().unwrap(),
            1,
            "the exchange endpoint should be hit exactly once"
        );
        assert_eq!(
            cap.last_dmk.lock().unwrap().clone(),
            Some("dmk_unit_fixture_placeholder".to_string()),
            "the dmk_ must be presented in the X-Device-Machine-Key header"
        );
        // Slot now holds the NEW device JWT (the expired one was replaced).
        assert_eq!(mgr.get_access_token().unwrap(), new_jwt);
    }

    #[tokio::test]
    async fn exchange_returns_none_when_no_dmk_stored() {
        // No dmk_ → the recovery path is unavailable: return None WITHOUT any
        // HTTP call, so the loop falls through to the credential-dark bail.
        let expired = synth_jwt(chrono::Utc::now().timestamp() - 60);
        let mgr = setup("no_dmk_stored", None, Some(&expired));

        // A 200-with-token mock that must NEVER be consulted.
        let new_jwt = synth_jwt(chrono::Utc::now().timestamp() + 4 * 60 * 60);
        let body = serde_json::json!({ "token": new_jwt }).to_string();
        let (base, cap, _shutdown) = spawn_mock(StatusCode::OK, body);

        let got = try_device_machine_key_exchange(&mgr, &base, DID, None).await;
        assert!(got.is_none(), "no dmk_ stored → None");
        assert_eq!(
            *cap.hits.lock().unwrap(),
            0,
            "the exchange endpoint must NOT be hit when no dmk_ is stored"
        );
        // Existing (expired) JWT untouched.
        assert_eq!(mgr.get_access_token().unwrap(), expired);
    }

    #[tokio::test]
    async fn exchange_403_returns_none_and_preserves_jwt() {
        // REPLACE-not-REVOKE: a revoked/expired/mismatched dmk_ → web 403 →
        // None and the existing JWT is left UNTOUCHED (never cleared).
        let existing = synth_jwt(chrono::Utc::now().timestamp() + 30 * 60);
        let mgr = setup("dmk_403_preserves", Some("dmk_revoked"), Some(&existing));

        let (base, cap, _shutdown) =
            spawn_mock(StatusCode::FORBIDDEN, r#"{"error":"revoked"}"#.to_string());

        let got = try_device_machine_key_exchange(&mgr, &base, DID, None).await;
        assert!(got.is_none(), "403 → None");
        assert_eq!(*cap.hits.lock().unwrap(), 1, "attempted exactly once");
        assert_eq!(
            mgr.get_access_token().unwrap(),
            existing,
            "REPLACE-not-REVOKE: existing JWT must be UNCHANGED after a 403"
        );
    }

    #[tokio::test]
    async fn exchange_503_returns_none_and_preserves_jwt() {
        // Web's COORD_ADMIN_SECRET unset (the deployment prerequisite) surfaces
        // as 503 → None, JWT preserved. A transient/config error never punishes
        // the runner by clearing its slot.
        let existing = synth_jwt(chrono::Utc::now().timestamp() + 30 * 60);
        let mgr = setup("dmk_503_preserves", Some("dmk_ok"), Some(&existing));

        let (base, _cap, _shutdown) = spawn_mock(
            StatusCode::SERVICE_UNAVAILABLE,
            r#"{"error":"coord admin secret unset"}"#.to_string(),
        );

        let got = try_device_machine_key_exchange(&mgr, &base, DID, None).await;
        assert!(got.is_none(), "503 → None");
        assert_eq!(
            mgr.get_access_token().unwrap(),
            existing,
            "existing JWT must be UNCHANGED after a 503"
        );
    }

    /// D2/D3: a 2xx exchange that mints for a DIFFERENT tenant than
    /// `expected_tenant` is refused at the seam — `None`, nothing persisted
    /// (not even the legacy slot), existing JWT untouched. This is the exact
    /// bypass plan
    /// `2026-09-17-device-jwt-refresh-drops-the-requested-tenant-and-coord-mints-the-home-tenant`
    /// names: the dmk exchange used to call `store_tokens` unconditionally.
    #[tokio::test]
    async fn exchange_refuses_a_foreign_tenant_and_preserves_jwt() {
        let expected = uuid::Uuid::parse_str("11111111-2222-4333-8444-555555555560").unwrap();
        let foreign = uuid::Uuid::parse_str("22222222-3333-4444-5555-666666666670").unwrap();
        let existing = synth_jwt(chrono::Utc::now().timestamp() - 60); // expired, cold-start
        let mgr = setup(
            "dmk_foreign_tenant_refused",
            Some("dmk_ok"),
            Some(&existing),
        );

        let minted_for_foreign =
            synth_jwt_with_tenant(&foreign, chrono::Utc::now().timestamp() + 4 * 60 * 60);
        let body = serde_json::json!({ "token": minted_for_foreign }).to_string();
        let (base, cap, _shutdown) = spawn_mock(StatusCode::OK, body);

        let got = try_device_machine_key_exchange(&mgr, &base, DID, Some(expected)).await;
        assert!(got.is_none(), "a foreign-tenant mint must be refused");
        assert_eq!(
            *cap.hits.lock().unwrap(),
            1,
            "the exchange was still attempted"
        );
        assert_eq!(
            mgr.get_access_token().unwrap(),
            existing,
            "REPLACE-not-REVOKE: a tenant mismatch must not touch the legacy slot either"
        );
        assert_eq!(
            mgr.get_tenant_device_jwt(&foreign).unwrap(),
            None,
            "the foreign tenant's slot must not be seeded either"
        );
    }
}
