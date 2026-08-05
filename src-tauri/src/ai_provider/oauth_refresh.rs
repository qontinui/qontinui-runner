//! Silent OAuth token refresh for Claude CLI credentials.
//!
//! When the CLI's `~/.claude/.credentials.json` access token expires, the
//! runner refreshes it transparently so subprocess invocations (`claude --print`
//! and interactive stream-json sessions) don't 401 without any user action.
//!
//! The token endpoint and client-id are taken from the Claude CLI's own source:
//!   TOKEN_URL  = https://platform.claude.com/v1/oauth/token
//!   CLIENT_ID  = 9d1c250a-e61b-44d9-88ed-5944d1962f5e

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Refresh proactively once the token is within this long of expiring, so an
/// account in ordinary use is topped up long before it can hard-expire and
/// never needs a refresh ON the spawn path. 10 minutes.
const REFRESH_LEAD_MS: i64 = 10 * 60 * 1000;

/// Minimum spacing between background refresh ATTEMPTS for one credentials
/// file. A refresh that fails transiently (offline, 5xx) must not turn every
/// subsequent spawn into another token POST.
const REFRESH_RETRY_BACKOFF: Duration = Duration::from_secs(60);

/// Ceiling for the exponential backoff applied after a HARD failure (the token
/// endpoint rejected the grant itself). A permanently revoked grant must never
/// keep POSTing `platform.claude.com` once a minute for the process lifetime,
/// but it must still be re-probed occasionally: the server can un-revoke
/// nothing, yet a 400 misclassified from a transient edge response would
/// otherwise strand the account until restart. 6 hours.
const REFRESH_HARD_FAIL_BACKOFF_CAP: Duration = Duration::from_secs(6 * 60 * 60);

/// Per-credentials-file background-refresh bookkeeping.
#[cfg_attr(test, allow(dead_code))]
#[derive(Debug, Default)]
struct RefreshState {
    in_flight: bool,
    last_attempt: Option<Instant>,
    /// Fingerprint ([`token_fingerprint`]) of the refresh token the token
    /// endpoint rejected with a hard `invalid_grant`-class failure (400/401).
    ///
    /// Keyed by the TOKEN, not just the path, so the verdict self-heals: a
    /// re-login (or a server-side rotation) writes a different refresh token
    /// into the same file, whose fingerprint no longer matches, and the account
    /// is live again with no restart. Keying on the path alone would make one
    /// revocation permanent for the process lifetime.
    hard_failed_token: Option<u64>,
    /// Consecutive hard failures for [`Self::hard_failed_token`] — drives the
    /// exponential backoff in [`retry_backoff`].
    hard_failures: u32,
}

/// Background refresh state, keyed by credentials path.
static REFRESH_STATE: once_cell::sync::Lazy<Mutex<HashMap<PathBuf, RefreshState>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

/// Spacing before the next refresh attempt for `entry`: the flat
/// [`REFRESH_RETRY_BACKOFF`] while nothing has hard-failed, doubling per
/// consecutive hard failure up to [`REFRESH_HARD_FAIL_BACKOFF_CAP`].
///
/// Pure so the escalation is unit-testable without any network or timing.
fn retry_backoff(hard_failures: u32) -> Duration {
    if hard_failures == 0 {
        return REFRESH_RETRY_BACKOFF;
    }
    let factor = 1u32.checked_shl(hard_failures.min(16)).unwrap_or(u32::MAX);
    REFRESH_RETRY_BACKOFF
        .checked_mul(factor)
        .unwrap_or(REFRESH_HARD_FAIL_BACKOFF_CAP)
        .min(REFRESH_HARD_FAIL_BACKOFF_CAP)
}

/// Stable fingerprint of a refresh token. Only ever compared against another
/// fingerprint — the token itself is never stored in the refresh state, so a
/// state dump can't leak a credential. `0` means "no token".
fn token_fingerprint(token: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    if token.is_empty() {
        return 0;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut h);
    // Never collide with the "no token" sentinel.
    match h.finish() {
        0 => 1,
        v => v,
    }
}

