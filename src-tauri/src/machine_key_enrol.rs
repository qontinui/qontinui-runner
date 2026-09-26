//! Device machine key (`dmk_`) enrolment through qontinui-web's self-mint
//! route, and the runner's view of whether it holds a usable key.
//!
//! Plan `2026-09-24-runner-coord-credential-stranded-after-outage` Phase 3
//! (runner half). The `dmk_` is the ONLY credential that re-derives a device
//! JWT after the JWT has expired (web's `/machine-credential/exchange`). Until
//! this module, the runner acquired one only from the pair-cli response, so a
//! device paired by pair code or by the browser flow never held one — and an
//! outage longer than its JWT's remaining life stranded it until an operator
//! re-paired.
//!
//! The route: `POST {web_base}/api/v1/devices/{device_id}/machine-credential/self-mint`,
//! authenticated by `Authorization: Bearer <live paired device JWT>` whose
//! `device_id` claim equals the path id, no body. Answers (qontinui-web
//! `self_mint_device_machine_credential`):
//!
//! | status | meaning | here |
//! |---|---|---|
//! | 201 | minted; the plaintext `dmk_` is in the body ONCE, with `expires_at` | [`SelfMintHttp::Minted`] |
//! | 409 `machine_key_still_usable` | web holds a key usable for > 7 days; nothing to do | [`SelfMintHttp::StillUsable`] |
//! | 401 / 403 | token expired or not a paired device token, device mismatch, coord refused the token, key revoked | [`SelfMintHttp::Refused`] |
//! | 404 / 405 | a web build that predates the route | [`SelfMintHttp::Unsupported`] |
//! | 429 / 5xx (503 coord unreachable, 502 coord answer malformed) / network | transient | [`SelfMintHttp::Transient`] |
//!
//! Callers:
//! - the device-JWT refresher, after every SUCCESSFUL refresh
//!   ([`spawn_ensure_after_refresh`]) — gated by [`plan_enrolment`] and
//!   rate-limited by [`EnrolGate`], so a refusal or an outage never hammers web;
//! - the pair-code and browser pairing paths, right after a successful pairing
//!   ([`self_mint_blocking`], via `pair::enrol_machine_key_into`) —
//!   best-effort, never failing the pairing.
//!
//! Step 4 ("make dmk-absent loud"): [`observe_machine_key_state`] logs ONE line
//! per state change, and [`machine_key_health_state`] feeds
//! `/health.coordCredential.machineKey`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use tracing::{info, warn};

/// Web renews a key through self-mint only when it is absent, expired, or
/// expires within this window (`SELF_MINT_RENEWAL_WINDOW` in qontinui-web).
/// The runner uses the same window to decide when to ask.
pub const RENEWAL_WINDOW_SECS: i64 = 7 * 24 * 60 * 60;

/// How long the runner waits before asking again after each non-success.
const BACKOFF_TRANSIENT_SECS: i64 = 60 * 60;
const BACKOFF_REFUSED_SECS: i64 = 6 * 60 * 60;
const BACKOFF_STILL_USABLE_SECS: i64 = 24 * 60 * 60;
const BACKOFF_UNSUPPORTED_SECS: i64 = 24 * 60 * 60;

/// HTTP budget for one self-mint call. Web's own coord lookup is bounded at
/// 5 s, so 15 s leaves room for it without holding a pairing hostage.
const SELF_MINT_TIMEOUT: Duration = Duration::from_secs(15);

/// Read/write access to the stored machine key. Implemented by
/// `auth::AuthManager` (in both crates `auth.rs` compiles into) so the
/// refresher's enrolment goes through the same test-injectable storage as
/// every other credential.
pub trait MachineKeyStore {
    /// `Ok(None)` when no key is stored; `Err` only on an unreadable store.
    fn machine_key(&self) -> anyhow::Result<Option<String>>;
    /// `Ok(None)` when the expiry is unknown or no key is stored.
    fn machine_key_expires_at(&self) -> anyhow::Result<Option<i64>>;
    /// Persist a freshly enrolled key and its reported expiry.
    fn store_machine_key(&self, key: &str, expires_at: Option<i64>) -> anyhow::Result<()>;
}

/// What the store holds, collapsed to the three cases the decision needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredKey {
    /// The credential store could not be read — UNKNOWN, never "absent".
    Unreadable,
    Absent,
    /// A key is held; `expires_at` is `None` when its expiry is unknown (a
    /// key from the pair-cli response, or one stored before expiry was kept).
    Present {
        expires_at: Option<i64>,
    },
}

