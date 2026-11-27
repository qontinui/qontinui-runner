//! Which token asks Anthropic's OAuth usage endpoint, and how soon a token it
//! refused is asked again.
//!
//! Plan `2026-11-26-runner-usage-source-is-dark-so-the-launch-budget-never-reads-a-session-figure`
//! Phase 1 (D4, D5, D8).
//!
//! # Why
//!
//! The endpoint ([`super::oauth_usage`]) reads only with a token granted the
//! `user:profile` scope. A config dir that holds a long-lived `claude
//! setup-token` token authenticates `claude` with it, and that token is
//! inference-only: the endpoint answers it `403 permission_error … user:profile`
//! for ever. Before this module the probe asked with exactly that token, about
//! 17 times an hour per account per box, and the per-token rate limiter answered
//! most of those asks with a 429 that masked the 403. The `.credentials.json`
//! beside it records a grant that DOES carry the scope, but nothing refreshed
//! it, because the probe skips the refresh for a long-lived dir.
//!
//! # What it decides
//!
//! * **The token** ([`candidates_for_dir`], D4). First the `.credentials.json`
//!   access token whose recorded `scopes` include `user:profile`. A background
//!   refresh is requested from [`REFRESH_AHEAD`] before it expires
//!   (`oauth_refresh::request_background_refresh`, deduped and backed off),
//!   only while the grant is still refreshable (the hard-failure memo), so a
//!   live grant is normally renewed before any probe finds it expired. An
//!   expired one is NOT sent: the probe reports `profile_refresh_pending` (or
//!   `profile_grant_dead` once the grant is rejected) rather than the state of
//!   a fallback token, and the next probe after the refresh lands reads the
//!   new token. The blocking `refresh_credentials_blocking` is never called
//!   from here. Second, the inference token the header probe sends, asked only
//!   until it is refused.
//!
//!   A refresh ROTATES the grant's refresh token (`oauth_refresh` writes the
//!   one the token endpoint returns). In a long-lived dir the runner did not
//!   refresh this grant before, so it is now the grant's sole refresher there:
//!   any other holder of a copy of that `.credentials.json` (a dir copied to
//!   another box) loses it on the first rotation, which that holder sees as a
//!   logout elsewhere. Nothing the runner runs uses such a copy.
//! * **The cooldown** ([`UsageGate`], D5). One shared-core
//!   [`qontinui_runner_lib::circuit::Circuit`] per token fingerprint, opened by
//!   one failure, backing off 5, 10, 20, 40, then 60 minutes, and holding for
//!   the server's `Retry-After` when that is longer. A scope refusal latches the
//!   token for the process lifetime. Fingerprints are in-memory `u64`s and are
//!   never logged; a new token (re-login, refresh) has a new one and is asked
//!   afresh.
//! * **The report** ([`UsageEndpointInfo`], D8). Its own typed field on the
//!   usage row, never `error`: `probe_result_exhausted` reads `error` as "this
//!   account will not serve a request", which an endpoint refusal is not.
//!
//! One INFO line per account per state transition, and one per admitted trial
//! that moves `retry_at`, so the count of `usage endpoint` lines per hour per
//! account is the number of times the endpoint was asked while cooling.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use once_cell::sync::Lazy;
use qontinui_runner_lib::circuit::{
    Admission, Backoff, Circuit, CircuitConfig, Clock, NonTripKind, Outcome, ProbeMode, SystemClock,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::oauth_usage::{OauthUsageSnapshot, UsageFetchError, PROFILE_SCOPE};

/// Prefix of every Claude subscription OAuth token (`sk-ant-oat01-…`). Only
/// these can read the endpoint; an API key has no usage windows.
const OAUTH_TOKEN_PREFIX: &str = "sk-ant-oat";

/// The cooldown schedule of a token the endpoint refused (D5): 5 minutes,
/// doubling per failed trial, held at an hour. Per token per box, a refused
/// token is asked at most once an hour once cooled, against about 17 times an
/// hour before.
pub(crate) const COOLDOWN: Backoff = Backoff {
    initial: Duration::from_secs(5 * 60),
    factor: 2,
    cap: Duration::from_secs(60 * 60),
};

/// How long before a profile access token's `expiresAt` its background
/// refresh is requested, so that a live grant never leaves a probe without a
/// token that can read the endpoint.
pub(crate) const REFRESH_AHEAD: Duration = Duration::from_secs(5 * 60);

/// A cooling entry whose `retry_at` passed this long ago without the token
/// being asked again belongs to a token no longer in use (a refreshed access
/// token, a replaced setup token), and is dropped.
const STALE_ENTRY_AFTER: Duration = Duration::from_secs(2 * 60 * 60);

fn circuit_config() -> CircuitConfig {
    CircuitConfig {
        failure_threshold: 1,
        success_threshold: 1,
        backoff: COOLDOWN,
        half_open_max_probes: 1,
        probe_mode: ProbeMode::CallerPermit,
    }
}

/// Which of an account's tokens asked the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageTokenKind {
    /// The `.credentials.json` access token (a `/login` grant).
    Credentials,
    /// The long-lived `claude setup-token` token in `settings.json`.
    LongLived,
}

impl UsageTokenKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UsageTokenKind::Credentials => "credentials",
            UsageTokenKind::LongLived => "long_lived",
        }
    }
}

/// What the OAuth usage endpoint did for one probe of one account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageEndpointState {
    /// It answered: the row's numbers came from it (`source: "oauth_usage"`).
    Ok,
    /// It refused the token's scope; that token is not asked again this
    /// process lifetime.
    ScopeRefused,
    /// The token is in its cooldown (`retry_at` says until when), after a
    /// 429, a 5xx, a 401, a non-scope 403 or an unreadable answer.
    Cooling,
    /// It was asked and gave no answer, for a reason that does not cool the
    /// token (no response arrived). The next probe asks again.
    Unavailable,
    /// The account holds no token the endpoint could be asked with: no
    /// profile-scoped grant and no inference token at all.
    NoProfileToken,
    /// The profile-scoped grant's access token has expired and the grant can
    /// still be refreshed: a refresh is due (requested in the background for a
    /// dir with a long-lived token; the probe's own pre-flight retries it for
    /// a dir without one). Reported instead of the fallback token's state,
    /// which would otherwise read `scope_refused` for every expiry.
    ProfileRefreshPending,
    /// The profile-scoped grant's access token has expired and the token
    /// endpoint rejected the grant (the hard-failure memo): only a `/login`
    /// of this account brings the endpoint, and its per-model caps, back.
    ProfileGrantDead,
    /// The account's token is not a subscription OAuth token (an API key),
    /// which has no usage windows to read.
    NotSubscription,
    /// A model gateway is declared, so subscription accounts are off
    /// (`crate::model_gateway`) and the endpoint is never asked.
    Gateway,
}