/// Record that the token endpoint rejected the grant behind `creds_path`
/// (400/401 `invalid_grant`-class): the refresh token on disk is DEAD and no
/// amount of retrying revives it. Bumps the consecutive-hard-failure counter
/// that [`retry_backoff`] escalates on.
fn mark_hard_failure(creds_path: &Path, token_fp: u64) {
    if token_fp == 0 {
        return;
    }
    let Ok(mut state) = REFRESH_STATE.lock() else {
        return;
    };
    let entry = state.entry(creds_path.to_path_buf()).or_default();
    if entry.hard_failed_token == Some(token_fp) {
        entry.hard_failures = entry.hard_failures.saturating_add(1);
    } else {
        entry.hard_failed_token = Some(token_fp);
        entry.hard_failures = 1;
    }
    warn!(
        path = %creds_path.display(),
        attempts = entry.hard_failures,
        "OAuth refresh: the grant was REJECTED (logout elsewhere, password change, revoked or \
         rotated refresh token) — this account is reported INVALID until it is re-authenticated"
    );
}

/// Clear any recorded hard failure for `creds_path` — a refresh succeeded, so
/// whatever grant is on disk now works.
fn clear_hard_failure(creds_path: &Path) {
    let Ok(mut state) = REFRESH_STATE.lock() else {
        return;
    };
    if let Some(entry) = state.get_mut(creds_path) {
        entry.hard_failed_token = None;
        entry.hard_failures = 0;
    }
}

/// Has the grant CURRENTLY on disk at `creds_path` been rejected by the token
/// endpoint? `token_fp` is the fingerprint of the refresh token the file holds
/// right now ([`CredsSnapshot::refresh_token_fp`]), so a re-login clears the
/// verdict implicitly.
fn grant_hard_failed(creds_path: &Path, token_fp: u64) -> bool {
    if token_fp == 0 {
        return false;
    }
    let Ok(state) = REFRESH_STATE.lock() else {
        // A poisoned state map must never demote a live account.
        return false;
    };
    state
        .get(creds_path)
        .is_some_and(|e| e.hard_failed_token == Some(token_fp))
}

/// Whether the token endpoint's rejection means the GRANT is dead rather than
/// the request having failed transiently.
///
/// `invalid_grant` is the RFC 6749 §5.2 code for "the refresh token is expired,
/// revoked, malformed, or was issued to another client" — exactly the logout-
/// elsewhere / password-change / revoked-grant / already-rotated cases. A bare
/// 401 with no parseable body is treated the same way: the endpoint refused to
/// authenticate the grant. Everything else (429, 5xx, network) stays transient.
fn is_hard_grant_failure(status_code: u16, body: &str) -> bool {
    if !matches!(status_code, 400 | 401) {
        return false;
    }
    if body.contains("invalid_grant") || body.contains("invalid_client") {
        return true;
    }
    // A 401 is an authentication verdict on the grant even without a body we
    // can parse; a bodyless 400 is a malformed-request signal we do NOT want to
    // convert into "the operator's account is dead".
    status_code == 401
}

/// Ask for a background OAuth refresh of `creds_path` and return IMMEDIATELY.
///
/// This is the whole point of the module's redesign: the refresh used to run
/// inline on the terminal/AI spawn path (`creds_path_is_valid` →
/// `try_refresh_credentials`), so one expired token put a full HTTPS round-trip
/// to `platform.claude.com` — unbounded, and serialized with every other spawn
/// through the tokio worker it parked — between the operator's click and their
/// PTY. Spawn now always uses the credentials that are on disk *right now*; the
/// refresh lands out of band and the next spawn picks it up.
///
/// Deduped (one in-flight refresh per file) and backed off
/// ([`REFRESH_RETRY_BACKOFF`]) so a burst of spawns, or a persistently failing
/// grant, cannot become a token-endpoint storm.
pub(crate) fn request_background_refresh(creds_path: &Path) {
    // Under test the request is RECORDED and never performed: a unit test must
    // not POST to platform.claude.com. Recording it is also what lets the tests
    // assert that the credential predicates now ASK for a refresh where they
    // used to block on one.
    #[cfg(test)]
    {
        if let Ok(mut log) = tests::REFRESH_REQUESTS.lock() {
            log.push(creds_path.to_path_buf());
        }
    }
    #[cfg(not(test))]
    spawn_background_refresh(creds_path);
}

