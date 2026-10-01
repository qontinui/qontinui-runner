//! Claim an operator-authorized pair-code redeem while this device's coord
//! credential is dark.
//!
//! Plan `2026-09-26-authenticate-and-perpetually-renew-a-specific-runner-from-qontinui-web`,
//! Phase 2 step 3 (missing link M1). A headless runner whose device JWT
//! lapsed could only be re-armed by TYPING a pair code into its UI. Instead,
//! an operator clicks **Authenticate** on qontinui-web
//! (`POST /api/v1/devices/{device_id}/authorize-redeem`), web mints a pair code
//! BOUND to that device, and this module collects it:
//!
//! ```text
//! GET {web_base}/api/v1/devices/{device_id}/pending-redeem
//! Authorization: Bearer <this device's own coord-signed device JWT, expiry tolerated>
//! ```
//!
//! - `200 {"code", "expires_at"}` — redeem the code with the steps of the
//!   interactive `redeem_pair_code` command (the same `pair.rs` redeem and
//!   persist, streak retirement, and the shared
//!   `web_integration::promote_tier_after_pairing`) EXCEPT its clear of the
//!   interactive-sign-out marker, which must never run from a background path;
//!   then kick the cloud relay. Without the kick the relay stays latched
//!   `Idle` for hours (coord finding `6ec70933`).
//! - Not at all while the local user is logged out (their withdrawal), nor on
//!   a runner the refresher holds at `IdleWrongTier` (the user's tier choice).
//! - `204` — nothing authorized; do nothing.
//! - anything else — a typed log line and a growing backoff. Never spam web.
//!
//! ## The trust anchor
//!
//! The bearer is the device JWT coord once issued to THIS device, presented
//! even though it is expired: it proves possession, and a third party knowing
//! only the `device_id` cannot forge it. Web verifies the signature, the
//! `device_id` claim against the path, a 30-day grace past `exp`, that the
//! device is not revoked, and that an operator authorized it. A device that
//! holds no such token (never paired, fully signed out, store wiped, or past
//! the grace window) is outside this door and still needs a typed or CLI pair
//! code — so we make no request at all rather than send one web must refuse.
//!
//! The refresher CLEARS dead slots (the `unrefreshable` path) and a default
//! logout clears `access_token`, so the store keeps the newest device JWT it
//! has held in a dedicated anchor slot (`secure_storage`'s
//! `redeem_anchor_jwt`, maintained on every write) that survives those clears.
//! It is used only as this poll's bearer and is reset after a successful redeem.
//!
//! ## When it runs
//!
//! Once per device-JWT refresher pass ([`on_refresher_pass`], called from
//! `device_jwt_refresher::refresher_loop` after the pass has published its
//! posture), and only when that posture cannot answer coord (`expired`,
//! `unrefreshable`, `absent` or `dark`; see [`posture_wants_poll`] for where an
//! operator revoke lands). The refresher can iterate faster than its 300 s
//! cadence (transient backoff, kicks), so this module carries its own spacing:
//! at most one poll per [`MIN_POLL_SPACING`], longer after a failure.
//!
//! ## Secrets
//!
//! Neither the pair code nor any token is ever logged. The code travels in a
//! [`RedeemCode`] whose `Debug` is redacted, and every error string that could
//! embed it (the redeem path formats its URL, which carries the code, into its
//! errors) is passed through [`redact_code`] first.

use std::fmt;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::mcp::device_jwt_refresher::{coord_credential_posture, CoordCredentialPosture};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

/// Minimum spacing between two polls. Just under the refresher's 300 s
/// `REFRESH_CHECK_INTERVAL` so a pass that lands a few seconds early still
/// polls, while a burst of kicked passes polls once.
pub(crate) const MIN_POLL_SPACING: Duration = Duration::from_secs(290);

/// Ceiling of the failure backoff: a persistently refusing web is asked at
/// most once an hour.
const FAILURE_BACKOFF_MAX: Duration = Duration::from_secs(3600);

/// How far past `exp` web accepts the anchor token. A token beyond this is
/// refused by web, so we do not send it.
const ANCHOR_GRACE_SECS: i64 = 30 * 24 * 3600;

/// Poll request timeout.
const POLL_TIMEOUT: Duration = Duration::from_secs(10);

/// A pair code handed out by `pending-redeem`. Its `Debug` never prints the
/// value, so an accidental `{:?}` cannot leak it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RedeemCode(String);

impl RedeemCode {
    /// The raw code — only for the redeem request itself.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RedeemCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RedeemCode(<redacted>)")
    }
}

/// Replace every spelling of `code` in `msg` with `<code>`. The redeem path
/// upper-cases the code into its URL, so both spellings are covered.
pub(crate) fn redact_code(msg: &str, code: &RedeemCode) -> String {
    let raw = code.expose().trim();
    if raw.is_empty() {
        return msg.to_string();
    }
    let mut out = msg.replace(raw, "<code>");
    for variant in [raw.to_uppercase(), raw.to_lowercase()] {
        out = out.replace(&variant, "<code>");
    }
    out
}