impl UsageEndpointState {
    pub fn as_str(self) -> &'static str {
        match self {
            UsageEndpointState::Ok => "ok",
            UsageEndpointState::ScopeRefused => "scope_refused",
            UsageEndpointState::Cooling => "cooling",
            UsageEndpointState::Unavailable => "unavailable",
            UsageEndpointState::ProfileRefreshPending => "profile_refresh_pending",
            UsageEndpointState::ProfileGrantDead => "profile_grant_dead",
            UsageEndpointState::NoProfileToken => "no_profile_token",
            UsageEndpointState::NotSubscription => "not_subscription",
            UsageEndpointState::Gateway => "gateway",
        }
    }
}

/// The usage endpoint's own state on a usage row (D8). Never folded into the
/// row's `error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageEndpointInfo {
    pub state: UsageEndpointState,
    /// Which token asked, or would have; `None` when there was none.
    #[serde(default)]
    pub token_kind: Option<UsageTokenKind>,
    /// Unix seconds of the token's next trial, set while `cooling`.
    #[serde(default)]
    pub retry_at: Option<u64>,
}

impl UsageEndpointInfo {
    /// A state with no token and no retry time.
    pub fn bare(state: UsageEndpointState) -> Self {
        Self {
            state,
            token_kind: None,
            retry_at: None,
        }
    }
}

/// One token the endpoint could be asked with. Its `Debug` never prints the
/// token.
pub(crate) struct Candidate {
    token: String,
    kind: UsageTokenKind,
    fingerprint: u64,
}

impl Candidate {
    fn new(token: &str, kind: UsageTokenKind) -> Self {
        Self {
            token: token.to_string(),
            kind,
            fingerprint: super::oauth_refresh::token_fingerprint(token),
        }
    }
}

impl std::fmt::Debug for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Candidate")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// The tokens a config dir holds, as read from disk.
#[derive(Default)]
pub(crate) struct DirTokens {
    /// `settings.json` `env.CLAUDE_CODE_OAUTH_TOKEN`.
    long_lived: Option<String>,
    /// `.credentials.json` `claudeAiOauth`.
    credentials: Option<CredentialsToken>,
}

/// The `.credentials.json` access token and what the file records about it.
pub(crate) struct CredentialsToken {
    access_token: String,
    scopes: Vec<String>,
    /// `expiresAt`, unix milliseconds; `0` when absent (expiry unknown, which
    /// is read as not expired, as `oauth_refresh` reads it).
    expires_at_ms: i64,
}

/// The candidates for one probe, in the order they are tried.
#[derive(Debug)]
pub(crate) struct CandidateSet {
    candidates: Vec<Candidate>,
    /// The profile-scoped grant exists but its access token has expired, so
    /// it was left out. A refresh is the caller's to request.
    profile_expired: bool,
    /// The profile grant's access token expires within [`REFRESH_AHEAD`] (or
    /// already has): a refresh is wanted.
    profile_refresh_wanted: bool,
    /// When the profile token was left out as expired, what to report about
    /// it ([`candidates_for_dir`] decides from the hard-failure memo) in place
    /// of a fallback candidate that is not asked.
    profile_hold: Option<UsageEndpointState>,
    /// What to report when there is nothing to ask.
    empty_state: UsageEndpointState,
}

/// The ordered candidates (D4), from the dir's tokens at `now_ms`. Pure.
///
/// 1. The `.credentials.json` access token when its `scopes` contain
///    `user:profile` and it has not expired. An expired one is never a
///    candidate: it sets `profile_expired` instead.
/// 2. The inference token the header probe sends (`read_oauth_token`'s
///    order): the long-lived token when there is one, else the
///    `.credentials.json` access token when it is unexpired and not already
///    candidate 1.
fn candidates_from(tokens: &DirTokens, now_ms: i64) -> CandidateSet {
    let expired = |ms: i64| ms != 0 && now_ms >= ms;
    let subscription = |token: &str| token.starts_with(OAUTH_TOKEN_PREFIX);
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut profile_expired = false;
    let mut profile_refresh_wanted = false;

    if let Some(creds) = &tokens.credentials {
        let profile = creds.scopes.iter().any(|s| s == PROFILE_SCOPE);
        if profile && subscription(creds.access_token.as_str()) {
            let ahead_ms = REFRESH_AHEAD.as_millis() as i64;
            profile_refresh_wanted =
                creds.expires_at_ms != 0 && now_ms >= creds.expires_at_ms - ahead_ms;
            if expired(creds.expires_at_ms) {
                profile_expired = true;
            } else {
                candidates.push(Candidate::new(
                    &creds.access_token,
                    UsageTokenKind::Credentials,
                ));
            }
        }
    }

    let already = |cands: &[Candidate], token: &str| cands.iter().any(|c| c.token == token);
    let mut inference_is_api_key = false;
    match (&tokens.long_lived, &tokens.credentials) {
        (Some(long_lived), _) => {
            if !already(&candidates, long_lived.as_str()) {
                candidates.push(Candidate::new(long_lived, UsageTokenKind::LongLived));
            }
        }
        (None, Some(creds)) => {
            if !subscription(creds.access_token.as_str()) {
                inference_is_api_key = !creds.access_token.is_empty();
            } else if !expired(creds.expires_at_ms)
                && !already(&candidates, creds.access_token.as_str())
            {
                candidates.push(Candidate::new(
                    &creds.access_token,
                    UsageTokenKind::Credentials,
                ));
            }
        }
        (None, None) => {}
    }

    let empty_state = if inference_is_api_key {
        UsageEndpointState::NotSubscription
    } else {
        UsageEndpointState::NoProfileToken
    };
    CandidateSet {
        candidates,
        profile_expired,
        profile_refresh_wanted,
        profile_hold: None,
        empty_state,
    }
}