/// Read the store once into a [`StoredKey`].
pub fn read_stored_key<S: MachineKeyStore + ?Sized>(store: &S) -> StoredKey {
    match store.machine_key() {
        Err(_) => StoredKey::Unreadable,
        Ok(None) => StoredKey::Absent,
        Ok(Some(k)) if k.trim().is_empty() => StoredKey::Absent,
        Ok(Some(_)) => match store.machine_key_expires_at() {
            Ok(expires_at) => StoredKey::Present { expires_at },
            Err(_) => StoredKey::Unreadable,
        },
    }
}

/// The `/health.coordCredential.machineKey` vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineKeyState {
    /// A key is held and is not known to expire within
    /// [`RENEWAL_WINDOW_SECS`] (including a key whose expiry is unknown).
    Present,
    /// No key is held: an expired device JWT cannot be recovered unattended.
    Absent,
    /// A key is held but its reported expiry is within
    /// [`RENEWAL_WINDOW_SECS`] (or already past).
    Expiring,
    /// Not assessed in this process yet, or the store could not be read.
    Unknown,
}

impl MachineKeyState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Absent => "absent",
            Self::Expiring => "expiring",
            Self::Unknown => "unknown",
        }
    }
}

/// Map what the store holds onto the health vocabulary at `now`.
pub fn classify(stored: StoredKey, now: i64) -> MachineKeyState {
    match stored {
        StoredKey::Unreadable => MachineKeyState::Unknown,
        StoredKey::Absent => MachineKeyState::Absent,
        StoredKey::Present {
            expires_at: Some(exp),
        } if exp - now <= RENEWAL_WINDOW_SECS => MachineKeyState::Expiring,
        StoredKey::Present { .. } => MachineKeyState::Present,
    }
}

/// Why the runner asks web for a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrolReason {
    /// No key is held.
    Absent,
    /// The held key's reported expiry is within the renewal window.
    Expiring,
    /// A key is held but its expiry is unknown. Web decides: it answers 409
    /// for a key usable beyond 7 days (after which the runner waits a day)
    /// and renews one inside the window.
    ExpiryUnknown,
}

/// Why the runner does not ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The held key is usable beyond the renewal window.
    Usable,
    /// The store could not be read; asking and then storing would risk
    /// writing over a store this process cannot see.
    StoreUnreadable,
    /// No web base URL is configured.
    NoWebBase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrolPlan {
    Enrol(EnrolReason),
    Skip(SkipReason),
}

/// Plan step 3: enrol when the key is absent, or known to expire within
/// [`RENEWAL_WINDOW_SECS`]. A key of unknown expiry is also asked about,
/// since web alone knows its expiry (see [`EnrolReason::ExpiryUnknown`]).
pub fn plan_enrolment(stored: StoredKey, now: i64) -> EnrolPlan {
    match stored {
        StoredKey::Unreadable => EnrolPlan::Skip(SkipReason::StoreUnreadable),
        StoredKey::Absent => EnrolPlan::Enrol(EnrolReason::Absent),
        StoredKey::Present {
            expires_at: Some(exp),
        } if exp - now <= RENEWAL_WINDOW_SECS => EnrolPlan::Enrol(EnrolReason::Expiring),
        StoredKey::Present {
            expires_at: Some(_),
        } => EnrolPlan::Skip(SkipReason::Usable),
        StoredKey::Present { expires_at: None } => EnrolPlan::Enrol(EnrolReason::ExpiryUnknown),
    }
}

/// Web's answer to one self-mint call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfMintHttp {
    /// 2xx: the plaintext key, returned ONCE, and its expiry (unix seconds).
    Minted {
        key: String,
        expires_at: Option<i64>,
    },
    /// 409: web holds a key usable for more than 7 days. Not an error.
    StillUsable,
    /// 401 / 403 / any other 4xx: web will not mint for this token. `code` is
    /// the structured `detail.code` when web sent one.
    Refused { status: u16, code: Option<String> },
    /// 404 / 405: this web build has no self-mint route.
    Unsupported { status: u16 },
    /// Network failure, 429, any 5xx, or a 2xx whose body carried no key.
    Transient { status: Option<u16>, detail: String },
}