/// Does this posture call for claiming an operator-authorized redeem?
///
/// Every posture that cannot answer coord polls: `expired`, `unrefreshable`,
/// `absent` and `dark(..)`. `live`/`expiring` do not, and neither does `None`
/// (no refresher pass has concluded yet), which is UNKNOWN rather than dark.
///
/// Where an operator REVOKE lands: coord then refuses the device's refresh
/// (`403 device_credential_revoked`) and web refuses the machine-key exchange,
/// so a per-tenant slot is cleared on that 403 and its re-derive fails —
/// `unrefreshable` — and a legacy-only runner reaches `expired` when its token
/// lapses. `dark(upstream_401)` is the remaining way coord says "not this
/// credential" (a locally valid token refused on use), and a fresh pairing is
/// its cure too. Polling in any of them is safe: web answers `204` or a typed
/// `403 device_credential_revoked` until an operator clicks Authenticate, and
/// the failure backoff bounds the rate.
pub(crate) fn posture_wants_poll(posture: Option<CoordCredentialPosture>) -> bool {
    posture.is_some_and(|p| !p.can_answer())
}

/// Pick the token to present as the trust anchor: one web's pending-redeem
/// door accepts as proof of THIS device (the shared rule,
/// [`crate::secure_storage::pending_redeem_anchor_exp`] — the same one the
/// store's anchor slot keeps by) whose `exp` is within the grace window. Among
/// several, the latest `exp` wins.
pub(crate) fn select_anchor(
    device_id: &str,
    candidates: &[String],
    now_unix: i64,
) -> Option<String> {
    candidates
        .iter()
        .filter_map(|token| {
            let exp = crate::secure_storage::pending_redeem_anchor_exp(token, device_id)?;
            (now_unix <= exp + ANCHOR_GRACE_SECS).then(|| (exp, token.trim().to_string()))
        })
        .max_by_key(|(exp, _)| *exp)
        .map(|(_, token)| token)
}

/// What one `pending-redeem` request answered.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PollOutcome {
    /// 200 with a code: an operator authorized this device.
    Pending { code: RedeemCode },
    /// 204: nothing authorized.
    NothingPending,
    /// 401/403 — web refused the anchor. `detail_code` is web's typed
    /// `detail.code` when it sent one.
    Refused {
        status: u16,
        detail_code: Option<String>,
    },
    /// Any other status, or a 200 whose body carried no code.
    Unexpected { status: u16 },
    /// The request never completed (connect, timeout, client build).
    Transport(String),
}

impl PollOutcome {
    /// A stable key naming the failure, used to warn once per distinct
    /// failure rather than once per pass.
    fn failure_key(&self) -> Option<String> {
        match self {
            PollOutcome::Pending { .. } | PollOutcome::NothingPending => None,
            PollOutcome::Refused {
                status,
                detail_code,
            } => Some(format!(
                "refused:{status}:{}",
                detail_code.as_deref().unwrap_or("-")
            )),
            PollOutcome::Unexpected { status } => Some(format!("unexpected:{status}")),
            PollOutcome::Transport(_) => Some("transport".to_string()),
        }
    }
}

/// `GET {web_base}/api/v1/devices/{device_id}/pending-redeem` with `anchor` as
/// the bearer.
pub(crate) async fn poll_pending_redeem(
    web_base: &str,
    device_id: &str,
    anchor: &str,
) -> PollOutcome {
    let url = format!(
        "{}/api/v1/devices/{}/pending-redeem",
        web_base.trim().trim_end_matches('/'),
        device_id.trim()
    );
    let client = match reqwest::Client::builder().timeout(POLL_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => return PollOutcome::Transport(format!("client build failed: {e}")),
    };
    // Not a coord route: qontinui-web, authenticated by this device's own
    // (possibly expired) coord-signed device JWT — the plan's M1 anchor.
    let resp = match client.get(&url).bearer_auth(anchor).send().await {
        Ok(r) => r,
        Err(e) => return PollOutcome::Transport(e.to_string()),
    };
    let status = resp.status().as_u16();
    match status {
        200 => {
            let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
            match body.get("code").and_then(|v| v.as_str()).map(str::trim) {
                Some(code) if !code.is_empty() => PollOutcome::Pending {
                    code: RedeemCode(code.to_string()),
                },
                _ => PollOutcome::Unexpected { status },
            }
        }
        204 => PollOutcome::NothingPending,
        401 | 403 => {
            let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
            let detail_code = body
                .get("detail")
                .and_then(|d| d.get("code"))
                .and_then(|c| c.as_str())
                .map(str::to_string);
            PollOutcome::Refused {
                status,
                detail_code,
            }
        }
        _ => PollOutcome::Unexpected { status },
    }
}

/// The side effects of a claimed redeem, behind a seam so the pass is testable
/// without a live pairing store or relay.
pub(crate) trait RedeemEffects {
    /// Redeem `code` against `web_base` for `device_id` — the SAME id the poll
    /// named, since web binds the code to it — and persist the resulting
    /// credential. An `Err` may embed the code; the caller redacts it.
    async fn redeem(
        &self,
        web_base: &str,
        code: &RedeemCode,
        device_id: &str,
    ) -> Result<(), String>;
    /// Drop the now-spent pending-redeem anchor (re-seeded from the fresh
    /// credential the redeem stored).
    async fn clear_anchor(&self);
    /// Wake the cloud relay so it reconnects with the fresh credential.
    async fn kick_relay(&self);
}

/// Carried across passes: when the next poll is allowed and which failure was
/// last announced.
#[derive(Debug, Default)]
pub(crate) struct PollState {
    next_poll_at: Option<Instant>,
    consecutive_failures: u32,
    last_warned: Option<String>,
    no_anchor_reported: bool,
}

impl PollState {
    fn schedule(&mut self, now: Instant, wait: Duration) {
        self.next_poll_at = Some(now + wait);
    }

    fn succeeded(&mut self, now: Instant) {
        self.consecutive_failures = 0;
        self.last_warned = None;
        self.schedule(now, MIN_POLL_SPACING);
    }