/// Claim the refresh slot for `creds_path` (dedupe + backoff) and run the
/// refresh on a dedicated OS thread. Split out of
/// [`request_background_refresh`] so the test build can stub the whole
/// side-effect at one seam.
#[cfg(not(test))]
fn spawn_background_refresh(creds_path: &Path) {
    let path = creds_path.to_path_buf();
    {
        let mut state = match REFRESH_STATE.lock() {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "OAuth refresh: state lock poisoned — skipping background refresh");
                return;
            }
        };
        let entry = state.entry(path.clone()).or_default();
        if entry.in_flight {
            return;
        }
        // Escalating spacing: flat 60s while failures look transient, doubling
        // per consecutive HARD failure so a permanently revoked grant stops
        // hammering the token endpoint once a minute forever.
        let backoff = retry_backoff(entry.hard_failures);
        if entry.last_attempt.is_some_and(|t| t.elapsed() < backoff) {
            return;
        }
        entry.in_flight = true;
        entry.last_attempt = Some(Instant::now());
    }

    // A dedicated OS thread rather than `spawn_blocking`: this must not consume
    // a runtime worker, and `refresh_credentials_blocking` builds/drops a
    // `reqwest::blocking` runtime that dislikes an ambient one.
    let spawned = std::thread::Builder::new()
        .name("oauth-refresh".to_string())
        .spawn(move || {
            if refresh_credentials_blocking(&path).is_none() {
                warn!(
                    path = %path.display(),
                    "OAuth background refresh failed — the next spawn uses the existing token and \
                     the provider surfaces its own auth prompt if it has expired"
                );
            }
            if let Ok(mut state) = REFRESH_STATE.lock() {
                if let Some(entry) = state.get_mut(&path) {
                    entry.in_flight = false;
                }
            }
        });
    if let Err(e) = spawned {
        warn!(error = %e, "OAuth refresh: could not spawn background refresh thread");
        if let Ok(mut state) = REFRESH_STATE.lock() {
            if let Some(entry) = state.get_mut(creds_path) {
                entry.in_flight = false;
            }
        }
    }
}

/// Refresh an expired Claude OAuth token, updating the credentials file in place.
///
/// Returns the new access token on success, `None` if the credentials are
/// missing a `refreshToken`, the network call fails, or the server rejects the
/// request.
///
/// **BLOCKS on the network.** Never call this from a spawn path — use
/// [`request_background_refresh`]. The remaining direct callers are surfaces
/// where the operator is explicitly waiting on the result (the account-settings
/// re-auth command, the warm API provider's own retry).
pub(crate) fn refresh_credentials_blocking(creds_path: &Path) -> Option<String> {
    let content = match std::fs::read_to_string(creds_path) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "OAuth refresh: cannot read credentials at {}: {}",
                creds_path.display(),
                e
            );
            return None;
        }
    };

    let mut json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            warn!("OAuth refresh: credentials JSON invalid: {}", e);
            return None;
        }
    };

    let refresh_token = match json["claudeAiOauth"]["refreshToken"].as_str() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            warn!("OAuth refresh: no refreshToken in credentials");
            return None;
        }
    };
    // Identifies the grant this attempt is about, so a rejection is recorded
    // against THIS token rather than against the file (see `mark_hard_failure`).
    let token_fp = token_fingerprint(&refresh_token);

    info!("OAuth refresh: requesting new access token");

    // Resolve scopes from the existing credentials (preserve what was granted).
    let scopes: Vec<String> = json["claudeAiOauth"]["scopes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    let scope_str = if scopes.is_empty() {
        "user:inference user:profile user:sessions:claude_code user:mcp_servers".to_string()
    } else {
        scopes.join(" ")
    };

    // The token endpoint requires JSON body (not form-encoded) and a Node-like
    // User-Agent to pass Cloudflare's bot detection on platform.claude.com.
    //
    // Run the blocking HTTP call on a dedicated OS thread so that
    // `reqwest::blocking::Client`'s internal tokio runtime is created and
    // dropped outside any existing async runtime context. Without this,
    // dropping the client inside `tokio::task::spawn_blocking` panics with
    // "Cannot drop a runtime in a context where blocking is not allowed" —
    // still load-bearing for the two `spawn_blocking` callers that remain
    // (`commands::ai_settings::probe_account_usage`, the warm API provider).
    let request_body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLIENT_ID,
        "scope": scope_str,
    });
    let http_result: Result<(bool, u16, String), String> = std::thread::spawn(move || {
        let client = reqwest::blocking::Client::new();
        let response = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/json")
            .header("User-Agent", "node/22.13.1")
            .header("Accept", "application/json, text/plain, */*")
            .json(&request_body)
            .send()
            .map_err(|e| format!("{e}"))?;
        let status = response.status();
        let body = response.text().unwrap_or_default();
        Ok((status.is_success(), status.as_u16(), body))
    })
    .join()
    .map_err(|_| "OAuth refresh thread panicked".to_string())
    .and_then(|r| r);

    let (success, status_code, body) = match http_result {
        Ok(t) => t,
        Err(e) => {
            warn!("OAuth refresh: request failed: {}", e);
            return None;
        }
    };

    if !success {
        warn!("OAuth refresh: server returned {}: {}", status_code, body);
        // A rejection of the GRANT itself (revoked / rotated / logged out
        // elsewhere) is terminal for this refresh token: record it so the
        // credential predicates stop reporting the account valid off the mere
        // PRESENCE of a refreshToken string on disk, and so the retry spacing
        // escalates instead of POSTing once a minute forever.
        if is_hard_grant_failure(status_code, &body) {
            mark_hard_failure(creds_path, token_fp);
        }
        return None;
    }

    let token_response: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!("OAuth refresh: response parse failed: {}", e);
            return None;
        }
    };

    let new_access_token = match token_response["access_token"].as_str() {
        Some(t) => t.to_string(),
        None => {
            warn!("OAuth refresh: no access_token in response");
            return None;
        }
    };

    // The grant works — clear any recorded hard failure so an account that was
    // re-authenticated (or whose rejection was misclassified) is live again.
    clear_hard_failure(creds_path);

    let expires_in_secs = token_response["expires_in"].as_i64().unwrap_or(86400);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let new_expires_at_ms = now_ms + expires_in_secs * 1000;

    json["claudeAiOauth"]["accessToken"] = serde_json::Value::String(new_access_token.clone());
    json["claudeAiOauth"]["expiresAt"] = serde_json::Value::Number(new_expires_at_ms.into());

    if let Some(new_refresh) = token_response["refresh_token"].as_str() {
        json["claudeAiOauth"]["refreshToken"] = serde_json::Value::String(new_refresh.to_string());
    }

    match serde_json::to_string_pretty(&json) {
        Ok(updated) => {
            if let Err(e) = std::fs::write(creds_path, updated) {
                warn!(
                    "OAuth refresh: failed to write updated credentials to {}: {}",
                    creds_path.display(),
                    e
                );
                // Return the new token anyway; the subprocess in this invocation
                // won't benefit from the file update, but the in-memory token works
                // for direct API calls.
            } else {
                info!(
                    "OAuth refresh: credentials refreshed (new expiry in {}s)",
                    expires_in_secs
                );
            }
        }
        Err(e) => warn!(
            "OAuth refresh: failed to serialize updated credentials: {}",
            e
        ),
    }

    Some(new_access_token)
}