#[derive(Deserialize)]
struct MintBody {
    #[serde(default)]
    device_machine_key: String,
    #[serde(default)]
    expires_at: Option<String>,
}

/// Parse web's ISO-8601 `expires_at` (`IsoDatetime`) into unix seconds. A
/// value with no offset is read as UTC, which is what web stores.
pub fn parse_expires_at(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(dt.timestamp());
    }
    chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .map(|n| n.and_utc().timestamp())
}

fn detail_code(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("detail")?.get("code")?.as_str().map(str::to_string)
}

/// Classify a self-mint HTTP response. Pure, so the table in the module doc
/// is tested without a server.
pub fn classify_self_mint_response(status: u16, body: &str) -> SelfMintHttp {
    match status {
        200..=299 => match serde_json::from_str::<MintBody>(body) {
            Ok(b) if !b.device_machine_key.trim().is_empty() => SelfMintHttp::Minted {
                key: b.device_machine_key.trim().to_string(),
                expires_at: b.expires_at.as_deref().and_then(parse_expires_at),
            },
            Ok(_) => SelfMintHttp::Transient {
                status: Some(status),
                detail: "self-mint answered success with no device_machine_key".to_string(),
            },
            Err(e) => SelfMintHttp::Transient {
                status: Some(status),
                detail: format!("self-mint body decode failed: {e}"),
            },
        },
        409 => SelfMintHttp::StillUsable,
        404 | 405 => SelfMintHttp::Unsupported { status },
        429 => SelfMintHttp::Transient {
            status: Some(status),
            detail: "rate limited".to_string(),
        },
        400..=499 => SelfMintHttp::Refused {
            status,
            code: detail_code(body),
        },
        _ => SelfMintHttp::Transient {
            status: Some(status),
            detail: detail_code(body).unwrap_or_else(|| "server error".to_string()),
        },
    }
}

fn self_mint_url(web_base: &str, device_id: &str) -> String {
    format!(
        "{}/api/v1/devices/{}/machine-credential/self-mint",
        web_base.trim().trim_end_matches('/'),
        device_id.trim()
    )
}

/// One self-mint call (async). Never stores anything; see [`ensure_machine_key`].
pub async fn self_mint(
    client: &reqwest::Client,
    web_base: &str,
    device_id: &str,
    device_jwt: &str,
) -> SelfMintHttp {
    // coord-auth-exempt(not-coord): `qontinui-web`
    // `/api/v1/devices/{id}/machine-credential/self-mint`, authenticated by the
    // device's own live device JWT. It mints a device MACHINE KEY, not a JWT.
    let resp = match client
        .post(self_mint_url(web_base, device_id))
        .bearer_auth(device_jwt.trim())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return SelfMintHttp::Transient {
                status: None,
                detail: format!("request failed: {e}"),
            }
        }
    };
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    classify_self_mint_response(status, &body)
}

/// One self-mint call (blocking) — for the pairing paths, which already run
/// on a blocking thread (`reqwest::blocking`). Never stores anything.
pub fn self_mint_blocking(web_base: &str, device_id: &str, device_jwt: &str) -> SelfMintHttp {
    let client = match reqwest::blocking::Client::builder()
        .timeout(SELF_MINT_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return SelfMintHttp::Transient {
                status: None,
                detail: format!("client build failed: {e}"),
            }
        }
    };
    // coord-auth-exempt(not-coord): `qontinui-web`
    // `/api/v1/devices/{id}/machine-credential/self-mint`, authenticated by the
    // device JWT the pairing just returned.
    let resp = match client
        .post(self_mint_url(web_base, device_id))
        .bearer_auth(device_jwt.trim())
        .send()
    {
        Ok(r) => r,
        Err(e) => {
            return SelfMintHttp::Transient {
                status: None,
                detail: format!("request failed: {e}"),
            }
        }
    };
    let status = resp.status().as_u16();
    let body = resp.text().unwrap_or_default();
    classify_self_mint_response(status, &body)
}

/// What one enrolment attempt ended as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrolOutcome {
    /// A key was minted and stored.
    Enrolled {
        expires_at: Option<i64>,
    },
    /// Web holds a key usable for > 7 days (409). Nothing to do.
    StillUsable,
    Refused {
        status: u16,
        code: Option<String>,
    },
    Unsupported {
        status: u16,
    },
    Transient {
        status: Option<u16>,
        detail: String,
    },
    /// Web minted a key but it could not be stored. The plaintext is gone;
    /// web now holds a key this runner does not have.
    StoreFailed(String),
}