    fn failed(&mut self, now: Instant) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.schedule(now, failure_backoff(self.consecutive_failures));
    }
}

/// Wait after the `n`th consecutive failure: doubling from
/// [`MIN_POLL_SPACING`], capped at [`FAILURE_BACKOFF_MAX`].
pub(crate) fn failure_backoff(n: u32) -> Duration {
    let factor = 1u32 << n.saturating_sub(1).min(6);
    (MIN_POLL_SPACING * factor).min(FAILURE_BACKOFF_MAX)
}

/// What one pass did — the tests assert on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassResult {
    /// Posture is not dark (or unknown): no poll.
    NotEligible,
    /// The runner is deliberately below the account tier (`IdleWrongTier`):
    /// it does not talk to the cloud, and a redeem would override that choice.
    WrongTier,
    /// The local user explicitly logged out of the interactive session. That
    /// is their withdrawal: a web operator must not re-pair the box over it.
    LocallySignedOut,
    /// Dark, but the spacing/backoff has not elapsed.
    Throttled,
    /// No qontinui-web base resolved.
    NoWebBase,
    /// No device identity to name in the path.
    NoDeviceId,
    /// No token usable as the anchor: outside this door.
    NoAnchor,
    /// Web answered 204.
    NothingPending,
    /// A code was redeemed and the relay kicked.
    Redeemed,
    /// Web handed out a code but redeeming it failed.
    RedeemFailed,
    /// The poll itself failed (refused, unexpected status, transport).
    PollFailed,
}

/// Inputs to one pass. The loaders run only once the pass has decided to
/// poll, so an eligible-but-throttled or healthy pass reads no credential store.
pub(crate) struct PassInputs<'a, S, D, A>
where
    S: FnOnce() -> bool,
    D: FnOnce() -> Option<String>,
    A: FnOnce(&str) -> Option<String>,
{
    pub posture: Option<CoordCredentialPosture>,
    /// The refresher decided `IdleWrongTier` this pass.
    pub wrong_tier: bool,
    pub web_base: &'a str,
    /// Has the local user explicitly logged out of the interactive session?
    pub load_signed_out: S,
    pub load_device_id: D,
    /// Given the device id, return the anchor token (see [`select_anchor`]).
    pub load_anchor: A,
}

/// One pass: decide, poll, act. Pure apart from the HTTP poll and `effects`.
pub(crate) async fn run_pass<S, D, A, E>(
    inputs: PassInputs<'_, S, D, A>,
    state: &mut PollState,
    now: Instant,
    effects: &E,
) -> PassResult
where
    S: FnOnce() -> bool,
    D: FnOnce() -> Option<String>,
    A: FnOnce(&str) -> Option<String>,
    E: RedeemEffects,
{
    if !posture_wants_poll(inputs.posture) {
        // A healthy credential ends any dark episode: the next one starts
        // with a prompt poll and fresh announcements.
        *state = PollState::default();
        return PassResult::NotEligible;
    }
    if inputs.wrong_tier {
        return PassResult::WrongTier;
    }
    if state.next_poll_at.is_some_and(|at| now < at) {
        return PassResult::Throttled;
    }
    if (inputs.load_signed_out)() {
        state.schedule(now, MIN_POLL_SPACING);
        if state.last_warned.as_deref() != Some("signed_out") {
            state.last_warned = Some("signed_out".to_string());
            info!(
                "pending_redeem: credential is dark but the local user logged out — not                  asking web for an operator-authorized redeem until they sign in again"
            );
        }
        return PassResult::LocallySignedOut;
    }
    if inputs.web_base.trim().is_empty() {
        state.failed(now);
        if state.last_warned.as_deref() != Some("no_web_base") {
            state.last_warned = Some("no_web_base".to_string());
            warn!(
                "pending_redeem: credential is dark but no qontinui-web base URL resolves —                  cannot ask web for an operator-authorized redeem"
            );
        }
        return PassResult::NoWebBase;
    }
    let Some(device_id) = (inputs.load_device_id)() else {
        state.failed(now);
        if !state.no_anchor_reported {
            state.no_anchor_reported = true;
            warn!("pending_redeem: credential is dark but this runner has no device_id — cannot ask web for an authorized redeem");
        }
        return PassResult::NoDeviceId;
    };
    let Some(anchor) = (inputs.load_anchor)(&device_id) else {
        state.failed(now);
        if !state.no_anchor_reported {
            state.no_anchor_reported = true;
            info!(
                "pending_redeem: credential is dark and no device JWT for device {device_id} \
                 is held within the {}-day grace window — an operator-authorized redeem \
                 cannot be claimed; pair with a typed or CLI pair code",
                ANCHOR_GRACE_SECS / 86_400
            );
        }
        return PassResult::NoAnchor;
    };
    state.no_anchor_reported = false;

    let outcome = poll_pending_redeem(inputs.web_base, &device_id, &anchor).await;
    match outcome {
        PollOutcome::NothingPending => {
            debug!("pending_redeem: no operator-authorized redeem pending for device {device_id}");
            state.succeeded(now);
            PassResult::NothingPending
        }
        PollOutcome::Pending { code } => {
            info!(
                "pending_redeem: operator authorized a redeem for device {device_id} — redeeming"
            );
            match effects.redeem(inputs.web_base, &code, &device_id).await {
                Ok(()) => {
                    effects.clear_anchor().await;
                    effects.kick_relay().await;
                    info!("pending_redeem: device {device_id} re-paired from an operator-authorized redeem; relay kicked");
                    state.succeeded(now);
                    PassResult::Redeemed
                }
                Err(e) => {
                    // The code was handed out once and is spent from web's
                    // side; a retry needs a fresh Authenticate.
                    warn!(
                        "pending_redeem: redeeming the authorized code for device {device_id} failed \
                         ({}); the operator must click Authenticate again",
                        redact_code(&e, &code)
                    );
                    state.failed(now);
                    PassResult::RedeemFailed
                }
            }
        }
        other => {
            let key = other.failure_key();
            let wait = failure_backoff(state.consecutive_failures.saturating_add(1));
            if key != state.last_warned {
                warn!(
                    "pending_redeem: pending-redeem poll for device {device_id} failed: {} \
                     (next poll in {}s)",
                    describe_failure(&other),
                    wait.as_secs()
                );
                state.last_warned = key;
            } else {
                debug!(
                    "pending_redeem: pending-redeem poll for device {device_id} still failing: {}",
                    describe_failure(&other)
                );
            }
            state.failed(now);
            PassResult::PollFailed
        }
    }
}