/// Best-effort pre-flight for subprocess invocations (`claude --print`,
/// interactive stream-json, `ClaudeSession::spawn`): if the credentials for
/// `config_dir` are expired or about to be, kick off a BACKGROUND refresh.
///
/// **Never blocks and never waits for the network.** It used to `.join()` a
/// token POST before the subprocess could start, which is exactly the
/// unbounded, session-count-multiplying stall Phase 6 removes. The subprocess
/// spawns against whatever credentials are on disk; if they are past expiry the
/// provider surfaces its own auth prompt — the same UX as a refresh that fails
/// today, which was already swallowed with a warn.
pub(crate) fn try_ensure_valid_credentials(config_dir: Option<&str>) {
    if let Some(path) = find_creds_path(config_dir) {
        let snap = read_creds_snapshot(&path);
        if snap.needs_refresh(now_ms()) && snap.refreshable(&path) {
            debug!("OAuth token expiring — requesting background refresh (spawn is not blocked)");
            request_background_refresh(&path);
        }
    }
}

/// Test-only seam: record the refresh token CURRENTLY held by
/// `<config_dir>/.credentials.json` as server-rejected, exactly as a background
/// refresh that came back `invalid_grant` would. Lets the credential-selection
/// regression tests exercise the revoked-account path with no network.
#[cfg(test)]
pub(crate) fn mark_grant_revoked_for_test(config_dir: &str) {
    let creds_path = PathBuf::from(config_dir).join(".credentials.json");
    let snap = read_creds_snapshot(&creds_path);
    mark_hard_failure(&creds_path, snap.refresh_token_fp);
}