/// The tokens `config_dir` holds. Lenient: an unreadable or unparsable file
/// contributes nothing.
fn read_dir_tokens(config_dir: &Path) -> DirTokens {
    let long_lived = qontinui_runner_lib::claude_oauth_token::from_config_dir(config_dir);
    let credentials = std::fs::read_to_string(config_dir.join(".credentials.json"))
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .and_then(|json| {
            let oauth = json.get("claudeAiOauth")?;
            let access_token = oauth.get("accessToken")?.as_str()?.to_string();
            let scopes = oauth
                .get("scopes")
                .and_then(|s| s.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let expires_at_ms = oauth.get("expiresAt").and_then(|v| v.as_i64()).unwrap_or(0);
            Some(CredentialsToken {
                access_token,
                scopes,
                expires_at_ms,
            })
        });
    DirTokens {
        long_lived,
        credentials,
    }
}

/// The candidates for `config_dir` at `now_ms`. Requests a background
/// refresh of the profile grant from [`REFRESH_AHEAD`] before its token
/// expires, while that grant is still refreshable, and decides what an
/// expired profile token reports (`profile_refresh_pending` while the grant
/// is refreshable, `profile_grant_dead` once it is rejected).
///
/// The refresh is requested here only for a dir that holds a long-lived
/// token. A dir without one already refreshed this probe (the probe's own
/// pre-flight refreshes the `.credentials.json` token it then sends), so
/// asking again here would double the token POST.
pub(crate) fn candidates_for_dir(config_dir: &Path, now_ms: i64) -> CandidateSet {
    let tokens = read_dir_tokens(config_dir);
    let mut set = candidates_from(&tokens, now_ms);
    if set.profile_refresh_wanted {
        let creds = config_dir.join(".credentials.json");
        let refreshable = super::oauth_refresh::profile_grant_refreshable(&creds);
        if set.profile_expired {
            set.profile_hold = Some(if refreshable {
                UsageEndpointState::ProfileRefreshPending
            } else {
                UsageEndpointState::ProfileGrantDead
            });
        }
        if refreshable && tokens.long_lived.is_some() {
            super::oauth_refresh::request_background_refresh(&creds);
        }
    }
    set
}

/// The gate's verdict on one token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Admit {
    /// Ask it.
    Pass,
    /// It is cooling until `retry_at` (unix seconds).
    Cooling { retry_at: u64 },
    /// The endpoint refused its scope; never ask it again.
    ScopeRefused,
}

/// One token's state. Present only while it is cooling or refused: a token
/// the endpoint last answered has no entry.
struct Entry {
    circuit: Circuit,
    scope_refused: bool,
    /// Unix seconds of the next trial, fixed when the circuit (re-)opened, so
    /// it moves only on a recorded trip.
    retry_at: Option<u64>,
}

/// The per-token cooldown (D5). Process-wide in production ([`GATE`]); tests
/// build their own over a hand-driven clock.
pub(crate) struct UsageGate {
    clock: Arc<dyn Clock>,
    entries: Mutex<HashMap<u64, Entry>>,
}

impl UsageGate {
    pub(crate) fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Entry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn unix_secs(at: SystemTime) -> u64 {
        at.duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Whether the token with fingerprint `fp` may be asked now. A `Pass` on a
    /// token that was cooling is its circuit's one trial, so the caller MUST
    /// follow it with [`UsageGate::record`].
    pub(crate) fn admit(&self, fp: u64) -> Admit {
        let mut entries = self.lock();
        let now = Self::unix_secs(self.clock.wall_now());
        Self::sweep_stale(&mut entries, now);
        let Some(entry) = entries.get(&fp) else {
            return Admit::Pass;
        };
        if entry.scope_refused {
            return Admit::ScopeRefused;
        }
        match entry.circuit.allow() {
            Admission::Pass | Admission::ProbePermit => Admit::Pass,
            // A trial already in flight (the same token asked for another
            // config dir) rejects with a `retry_at` that has passed: report
            // now, never a time in the past.
            Admission::Reject(_) => Admit::Cooling {
                retry_at: entry.retry_at.map_or(now, |at| at.max(now)),
            },
        }
    }

    /// Drop the entries of tokens no longer in use: not refused, and cooling
    /// with a `retry_at` more than [`STALE_ENTRY_AFTER`] past. Run on every
    /// admit and record, so an entry left in its trial by an ask that never
    /// recorded (a panicked fetch) is freed on a one-token box too.
    fn sweep_stale(entries: &mut HashMap<u64, Entry>, now: u64) {
        entries.retain(|_, e| {
            e.scope_refused
                || e.retry_at
                    .is_none_or(|at| now < at.saturating_add(STALE_ENTRY_AFTER.as_secs()))
        });
    }

    /// Record one ask's outcome and return the state it leaves the token in,
    /// with its `retry_at` while cooling.
    ///
    /// * success: the token is healthy; its entry is dropped.
    /// * `scope_refused`: latched for the process lifetime, never persisted.
    /// * 429 and 5xx: trip, holding for the server's `Retry-After` when that is
    ///   longer than the backoff.
    /// * any other answered failure (401, a non-scope 403, another 4xx, a 2xx
    ///   that did not parse): trip on the backoff alone, no latch.
    /// * no response at all: a non-tripping ambiguous failure; asked again on
    ///   the next probe.
    pub(crate) fn record(
        &self,
        fp: u64,
        result: Result<(), &UsageFetchError>,
    ) -> (UsageEndpointState, Option<u64>) {
        let mut entries = self.lock();
        let now = Self::unix_secs(self.clock.wall_now());
        Self::sweep_stale(&mut entries, now);
        let err = match result {
            Ok(()) => {
                if let Some(entry) = entries.remove(&fp) {
                    entry.circuit.record(Outcome::Success);
                }
                return (UsageEndpointState::Ok, None);
            }
            Err(err) => err,
        };
        let entry = entries.entry(fp).or_insert_with(|| Entry {
            circuit: Circuit::with_clock(circuit_config(), Arc::clone(&self.clock)),
            scope_refused: false,
            retry_at: None,
        });
        if err.scope_refused {
            entry.scope_refused = true;
            entry.retry_at = None;
            // Releases a trial slot when this ask was one; the latch, not the
            // circuit, keeps the token from being asked again.
            entry
                .circuit
                .record(Outcome::NonTripFailure(NonTripKind::BackendError));
            return (UsageEndpointState::ScopeRefused, None);
        }
        let Some(status) = err.status else {
            entry
                .circuit
                .record(Outcome::NonTripFailure(NonTripKind::Ambiguous));
            if entry.circuit.is_closed() {
                entries.remove(&fp);
            }
            return (UsageEndpointState::Unavailable, None);
        };
        let hint = (status == 429 || (500..=599).contains(&status))
            .then_some(err.retry_after)
            .flatten();
        entry
            .circuit
            .record_with_hint(Outcome::TripFailure(format!("HTTP {status}")), hint);
        let wait = entry.circuit.retry_after().unwrap_or(Duration::ZERO);
        let retry_at = Self::unix_secs(self.clock.wall_now() + wait);
        entry.retry_at = Some(retry_at);
        (UsageEndpointState::Cooling, Some(retry_at))
    }
}

/// The process-wide gate the probe consults.
static GATE: Lazy<UsageGate> = Lazy::new(|| UsageGate::new(Arc::new(SystemClock)));

/// What one probe learned from the endpoint.
#[derive(Debug)]
pub(crate) struct EndpointRead {
    /// The endpoint's answer, when it gave one.
    pub(crate) snapshot: Option<OauthUsageSnapshot>,
    pub(crate) info: UsageEndpointInfo,
}

/// Ask the first admitted candidate, at most one ask per probe, and record its
/// outcome. A candidate that is cooling or refused is skipped for the next;
/// when none is admitted nothing is asked, and the state reported is the
/// expired profile token's (`profile_hold`) when there is one, else the first
/// candidate's. `fetch` is the endpoint call, injected so tests count asks.
pub(crate) async fn read_usage<F, Fut>(
    gate: &UsageGate,
    set: CandidateSet,
    fetch: F,
) -> EndpointRead
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<OauthUsageSnapshot, UsageFetchError>>,
{
    let mut held: Option<UsageEndpointInfo> = None;
    for candidate in set.candidates {
        let blocked = match gate.admit(candidate.fingerprint) {
            Admit::Pass => {
                let result = fetch(candidate.token).await;
                let (state, retry_at) =
                    gate.record(candidate.fingerprint, result.as_ref().map(|_| ()));
                if let Err(e) = &result {
                    debug!(token_kind = candidate.kind.as_str(), error = %e, "oauth usage endpoint ask failed");
                }
                return EndpointRead {
                    snapshot: result.ok(),
                    info: UsageEndpointInfo {
                        state,
                        token_kind: Some(candidate.kind),
                        retry_at,
                    },
                };
            }
            Admit::Cooling { retry_at } => UsageEndpointInfo {
                state: UsageEndpointState::Cooling,
                token_kind: Some(candidate.kind),
                retry_at: Some(retry_at),
            },
            Admit::ScopeRefused => UsageEndpointInfo {
                state: UsageEndpointState::ScopeRefused,
                token_kind: Some(candidate.kind),
                retry_at: None,
            },
        };
        held.get_or_insert(blocked);
    }
    EndpointRead {
        snapshot: None,
        info: set
            .profile_hold
            .map(|state| UsageEndpointInfo {
                state,
                token_kind: Some(UsageTokenKind::Credentials),
                retry_at: None,
            })
            .or(held)
            .unwrap_or_else(|| UsageEndpointInfo::bare(set.empty_state)),
    }
}

/// The INFO line for an account whose endpoint state is now `now`, or `None`
/// when nothing changed since `prev`. A change of state, of token kind, or of
/// `retry_at` (each admitted trial that failed again) is a transition. The
/// line names the account by its label and carries no token, fingerprint,
/// email or account uuid.
fn transition_line(
    prev: Option<&UsageEndpointInfo>,
    now: &UsageEndpointInfo,
    label: &str,
) -> Option<String> {
    if prev == Some(now) {
        return None;
    }
    let retry_at = now
        .retry_at
        .and_then(|secs| chrono::DateTime::from_timestamp(secs as i64, 0))
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| "none".to_string());
    Some(format!(
        "Account '{label}': usage endpoint {} token_kind={} retry_at={retry_at}",
        now.state.as_str(),
        now.token_kind.map_or("none", UsageTokenKind::as_str),
    ))
}