fn describe_failure(outcome: &PollOutcome) -> String {
    match outcome {
        PollOutcome::Refused {
            status,
            detail_code,
        } => format!(
            "refused HTTP {status} ({})",
            detail_code.as_deref().unwrap_or("no detail.code")
        ),
        PollOutcome::Unexpected { status: 200 } => "HTTP 200 without a usable code — if an \
             operator authorized this device the code is now spent; they must click \
             Authenticate again"
            .to_string(),
        PollOutcome::Unexpected { status } => format!("unexpected HTTP {status}"),
        PollOutcome::Transport(e) => format!("transport error: {e}"),
        PollOutcome::Pending { .. } | PollOutcome::NothingPending => "ok".to_string(),
    }
}

/// The live effects.
struct LiveEffects;

impl RedeemEffects for LiveEffects {
    /// The redeem steps of `commands::web_integration::redeem_pair_code`
    /// MINUS its interactive-sign-out clear. That command is an EXPLICIT local
    /// acquisition, so it ends a local logout; this is a background path a
    /// remote operator triggers, and the command's own comment says the clear
    /// must never run from one (it would silently un-logout the local user).
    /// [`run_pass`] does not poll while signed out at all; this keeps the
    /// marker untouched even if that gate is ever bypassed. The device id is
    /// the one the poll named, never re-resolved.
    async fn redeem(
        &self,
        web_base: &str,
        code: &RedeemCode,
        device_id: &str,
    ) -> Result<(), String> {
        use qontinui_runner_lib::pair::{pair_with_pair_code, persist_pairing};
        let (base, code, did) = (web_base.to_string(), code.clone(), device_id.to_string());
        let resp = spawn_blocking_tracked(move || pair_with_pair_code(&base, code.expose(), &did))
            .await
            .map_err(|e| format!("pair-code redeem task panicked: {e}"))??;
        let tenant_id = resp
            .tenant_id
            .as_deref()
            .and_then(|t| uuid::Uuid::parse_str(t.trim()).ok())
            .ok_or_else(|| "redeem response carried no usable tenant_id".to_string())?;
        persist_pairing(&resp, tenant_id).map_err(|e| format!("persist pairing: {e}"))?;
        crate::mcp::device_jwt_refresher::retire_rejection_streaks_after_pairing(
            tenant_id,
            crate::auth::default_binding_tenant(),
        );
        crate::commands::web_integration::promote_tier_after_pairing("pending_redeem");
        crate::mcp::device_jwt_refresher::commands::kick_device_jwt_refresher().await;
        Ok(())
    }

    async fn clear_anchor(&self) {
        if let Err(e) = crate::auth::AuthManager::new().reset_redeem_anchor() {
            warn!("pending_redeem: could not reset the spent redeem anchor: {e:#}");
        }
    }

    async fn kick_relay(&self) {
        crate::mcp::backend_relay::commands::kick_cloud_relay().await;
    }
}

fn poll_state_cell() -> &'static tokio::sync::Mutex<PollState> {
    static CELL: std::sync::OnceLock<tokio::sync::Mutex<PollState>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| tokio::sync::Mutex::new(PollState::default()))
}

/// Every stored device JWT — the retained redeem anchor, the legacy
/// `access_token` slot and each per-tenant slot — expired or not. The anchor
/// is what a stranded device still holds after its slots were cleared.
fn stored_device_jwts() -> Vec<String> {
    let am = crate::auth::AuthManager::new();
    let mut out = Vec::new();
    if let Ok(Some(t)) = am.get_redeem_anchor_jwt() {
        out.push(t);
    }
    if let Ok(t) = am.get_access_token() {
        out.push(t);
    }
    if let Ok(tenants) = am.try_list_tenant_device_jwt_tenants() {
        for tenant in tenants {
            if let Ok(Some(t)) = am.get_tenant_device_jwt(&tenant) {
                out.push(t);
            }
        }
    }
    out
}