/// Whether `config_dir` has live, usable Claude OAuth credentials: a
/// `.credentials.json` exists *in that exact dir* AND the token is either
/// unexpired or still refreshable (a `refreshToken` the token endpoint has not
/// rejected — see [`creds_path_is_valid`]).
///
/// This is the highest-precedence account-selection filter (see
/// [`super::account_usage::pick_best_account`]): selection must never pin a
/// config dir that would 401 the moment a `claude` subprocess spawns under it.
///
/// IMPORTANT — checks `config_dir` **directly**, NOT via [`find_creds_path`].
/// `find_creds_path` falls back to `$CLAUDE_CONFIG_DIR` then `~/.claude` when
/// `dir` lacks creds; used as a per-candidate selection filter that fallback
/// would collapse every credential-less candidate onto the same shared file
/// and report them all identically — silently defeating the per-account
/// distinction this predicate exists to make.
pub(crate) fn has_valid_credentials(config_dir: &str) -> bool {
    let creds_path = PathBuf::from(config_dir).join(".credentials.json");
    creds_path_is_valid(&creds_path)
}

/// Whether the **ambient default** credential location has live, usable
/// credentials. This is the location a `claude` subprocess inherits when
/// `CLAUDE_CONFIG_DIR` is left unset — i.e. when account selection resolves no
/// explicit dir to pin. Resolution order matches [`find_creds_path`] with no
/// override: `$CLAUDE_CONFIG_DIR` then `~/.claude/.credentials.json`.
///
/// Spawn paths use this to decide whether an unset-`CLAUDE_CONFIG_DIR` spawn
/// would land on a real login or 401-zombie under a dead default.
pub(crate) fn default_location_has_valid_credentials() -> bool {
    if let Some(path) = find_creds_path(None) {
        if creds_path_is_valid(&path) {
            return true;
        }
    }
    // macOS keeps Claude Code's OAuth credentials in the login Keychain
    // (service "Claude Code-credentials"), NOT in ~/.claude/.credentials.json.
    // Without this, a logged-in macOS operator with an empty account roster
    // trips the spawn-path "no authenticated Claude account — run /login" abort
    // (e.g. `/spawn-ai`) even though a `claude` subprocess inheriting the unset
    // CLAUDE_CONFIG_DIR would authenticate fine via the Keychain.
    #[cfg(target_os = "macos")]
    {
        if macos_keychain_has_claude_credentials() {
            return true;
        }
    }
    false
}