impl EnrolOutcome {
    /// Seconds before the next attempt is allowed, or `None` for no wait.
    fn backoff_secs(&self) -> Option<i64> {
        match self {
            Self::Enrolled { .. } => None,
            Self::StillUsable => Some(BACKOFF_STILL_USABLE_SECS),
            Self::Refused { .. } => Some(BACKOFF_REFUSED_SECS),
            Self::Unsupported { .. } => Some(BACKOFF_UNSUPPORTED_SECS),
            Self::Transient { .. } | Self::StoreFailed(_) => Some(BACKOFF_TRANSIENT_SECS),
        }
    }
}

/// Call self-mint and, on 201, store the key and its expiry.
pub async fn enrol_with<S: MachineKeyStore + ?Sized>(
    store: &S,
    client: &reqwest::Client,
    web_base: &str,
    device_id: &str,
    device_jwt: &str,
) -> EnrolOutcome {
    match self_mint(client, web_base, device_id, device_jwt).await {
        SelfMintHttp::Minted { key, expires_at } => match store.store_machine_key(&key, expires_at)
        {
            Ok(()) => EnrolOutcome::Enrolled { expires_at },
            Err(e) => EnrolOutcome::StoreFailed(format!("{e:#}")),
        },
        SelfMintHttp::StillUsable => EnrolOutcome::StillUsable,
        SelfMintHttp::Refused { status, code } => EnrolOutcome::Refused { status, code },
        SelfMintHttp::Unsupported { status } => EnrolOutcome::Unsupported { status },
        SelfMintHttp::Transient { status, detail } => EnrolOutcome::Transient { status, detail },
    }
}

/// Rate limit on enrolment attempts. An attempt claims the gate for
/// [`BACKOFF_TRANSIENT_SECS`] BEFORE calling web, so two concurrent triggers
/// cannot both call; the outcome then sets the real next-allowed time.
#[derive(Debug, Default)]
pub struct EnrolGate {
    next_attempt_at: Mutex<Option<i64>>,
}

impl EnrolGate {
    pub const fn new() -> Self {
        Self {
            next_attempt_at: Mutex::new(None),
        }
    }

    /// Claim the gate at `now`. `Err(until)` when an earlier outcome (or an
    /// attempt in flight) holds it.
    pub fn try_claim(&self, now: i64) -> Result<(), i64> {
        let mut next = self
            .next_attempt_at
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(until) = *next {
            if now < until {
                return Err(until);
            }
        }
        *next = Some(now + BACKOFF_TRANSIENT_SECS);
        Ok(())
    }

    /// Record an attempt's outcome, setting when the next may run.
    pub fn record(&self, outcome: &EnrolOutcome, now: i64) {
        let mut next = self
            .next_attempt_at
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *next = outcome.backoff_secs().map(|s| now + s);
    }
}

/// Remembers the last observed [`MachineKeyState`] so a change is logged
/// exactly once.
#[derive(Debug, Default)]
pub struct StateTracker {
    last: Mutex<Option<MachineKeyState>>,
}

impl StateTracker {
    pub const fn new() -> Self {
        Self {
            last: Mutex::new(None),
        }
    }

    /// Record `state`. Returns `true` when it differs from the previous
    /// observation (and logs one line: warn for anything but `present`).
    pub fn observe(&self, state: MachineKeyState) -> bool {
        let mut last = self.last.lock().unwrap_or_else(|p| p.into_inner());
        if *last == Some(state) {
            return false;
        }
        let previous = last.map(MachineKeyState::as_str).unwrap_or("unobserved");
        *last = Some(state);
        match state {
            MachineKeyState::Present => info!(
                "device machine key: {previous} -> present — an expired device JWT can be \
                 re-derived through web's machine-credential exchange"
            ),
            MachineKeyState::Absent => warn!(
                "device machine key: {previous} -> ABSENT — if the device JWT expires (an \
                 outage longer than its life), this runner cannot recover without an operator \
                 re-pair; enrolment via web self-mint runs after each successful refresh"
            ),
            MachineKeyState::Expiring => warn!(
                "device machine key: {previous} -> EXPIRING — the stored key lapses within 7 \
                 days; enrolment via web self-mint runs after each successful refresh"
            ),
            MachineKeyState::Unknown => warn!(
                "device machine key: {previous} -> UNKNOWN — the credential store could not \
                 be read"
            ),
        }
        true
    }

