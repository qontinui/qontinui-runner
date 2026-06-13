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

use tokio::sync::{watch, Mutex};
use tracing::{info, warn};

use crate::mcp::types::ApiState;
use crate::settings::{self, RunnerTier};

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

    // tenant_id: machine.json::active_tenant_id, then the outgoing device-JWT
    // claim (still parseable when expired). NULL is acceptable on the wire
    // (coord's StatusUpsert.tenant_id is Option), but we send it when known so
    // the row is tenant-scoped.
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
    // No Cognito session → legacy/local-login install; let the caller use its
    // existing bearer source.
    let refresh_token = match auth_manager.get_oauth_refresh_token() {
        Ok(t) if !t.trim().is_empty() => t,
        _ => return None,
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
                return Some(resp.access_token);
            }
            Ok(Err(e)) => {
                warn!(
                    "device_jwt_refresher: Cognito token refresh failed: {e} — using stored token"
                );
            }
            Err(join_err) => {
                warn!("device_jwt_refresher: Cognito refresh task join failed: {join_err}");
            }
        }
    }

    // Fresh enough (or refresh failed) — use whatever access token is stored.
    auth_manager.get_oauth_access_token().ok()
}

async fn refresher_loop(
    _api_state: Arc<ApiState>,
    mut shutdown_rx: watch::Receiver<bool>,
    mut kick_rx: watch::Receiver<u64>,
) {
    let auth_manager = crate::auth::AuthManager::new();
    info!("Device-JWT refresher started (check interval = 5m, threshold = 80m)");

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
                // The web backend's pair-cli endpoint gates on the user
                // bearer (`Authorization: Bearer <user-token>`). Phase 5
                // (unified-Cognito-identity): when the runner was signed in
                // via Cognito, the bearer is the Cognito **access token** —
                // refreshed first if it's stale (so the re-bind presents a
                // valid user token). For legacy/local-login installs there's
                // no Cognito session, so we fall back to the device-JWT slot
                // bearer (the historical Phase-2 source).
                let bearer_token = match ensure_fresh_cognito_bearer(&auth_manager).await {
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
                            if wait_with_signals(
                                REFRESH_CHECK_INTERVAL,
                                &mut shutdown_rx,
                                &mut kick_rx,
                            )
                            .await
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

                // Brief sleep before next iteration (success or failure)
                // so we don't hammer coord on persistent errors.
                if wait_with_signals(REFRESH_CHECK_INTERVAL, &mut shutdown_rx, &mut kick_rx).await {
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