/// Refresher hook: run one pass against the live posture, store and effects.
/// `web_base` is resolved by the caller exactly as the device-machine-key
/// exchange resolves it.
///
/// Returns `true` when a redeem landed, so the refresher re-derives its pass
/// from the fresh credential instead of acting on the decision it took before.
pub(crate) async fn on_refresher_pass(web_base: &str, wrong_tier: bool) -> bool {
    let mut state = poll_state_cell().lock().await;
    let inputs = PassInputs {
        posture: coord_credential_posture().map(|s| s.posture),
        wrong_tier,
        web_base,
        // Fails CLOSED on an unreadable store (reads as signed out).
        load_signed_out: || crate::auth::AuthManager::new().is_interactive_signed_out(),
        // The ONE resolution the poll, the redeem and the anchor slot share.
        load_device_id: crate::machine_identity::resolve_device_id,
        // A small synchronous store read, as the refresher's own
        // `probe_access_token` is; it runs only on a dark, un-throttled pass.
        load_anchor: |device_id: &str| {
            select_anchor(
                device_id,
                &stored_device_jwts(),
                chrono::Utc::now().timestamp(),
            )
        },
    };
    run_pass(inputs, &mut state, Instant::now(), &LiveEffects).await == PassResult::Redeemed
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Path, State},
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::get,
        Router,
    };
    use std::sync::{Arc, Mutex};

    const DID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const OTHER_DID: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    const CODE: &str = "Q7XK2P";

    /// A JWT-shaped token with the given claims (signature is not checked
    /// locally; web is the verifier).
    fn jwt(claims: serde_json::Value) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes());
        format!("{header}.{payload}.sig-SECRET-anchor")
    }

    const UID: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

    /// The exact shape coord's `issue_device` mints, which web's
    /// pending-redeem door requires.
    fn device_jwt(device_id: &str, exp: i64) -> String {
        jwt(serde_json::json!({
            "sub": format!("device:{device_id}"),
            "sub_type": "device",
            "device_id": device_id,
            "user_id": UID,
            "mint_provenance": "paired",
            "exp": exp,
        }))
    }

    #[derive(Clone)]
    struct MockState {
        status: StatusCode,
        body: Option<serde_json::Value>,
        hits: Arc<Mutex<u32>>,
        last_auth: Arc<Mutex<Option<String>>>,
        last_path_device: Arc<Mutex<Option<String>>>,
    }

    async fn handler(
        State(s): State<MockState>,
        Path(device_id): Path<String>,
        headers: HeaderMap,
    ) -> Response {
        *s.hits.lock().unwrap() += 1;
        *s.last_auth.lock().unwrap() = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        *s.last_path_device.lock().unwrap() = Some(device_id);
        match &s.body {
            Some(b) => (s.status, axum::Json(b.clone())).into_response(),
            None => s.status.into_response(),
        }
    }

    struct Mock {
        base: String,
        hits: Arc<Mutex<u32>>,
        last_auth: Arc<Mutex<Option<String>>>,
        last_path_device: Arc<Mutex<Option<String>>>,
        _shutdown: tokio::sync::oneshot::Sender<()>,
    }

    impl Mock {
        fn hits(&self) -> u32 {
            *self.hits.lock().unwrap()
        }
    }

    /// Mock qontinui-web serving only `pending-redeem`, on its own thread and
    /// runtime — the same shape as the refresher's pair-cli mock.
    fn spawn_web(status: StatusCode, body: Option<serde_json::Value>) -> Mock {
        let hits = Arc::new(Mutex::new(0u32));
        let last_auth = Arc::new(Mutex::new(None));
        let last_path_device = Arc::new(Mutex::new(None));
        let state = MockState {
            status,
            body,
            hits: hits.clone(),
            last_auth: last_auth.clone(),
            last_path_device: last_path_device.clone(),
        };
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
                let app: Router = Router::new()
                    .route("/api/v1/devices/{device_id}/pending-redeem", get(handler))
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
        Mock {
            base: format!("http://127.0.0.1:{port}"),
            hits,
            last_auth,
            last_path_device,
            _shutdown: tx,
        }
    }

    /// Records the order of effect calls and the code the redeem received.
    #[derive(Default)]
    struct FakeEffects {
        calls: Mutex<Vec<&'static str>>,
        redeemed_code: Mutex<Option<String>>,
        redeemed_base: Mutex<Option<String>>,
        redeemed_device: Mutex<Option<String>>,
        /// When set, `redeem` fails with this message (the code is appended,
        /// as the real redeem path's URL-bearing errors do).
        fail_with: Option<&'static str>,
    }

    impl RedeemEffects for FakeEffects {
        async fn redeem(
            &self,
            web_base: &str,
            code: &RedeemCode,
            device_id: &str,
        ) -> Result<(), String> {
            self.calls.lock().unwrap().push("redeem");
            *self.redeemed_device.lock().unwrap() = Some(device_id.to_string());
            *self.redeemed_code.lock().unwrap() = Some(code.expose().to_string());
            *self.redeemed_base.lock().unwrap() = Some(web_base.to_string());
            match self.fail_with {
                Some(msg) => Err(format!(
                    "POST {web_base}/api/v1/devices/pair-codes/{}/redeem -> {msg}",
                    code.expose().to_uppercase()
                )),
                None => Ok(()),
            }
        }

        async fn clear_anchor(&self) {
            self.calls.lock().unwrap().push("clear_anchor");
        }

        async fn kick_relay(&self) {
            self.calls.lock().unwrap().push("kick_relay");
        }
    }

    impl FakeEffects {
        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    fn now_unix() -> i64 {
        chrono::Utc::now().timestamp()
    }

    /// Run one pass with a fixed device id and anchor token.
    async fn pass(
        posture: Option<CoordCredentialPosture>,
        web_base: &str,
        anchor: Option<String>,
        state: &mut PollState,
        now: Instant,
        effects: &FakeEffects,
    ) -> PassResult {
        pass_with(posture, false, false, web_base, anchor, state, now, effects).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn pass_with(
        posture: Option<CoordCredentialPosture>,
        wrong_tier: bool,
        signed_out: bool,
        web_base: &str,
        anchor: Option<String>,
        state: &mut PollState,
        now: Instant,
        effects: &FakeEffects,
    ) -> PassResult {
        run_pass(
            PassInputs {
                posture,
                wrong_tier,
                web_base,
                load_signed_out: move || signed_out,
                load_device_id: || Some(DID.to_string()),
                load_anchor: move |_did: &str| anchor,
            },
            state,
            now,
            effects,
        )
        .await
    }

    #[tokio::test]
    async fn live_posture_never_polls() {
        let web = spawn_web(StatusCode::OK, Some(serde_json::json!({"code": CODE})));
        let fx = FakeEffects::default();
        let anchor = device_jwt(DID, now_unix() - 60);
        for posture in [
            Some(CoordCredentialPosture::Live),
            Some(CoordCredentialPosture::Expiring),
            None, // UNKNOWN is not dark
        ] {
            let mut state = PollState::default();
            let r = pass(
                posture,
                &web.base,
                Some(anchor.clone()),
                &mut state,
                Instant::now(),
                &fx,
            )
            .await;
            assert_eq!(r, PassResult::NotEligible, "{posture:?}");
        }
        assert_eq!(web.hits(), 0, "a non-dark posture must not reach web");
        assert!(fx.calls().is_empty());
    }

    #[tokio::test]
    async fn expired_posture_with_204_polls_once_and_does_not_redeem() {
        let web = spawn_web(StatusCode::NO_CONTENT, None);
        let fx = FakeEffects::default();
        let anchor = device_jwt(DID, now_unix() - 3600);
        let mut state = PollState::default();
        let now = Instant::now();
        let r = pass(
            Some(CoordCredentialPosture::Expired),
            &web.base,
            Some(anchor.clone()),
            &mut state,
            now,
            &fx,
        )
        .await;
        assert_eq!(r, PassResult::NothingPending);
        assert_eq!(web.hits(), 1);
        assert_eq!(
            web.last_auth.lock().unwrap().as_deref(),
            Some(format!("Bearer {anchor}").as_str()),
            "the expired device JWT is the bearer"
        );
        assert_eq!(web.last_path_device.lock().unwrap().as_deref(), Some(DID));
        assert!(fx.calls().is_empty(), "204 → nothing redeemed");

        // A kicked pass seconds later is spaced out, not a second request.
        let r = pass(
            Some(CoordCredentialPosture::Expired),
            &web.base,
            Some(anchor.clone()),
            &mut state,
            now + Duration::from_secs(5),
            &fx,
        )
        .await;
        assert_eq!(r, PassResult::Throttled);
        assert_eq!(web.hits(), 1);

        // The next refresher cadence polls again.
        let r = pass(
            Some(CoordCredentialPosture::Expired),
            &web.base,
            Some(anchor),
            &mut state,
            now + Duration::from_secs(300),
            &fx,
        )
        .await;
        assert_eq!(r, PassResult::NothingPending);
        assert_eq!(web.hits(), 2);
    }

    #[tokio::test]
    async fn expired_posture_with_200_redeems_then_kicks_the_relay() {
        let web = spawn_web(
            StatusCode::OK,
            Some(serde_json::json!({"code": CODE, "expires_at": "2026-09-30T12:00:00Z"})),
        );
        for posture in [
            CoordCredentialPosture::Expired,
            CoordCredentialPosture::Unrefreshable,
            CoordCredentialPosture::Absent,
            CoordCredentialPosture::Dark(
                crate::mcp::device_jwt_refresher::DarkCause::UpstreamRejected,
            ),
        ] {
            let fx = FakeEffects::default();
            let mut state = PollState::default();
            let r = pass(
                Some(posture),
                &web.base,
                Some(device_jwt(DID, now_unix() - 86_400)),
                &mut state,
                Instant::now(),
                &fx,
            )
            .await;
            assert_eq!(r, PassResult::Redeemed, "{posture:?}");
            assert_eq!(
                fx.calls(),
                vec!["redeem", "clear_anchor", "kick_relay"],
                "redeem, then drop the spent anchor, then kick"
            );
            assert_eq!(fx.redeemed_code.lock().unwrap().as_deref(), Some(CODE));
            assert_eq!(
                fx.redeemed_base.lock().unwrap().as_deref(),
                Some(web.base.as_str())
            );
            assert_eq!(
                fx.redeemed_device.lock().unwrap().as_deref(),
                Some(DID),
                "the redeem names the device the poll named"
            );
        }
    }

    #[tokio::test]
    async fn no_anchor_makes_no_request() {
        let web = spawn_web(StatusCode::NO_CONTENT, None);
        let fx = FakeEffects::default();
        let mut state = PollState::default();
        let r = pass(
            Some(CoordCredentialPosture::Absent),
            &web.base,
            None,
            &mut state,
            Instant::now(),
            &fx,
        )
        .await;
        assert_eq!(r, PassResult::NoAnchor);
        assert_eq!(web.hits(), 0);
    }

    #[tokio::test]
    async fn a_refusal_is_typed_and_backs_off() {
        let web = spawn_web(
            StatusCode::FORBIDDEN,
            Some(serde_json::json!({"detail": {"code": "device_credential_revoked"}})),
        );
        let outcome = poll_pending_redeem(&web.base, DID, &device_jwt(DID, now_unix())).await;
        assert_eq!(
            outcome,
            PollOutcome::Refused {
                status: 403,
                detail_code: Some("device_credential_revoked".to_string())
            }
        );

        let fx = FakeEffects::default();
        let mut state = PollState::default();
        let now = Instant::now();
        let anchor = device_jwt(DID, now_unix() - 60);
        let r = pass(
            Some(CoordCredentialPosture::Expired),
            &web.base,
            Some(anchor.clone()),
            &mut state,
            now,
            &fx,
        )
        .await;
        assert_eq!(r, PassResult::PollFailed);
        // The first failure waits MIN_POLL_SPACING, so one spacing later polls.
        let r = pass(
            Some(CoordCredentialPosture::Expired),
            &web.base,
            Some(anchor.clone()),
            &mut state,
            now + MIN_POLL_SPACING,
            &fx,
        )
        .await;
        assert_eq!(r, PassResult::PollFailed);
        // Second consecutive failure doubled the wait.
        let r = pass(
            Some(CoordCredentialPosture::Expired),
            &web.base,
            Some(anchor),
            &mut state,
            now + MIN_POLL_SPACING + MIN_POLL_SPACING,
            &fx,
        )
        .await;
        assert_eq!(r, PassResult::Throttled);
        assert_eq!(web.hits(), 3, "one direct poll + two passes");
        assert!(fx.calls().is_empty());
    }

    /// Revoke → Authenticate end to end, runner side: while the device is
    /// revoked web answers 403 `device_credential_revoked` and the runner backs
    /// off; once an operator authorizes, the next due poll collects the code.
    #[tokio::test]
    async fn a_revoked_device_polls_and_collects_the_code_once_authorized() {
        for posture in [
            CoordCredentialPosture::Unrefreshable,
            CoordCredentialPosture::Expired,
            CoordCredentialPosture::Dark(
                crate::mcp::device_jwt_refresher::DarkCause::UpstreamRejected,
            ),
        ] {
            assert!(posture_wants_poll(Some(posture)), "{posture:?}");
        }
        let anchor = device_jwt(DID, now_unix() - 600);
        let fx = FakeEffects::default();
        let mut state = PollState::default();
        let now = Instant::now();

        let revoked = spawn_web(
            StatusCode::FORBIDDEN,
            Some(serde_json::json!({"detail": {"code": "device_credential_revoked"}})),
        );
        let r = pass(
            Some(CoordCredentialPosture::Unrefreshable),
            &revoked.base,
            Some(anchor.clone()),
            &mut state,
            now,
            &fx,
        )
        .await;
        assert_eq!(r, PassResult::PollFailed);
        assert_eq!(revoked.hits(), 1);

        let authorized = spawn_web(StatusCode::OK, Some(serde_json::json!({"code": CODE})));
        let r = pass(
            Some(CoordCredentialPosture::Unrefreshable),
            &authorized.base,
            Some(anchor),
            &mut state,
            now + MIN_POLL_SPACING,
            &fx,
        )
        .await;
        assert_eq!(r, PassResult::Redeemed);
        assert_eq!(fx.calls(), vec!["redeem", "clear_anchor", "kick_relay"]);
    }

    #[test]
    fn failure_backoff_doubles_and_caps() {
        assert_eq!(failure_backoff(1), MIN_POLL_SPACING);
        assert_eq!(failure_backoff(2), MIN_POLL_SPACING * 2);
        assert_eq!(failure_backoff(3), MIN_POLL_SPACING * 4);
        assert_eq!(failure_backoff(50), FAILURE_BACKOFF_MAX);
    }

    #[test]
    fn anchor_selection_requires_this_device_a_device_token_and_the_grace_window() {
        let now = 1_800_000_000;
        let ok_old = device_jwt(DID, now - 10 * 86_400);
        let ok_new = device_jwt(DID, now - 3600);
        let other_device = device_jwt(OTHER_DID, now);
        let past_grace = device_jwt(DID, now - 31 * 86_400);
        let shaped = |patch: serde_json::Value| {
            let mut c = serde_json::json!({
                "sub": format!("device:{DID}"),
                "sub_type": "device",
                "device_id": DID,
                "user_id": UID,
                "exp": now,
            });
            for (k, v) in patch.as_object().unwrap() {
                if v.is_null() {
                    c.as_object_mut().unwrap().remove(k);
                } else {
                    c[k] = v.clone();
                }
            }
            jwt(c)
        };
        let pre_provenance = shaped(serde_json::json!({}));
        let agent = shaped(serde_json::json!({"sub_type": "agent"}));
        let no_sub_type = shaped(serde_json::json!({"sub_type": null}));
        let push_token = shaped(serde_json::json!({"sub": "push:abc", "user_id": null}));
        let wrong_sub = shaped(serde_json::json!({"sub": format!("device:{OTHER_DID}")}));
        let no_user = shaped(serde_json::json!({"user_id": null}));
        let bootstrap = shaped(serde_json::json!({"mint_provenance": "bootstrap"}));
        let opaque = "qontinui_runner_opaque".to_string();

        assert_eq!(
            select_anchor(DID, std::slice::from_ref(&pre_provenance), now),
            Some(pre_provenance.clone()),
            "absent mint_provenance is admitted, as web admits it"
        );

        assert_eq!(
            select_anchor(
                DID,
                &[
                    ok_old.clone(),
                    other_device.clone(),
                    ok_new.clone(),
                    past_grace.clone()
                ],
                now
            ),
            Some(ok_new),
            "the latest in-grace token for THIS device wins"
        );
        assert_eq!(
            select_anchor(DID, std::slice::from_ref(&ok_old), now),
            Some(ok_old.clone())
        );
        for rejected in [
            other_device,
            past_grace,
            agent,
            no_sub_type,
            push_token,
            wrong_sub,
            no_user,
            bootstrap,
            opaque,
        ] {
            assert_eq!(
                select_anchor(DID, std::slice::from_ref(&rejected), now),
                None,
                "{rejected}"
            );
        }
    }

    /// An explicit local logout is the user's withdrawal: no poll, so no web
    /// operator can re-pair the box over it.
    #[tokio::test]
    async fn a_local_logout_is_respected_and_nothing_is_polled() {
        let web = spawn_web(StatusCode::OK, Some(serde_json::json!({"code": CODE})));
        let fx = FakeEffects::default();
        let mut state = PollState::default();
        let r = pass_with(
            Some(CoordCredentialPosture::Expired),
            false,
            true,
            &web.base,
            Some(device_jwt(DID, now_unix() - 60)),
            &mut state,
            Instant::now(),
            &fx,
        )
        .await;
        assert_eq!(r, PassResult::LocallySignedOut);
        assert_eq!(web.hits(), 0);
        assert!(fx.calls().is_empty());
    }

    /// `IdleWrongTier`: a runner deliberately below the account tier does not
    /// talk to the cloud, and a redeem would promote it over that choice.
    #[tokio::test]
    async fn a_wrong_tier_runner_never_polls() {
        let web = spawn_web(StatusCode::OK, Some(serde_json::json!({"code": CODE})));
        let fx = FakeEffects::default();
        let mut state = PollState::default();
        let r = pass_with(
            Some(CoordCredentialPosture::Unrefreshable),
            true,
            false,
            &web.base,
            Some(device_jwt(DID, now_unix() - 60)),
            &mut state,
            Instant::now(),
            &fx,
        )
        .await;
        assert_eq!(r, PassResult::WrongTier);
        assert_eq!(web.hits(), 0);
        assert!(fx.calls().is_empty());
    }

    /// The live redeem must never clear the interactive-sign-out marker —
    /// `redeem_pair_code`'s own comment forbids that from a background path. A
    /// behavioural test cannot drive a network redeem into a real store, so
    /// this pins it at the source (the technique the refresher's
    /// `every_in_process_re_pair_path_retires_the_stale_rejection_streak`
    /// uses), comments stripped so prose cannot satisfy or trip it.
    #[test]
    fn the_live_redeem_never_clears_the_sign_out_marker() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mcp/pending_redeem.rs");
        let text = std::fs::read_to_string(&path).expect("read pending_redeem.rs");
        let (live, _tests) = text
            .split_once("#[cfg(test)]\nmod tests")
            .expect("test module marker");
        let code: String = live
            .lines()
            .map(|l| l.split_once("//").map_or(l, |(c, _)| c))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code.contains("persist_pairing("),
            "the live redeem persists"
        );
        assert!(
            !code.contains("clear_interactive_signed_out"),
            "the refresher's redeem must not end a local logout"
        );
        assert!(
            !code.contains("redeem_pair_code("),
            "the interactive command clears the marker; this path must not call it"
        );
    }

    #[test]
    fn a_redeem_code_never_prints_through_debug() {
        let code = RedeemCode(CODE.to_string());
        assert!(!format!("{code:?}").contains(CODE));
        let outcome = PollOutcome::Pending { code };
        assert!(!format!("{outcome:?}").contains(CODE));
    }

    #[test]
    fn redaction_covers_both_spellings() {
        let code = RedeemCode("q7xk2p".to_string());
        let msg = "POST http://h/api/v1/devices/pair-codes/Q7XK2P/redeem -> 410 (q7xk2p gone)";
        let out = redact_code(msg, &code);
        assert!(!out.to_lowercase().contains("q7xk2p"), "{out}");
    }

    /// A `MakeWriter` that appends every formatted log line to a shared buffer.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Neither the pair code nor the anchor token may appear in any log line,
    /// on the success path or on a failed redeem whose error embeds the code.
    /// Current-thread runtime, so the thread-local subscriber sees every poll.
    #[tokio::test(flavor = "current_thread")]
    async fn the_code_and_the_token_never_reach_the_logs() {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(capture.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let web = spawn_web(StatusCode::OK, Some(serde_json::json!({"code": CODE})));
        let anchor = device_jwt(DID, now_unix() - 600);

        let ok = FakeEffects::default();
        let mut state = PollState::default();
        let r = pass(
            Some(CoordCredentialPosture::Expired),
            &web.base,
            Some(anchor.clone()),
            &mut state,
            Instant::now(),
            &ok,
        )
        .await;
        assert_eq!(r, PassResult::Redeemed);

        let failing = FakeEffects {
            fail_with: Some("HTTP 410: pair code already redeemed"),
            ..FakeEffects::default()
        };
        let mut state = PollState::default();
        let r = pass(
            Some(CoordCredentialPosture::Expired),
            &web.base,
            Some(anchor.clone()),
            &mut state,
            Instant::now(),
            &failing,
        )
        .await;
        assert_eq!(r, PassResult::RedeemFailed);

        let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(
            logs.contains("pending_redeem"),
            "the capture saw the module's lines: {logs}"
        );
        assert!(
            logs.contains("<code>"),
            "the failed redeem was logged, redacted: {logs}"
        );
        assert!(
            !logs.to_uppercase().contains(CODE),
            "pair code leaked: {logs}"
        );
        assert!(!logs.contains(&anchor), "anchor token leaked: {logs}");
        assert!(
            !logs.contains("SECRET-anchor"),
            "token signature leaked: {logs}"
        );
    }
}