    pub fn current(&self) -> MachineKeyState {
        self.last
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .unwrap_or(MachineKeyState::Unknown)
    }
}

static GATE: EnrolGate = EnrolGate::new();
static TRACKER: StateTracker = StateTracker::new();

/// Record the process-wide machine-key state (one log line per change).
pub fn observe_machine_key_state(state: MachineKeyState) -> bool {
    TRACKER.observe(state)
}

/// The process-wide machine-key state for `/health.coordCredential.machineKey`
/// — `unknown` until something in this process has read the store.
pub fn machine_key_health_state() -> MachineKeyState {
    TRACKER.current()
}

/// What [`ensure_machine_key`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureResult {
    Skipped(SkipReason),
    /// An enrolment was due but an earlier outcome's backoff holds until this
    /// unix second.
    RateLimited {
        until: i64,
    },
    Attempted {
        reason: EnrolReason,
        outcome: EnrolOutcome,
    },
}

/// Read the stored key, record its state, and — when [`plan_enrolment`] says
/// so and `gate` allows — enrol through self-mint. Called after a successful
/// device-JWT refresh with that refresh's (live) JWT.
#[allow(clippy::too_many_arguments)]
pub async fn ensure_machine_key<S: MachineKeyStore + ?Sized>(
    store: &S,
    gate: &EnrolGate,
    tracker: &StateTracker,
    client: &reqwest::Client,
    web_base: &str,
    device_id: &str,
    device_jwt: &str,
    now: i64,
) -> EnsureResult {
    let stored = read_stored_key(store);
    tracker.observe(classify(stored, now));
    let reason = match plan_enrolment(stored, now) {
        EnrolPlan::Skip(why) => return EnsureResult::Skipped(why),
        EnrolPlan::Enrol(reason) => reason,
    };
    if web_base.trim().is_empty() {
        return EnsureResult::Skipped(SkipReason::NoWebBase);
    }
    if let Err(until) = gate.try_claim(now) {
        return EnsureResult::RateLimited { until };
    }
    let outcome = enrol_with(store, client, web_base, device_id, device_jwt).await;
    gate.record(&outcome, now);
    match &outcome {
        EnrolOutcome::Enrolled { expires_at } => info!(
            "device machine key enrolled via web self-mint ({reason:?}; expires_at={expires_at:?})"
        ),
        EnrolOutcome::StillUsable if stored == StoredKey::Absent => warn!(
            "device machine key: web holds a key usable for > 7 days that this runner does not \
             have, so self-mint will not replace it — the device owner must re-mint it \
             (user-bearer /machine-credential/mint) or re-pair"
        ),
        EnrolOutcome::StillUsable => {
            info!("device machine key: web reports the key usable for > 7 days")
        }
        other => warn!("device machine key: self-mint enrolment did not complete: {other:?}"),
    }
    tracker.observe(classify(read_stored_key(store), now));
    EnsureResult::Attempted { reason, outcome }
}