/// The last state logged per account, keyed by config dir.
static LAST_LOGGED: Lazy<Mutex<HashMap<String, UsageEndpointInfo>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Log `info` for the account at `account_key` when it is a transition.
fn note_transition(account_key: &str, label: &str, info: &UsageEndpointInfo) {
    let mut last = LAST_LOGGED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(line) = transition_line(last.get(account_key), info, label) {
        info!("{line}");
    }
    last.insert(account_key.to_string(), info.clone());
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Ask the usage endpoint for `config_dir`'s account, through the gate. The
/// probe's one entry point here.
pub(crate) async fn ask_usage_endpoint(config_dir: &str, label: &str) -> EndpointRead {
    let set = candidates_for_dir(Path::new(config_dir), now_ms());
    let read = read_usage(&GATE, set, |token| async move {
        super::oauth_usage::fetch(&token).await
    })
    .await;
    note_transition(config_dir, label, &read.info);
    read
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    /// `qontinui_runner_lib::circuit::ManualClock` is `cfg(test)` in the lib,
    /// so this bin's tests cannot see it; this is the same clock.
    struct TestClock {
        base: Instant,
        offset: Mutex<Duration>,
    }

    /// The wall clock's origin: unix second 1_800_000_000.
    const T0: u64 = 1_800_000_000;

    impl TestClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                base: Instant::now(),
                offset: Mutex::new(Duration::ZERO),
            })
        }
        fn advance(&self, by: Duration) {
            *self.offset.lock().unwrap() += by;
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> Instant {
            self.base + *self.offset.lock().unwrap()
        }
        fn wall_now(&self) -> SystemTime {
            UNIX_EPOCH + Duration::from_secs(T0) + *self.offset.lock().unwrap()
        }
    }

    fn gate() -> (UsageGate, Arc<TestClock>) {
        let clock = TestClock::new();
        (UsageGate::new(clock.clone()), clock)
    }

    const NOW_MS: i64 = 1_800_000_000_000;
    const LONG_LIVED: &str = "sk-ant-oat01-test-long-lived";
    const PROFILE: &str = "sk-ant-oat01-test-profile";

    fn creds(token: &str, scopes: &[&str], expires_at_ms: i64) -> CredentialsToken {
        CredentialsToken {
            access_token: token.to_string(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            expires_at_ms,
        }
    }

    fn tokens(long_lived: Option<&str>, credentials: Option<CredentialsToken>) -> DirTokens {
        DirTokens {
            long_lived: long_lived.map(str::to_string),
            credentials,
        }
    }

    fn kinds(set: &CandidateSet) -> Vec<UsageTokenKind> {
        set.candidates.iter().map(|c| c.kind).collect()
    }

    fn set_of(cands: &[(&str, UsageTokenKind)]) -> CandidateSet {
        CandidateSet {
            candidates: cands.iter().map(|(t, k)| Candidate::new(t, *k)).collect(),
            profile_expired: false,
            profile_refresh_wanted: false,
            profile_hold: None,
            empty_state: UsageEndpointState::NoProfileToken,
        }
    }

    fn err(status: Option<u16>, retry_after: Option<u64>, scope_refused: bool) -> UsageFetchError {
        UsageFetchError {
            status,
            retry_after: retry_after.map(Duration::from_secs),
            scope_refused,
            detail: "test".to_string(),
        }
    }

    /// A fake endpoint that answers every ask with `answer` and records which
    /// token asked.
    struct FakeEndpoint {
        answer: Result<(), UsageFetchError>,
        asked: Mutex<Vec<String>>,
        calls: AtomicUsize,
    }

    impl FakeEndpoint {
        fn new(answer: Result<(), UsageFetchError>) -> Self {
            Self {
                answer,
                asked: Mutex::new(Vec::new()),
                calls: AtomicUsize::new(0),
            }
        }
        async fn read(&self, gate: &UsageGate, set: CandidateSet) -> EndpointRead {
            read_usage(gate, set, |token| async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.asked.lock().unwrap().push(token);
                self.answer.clone().map(|()| OauthUsageSnapshot::default())
            })
            .await
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
        fn asked(&self) -> Vec<String> {
            self.asked.lock().unwrap().clone()
        }
    }

    fn info(
        state: UsageEndpointState,
        kind: Option<UsageTokenKind>,
        retry_at: Option<u64>,
    ) -> UsageEndpointInfo {
        UsageEndpointInfo {
            state,
            token_kind: kind,
            retry_at,
        }
    }

    // ── Candidates (D4) ───────────────────────────────────────────────────

    #[test]
    fn no_token_at_all_is_zero_candidates_and_no_profile_token() {
        let set = candidates_from(&tokens(None, None), NOW_MS);
        assert!(set.candidates.is_empty());
        assert_eq!(set.empty_state, UsageEndpointState::NoProfileToken);
        assert!(!set.profile_expired);
    }

    #[test]
    fn an_api_key_is_zero_candidates_and_not_subscription() {
        let set = candidates_from(
            &tokens(None, Some(creds("sk-ant-api03-key", &[], 0))),
            NOW_MS,
        );
        assert!(set.candidates.is_empty());
        assert_eq!(set.empty_state, UsageEndpointState::NotSubscription);
    }

    #[test]
    fn a_long_lived_token_alone_is_one_candidate() {
        let set = candidates_from(&tokens(Some(LONG_LIVED), None), NOW_MS);
        assert_eq!(kinds(&set), vec![UsageTokenKind::LongLived]);
    }

    #[test]
    fn a_profile_grant_alone_is_one_candidate() {
        let c = creds(
            PROFILE,
            &["user:inference", "user:profile"],
            NOW_MS + 60_000,
        );
        let set = candidates_from(&tokens(None, Some(c)), NOW_MS);
        assert_eq!(kinds(&set), vec![UsageTokenKind::Credentials]);
    }

    /// The box this plan was measured on: every dir has both. The profile
    /// grant is asked first, the inference token second.
    #[test]
    fn a_live_profile_grant_beside_a_long_lived_token_is_two_candidates_profile_first() {
        let c = creds(
            PROFILE,
            &["user:inference", "user:profile"],
            NOW_MS + 60_000,
        );
        let set = candidates_from(&tokens(Some(LONG_LIVED), Some(c)), NOW_MS);
        assert_eq!(
            kinds(&set),
            vec![UsageTokenKind::Credentials, UsageTokenKind::LongLived]
        );
        assert_eq!(set.candidates[0].token, PROFILE);
        assert_eq!(set.candidates[1].token, LONG_LIVED);
    }

    /// An expired profile token is never a candidate: it asks for a refresh.
    #[test]
    fn an_expired_profile_grant_is_not_a_candidate_and_wants_a_refresh() {
        let c = creds(PROFILE, &["user:profile"], NOW_MS - 1);
        let set = candidates_from(&tokens(Some(LONG_LIVED), Some(c)), NOW_MS);
        assert_eq!(kinds(&set), vec![UsageTokenKind::LongLived]);
        assert!(set.profile_expired);
        assert!(set.candidates.iter().all(|c| c.token != PROFILE));
    }

    /// Without a long-lived token the inference token IS the credentials
    /// token, and an expired one is not sent either.
    #[test]
    fn an_expired_credentials_token_without_a_long_lived_one_is_never_sent() {
        let c = creds(PROFILE, &["user:inference"], NOW_MS - 1);
        let set = candidates_from(&tokens(None, Some(c)), NOW_MS);
        assert!(set.candidates.is_empty());
        assert_eq!(set.empty_state, UsageEndpointState::NoProfileToken);
    }

    /// A grant that records no profile scope is not candidate 1; beside a
    /// long-lived token it is not asked at all.
    #[test]
    fn a_grant_without_the_profile_scope_is_not_preferred() {
        let c = creds(PROFILE, &["user:inference"], NOW_MS + 60_000);
        let set = candidates_from(&tokens(Some(LONG_LIVED), Some(c)), NOW_MS);
        assert_eq!(kinds(&set), vec![UsageTokenKind::LongLived]);
    }

    // ── Fetch outcomes and the cooldown (D5) ──────────────────────────────

    #[tokio::test]
    async fn a_200_reads_ok_and_leaves_the_token_admitted() {
        let (g, _) = gate();
        let ep = FakeEndpoint::new(Ok(()));
        let read = ep
            .read(&g, set_of(&[(LONG_LIVED, UsageTokenKind::LongLived)]))
            .await;
        assert!(read.snapshot.is_some());
        assert_eq!(
            read.info,
            info(
                UsageEndpointState::Ok,
                Some(UsageTokenKind::LongLived),
                None
            )
        );
        let fp = super::super::oauth_refresh::token_fingerprint(LONG_LIVED);
        assert_eq!(g.admit(fp), Admit::Pass);
    }

    /// The scope refusal latches: the same token asked again issues zero
    /// calls, for as long as the process lives.
    #[tokio::test]
    async fn a_403_scope_refusal_latches_the_token() {
        let (g, clock) = gate();
        let ep = FakeEndpoint::new(Err(err(Some(403), None, true)));
        let set = || set_of(&[(LONG_LIVED, UsageTokenKind::LongLived)]);
        let read = ep.read(&g, set()).await;
        assert!(read.snapshot.is_none());
        assert_eq!(
            read.info,
            info(
                UsageEndpointState::ScopeRefused,
                Some(UsageTokenKind::LongLived),
                None
            )
        );
        clock.advance(Duration::from_secs(24 * 3600));
        let again = ep.read(&g, set()).await;
        assert_eq!(ep.calls(), 1, "a refused token is never asked again");
        assert_eq!(again.info.state, UsageEndpointState::ScopeRefused);
    }

    /// A 403 that is not the scope refusal, and a 401, trip the backoff and
    /// do NOT latch: after the 5-minute cooldown the token is asked again.
    #[tokio::test]
    async fn a_non_scope_403_and_a_401_trip_without_latching() {
        for status in [403u16, 401] {
            let (g, clock) = gate();
            let ep = FakeEndpoint::new(Err(err(Some(status), None, false)));
            let set = || set_of(&[(LONG_LIVED, UsageTokenKind::LongLived)]);
            let read = ep.read(&g, set()).await;
            assert_eq!(
                read.info,
                info(
                    UsageEndpointState::Cooling,
                    Some(UsageTokenKind::LongLived),
                    Some(T0 + 300)
                ),
                "{status}"
            );
            clock.advance(Duration::from_secs(299));
            ep.read(&g, set()).await;
            assert_eq!(ep.calls(), 1, "{status}: still cooling at 299 s");
            clock.advance(Duration::from_secs(1));
            let trial = ep.read(&g, set()).await;
            assert_eq!(ep.calls(), 2, "{status}: not latched, so asked after 300 s");
            assert_eq!(
                trial.info.retry_at,
                Some(T0 + 300 + 600),
                "{status}: backoff doubles"
            );
        }
    }

    #[tokio::test]
    async fn a_429_with_retry_after_cools_for_the_longer_hint() {
        let (g, _) = gate();
        let ep = FakeEndpoint::new(Err(err(Some(429), Some(1800), false)));
        let read = ep
            .read(&g, set_of(&[(LONG_LIVED, UsageTokenKind::LongLived)]))
            .await;
        assert_eq!(
            read.info,
            info(
                UsageEndpointState::Cooling,
                Some(UsageTokenKind::LongLived),
                Some(T0 + 1800)
            )
        );
    }

    #[tokio::test]
    async fn a_429_without_retry_after_cools_for_the_backoff() {
        let (g, _) = gate();
        let ep = FakeEndpoint::new(Err(err(Some(429), None, false)));
        let read = ep
            .read(&g, set_of(&[(LONG_LIVED, UsageTokenKind::LongLived)]))
            .await;
        assert_eq!(read.info.state, UsageEndpointState::Cooling);
        assert_eq!(read.info.retry_at, Some(T0 + 300));
    }

    #[tokio::test]
    async fn a_5xx_cools_and_honours_its_retry_after() {
        let (g, _) = gate();
        let ep = FakeEndpoint::new(Err(err(Some(503), Some(900), false)));
        let read = ep
            .read(&g, set_of(&[(LONG_LIVED, UsageTokenKind::LongLived)]))
            .await;
        assert_eq!(read.info.state, UsageEndpointState::Cooling);
        assert_eq!(read.info.retry_at, Some(T0 + 900));
    }

    /// A Retry-After on a 401 is not a rate-limit hint: the backoff alone.
    #[tokio::test]
    async fn a_retry_after_on_a_401_is_ignored() {
        let (g, _) = gate();
        let ep = FakeEndpoint::new(Err(err(Some(401), Some(5000), false)));
        let read = ep
            .read(&g, set_of(&[(LONG_LIVED, UsageTokenKind::LongLived)]))
            .await;
        assert_eq!(read.info.retry_at, Some(T0 + 300));
    }

    /// No response is not evidence about the token: it does not cool, and the
    /// next probe asks again.
    #[tokio::test]
    async fn a_network_error_reads_unavailable_and_does_not_cool() {
        let (g, _) = gate();
        let ep = FakeEndpoint::new(Err(err(None, None, false)));
        let set = || set_of(&[(LONG_LIVED, UsageTokenKind::LongLived)]);
        let read = ep.read(&g, set()).await;
        assert_eq!(
            read.info,
            info(
                UsageEndpointState::Unavailable,
                Some(UsageTokenKind::LongLived),
                None
            )
        );
        ep.read(&g, set()).await;
        assert_eq!(ep.calls(), 2);
    }

    /// The same token asked twice inside its cooldown issues zero calls the
    /// second time, and the reported `retry_at` does not move.
    #[tokio::test]
    async fn the_same_token_inside_its_cooldown_issues_zero_calls() {
        let (g, clock) = gate();
        let ep = FakeEndpoint::new(Err(err(Some(429), None, false)));
        let set = || set_of(&[(LONG_LIVED, UsageTokenKind::LongLived)]);
        let first = ep.read(&g, set()).await;
        clock.advance(Duration::from_secs(120));
        let second = ep.read(&g, set()).await;
        assert_eq!(ep.calls(), 1);
        assert_eq!(second.info, first.info);
    }

    /// A new token (re-login, refresh, new setup token) is asked afresh even
    /// though the old one is refused.
    #[tokio::test]
    async fn a_changed_token_after_a_refusal_is_asked() {
        let (g, _) = gate();
        let ep = FakeEndpoint::new(Err(err(Some(403), None, true)));
        ep.read(&g, set_of(&[(LONG_LIVED, UsageTokenKind::LongLived)]))
            .await;
        let replaced = "sk-ant-oat01-test-replaced";
        ep.read(&g, set_of(&[(replaced, UsageTokenKind::LongLived)]))
            .await;
        assert_eq!(ep.calls(), 2);
        assert_eq!(
            ep.asked(),
            vec![LONG_LIVED.to_string(), replaced.to_string()]
        );
    }

    /// With two candidates the first admitted one is asked: the profile token
    /// while it is healthy, the inference token while the profile token
    /// cools, and nothing at all when both are held.
    #[tokio::test]
    async fn the_first_admitted_candidate_is_asked() {
        let (g, _) = gate();
        let both = || {
            set_of(&[
                (PROFILE, UsageTokenKind::Credentials),
                (LONG_LIVED, UsageTokenKind::LongLived),
            ])
        };
        let ok = FakeEndpoint::new(Ok(()));
        let read = ok.read(&g, both()).await;
        assert_eq!(ok.asked(), vec![PROFILE.to_string()]);
        assert_eq!(read.info.token_kind, Some(UsageTokenKind::Credentials));

        // The profile token gets a 429: next probe the inference token asks.
        let limited = FakeEndpoint::new(Err(err(Some(429), None, false)));
        limited.read(&g, both()).await;
        let refused = FakeEndpoint::new(Err(err(Some(403), None, true)));
        let read = refused.read(&g, both()).await;
        assert_eq!(refused.asked(), vec![LONG_LIVED.to_string()]);
        assert_eq!(read.info.state, UsageEndpointState::ScopeRefused);

        // Both held: nothing is asked, and the first candidate's state shows.
        let idle = FakeEndpoint::new(Ok(()));
        let read = idle.read(&g, both()).await;
        assert_eq!(idle.calls(), 0);
        assert_eq!(
            read.info,
            info(
                UsageEndpointState::Cooling,
                Some(UsageTokenKind::Credentials),
                Some(T0 + 300)
            )
        );
    }

    #[tokio::test]
    async fn zero_candidates_ask_nothing_and_report_the_empty_state() {
        let (g, _) = gate();
        let ep = FakeEndpoint::new(Ok(()));
        let read = ep.read(&g, set_of(&[])).await;
        assert_eq!(ep.calls(), 0);
        assert_eq!(
            read.info,
            UsageEndpointInfo::bare(UsageEndpointState::NoProfileToken)
        );
    }

    /// A success after cooling closes the circuit and drops the entry: the
    /// token is admitted with no wait from then on.
    #[tokio::test]
    async fn a_success_on_the_trial_clears_the_cooldown() {
        let (g, clock) = gate();
        let set = || set_of(&[(LONG_LIVED, UsageTokenKind::LongLived)]);
        FakeEndpoint::new(Err(err(Some(503), None, false)))
            .read(&g, set())
            .await;
        clock.advance(Duration::from_secs(300));
        let read = FakeEndpoint::new(Ok(())).read(&g, set()).await;
        assert_eq!(read.info.state, UsageEndpointState::Ok);
        let fp = super::super::oauth_refresh::token_fingerprint(LONG_LIVED);
        assert_eq!(g.admit(fp), Admit::Pass);
        assert!(g.lock().is_empty(), "a healthy token keeps no entry");
    }

    // ── Expired profile grant: the refresh request (D4) ───────────────────

    fn long_lived_dir_with_profile_creds(
        expires_at_ms: i64,
        access_token: &str,
        refresh_token: &str,
    ) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("settings.json"),
            format!(r#"{{"env":{{"CLAUDE_CODE_OAUTH_TOKEN":"{LONG_LIVED}"}}}}"#),
        )
        .expect("settings");
        write_creds(dir.path(), expires_at_ms, access_token, refresh_token);
        dir
    }

    fn write_creds(dir: &Path, expires_at_ms: i64, access_token: &str, refresh_token: &str) {
        let body = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": access_token,
                "refreshToken": refresh_token,
                "expiresAt": expires_at_ms,
                "scopes": ["user:inference", "user:profile"],
            }
        });
        std::fs::write(dir.join(".credentials.json"), body.to_string()).expect("creds");
    }

    /// A refresh that SUCCEEDS writes a new, unexpired token into the file;
    /// the next probe reads it as candidate 1 and asks with it.
    #[tokio::test]
    async fn an_expired_profile_grant_whose_refresh_succeeds_is_asked_next_probe() {
        let _log = super::super::oauth_refresh::refresh_log_guard_for_test();
        let dir = long_lived_dir_with_profile_creds(NOW_MS - 1, PROFILE, "rt-live");
        let creds = dir.path().join(".credentials.json");
        super::super::oauth_refresh::refresh_requests_for_test(&creds);

        let first = candidates_for_dir(dir.path(), NOW_MS);
        assert_eq!(kinds(&first), vec![UsageTokenKind::LongLived]);
        assert_eq!(
            super::super::oauth_refresh::refresh_requests_for_test(&creds),
            1
        );

        // The background refresh lands: a new access token, a day to run.
        let refreshed = "sk-ant-oat01-test-refreshed";
        write_creds(dir.path(), NOW_MS + 86_400_000, refreshed, "rt-rotated");
        let (g, _) = gate();
        let ep = FakeEndpoint::new(Ok(()));
        let read = ep.read(&g, candidates_for_dir(dir.path(), NOW_MS)).await;
        assert_eq!(ep.asked(), vec![refreshed.to_string()]);
        assert_eq!(read.info.token_kind, Some(UsageTokenKind::Credentials));
        assert_eq!(
            super::super::oauth_refresh::refresh_requests_for_test(&creds),
            0
        );
    }

    /// A refresh that HARD-FAILS marks the grant dead: probed N times, the
    /// gate requests exactly one background refresh in all (the request, not
    /// the POST: the spawner's own dedupe and backoff are not run under test),
    /// and the expired token is never sent.
    #[tokio::test]
    async fn a_hard_failed_grant_probed_n_times_issues_exactly_one_refresh_request() {
        let _log = super::super::oauth_refresh::refresh_log_guard_for_test();
        let dir = long_lived_dir_with_profile_creds(NOW_MS - 1, PROFILE, "rt-dead");
        let creds = dir.path().join(".credentials.json");
        super::super::oauth_refresh::refresh_requests_for_test(&creds);

        let (g, _) = gate();
        let ep = FakeEndpoint::new(Err(err(Some(403), None, true)));
        ep.read(&g, candidates_for_dir(dir.path(), NOW_MS)).await;
        // That one POST came back invalid_grant.
        super::super::oauth_refresh::mark_grant_revoked_for_test(&dir.path().to_string_lossy());
        for _ in 0..9 {
            ep.read(&g, candidates_for_dir(dir.path(), NOW_MS)).await;
        }
        assert_eq!(
            super::super::oauth_refresh::refresh_requests_for_test(&creds),
            1,
            "a dead grant is asked to refresh once, not once per probe"
        );
        assert!(
            ep.asked().iter().all(|t| t != PROFILE),
            "an expired profile token is never sent"
        );
        assert_eq!(ep.calls(), 1, "the refused inference token is latched");
    }

    /// A TRANSIENT refresh failure leaves the grant refreshable, so the next
    /// probe asks again (the background path's own 60 s spacing bounds the
    /// POSTs), and the expired token is still never sent.
    #[tokio::test]
    async fn a_transiently_failing_refresh_is_requested_again_and_never_sends_the_expired_token() {
        let _log = super::super::oauth_refresh::refresh_log_guard_for_test();
        let dir = long_lived_dir_with_profile_creds(NOW_MS - 1, PROFILE, "rt-flaky");
        let creds = dir.path().join(".credentials.json");
        super::super::oauth_refresh::refresh_requests_for_test(&creds);

        let (g, _) = gate();
        let ep = FakeEndpoint::new(Ok(()));
        for _ in 0..3 {
            let read = ep.read(&g, candidates_for_dir(dir.path(), NOW_MS)).await;
            assert_eq!(read.info.token_kind, Some(UsageTokenKind::LongLived));
        }
        assert_eq!(
            super::super::oauth_refresh::refresh_requests_for_test(&creds),
            3
        );
        assert!(ep.asked().iter().all(|t| t != PROFILE));
    }

    /// A dir with no long-lived token was refreshed by the probe's own
    /// pre-flight, so this module requests nothing more for it.
    #[test]
    fn an_expired_grant_without_a_long_lived_token_requests_no_second_refresh() {
        let _log = super::super::oauth_refresh::refresh_log_guard_for_test();
        let dir = tempfile::tempdir().expect("tempdir");
        write_creds(dir.path(), NOW_MS - 1, PROFILE, "rt-live");
        let creds = dir.path().join(".credentials.json");
        super::super::oauth_refresh::refresh_requests_for_test(&creds);
        let set = candidates_for_dir(dir.path(), NOW_MS);
        assert!(set.candidates.is_empty());
        assert_eq!(
            super::super::oauth_refresh::refresh_requests_for_test(&creds),
            0
        );
    }

    /// Review round 1, finding 1. A dir with a long-lived token beside a live
    /// profile grant: the profile token answers; once it expires, the probe
    /// must NOT report the latched long-lived token's `scope_refused` (one
    /// line per token lifetime, and the wrong state) but the grant's own
    /// state, asking nothing.
    #[tokio::test]
    async fn an_expired_profile_token_reports_its_own_state_not_the_refused_fallback() {
        let _log = super::super::oauth_refresh::refresh_log_guard_for_test();
        let (g, _) = gate();
        // The long-lived token was refused once this start, and is latched.
        FakeEndpoint::new(Err(err(Some(403), None, true)))
            .read(&g, set_of(&[(LONG_LIVED, UsageTokenKind::LongLived)]))
            .await;
        let dir = long_lived_dir_with_profile_creds(NOW_MS + 3_600_000, PROFILE, "rt-live");
        let ok = FakeEndpoint::new(Ok(()));
        let live = ok.read(&g, candidates_for_dir(dir.path(), NOW_MS)).await;
        assert_eq!(
            live.info,
            info(
                UsageEndpointState::Ok,
                Some(UsageTokenKind::Credentials),
                None
            )
        );

        write_creds(dir.path(), NOW_MS - 1, PROFILE, "rt-live");
        let expired = ok.read(&g, candidates_for_dir(dir.path(), NOW_MS)).await;
        assert_eq!(
            ok.calls(),
            1,
            "nothing is asked while the profile token is expired"
        );
        assert_eq!(
            transition_line(Some(&live.info), &expired.info, ".claude-a").as_deref(),
            Some("Account '.claude-a': usage endpoint profile_refresh_pending token_kind=credentials retry_at=none")
        );

        // Once the token endpoint rejects the grant, the state says so.
        super::super::oauth_refresh::mark_grant_revoked_for_test(&dir.path().to_string_lossy());
        let dead = ok.read(&g, candidates_for_dir(dir.path(), NOW_MS)).await;
        assert_eq!(
            dead.info,
            info(
                UsageEndpointState::ProfileGrantDead,
                Some(UsageTokenKind::Credentials),
                None
            )
        );
        assert_eq!(ok.calls(), 1);
        super::super::oauth_refresh::refresh_requests_for_test(
            &dir.path().join(".credentials.json"),
        );
    }

    /// The refresh is requested AHEAD of expiry: a profile token four minutes
    /// from its `expiresAt` is still asked, and its refresh is already wanted.
    #[test]
    fn a_profile_token_near_expiry_is_still_asked_and_its_refresh_requested() {
        let _log = super::super::oauth_refresh::refresh_log_guard_for_test();
        let dir = long_lived_dir_with_profile_creds(NOW_MS + 4 * 60_000, PROFILE, "rt-live");
        let creds = dir.path().join(".credentials.json");
        super::super::oauth_refresh::refresh_requests_for_test(&creds);
        let set = candidates_for_dir(dir.path(), NOW_MS);
        assert_eq!(
            kinds(&set),
            vec![UsageTokenKind::Credentials, UsageTokenKind::LongLived]
        );
        assert_eq!(
            super::super::oauth_refresh::refresh_requests_for_test(&creds),
            1
        );
        // Six minutes out, outside the lead: no refresh yet.
        write_creds(dir.path(), NOW_MS + 6 * 60_000, PROFILE, "rt-live");
        candidates_for_dir(dir.path(), NOW_MS);
        assert_eq!(
            super::super::oauth_refresh::refresh_requests_for_test(&creds),
            0
        );
    }

    /// A trial in flight for the same token (another config dir) rejects a
    /// second ask; its `retry_at` is now, never one already past.
    #[tokio::test]
    async fn a_rejection_behind_a_trial_in_flight_never_reports_a_past_retry_at() {
        let (g, clock) = gate();
        let fp = super::super::oauth_refresh::token_fingerprint(LONG_LIVED);
        FakeEndpoint::new(Err(err(Some(503), None, false)))
            .read(&g, set_of(&[(LONG_LIVED, UsageTokenKind::LongLived)]))
            .await;
        clock.advance(Duration::from_secs(400));
        assert_eq!(g.admit(fp), Admit::Pass, "the trial");
        assert_eq!(g.admit(fp), Admit::Cooling { retry_at: T0 + 400 });
    }

    /// An entry whose trial was admitted and never recorded is swept by the
    /// next admit once stale, so a one-token box is not wedged for good.
    #[tokio::test]
    async fn an_unrecorded_trial_is_swept_by_a_later_admit() {
        let (g, clock) = gate();
        let fp = super::super::oauth_refresh::token_fingerprint(LONG_LIVED);
        FakeEndpoint::new(Err(err(Some(503), None, false)))
            .read(&g, set_of(&[(LONG_LIVED, UsageTokenKind::LongLived)]))
            .await;
        clock.advance(Duration::from_secs(300));
        assert_eq!(g.admit(fp), Admit::Pass, "the trial, never recorded");
        assert!(matches!(g.admit(fp), Admit::Cooling { .. }));
        clock.advance(STALE_ENTRY_AFTER);
        assert_eq!(g.admit(fp), Admit::Pass, "swept: asked afresh");
    }

    // ── The transition line ───────────────────────────────────────────────

    #[test]
    fn a_transition_logs_once_and_a_repeat_logs_nothing() {
        let cooling = info(
            UsageEndpointState::Cooling,
            Some(UsageTokenKind::LongLived),
            Some(T0 + 300),
        );
        assert_eq!(
            transition_line(None, &cooling, ".claude-a").as_deref(),
            Some("Account '.claude-a': usage endpoint cooling token_kind=long_lived retry_at=2027-01-15T08:05:00Z")
        );
        assert_eq!(transition_line(Some(&cooling), &cooling, ".claude-a"), None);
    }

    /// Each admitted trial that fails again moves `retry_at`, and that is a
    /// line: the hourly count of lines is the attempt count while cooling.
    #[test]
    fn a_moved_retry_at_is_a_transition() {
        let first = info(
            UsageEndpointState::Cooling,
            Some(UsageTokenKind::LongLived),
            Some(T0 + 300),
        );
        let next = info(
            UsageEndpointState::Cooling,
            Some(UsageTokenKind::LongLived),
            Some(T0 + 900),
        );
        assert!(transition_line(Some(&first), &next, ".claude-a").is_some());
    }

    #[test]
    fn the_line_names_state_kind_and_none() {
        let refused = info(
            UsageEndpointState::ScopeRefused,
            Some(UsageTokenKind::LongLived),
            None,
        );
        assert_eq!(
            transition_line(None, &refused, ".claude-a").as_deref(),
            Some("Account '.claude-a': usage endpoint scope_refused token_kind=long_lived retry_at=none")
        );
        let bare = UsageEndpointInfo::bare(UsageEndpointState::NoProfileToken);
        assert_eq!(
            transition_line(None, &bare, ".claude-b").as_deref(),
            Some("Account '.claude-b': usage endpoint no_profile_token token_kind=none retry_at=none")
        );
    }

    /// The wire spelling is the D8 contract the API carries.
    #[test]
    fn the_endpoint_info_serializes_in_snake_case() {
        let v = serde_json::to_value(info(
            UsageEndpointState::ScopeRefused,
            Some(UsageTokenKind::LongLived),
            None,
        ))
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"state": "scope_refused", "token_kind": "long_lived", "retry_at": null})
        );
        let v = serde_json::to_value(UsageEndpointInfo::bare(UsageEndpointState::NotSubscription))
            .unwrap();
        assert_eq!(v["state"], serde_json::json!("not_subscription"));
    }
}
