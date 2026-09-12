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
        },
        None => CoordCredentialBag {
            ok: fallback.ok,
            reason: fallback.reason.clone(),
            posture: "unknown".to_string(),
            since: iso8601(chrono::Utc::now().timestamp()),
            tenant_id: None,
            exp: None,
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
    // owned by coord, not by an OAuth refresh chain).
    match auth_manager.store_tokens(&resp.token, "") {
        Ok(()) => {
            // M1: a NEW credential is in the slot, so every rejection coord
            // recorded against the old one is spent. Without this the streak
            // survives replacement and `dark` latches on a working token.
            reset_upstream_rejections_for(None);
            RefreshOutcome::Replaced {
                new_jwt: resp.token,
            }
        }
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

/// Drop every bucket that NOTHING CAN EVER WRITE AGAIN.
///
/// A tenant-keyed bucket is only ever written by a forwarder that resolved
/// that tenant and presented a credential for it. The three routes to such a
/// resolution are the per-tenant slots
/// ([`crate::auth::AuthManager::list_tenant_device_jwt_tenants`]), the device's
/// DEFAULT binding ([`crate::auth::default_binding_tenant`], which
/// `select_device_bearer` serves out of the legacy slot) and the live machine
/// pin ([`crate::session::tenant_pin::resolve_tenant_pin`]). A key in none of them
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

/// Resolve this machine's writable slot keys and run the sweep. The I/O half
/// of [`evict_unwritable_upstream_buckets`], called once per refresher pass
/// from BOTH branches — the tenant-slot pass and the legacy one — because a
/// re-pair can happen from either shape.
fn sweep_unwritable_upstream_buckets(tenant_slots: &[uuid::Uuid]) {
    let mut writable: std::collections::HashSet<String> =
        tenant_slots.iter().map(|t| t.to_string()).collect();
    if let Some(t) = crate::auth::default_binding_tenant() {
        writable.insert(t.to_string());
    }
    if let crate::session::tenant_pin::TenantPin::Pinned(t) =
        crate::session::tenant_pin::resolve_tenant_pin()
    {
        writable.insert(t.to_string());
    }
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
/// * **A pin with no slot**: tenant slots for Y and Z, `machine.json` pinned
///   to X. Same shape, same silence.
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

fn unclaimed_upstream_verdict(
    observations: &[SlotObservation],
    fold_default: bool,
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
            let fresh = s
                .last_rejection_at
                .is_some_and(|t| now - t <= UPSTREAM_ORPHAN_STALE_AFTER_SECS);
            if fresh {
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
    publish_coord_credential_posture_with(posture, tenant_id, exp, last_refresh_outcome, signal)
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
pub(crate) fn derive_and_publish_posture(
    observations: &[SlotObservation],
    now: i64,
) -> Option<PostureTransition> {
    // One usable slot means the default-slot bucket describes the same
    // credential — see [`upstream_signal_for_observation`].
    let fold_default = observations.iter().filter(|o| !o.unknown).count() == 1;
    let worst = observations
        .iter()
        .filter(|o| !o.unknown)
        .map(|o| {
            let signal = upstream_signal_for_observation(o.tenant_id.as_deref(), fold_default);
            (derive_coord_credential_posture(o, signal, now), o)
        })
        .max_by_key(|(p, _)| p.severity());

    // N1 — the unattributable arm, as a MAP DIFF rather than a special case
    // for the default key. See [`unclaimed_upstream_verdict`] for the class
    // and the shapes beyond the default key that reach it.
    //
    // A per-slot verdict that is ALREADY non-answering wins over both arms
    // below: it names a slot an operator can actually fix.
    let slot_already_non_answering = worst.as_ref().is_some_and(|(p, _)| !p.can_answer());
    if !slot_already_non_answering {
        match unclaimed_upstream_verdict(observations, fold_default, now) {
            UnclaimedVerdict::Dark(orphan_key, orphan_signal) => {
                // The KEY is the one actionable fact the published posture
                // cannot carry (see below), so it goes to the log — LATCHED,
                // because this condition holds across every pass and an
                // unlatched line would repeat every five minutes forever.
                // A tenant UUID, never a credential.
                if latch_orphan_warning(&orphan_key) {
                    warn!(
                        "device_jwt_refresher: coord has refused the credential filed under \
                         slot key {:?} {} consecutive times, and NO slot this pass observed \
                         describes it — publishing dark with no tenant (legacy-only install, \
                         a pin with no slot, or a slot this pass could not read)",
                        if orphan_key.is_empty() {
                            "<default>"
                        } else {
                            orphan_key.as_str()
                        },
                        orphan_signal.consecutive_rejections,
                    );
                }
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
                    // the key goes to the log line above, which is honest in
                    // every shape.
                    None,
                    // Likewise no `exp`: the answer to "which slot's?" is none.
                    None,
                    None,
                    orphan_signal,
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

    let (posture, obs) = worst?;
    publish_coord_credential_posture_with(
        posture,
        obs.tenant_id.clone(),
        obs.exp,
        obs.outcome.map(tenant_slot_outcome_token),
        upstream_signal_for_observation(obs.tenant_id.as_deref(), fold_default),
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
/// situation a dead slot is in. The re-minted JWT is written back into this
/// tenant's slot ONLY when its own `tenant_id` claim names this tenant —
/// keyed by the tenant coord actually issued for, never by the key we happened
/// to be repairing. When it names another tenant (or none), the slot stays
/// cleared: the exchange has already refreshed the default slot, and seeding a
/// tenant-keyed slot with a credential for a different tenant is the
/// cross-tenant substitution `select_device_bearer` refuses by design.
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
    let Some(jwt) = try_device_machine_key_exchange(auth_manager, web_base, device_id).await else {
        return TenantSlotOutcome::Cleared {
            cause,
            rederived: false,
        };
    };
    let minted_for = qontinui_runner_lib::pair::tenant_id_from_oauth_claim(jwt.trim())
        .and_then(|raw| uuid::Uuid::parse_str(raw.trim()).ok());
    if minted_for != Some(*tenant) {
        warn!(
            "device_jwt_refresher: device-machine-key exchange re-minted for {minted_for:?}, \
             not tenant {tenant} — leaving that slot cleared rather than seeding it with \
             another tenant's credential (the default slot was refreshed)"
        );
        return TenantSlotOutcome::Cleared {
            cause,
            rederived: false,
        };
    }
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
pub(crate) async fn refresh_tenant_slots(
    auth_manager: &crate::auth::AuthManager,
    coord_base: &str,
    web_base: &str,
    device_id: &str,
    app: Option<&tauri::AppHandle>,
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
        if let Some(transition) = derive_and_publish_posture(&boot_obs, now) {
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
    if let Some(transition) = derive_and_publish_posture(&observations, now) {
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
///     web's `COORD_ADMIN_SECRET` is unset, anything else), OR
///   - the network call fails, the body fails to decode, or the token is empty.
///
/// REPLACE-not-REVOKE: on ANY failure the existing JWT is left UNTOUCHED — a
/// missed exchange means "stay credential-dark", NEVER "clear the slot".
pub(crate) async fn try_device_machine_key_exchange(
    auth_manager: &crate::auth::AuthManager,
    web_base: &str,
    device_id: &str,
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
    // device-JWT lifecycle is coord-owned).
    match auth_manager.store_tokens(&body.token, "") {
        Ok(()) => {
            info!(
                "device_jwt_refresher: device JWT re-minted via device-machine-key \
                 exchange (len={})",
                body.token.len()
            );
            Some(body.token)
        }
        Err(e) => {
            warn!("device_jwt_refresher: persist dmk-exchanged JWT failed: {e}");
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
        let tenant_slots = auth_manager.list_tenant_device_jwt_tenants();
        let has_tenant_slots = !tenant_slots.is_empty();
        // Retire every upstream-verdict bucket this runner can no longer write
        // (a re-pair or an unpair), BEFORE the posture is derived from what is
        // left. Runs in BOTH branches because a re-pair can happen from either
        // shape, and it is the IMPOSSIBILITY half of retiring an orphan's
        // evidence — the half that replaced a guessed clock.
        sweep_unwritable_upstream_buckets(&tenant_slots);
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
            if let Some(transition) =
                derive_and_publish_posture(&[obs], chrono::Utc::now().timestamp())
            {
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

                // Phase 4b: FINAL cold-start fallback. Self-refresh (4a) AND the
                // Cognito pair-cli path have BOTH failed to advance the slot this
                // tick (progress != Healthy). If a device machine key (`dmk_`) is
                // stored, exchange it with web for a fresh device JWT — this
                // recovers a runner offline past both the device-JWT TTL and the
                // Cognito refresh-token window (>30d) with no user session. On
                // success: same healthy-tick handling as the other re-mints (kick
                // relay, publish healthy, reset backoff, emit "resumed" if we were
                // dark, continue). On None: fall through to the existing bail.
                // REPLACE-not-REVOKE: a miss never clears the existing JWT.
                if !matches!(progress, PairProgress::Healthy) {
                    let dmk_device_id = std::env::var("QONTINUI_MACHINE_ID")
                        .ok()
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| s.trim().to_string())
                        .or_else(|| qontinui_runner_lib::pair::read_device_id_from_disk().ok());
                    if let Some(did) = dmk_device_id {
                        if let Some(new_jwt) =
                            try_device_machine_key_exchange(&auth_manager, &pair_base, &did).await
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
    /// token), and answers any bearer listed in `failures` with the status
    /// paired with it — so failure isolation, a 401 rejection and a 503 outage
    /// are each testable against the same mock. Captures every presented
    /// bearer in order.
    #[derive(Clone)]
    struct MockState {
        failures: Arc<Mutex<Vec<(String, u16)>>>,
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
    }

    fn spawn_mock(
        failures: Vec<(String, u16)>,
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
                    failures: Arc::new(Mutex::new(failures)),
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
        let outcomes = refresh_tenant_slots(&mgr, &base, "", DID, None).await;

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
        let outcomes = refresh_tenant_slots(&mgr, &base, "", DID, None).await;

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
        let outcomes = refresh_tenant_slots(&mgr, &base, "", DID, None).await;

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
        let outcomes = refresh_tenant_slots(&mgr, &base, "", DID, None).await;

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
        let outcomes = refresh_tenant_slots(&mgr, &base, "", DID, None).await;

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
        let outcomes = refresh_tenant_slots(&mgr, &base, "", DID, None).await;

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
        let outcomes = refresh_tenant_slots(&mgr, &base, "", DID, None).await;

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
        let outcomes = refresh_tenant_slots(&mgr, &dead_base, &dead_base, DID, None).await;

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
        let _ = refresh_tenant_slots(&mgr, &base, "", DID, None).await;

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
        let outcomes = refresh_tenant_slots(&mgr, &base, "", DID, None).await;
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
        let _ = refresh_tenant_slots(&mgr, &base, "", DID, None).await;

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
        let outcomes = refresh_tenant_slots(&mgr, &dead_base, &dead_base, DID, None).await;
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
        assert_eq!(derive_and_publish_posture(&[unreadable], now), None);
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
        let outcomes = refresh_tenant_slots(&mgr, &base, "", DID, None).await;
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
            derive_and_publish_posture(std::slice::from_ref(&fresh_obs), now),
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
        let outcomes = refresh_tenant_slots(&mgr, &base, "", DID, None).await;
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
            derive_and_publish_posture(std::slice::from_ref(&fresh_obs), now),
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
        let _ = refresh_tenant_slots(&mgr, &base, "", DID, None).await;

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
        let transition = derive_and_publish_posture(&[slot(a), slot(b)], now);
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
        let _ = derive_and_publish_posture(&[slot(b), slot(a)], now);
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
        let transition = derive_and_publish_posture(std::slice::from_ref(&slot), now);
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
        let transition = derive_and_publish_posture(&[slot, other], now);
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
            derive_and_publish_posture(&slots, now).map(|t| t.to),
            Some(CoordCredentialPosture::Live),
            "two transient 401s on an unattributable credential are not a verdict"
        );

        // The threshold'th consecutive rejection IS the verdict.
        note_coord_upstream_verdict(None, true, 401, br#"{"code":"token_revoked"}"#);
        let transition = derive_and_publish_posture(&slots, now);
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
        let _ = derive_and_publish_posture(&[healthy_slot(tenant(0)), dead], now);
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
            derive_and_publish_posture(&slots, now).map(|t| t.to),
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

        let transition = derive_and_publish_posture(std::slice::from_ref(&legacy_obs), now);
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

        let transition = derive_and_publish_posture(&slots, now);
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
        let _ = derive_and_publish_posture(&slots, now);
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
            derive_and_publish_posture(std::slice::from_ref(&legacy_obs), t5).map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected))
        );

        // …and then silence, because every call fails.
        let transitions_before = recorded_posture_transitions().len();
        for minutes in [35i64, 90, 6 * 60, 24 * 60] {
            let later = t0 + minutes * 60;
            let transition = derive_and_publish_posture(std::slice::from_ref(&legacy_obs), later);
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
            derive_and_publish_posture(std::slice::from_ref(&legacy_obs), t0 + 48 * 60 * 60)
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
            derive_and_publish_posture(&slots, now).map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected))
        );

        // The re-pair: `gone` is in no slot, is not the default binding and is
        // not the live pin, so nothing can ever write its bucket again.
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
            derive_and_publish_posture(&slots, now).map(|t| t.to),
            Some(CoordCredentialPosture::Live),
            "a re-paired runner with two live slots is live"
        );
        reset_posture();
    }

    /// The eviction predicate's three writable routes, and the one key that is
    /// never evicted.
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
            derive_and_publish_posture(&slots, real_now).map(|t| t.to),
            Some(CoordCredentialPosture::Dark(DarkCause::UpstreamRejected))
        );

        // A week and a second later, with nothing else changed: ABSTAIN.
        let later = real_now + UPSTREAM_ORPHAN_STALE_AFTER_SECS + 1;
        assert_eq!(
            derive_and_publish_posture(&slots, later),
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
            derive_and_publish_posture(&slots, chrono::Utc::now().timestamp()).map(|t| t.to),
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
        }

        // No pass has concluded: UNKNOWN. The posture is not the source and
        // says so, and an absent measurement does not manufacture a fault.
        let unknown = coord_credential_bag(&fallback, None);
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
        let outcomes = refresh_tenant_slots(&mgr, &base, "", DID, None).await;
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

        let got = try_device_machine_key_exchange(&mgr, &base, DID).await;
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

        let got = try_device_machine_key_exchange(&mgr, &base, DID).await;
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

        let got = try_device_machine_key_exchange(&mgr, &base, DID).await;
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

        let got = try_device_machine_key_exchange(&mgr, &base, DID).await;
        assert!(got.is_none(), "503 → None");
        assert_eq!(
            mgr.get_access_token().unwrap(),
            existing,
            "existing JWT must be UNCHANGED after a 503"
        );
    }
}