/// The refresher's post-refresh hook: run [`ensure_machine_key`] on its own
/// task against the process-wide gate and tracker, so the refresh pass never
/// waits on web.
pub fn spawn_ensure_after_refresh<S>(
    store: Arc<S>,
    web_base: String,
    device_id: String,
    device_jwt: String,
) where
    S: MachineKeyStore + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(SELF_MINT_TIMEOUT)
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                warn!("device machine key: self-mint client build failed: {e}");
                return;
            }
        };
        let now = chrono::Utc::now().timestamp();
        let _ = ensure_machine_key(
            store.as_ref(),
            &GATE,
            &TRACKER,
            &client,
            &web_base,
            &device_id,
            &device_jwt,
            now,
        )
        .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const DAY: i64 = 24 * 60 * 60;
    const NOW: i64 = 1_800_000_000;
    const DID: &str = "0199a1b2-0000-7000-8000-000000000001";

    #[derive(Default)]
    struct MemStore {
        key: Mutex<Option<String>>,
        exp: Mutex<Option<i64>>,
        unreadable: bool,
        fail_store: bool,
    }

    impl MachineKeyStore for MemStore {
        fn machine_key(&self) -> anyhow::Result<Option<String>> {
            if self.unreadable {
                anyhow::bail!("undecryptable");
            }
            Ok(self.key.lock().unwrap().clone())
        }
        fn machine_key_expires_at(&self) -> anyhow::Result<Option<i64>> {
            Ok(*self.exp.lock().unwrap())
        }
        fn store_machine_key(&self, key: &str, expires_at: Option<i64>) -> anyhow::Result<()> {
            if self.fail_store {
                anyhow::bail!("disk full");
            }
            *self.key.lock().unwrap() = Some(key.to_string());
            *self.exp.lock().unwrap() = expires_at;
            Ok(())
        }
    }

    fn store_with(key: Option<&str>, exp: Option<i64>) -> MemStore {
        MemStore {
            key: Mutex::new(key.map(str::to_string)),
            exp: Mutex::new(exp),
            ..Default::default()
        }
    }

    // ---- decision helper (plan step 3) ----

    #[test]
    fn absent_key_enrols() {
        assert_eq!(
            plan_enrolment(StoredKey::Absent, NOW),
            EnrolPlan::Enrol(EnrolReason::Absent)
        );
    }

    #[test]
    fn key_expiring_within_seven_days_enrols() {
        for left in [6 * DAY, 7 * DAY, 0, -DAY] {
            assert_eq!(
                plan_enrolment(
                    StoredKey::Present {
                        expires_at: Some(NOW + left)
                    },
                    NOW
                ),
                EnrolPlan::Enrol(EnrolReason::Expiring),
                "{left}s left must enrol"
            );
        }
    }

    #[test]
    fn key_with_thirty_days_left_skips() {
        assert_eq!(
            plan_enrolment(
                StoredKey::Present {
                    expires_at: Some(NOW + 30 * DAY)
                },
                NOW
            ),
            EnrolPlan::Skip(SkipReason::Usable)
        );
    }

    #[test]
    fn unknown_expiry_asks_web_and_unreadable_store_never_does() {
        assert_eq!(
            plan_enrolment(StoredKey::Present { expires_at: None }, NOW),
            EnrolPlan::Enrol(EnrolReason::ExpiryUnknown)
        );
        assert_eq!(
            plan_enrolment(StoredKey::Unreadable, NOW),
            EnrolPlan::Skip(SkipReason::StoreUnreadable)
        );
    }

    // ---- health state mapping (plan step 4) ----

    #[test]
    fn machine_key_health_state_mapping() {
        let cases = [
            (StoredKey::Absent, "absent"),
            (StoredKey::Unreadable, "unknown"),
            (StoredKey::Present { expires_at: None }, "present"),
            (
                StoredKey::Present {
                    expires_at: Some(NOW + 30 * DAY),
                },
                "present",
            ),
            (
                StoredKey::Present {
                    expires_at: Some(NOW + 3 * DAY),
                },
                "expiring",
            ),
            (
                StoredKey::Present {
                    expires_at: Some(NOW - DAY),
                },
                "expiring",
            ),
        ];
        for (stored, want) in cases {
            assert_eq!(classify(stored, NOW).as_str(), want, "{stored:?}");
        }
        assert_eq!(
            read_stored_key(&store_with(Some("  "), None)),
            StoredKey::Absent
        );
        let unreadable = MemStore {
            unreadable: true,
            ..Default::default()
        };
        assert_eq!(read_stored_key(&unreadable), StoredKey::Unreadable);
    }

    #[test]
    fn tracker_reports_each_change_once_and_starts_unknown() {
        let t = StateTracker::new();
        assert_eq!(t.current(), MachineKeyState::Unknown);
        assert!(t.observe(MachineKeyState::Absent));
        assert!(
            !t.observe(MachineKeyState::Absent),
            "same state: no second log"
        );
        assert!(t.observe(MachineKeyState::Present));
        assert_eq!(t.current(), MachineKeyState::Present);
    }

    // ---- response classification ----

    #[test]
    fn response_table() {
        assert_eq!(
            classify_self_mint_response(
                201,
                r#"{"device_id":"x","device_machine_key":"dmk_abc","prefix":"dmk_abc","expires_at":"2026-11-25T10:00:00Z"}"#
            ),
            SelfMintHttp::Minted {
                key: "dmk_abc".into(),
                expires_at: parse_expires_at("2026-11-25T10:00:00+00:00"),
            }
        );
        assert_eq!(
            classify_self_mint_response(409, r#"{"detail":{"code":"machine_key_still_usable"}}"#),
            SelfMintHttp::StillUsable
        );
        assert_eq!(
            classify_self_mint_response(
                403,
                r#"{"detail":{"code":"device_mismatch","message":"m"}}"#
            ),
            SelfMintHttp::Refused {
                status: 403,
                code: Some("device_mismatch".into())
            }
        );
        assert_eq!(
            classify_self_mint_response(401, r#"{"detail":"Device token expired."}"#),
            SelfMintHttp::Refused {
                status: 401,
                code: None
            }
        );
        assert_eq!(
            classify_self_mint_response(404, "{}"),
            SelfMintHttp::Unsupported { status: 404 }
        );
        for s in [429u16, 500, 502, 503] {
            assert!(
                matches!(
                    classify_self_mint_response(s, "{}"),
                    SelfMintHttp::Transient { .. }
                ),
                "{s} is transient"
            );
        }
        assert!(matches!(
            classify_self_mint_response(201, r#"{"device_machine_key":""}"#),
            SelfMintHttp::Transient { .. }
        ));
    }

    #[test]
    fn expires_at_parses_with_and_without_offset() {
        let z = parse_expires_at("2026-11-25T10:00:00Z").unwrap();
        assert_eq!(parse_expires_at("2026-11-25T10:00:00+00:00"), Some(z));
        assert_eq!(parse_expires_at("2026-11-25T10:00:00.123456"), Some(z));
        assert_eq!(parse_expires_at("not a date"), None);
    }

    // ---- hermetic HTTP: an in-process web mock ----

    struct Mock {
        base: String,
        hits: Arc<AtomicUsize>,
        bearer: Arc<Mutex<Option<String>>>,
        _shutdown: tokio::sync::oneshot::Sender<()>,
    }

    async fn spawn_web(status: u16, body: &'static str) -> Mock {
        use axum::{extract::Path, http::HeaderMap, routing::post, Router};
        let hits = Arc::new(AtomicUsize::new(0));
        let bearer = Arc::new(Mutex::new(None));
        let (h, b) = (hits.clone(), bearer.clone());
        let app = Router::new().route(
            "/api/v1/devices/{device_id}/machine-credential/self-mint",
            post(move |Path(device_id): Path<String>, headers: HeaderMap| {
                let (h, b) = (h.clone(), b.clone());
                async move {
                    assert_eq!(device_id, DID, "the path carries this device's id");
                    h.fetch_add(1, Ordering::SeqCst);
                    *b.lock().unwrap() = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        [("content-type", "application/json")],
                        body,
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .unwrap();
        });
        Mock {
            base: format!("http://{addr}"),
            hits,
            bearer,
            _shutdown: tx,
        }
    }

    const MINTED: &str = r#"{"device_id":"0199a1b2-0000-7000-8000-000000000001","device_machine_key":"dmk_fresh_fixture","prefix":"dmk_fresh_fix","expires_at":"2026-11-25T10:00:00Z"}"#;

    async fn run(store: &MemStore, gate: &EnrolGate, base: &str, now: i64) -> EnsureResult {
        let tracker = StateTracker::new();
        ensure_machine_key(
            store,
            gate,
            &tracker,
            &reqwest::Client::new(),
            base,
            DID,
            "live.device.jwt",
            now,
        )
        .await
    }

    #[tokio::test]
    async fn created_stores_the_key_and_its_expiry() {
        let web = spawn_web(201, MINTED).await;
        let store = store_with(None, None);
        let gate = EnrolGate::new();
        let got = run(&store, &gate, &web.base, NOW).await;
        let exp = parse_expires_at("2026-11-25T10:00:00Z");
        assert_eq!(
            got,
            EnsureResult::Attempted {
                reason: EnrolReason::Absent,
                outcome: EnrolOutcome::Enrolled { expires_at: exp }
            }
        );
        assert_eq!(
            store.key.lock().unwrap().as_deref(),
            Some("dmk_fresh_fixture")
        );
        assert_eq!(*store.exp.lock().unwrap(), exp);
        assert_eq!(
            web.bearer.lock().unwrap().as_deref(),
            Some("Bearer live.device.jwt"),
            "the live device JWT is the only credential presented"
        );
        assert!(gate.try_claim(NOW).is_ok(), "success leaves no backoff");
    }

    #[tokio::test]
    async fn conflict_leaves_the_stored_key_untouched_and_is_not_an_error() {
        let web = spawn_web(409, r#"{"detail":{"code":"machine_key_still_usable"}}"#).await;
        let store = store_with(Some("dmk_existing"), Some(NOW + 2 * DAY));
        let gate = EnrolGate::new();
        let got = run(&store, &gate, &web.base, NOW).await;
        assert_eq!(
            got,
            EnsureResult::Attempted {
                reason: EnrolReason::Expiring,
                outcome: EnrolOutcome::StillUsable
            }
        );
        assert_eq!(store.key.lock().unwrap().as_deref(), Some("dmk_existing"));
        assert_eq!(*store.exp.lock().unwrap(), Some(NOW + 2 * DAY));
        // Not an error: no hourly retry, but a daily re-ask.
        assert_eq!(
            gate.try_claim(NOW + BACKOFF_TRANSIENT_SECS),
            Err(NOW + BACKOFF_STILL_USABLE_SECS)
        );
    }

    #[tokio::test]
    async fn service_unavailable_is_transient_and_rate_limited() {
        let web = spawn_web(
            503,
            r#"{"detail":{"code":"coord_device_lookup_unavailable","message":"retry"}}"#,
        )
        .await;
        let store = store_with(None, None);
        let gate = EnrolGate::new();
        let first = run(&store, &gate, &web.base, NOW).await;
        assert!(
            matches!(
                first,
                EnsureResult::Attempted {
                    outcome: EnrolOutcome::Transient {
                        status: Some(503),
                        ..
                    },
                    ..
                }
            ),
            "{first:?}"
        );
        assert!(store.key.lock().unwrap().is_none(), "nothing stored");
        // A second successful refresh ten minutes later must NOT knock again.
        let second = run(&store, &gate, &web.base, NOW + 600).await;
        assert_eq!(
            second,
            EnsureResult::RateLimited {
                until: NOW + BACKOFF_TRANSIENT_SECS
            }
        );
        assert_eq!(web.hits.load(Ordering::SeqCst), 1, "one call, not two");
        // After the backoff it tries again.
        let third = run(&store, &gate, &web.base, NOW + BACKOFF_TRANSIENT_SECS).await;
        assert!(matches!(third, EnsureResult::Attempted { .. }));
        assert_eq!(web.hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn refused_backs_off_longer_than_transient() {
        let web = spawn_web(403, r#"{"detail":{"code":"device_machine_key_revoked"}}"#).await;
        let store = store_with(None, None);
        let gate = EnrolGate::new();
        let got = run(&store, &gate, &web.base, NOW).await;
        assert_eq!(
            got,
            EnsureResult::Attempted {
                reason: EnrolReason::Absent,
                outcome: EnrolOutcome::Refused {
                    status: 403,
                    code: Some("device_machine_key_revoked".into())
                }
            }
        );
        assert_eq!(
            gate.try_claim(NOW + BACKOFF_TRANSIENT_SECS),
            Err(NOW + BACKOFF_REFUSED_SECS)
        );
    }

    #[tokio::test]
    async fn a_usable_key_never_calls_web() {
        let web = spawn_web(201, MINTED).await;
        let store = store_with(Some("dmk_existing"), Some(NOW + 30 * DAY));
        let got = run(&store, &EnrolGate::new(), &web.base, NOW).await;
        assert_eq!(got, EnsureResult::Skipped(SkipReason::Usable));
        assert_eq!(web.hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_minted_key_that_cannot_be_stored_is_reported() {
        let web = spawn_web(201, MINTED).await;
        let store = MemStore {
            fail_store: true,
            ..Default::default()
        };
        let got = run(&store, &EnrolGate::new(), &web.base, NOW).await;
        assert!(
            matches!(
                got,
                EnsureResult::Attempted {
                    outcome: EnrolOutcome::StoreFailed(_),
                    ..
                }
            ),
            "{got:?}"
        );
    }

    #[test]
    fn blocking_self_mint_reports_a_dead_web_as_transient() {
        // Port 9 (discard) on loopback: nothing listens, the connect fails.
        let got = self_mint_blocking("http://127.0.0.1:9", DID, "jwt");
        assert!(
            matches!(got, SelfMintHttp::Transient { status: None, .. }),
            "{got:?}"
        );
    }
}