/// True iff the macOS login Keychain holds a Claude Code credentials item.
/// Attribute-only query (no `-w`/`-g`), so it never reads the secret and never
/// triggers a Keychain access-control prompt.
#[cfg(target_os = "macos")]
fn macos_keychain_has_claude_credentials() -> bool {
    std::process::Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Shared validity core, NON-BLOCKING: an existing creds file that is
/// unexpired, or expired but still holding a `refreshToken` (so a refresh can
/// bring it back).
///
/// This used to answer the expired case by performing the refresh INLINE — a
/// network round-trip on the terminal- and AI-spawn paths (`config.rs` →
/// `has_valid_credentials` → here). It now answers from the file alone and
/// hands the refresh to [`request_background_refresh`], which also fires
/// PROACTIVELY within [`REFRESH_LEAD_MS`] of expiry so a live account is
/// normally refreshed long before any spawn could care.
///
/// The verdict, relative to the pre-Phase-6 blocking predicate:
/// - unexpired → valid (as before);
/// - expired with NO `refreshToken` → invalid, deterministically and with no
///   network call (as before — the old inline refresh returned `None` there);
/// - expired WITH a `refreshToken` the token endpoint has NOT rejected → valid.
///   Before, this was "valid iff the inline refresh succeeded". The account is
///   the one the operator has, the refresh is already in flight, and a lost race
///   surfaces the provider's own auth prompt instead of an "no authenticated
///   Claude account" abort;
/// - expired with a refresh token the server REJECTED (`invalid_grant` — logout
///   elsewhere, password change, revoked or already-rotated grant) → INVALID.
///   This arm is what keeps the non-blocking answer FALSIFIABLE. A revoked
///   refresh token is still a non-empty string on disk forever, so without it
///   the predicate would report a dead account valid for the process lifetime —
///   and since it is `pick_best_account`'s highest-precedence filter, a dead
///   account would be selectable OVER a healthy one, and the autonomous flows
///   gated on `default_location_has_valid_credentials` would march into a 401
///   loop. The rejection is observed by the BACKGROUND refresh, so the spawn
///   path still never touches the network.
fn creds_path_is_valid(creds_path: &Path) -> bool {
    if !creds_path.exists() {
        return false;
    }
    let snap = read_creds_snapshot(creds_path);
    let now = now_ms();
    let refreshable = snap.refreshable(creds_path);
    if snap.needs_refresh(now) && refreshable {
        request_background_refresh(creds_path);
    }
    !snap.is_expired(now) || refreshable
}

/// The facts every credential decision here needs, read in ONE parse.
///
/// Deliberately lenient: an unreadable or unparsable file, or one with no
/// `expiresAt`, yields `expires_at_ms == 0` — "expiry unknown", treated as NOT
/// expired. That is the pre-existing behaviour of `is_expired`, preserved so a
/// hand-edited or provider-specific credentials layout is never demoted to
/// "invalid account" on a parse quirk.
#[derive(Debug, Default, Clone, Copy)]
struct CredsSnapshot {
    expires_at_ms: i64,
    has_refresh_token: bool,
    /// Fingerprint of the refresh token currently on disk (`0` = none). Lets
    /// the hard-failure verdict be keyed to the exact grant that was rejected,
    /// so a re-login self-heals — see [`RefreshState::hard_failed_token`].
    refresh_token_fp: u64,
}

impl CredsSnapshot {
    fn is_expired(&self, now_ms: i64) -> bool {
        self.expires_at_ms != 0 && now_ms >= self.expires_at_ms
    }

    /// Expired, or close enough that a proactive refresh is worth starting.
    fn needs_refresh(&self, now_ms: i64) -> bool {
        self.expires_at_ms != 0 && now_ms >= self.expires_at_ms - REFRESH_LEAD_MS
    }

    /// Is there a refresh token that could actually bring this account back?
    /// A token the server has already rejected cannot — see
    /// [`grant_hard_failed`].
    fn refreshable(&self, creds_path: &Path) -> bool {
        self.has_refresh_token && !grant_hard_failed(creds_path, self.refresh_token_fp)
    }
}

fn read_creds_snapshot(creds_path: &Path) -> CredsSnapshot {
    let Ok(content) = std::fs::read_to_string(creds_path) else {
        return CredsSnapshot::default();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return CredsSnapshot::default();
    };
    let oauth = &json["claudeAiOauth"];
    let refresh_token_fp = oauth["refreshToken"]
        .as_str()
        .map(token_fingerprint)
        .unwrap_or(0);
    CredsSnapshot {
        expires_at_ms: oauth["expiresAt"].as_i64().unwrap_or(0),
        has_refresh_token: refresh_token_fp != 0,
        refresh_token_fp,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn find_creds_path(config_dir: Option<&str>) -> Option<PathBuf> {
    if let Some(dir) = config_dir {
        let p = PathBuf::from(dir).join(".credentials.json");
        if p.exists() {
            return Some(p);
        }
    }

    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let p = PathBuf::from(&dir).join(".credentials.json");
        if p.exists() {
            return Some(p);
        }
    }

    if let Some(home) = dirs::home_dir() {
        let p = home.join(".claude").join(".credentials.json");
        if p.exists() {
            return Some(p);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Background-refresh requests recorded instead of performed under test
    /// (see [`request_background_refresh`]).
    pub(super) static REFRESH_REQUESTS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    /// Serializes every test that touches [`REFRESH_REQUESTS`]. The log is
    /// process-global while `cargo test` runs tests in parallel threads, so
    /// without this a peer test's queued refresh lands inside another test's
    /// drain-and-assert and the exact-contents assertions flake.
    static REFRESH_LOG_GUARD: Mutex<()> = Mutex::new(());

    /// Hold for the duration of any test that queues or inspects refresh
    /// requests. Recovers from a poisoned guard (a panicking test must not
    /// cascade into every sibling).
    fn refresh_log_guard() -> std::sync::MutexGuard<'static, ()> {
        REFRESH_LOG_GUARD.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Drain the recorded refresh requests.
    fn taken_refresh_requests() -> Vec<PathBuf> {
        std::mem::take(&mut *REFRESH_REQUESTS.lock().expect("refresh log"))
    }

    /// Write a `.credentials.json` into a fresh temp dir and return
    /// `(tempdir_guard, dir_path_string)`. The guard must be kept alive for
    /// the duration of the test (drop removes the dir).
    fn dir_with_creds(expires_at_ms: i64, refresh_token: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let body = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "sk-oauth-test",
                "refreshToken": refresh_token,
                "expiresAt": expires_at_ms,
                "scopes": ["user:inference"],
            }
        });
        let mut f = std::fs::File::create(dir.path().join(".credentials.json")).expect("create");
        write!(f, "{body}").expect("write");
        let path = dir.path().to_string_lossy().to_string();
        (dir, path)
    }

    fn future_ms() -> i64 {
        (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64)
            + 3_600_000
    }

    fn past_ms() -> i64 {
        (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64)
            - 60_000
    }

    #[test]
    fn has_valid_credentials_false_when_dir_has_no_creds_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_string_lossy().to_string();
        assert!(!has_valid_credentials(&path));
    }

    #[test]
    fn has_valid_credentials_true_when_unexpired() {
        let (_guard, path) = dir_with_creds(future_ms(), "rt-present");
        assert!(has_valid_credentials(&path));
    }

    #[test]
    fn has_valid_credentials_false_when_expired_and_no_refresh_token() {
        let _log = refresh_log_guard();
        // Empty refreshToken → nothing can bring the account back, so an
        // expired, unrefreshable account is rejected deterministically and no
        // refresh is even requested (no live HTTP in this unit test).
        let _ = taken_refresh_requests();
        let (_guard, path) = dir_with_creds(past_ms(), "");
        assert!(!has_valid_credentials(&path));
        assert!(
            taken_refresh_requests().is_empty(),
            "an unrefreshable account must not queue a pointless token POST"
        );
    }

    /// Phase 6 (B3): the spawn-path credential predicate must never perform a
    /// network call. An expired-but-refreshable account is now reported VALID
    /// with a BACKGROUND refresh requested, instead of blocking the spawn on an
    /// inline token POST whose result decided the verdict.
    #[test]
    fn expired_but_refreshable_is_valid_and_only_queues_a_background_refresh() {
        let _log = refresh_log_guard();
        let _ = taken_refresh_requests();
        let (guard, path) = dir_with_creds(past_ms(), "rt-present");
        assert!(
            has_valid_credentials(&path),
            "an expired account with a refresh token must stay usable — the provider surfaces \
             its own auth prompt if the refresh loses the race"
        );
        let requested = taken_refresh_requests();
        assert_eq!(
            requested,
            vec![guard.path().join(".credentials.json")],
            "exactly one background refresh should have been requested, and no inline one performed"
        );
    }

    /// Proactive top-up: a token inside [`REFRESH_LEAD_MS`] of expiry is still
    /// valid AND queues a refresh, so ordinary use keeps an account current and
    /// a spawn never meets a hard-expired token in the first place.
    #[test]
    fn near_expiry_token_is_valid_and_queues_a_proactive_refresh() {
        let _log = refresh_log_guard();
        let _ = taken_refresh_requests();
        let soon = now_ms() + REFRESH_LEAD_MS / 2;
        let (guard, path) = dir_with_creds(soon, "rt-present");
        assert!(has_valid_credentials(&path));
        assert_eq!(
            taken_refresh_requests(),
            vec![guard.path().join(".credentials.json")]
        );
    }

    /// A comfortably-live token must not generate any refresh traffic at all.
    #[test]
    fn healthy_token_queues_no_refresh() {
        let _log = refresh_log_guard();
        let _ = taken_refresh_requests();
        let (_guard, path) = dir_with_creds(future_ms(), "rt-present");
        assert!(has_valid_credentials(&path));
        assert!(taken_refresh_requests().is_empty());
    }

    /// An unparsable credentials file keeps its historical verdict: expiry is
    /// UNKNOWN, which is not "expired", so the account is not demoted.
    #[test]
    fn unparsable_credentials_are_not_treated_as_expired() {
        let _log = refresh_log_guard();
        let _ = taken_refresh_requests();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".credentials.json"), b"{not json").expect("write");
        assert!(has_valid_credentials(&dir.path().to_string_lossy()));
        assert!(taken_refresh_requests().is_empty());
    }

    /// G1 REGRESSION. A refresh token the server has REVOKED (logout
    /// elsewhere, password change, grant revoked, already-rotated) is still a
    /// non-empty string on disk forever. The non-blocking predicate must
    /// therefore not answer off its mere PRESENCE: once the background refresh
    /// has observed the `invalid_grant`, the expired account is INVALID.
    #[test]
    fn expired_account_with_a_revoked_grant_is_invalid() {
        let _log = refresh_log_guard();
        let _ = taken_refresh_requests();
        let (_guard, path) = dir_with_creds(past_ms(), "rt-revoked");
        // Live until the server rejects the grant...
        assert!(has_valid_credentials(&path));
        let _ = taken_refresh_requests();

        // ...which is exactly what a background refresh returning 400
        // `invalid_grant` records.
        mark_grant_revoked_for_test(&path);

        assert!(
            !has_valid_credentials(&path),
            "a revoked grant must not keep reporting the account valid — it would be selected \
             over a healthy account and 401-zombie every spawn under it"
        );
        assert!(
            taken_refresh_requests().is_empty(),
            "a permanently revoked grant must not keep POSTing the token endpoint"
        );
    }

    /// The revocation verdict is keyed to the GRANT, not the file: a re-login
    /// writes a different refresh token into the same path and the account is
    /// usable again with no restart.
    #[test]
    fn re_login_clears_a_revoked_grant_verdict() {
        let _log = refresh_log_guard();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_string_lossy().to_string();
        let write_creds = |token: &str| {
            let body = serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "sk-oauth-test",
                    "refreshToken": token,
                    "expiresAt": past_ms(),
                    "scopes": ["user:inference"],
                }
            });
            std::fs::write(dir.path().join(".credentials.json"), body.to_string()).expect("write");
        };

        write_creds("rt-dead");
        mark_grant_revoked_for_test(&path);
        assert!(!has_valid_credentials(&path), "revoked grant ⇒ invalid");

        // The operator re-authenticates: a NEW refresh token lands in the same
        // file. The old verdict must not stick to the path.
        write_creds("rt-fresh-after-relogin");
        assert!(
            has_valid_credentials(&path),
            "a re-login must clear the revocation — keying the verdict on the path alone would \
             strand the account for the process lifetime"
        );
    }

    /// An UNEXPIRED access token stays usable even after its refresh grant was
    /// revoked: the token itself is still accepted until it expires. Only the
    /// expired-and-unrefreshable combination is invalid.
    #[test]
    fn unexpired_token_survives_a_revoked_refresh_grant() {
        let _log = refresh_log_guard();
        let (_guard, path) = dir_with_creds(future_ms(), "rt-revoked-but-token-live");
        mark_grant_revoked_for_test(&path);
        assert!(has_valid_credentials(&path));
    }

    /// Only a rejection of the GRANT is terminal. A 429/5xx/offline failure is
    /// transient and must never demote the account.
    #[test]
    fn only_grant_rejections_count_as_hard_failures() {
        assert!(is_hard_grant_failure(400, r#"{"error":"invalid_grant"}"#));
        assert!(is_hard_grant_failure(401, r#"{"error":"invalid_grant"}"#));
        assert!(is_hard_grant_failure(400, r#"{"error":"invalid_client"}"#));
        assert!(
            is_hard_grant_failure(401, ""),
            "a bare 401 rejects the grant"
        );
        assert!(
            !is_hard_grant_failure(400, r#"{"error":"invalid_request"}"#),
            "a malformed request is our bug, not a dead account"
        );
        for status in [429u16, 500, 502, 503] {
            assert!(
                !is_hard_grant_failure(status, "whatever"),
                "{status} is transient"
            );
        }
    }

    /// A permanently revoked grant must back off exponentially, not POST the
    /// token endpoint once a minute for the process lifetime.
    #[test]
    fn hard_failures_back_off_exponentially_up_to_a_cap() {
        assert_eq!(retry_backoff(0), REFRESH_RETRY_BACKOFF);
        assert_eq!(retry_backoff(1), REFRESH_RETRY_BACKOFF * 2);
        assert_eq!(retry_backoff(4), REFRESH_RETRY_BACKOFF * 16);
        // Monotonic, and capped rather than overflowing.
        let mut prev = retry_backoff(0);
        for n in 1..64u32 {
            let cur = retry_backoff(n);
            assert!(cur >= prev, "backoff must not shrink at {n}");
            assert!(cur <= REFRESH_HARD_FAIL_BACKOFF_CAP, "capped at {n}");
            prev = cur;
        }
        assert_eq!(retry_backoff(u32::MAX), REFRESH_HARD_FAIL_BACKOFF_CAP);
    }

    #[test]
    fn has_valid_credentials_checks_the_exact_dir_not_a_fallback() {
        // A dir that contains NO `.credentials.json` must report invalid even
        // when `$CLAUDE_CONFIG_DIR`/`~/.claude` happen to have one — this is the
        // distinction `find_creds_path`'s fallback chain would erase.
        let empty = tempfile::tempdir().expect("tempdir");
        let path = empty.path().to_string_lossy().to_string();
        assert!(
            !has_valid_credentials(&path),
            "credential-less dir must be invalid regardless of fallback locations"
        );
    }
}
