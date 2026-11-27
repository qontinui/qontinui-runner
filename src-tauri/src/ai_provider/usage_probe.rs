//! The per-account usage probe: what one Claude account has left, read from
//! Anthropic's OAuth usage endpoint when the endpoint's gate admits a token
//! that can read it (`super::oauth_usage_gate`), else from the rate-limit
//! headers of a 1-token Messages call.
//!
//! Moved out of `commands::ai_settings` (plan
//! `2026-11-26-runner-usage-source-is-dark-so-the-launch-budget-never-reads-a-session-figure`
//! Phase 2), behaviour-preserving: that file was past the size at which a split
//! is owed. The usage row type (`AccountUsageInfo`), the exhaustion predicate
//! and the refresh entry points stay there.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use once_cell::sync::Lazy;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
use serde::{Deserialize, Serialize};
use tracing::info;

use super::oauth_usage_gate::{UsageEndpointInfo, UsageEndpointState};
use crate::commands::ai_settings::{
    account_hash_for, account_label, AccountUsageInfo, ModelLimitInfo,
};
use crate::error::AppError;

/// Compute expected usage fields from the reset timestamp.
///
/// The Anthropic 7-day window resets at `resets_at`. We compute how far
/// through the current 7-day period we are and derive expected linear usage.
fn compute_expected_usage(
    utilization: f64,
    resets_at: Option<u64>,
) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>) {
    let Some(reset_ts) = resets_at else {
        return (None, None, None, None);
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now == 0 || reset_ts <= now {
        // Period already expired or clock issue
        return (None, None, None, None);
    }
    const PERIOD_SECS: f64 = 7.0 * 24.0 * 3600.0; // 7 days in seconds
    let remaining_secs = (reset_ts - now) as f64;
    let elapsed_secs = PERIOD_SECS - remaining_secs;
    // Clamp: if elapsed_secs < 0 the period just started fresh
    let elapsed_secs = elapsed_secs.max(0.0);
    let elapsed_fraction = (elapsed_secs / PERIOD_SECS).clamp(0.0, 1.0);
    let remaining_days = remaining_secs / 86400.0;
    let expected = elapsed_fraction; // linear expectation
    let delta = utilization - expected;
    (
        Some(expected),
        Some(delta),
        Some(elapsed_fraction),
        Some(remaining_days),
    )
}

/// Read the OAuth token a Claude config directory authenticates with: its
/// long-lived `claude setup-token` token (`settings.json`
/// `env.CLAUDE_CODE_OAUTH_TOKEN`) when it has one — that is what a `claude`
/// child under this dir uses, since Claude Code applies the settings `env`
/// block — else the access token in its `.credentials.json`.
pub(crate) fn read_oauth_token(config_dir: &str) -> Result<String, String> {
    if let Some(token) =
        qontinui_runner_lib::claude_oauth_token::from_config_dir(std::path::Path::new(config_dir))
    {
        return Ok(token);
    }
    let creds_path = std::path::PathBuf::from(config_dir).join(".credentials.json");
    let content = std::fs::read_to_string(&creds_path).map_err(|e| {
        String::from(AppError::IoError(std::io::Error::new(
            e.kind(),
            format!("Cannot read {}: {}", creds_path.display(), e),
        )))
    })?;
    let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        String::from(AppError::ParseError(format!(
            "Invalid credentials JSON: {}",
            e
        )))
    })?;
    json["claudeAiOauth"]["accessToken"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "No accessToken in credentials".to_string())
}

/// Whether the OAuth credentials at `creds_path` carry an `expiresAt` that is
/// already in the past. Returns `false` on any read/parse failure or when the
/// field is absent — callers fall back to the existing error-surfacing path.
pub(crate) fn is_oauth_token_expired(creds_path: &std::path::Path) -> bool {
    let Ok(content) = std::fs::read_to_string(creds_path) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    let expires_at_ms = json["claudeAiOauth"]["expiresAt"].as_i64().unwrap_or(0);
    if expires_at_ms <= 0 {
        return false;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    now_ms >= expires_at_ms
}

/// How long a SUCCESSFUL probe result is reused for the pollers: the
/// Terminal page's 5-minute usage poll (`check_accounts_usage`), the Settings
/// "check usage" button beside it, `/analytics/account-usage`, and the
/// 10-minute fleet report (plan
/// `2026-11-26-runner-usage-source-is-dark-so-the-launch-budget-never-reads-a-session-figure`
/// D7).
///
/// 270 s, strictly below the 300 s Terminal period minus a fetch's latency,
/// so each Terminal poll refetches and the 10-minute timer is absorbed
/// wherever it phases: about 18 -> 12 calls an hour per account where the two
/// are out of phase. A max age EQUAL to the period would make every poll a
/// race between timer lateness and fetch latency, since a result is stamped
/// when its fetch completes. A reading this old is well inside the picker's
/// own snapshot TTLs (`ai_provider::config`, 15 minutes and 1 hour).
pub const USAGE_PROBE_POLL_MAX_AGE: Duration = Duration::from_secs(270);

/// How long a successful probe result is reused for a caller whose reason to
/// probe is a FRESH reading: the selection snapshot (startup, and
/// `account_migration`'s roster probe), `account_migration`'s own-account
/// probe, and `limit_hit::cool_until_reset` — confirming a limit that was just
/// hit. A minute, the probe's reuse before D7.
pub const USAGE_PROBE_CONFIRM_MAX_AGE: Duration = Duration::from_secs(60);

/// The fleet report's poll period (`main.rs`'s 10-minute timer driving
/// `refresh_fleet_usage_report`): the only steady prober of every account.
/// `oauth_usage_gate::REFRESH_AHEAD` is derived from it.
pub const FLEET_USAGE_REPORT_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// How long a FAILED probe result (an `error` row) is reused, whatever the
/// caller's max age: a minute, so a 429'd account is not re-probed into
/// another 429 on the next caller's tick, nor held unprobed for minutes.
pub(crate) const USAGE_PROBE_FAILURE_TTL: Duration = Duration::from_secs(60);

/// Shared single-flight + TTL cache in front of the live probe. Keyed by
/// `config_dir`, which is exactly the per-account identity the probe and the
/// selection snapshot already use.
pub(crate) static USAGE_PROBE_CACHE: Lazy<
    crate::commands::usage_probe_cache::CoalescingCache<AccountUsageInfo>,
> = Lazy::new(|| {
    crate::commands::usage_probe_cache::CoalescingCache::new(USAGE_PROBE_FAILURE_TTL, |info| {
        info.error.is_some()
    })
});

/// Whether a usage reading says the account has a five-hour session window
/// (plan `2026-11-26-…` D1). Sent to coord beside the reading, so the launch
/// budget can tell an account that HAS no session window on this path from
/// one whose window simply was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionWindow {
    /// The source reported a five-hour window; `session_utilization` holds it.
    Reported,
    /// Positive evidence there is no five-hour window on this path: the
    /// header probe's answer carries the whole unified rate-limit family
    /// (`-unified-status`, `-unified-representative-claim` and
    /// `-unified-7d-utilization`) and no `-unified-5h-*` header at all, or a
    /// 2xx OAuth usage body reports `five_hour` null or missing with no
    /// `kind: "session"` limit. Measured for every Max 20x account on the
    /// box this plan was written on.
    Absent,
    /// Neither: the reading says nothing either way (a failed probe, a
    /// response missing the unified family, a five-hour object with no
    /// usable figure). UNKNOWN, never "no window".
    #[default]
    Unread,
}

/// The rate-limit header family every usage-probe header starts with.
const UNIFIED: &str = "anthropic-ratelimit-unified-";

/// Classify a header-probe response's session window (D1). Pure. Reads the
/// headers only, never the status: a 429 carrying the unified family is as
/// much an observation of which windows exist as a 200.
pub fn classify_session_window(headers: &reqwest::header::HeaderMap) -> SessionWindow {
    let five_hour = format!("{UNIFIED}5h-");
    let utilization_5h = headers
        .get(format!("{five_hour}utilization").as_str())
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<f64>().ok());
    if utilization_5h.is_some() {
        return SessionWindow::Reported;
    }
    if headers.keys().any(|k| k.as_str().starts_with(&five_hour)) {
        // A five-hour header with no readable utilization: the window exists
        // and its figure was not read.
        return SessionWindow::Unread;
    }
    let family = ["status", "representative-claim", "7d-utilization"]
        .iter()
        .all(|suffix| headers.contains_key(format!("{UNIFIED}{suffix}").as_str()));
    if family {
        SessionWindow::Absent
    } else {
        SessionWindow::Unread
    }
}

/// The accounts for which the OAuth usage endpoint has reported a five-hour
/// window this process lifetime (D1's guard). Keyed by `account_hash`, else by
/// `config_dir` for a dir with no readable identity.
static FIVE_HOUR_SEEN: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

fn latch_key(info: &AccountUsageInfo) -> String {
    match &info.account_hash {
        Some(hash) => format!("hash:{hash}"),
        None => format!("dir:{}", info.config_dir),
    }
}

/// D1's latch over one reading, after its `account_hash` is known. An OAuth
/// reading that reports a five-hour window latches the account; from then on a
/// header-probe `Absent` for that account is reported `Unread`, because the two
/// sources disagreeing is evidence that the header inference is wrong for it.
fn apply_five_hour_latch(seen: &mut HashSet<String>, info: &mut AccountUsageInfo) {
    let key = latch_key(info);
    match info.source.as_deref() {
        Some("oauth_usage") if info.session_window == SessionWindow::Reported => {
            seen.insert(key);
        }
        Some("probe") if info.session_window == SessionWindow::Absent && seen.contains(&key) => {
            info.session_window = SessionWindow::Unread;
        }
        _ => {}
    }
}

/// The `anthropic-ratelimit-*` header NAMES of a response, sorted and
/// deduplicated. Names only: the values are never logged beyond what the
/// probe's own line prints.
fn rate_limit_header_names(headers: &reqwest::header::HeaderMap) -> Vec<String> {
    let mut names: Vec<String> = headers
        .keys()
        .map(|k| k.as_str().to_string())
        .filter(|k| k.starts_with("anthropic-ratelimit-"))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The (account, token fingerprint) pairs whose header names were logged.
static HEADER_NAMES_LOGGED: Lazy<Mutex<HashSet<(String, u64)>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

/// Log the rate-limit header names a probe saw, once per account and token
/// per process at INFO: the evidence D1's header inference is drawn from.
fn log_header_names_once(
    config_dir: &str,
    label: &str,
    token_fp: u64,
    headers: &reqwest::header::HeaderMap,
) {
    let first = HEADER_NAMES_LOGGED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert((config_dir.to_string(), token_fp));
    if first {
        info!(
            "Account '{}': header probe rate-limit header names: {}",
            label,
            rate_limit_header_names(headers).join(",")
        );
    }
}

/// Probe a single account for its weekly rate limit utilization.
///
/// Reads through a shared single-flight + TTL cache
/// ([`USAGE_PROBE_CACHE`]): concurrent callers for the same account await ONE
/// upstream request, and a successful result is reused for `max_age`
/// ([`USAGE_PROBE_POLL_MAX_AGE`] for a poller, [`USAGE_PROBE_CONFIRM_MAX_AGE`]
/// where a fresh reading is the point), a failed one for at most
/// [`USAGE_PROBE_FAILURE_TTL`].
///
/// This is not an optimization. The probe has four independent callers that
/// each fanned out one request per configured account with no coalescing and
/// no TTL — measured at 25 usage checks per 5-minute tick and **18,981 HTTP
/// 429s per day** in the dev logs. Because the probe consumes the same
/// per-account quota the CLI does, that stampede both competed with real work
/// and then reported the 429s it earned as account exhaustion.
///
/// See [`crate::commands::usage_probe_cache`] for the mechanism.
pub async fn probe_account_usage(config_dir: String, max_age: Duration) -> AccountUsageInfo {
    // Subscription accounts are off while a model gateway is declared, and a
    // subscription token must not be spent or sent on their behalf
    // (`crate::model_gateway`). Answered before the cache so no stale sample
    // outlives the switch.
    if crate::model_gateway::gateway_declared() {
        let label = account_label(&config_dir);
        let account_hash = account_hash_for(&config_dir, dirs::home_dir().as_deref());
        return AccountUsageInfo {
            config_dir,
            label,
            error: Some(crate::model_gateway::SUBSCRIPTION_OFF_REASON.to_string()),
            account_hash,
            usage_endpoint: Some(UsageEndpointInfo::bare(UsageEndpointState::Gateway)),
            ..Default::default()
        };
    }
    let key = config_dir.clone();
    USAGE_PROBE_CACHE
        .get_or_fetch(&key, max_age, move || async move {
            // Identity is read here, after the probe, so every return path of
            // the probe (token error, probe error, success) carries it: an
            // errored row is exactly the one a consumer must collapse onto its
            // healthy twin on another device.
            let hash_dir = config_dir.clone();
            let mut info = probe_account_usage_uncached(config_dir).await;
            info.account_hash = account_hash_for(&hash_dir, dirs::home_dir().as_deref());
            apply_five_hour_latch(
                &mut FIVE_HOUR_SEEN.lock().unwrap_or_else(|e| e.into_inner()),
                &mut info,
            );
            info
        })
        .await
}

/// The live probe. Asks the OAuth usage endpoint first, through its gate
/// (`ai_provider::oauth_usage_gate`: the token that can read it, and a
/// cooldown for a token it refused). When that gives no answer, makes a
/// minimal API call (1 token, cheapest model) and reads the
/// `anthropic-ratelimit-unified-7d-utilization` response header, which always
/// contains the exact weekly usage fraction regardless of threshold.
///
/// Call [`probe_account_usage`] instead — going straight to this bypasses the
/// coalescing that keeps the runner off Anthropic's rate limiter.
async fn probe_account_usage_uncached(config_dir: String) -> AccountUsageInfo {
    let label = account_label(&config_dir);

    // Pre-flight: if the stored OAuth token is already expired, attempt a
    // silent refresh BEFORE issuing the probe. Mirrors the warm-path discipline
    // at `claude_api_warm.rs:174-188`. Refresh failure is non-fatal — we still
    // try the probe with whatever token we have so the caller sees a real auth
    // error rather than a swallowed "couldn't refresh."
    //
    // Skipped when the dir holds a long-lived `claude setup-token` token: that
    // is the token `read_oauth_token` returns and the header probe sends. The
    // `.credentials.json` grant beside it is the usage endpoint's token, which
    // the endpoint's gate refreshes in the BACKGROUND, gated on the grant
    // still being refreshable (`oauth_usage_gate::candidates_for_dir`), so a
    // dead grant is not POSTed once per probe.
    let creds_path = std::path::PathBuf::from(&config_dir).join(".credentials.json");
    let has_long_lived_token =
        qontinui_runner_lib::claude_oauth_token::from_config_dir(std::path::Path::new(&config_dir))
            .is_some();
    if !has_long_lived_token && is_oauth_token_expired(&creds_path) {
        // refresh_credentials_blocking uses reqwest::blocking::Client internally.
        // Calling it directly from async context panics ("Cannot drop a runtime
        // in a context where blocking is not allowed"), so offload to a blocking
        // thread. Refresh failure is non-fatal — we still probe with whatever
        // token we have.
        let creds_for_refresh = creds_path.clone();
        let _ = spawn_blocking_tracked(move || {
            crate::ai_provider::oauth_refresh::refresh_credentials_blocking(&creds_for_refresh)
        })
        .await;
    }

    // Preferred source: the OAuth usage endpoint — zero token cost, exact
    // session-window (5h) + weekly utilization and reset times (the same
    // numbers `/usage` shows), and the per-model caps no header carries. The
    // gate picks the token that can read it and holds back one it refused.
    let endpoint =
        crate::ai_provider::oauth_usage_gate::ask_usage_endpoint(&config_dir, &label).await;
    if let Some(snap) = endpoint.snapshot {
        return oauth_usage_row(config_dir, label, snap, endpoint.info);
    }

    let token = match read_oauth_token(&config_dir) {
        Ok(t) => t,
        Err(e) => {
            return AccountUsageInfo {
                config_dir,
                label,
                utilization: 1.0,
                error: Some(e),
                usage_endpoint: Some(endpoint.info),
                ..Default::default()
            };
        }
    };

    // Make a minimal API call — Haiku with max_tokens=1 is the cheapest possible.
    // OAuth tokens (`sk-ant-oat*`) must go via `Authorization: Bearer` +
    // `anthropic-beta: oauth-2025-04-20`; API keys (`sk-ant-api*`) go via
    // `x-api-key`. `anthropic_auth::apply_async` dispatches on token prefix.
    let client = reqwest::Client::new();
    // Not a coord write: Anthropic's API, authenticated by the operator's own
    // OAuth/API credential via `anthropic_auth::apply_async`.
    // Always the vendor route: this measures a subscription account's own quota,
    // and `probe_account_usage` never reaches here while a gateway is declared.
    let request = crate::model_gateway::ModelCall::Vendor {
        token: token.to_string(),
    }
    .post_async(&client, crate::model_gateway::MESSAGES_PATH)
    .header("anthropic-version", "2023-06-01")
    .header("content-type", "application/json")
    .json(&serde_json::json!({
        "model": "claude-haiku-4-5-20251001",
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "hi"}]
    }));
    let response = request.send().await;

    match response {
        Ok(resp) => {
            let headers = resp.headers().clone();
            log_header_names_once(
                &config_dir,
                &label,
                super::oauth_refresh::token_fingerprint(&token),
                &headers,
            );
            let error = if resp.status().is_success() {
                None
            } else {
                let status_code = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                Some(format!("API error ({}): {}", status_code, body))
            };
            let row = header_probe_row(config_dir, label, &headers, error, endpoint.info);
            info!(
                "Account '{}': 7d={:.0}% 5h={:.0}% status={:?}",
                row.label,
                row.utilization * 100.0,
                row.session_utilization.unwrap_or(0.0) * 100.0,
                row.status
            );
            row
        }
        Err(e) => AccountUsageInfo {
            config_dir,
            label,
            utilization: 1.0,
            error: Some(format!("Network error: {}", e)),
            source: Some("probe".to_string()),
            usage_endpoint: Some(endpoint.info),
            ..Default::default()
        },
    }
}

/// The usage row the OAuth usage endpoint's answer makes.
fn oauth_usage_row(
    config_dir: String,
    label: String,
    snap: crate::ai_provider::oauth_usage::OauthUsageSnapshot,
    endpoint: UsageEndpointInfo,
) -> AccountUsageInfo {
    let weekly = snap.seven_day.clone();
    let session = snap.five_hour.clone();
    let utilization_7d = weekly.as_ref().map(|w| w.utilization).unwrap_or(0.0);
    let resets_at_7d = weekly.as_ref().and_then(|w| w.resets_at);
    let (expected, delta, elapsed_frac, remaining_days) =
        compute_expected_usage(utilization_7d, resets_at_7d);
    // `5h=null` when the endpoint reported no five-hour window: that is an
    // observation, distinct from a 0 % window.
    info!(
        "Account '{}' (oauth_usage): 7d={:.0}% 5h={}",
        label,
        utilization_7d * 100.0,
        session
            .as_ref()
            .map(|s| format!("{:.0}%", s.utilization * 100.0))
            .unwrap_or_else(|| "null".to_string()),
    );
    AccountUsageInfo {
        config_dir,
        label,
        utilization: utilization_7d,
        rate_limit_type: Some("seven_day".to_string()),
        resets_at: resets_at_7d,
        status: snap.blocking_severity.then(|| "blocked".to_string()),
        error: None,
        expected_utilization: expected,
        usage_delta: delta,
        period_elapsed_fraction: elapsed_frac,
        period_remaining_days: remaining_days,
        session_utilization: session.as_ref().map(|s| s.utilization),
        session_resets_at: session.as_ref().and_then(|s| s.resets_at),
        model_limits: snap
            .model_limits
            .into_iter()
            .map(|m| ModelLimitInfo {
                model: m.model,
                utilization: m.utilization,
                resets_at: m.resets_at,
            })
            .collect(),
        source: Some("oauth_usage".to_string()),
        // Filled by `probe_account_usage`'s cached closure.
        account_hash: None,
        usage_endpoint: Some(endpoint),
        session_window: snap.session_window,
        representative_claim: None,
    }
}

/// The usage row a header-probe response makes. `error` is `Some` exactly
/// when the probe call itself failed (a non-2xx); the endpoint's state rides
/// in `usage_endpoint` and NEVER becomes an `error`, whatever it is.
pub(crate) fn header_probe_row(
    config_dir: String,
    label: String,
    headers: &reqwest::header::HeaderMap,
    error: Option<String>,
    endpoint: UsageEndpointInfo,
) -> AccountUsageInfo {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());

    // Parse weekly (7-day) utilization from response headers
    let utilization_7d = header("anthropic-ratelimit-unified-7d-utilization")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);
    let status_7d = header("anthropic-ratelimit-unified-7d-status").map(|s| s.to_string());
    let resets_at_7d =
        header("anthropic-ratelimit-unified-7d-reset").and_then(|v| v.parse::<u64>().ok());
    // The 5-hour session window, where the provider sends one, and its reset.
    let utilization_5h =
        header("anthropic-ratelimit-unified-5h-utilization").and_then(|v| v.parse::<f64>().ok());
    let resets_at_5h =
        header("anthropic-ratelimit-unified-5h-reset").and_then(|v| v.parse::<u64>().ok());

    let (expected, delta, elapsed_frac, remaining_days) =
        compute_expected_usage(utilization_7d, resets_at_7d);

    AccountUsageInfo {
        config_dir,
        label,
        utilization: utilization_7d,
        rate_limit_type: Some("seven_day".to_string()),
        resets_at: resets_at_7d,
        status: status_7d,
        error,
        expected_utilization: expected,
        usage_delta: delta,
        period_elapsed_fraction: elapsed_frac,
        period_remaining_days: remaining_days,
        session_utilization: utilization_5h,
        session_resets_at: resets_at_5h,
        source: Some("probe".to_string()),
        usage_endpoint: Some(endpoint),
        session_window: classify_session_window(headers),
        representative_claim: header("anthropic-ratelimit-unified-representative-claim")
            .map(str::to_string),
        ..Default::default()
    }
}

/// A header-probe row built from literal `(name, value)` headers, as the
/// probe builds it from a real response. Test support for the consumers of
/// the row's fields.
#[cfg(test)]
pub(crate) fn probe_row_from_headers(
    config_dir: &str,
    headers: &[(&str, &str)],
) -> AccountUsageInfo {
    let mut map = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        map.insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            value.parse().expect("header value"),
        );
    }
    header_probe_row(
        config_dir.to_string(),
        account_label(config_dir),
        &map,
        None,
        UsageEndpointInfo::bare(UsageEndpointState::ScopeRefused),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ai_settings::probe_result_exhausted;

    /// The header-probe answer of a healthy account, as literal headers.
    fn healthy_probe_headers() -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        for (name, value) in [
            ("anthropic-ratelimit-unified-status", "allowed"),
            ("anthropic-ratelimit-unified-7d-utilization", "0.25"),
            ("anthropic-ratelimit-unified-7d-status", "allowed"),
            ("anthropic-ratelimit-unified-7d-reset", "1800000000"),
        ] {
            h.insert(name, value.parse().unwrap());
        }
        h
    }

    /// D8: the usage endpoint refusing an account's token is not the account
    /// refusing work. A healthy header probe beside a scope refusal reads
    /// NOT exhausted, with the refusal in its own field and `error` empty.
    /// Folding the refusal into `error` would mark every account on a
    /// setup-token box exhausted.
    #[test]
    fn a_scope_refused_endpoint_leaves_the_account_unexhausted() {
        use crate::ai_provider::oauth_usage_gate::UsageTokenKind;
        let refused = UsageEndpointInfo {
            state: UsageEndpointState::ScopeRefused,
            token_kind: Some(UsageTokenKind::LongLived),
            retry_at: None,
        };
        let row = header_probe_row(
            "/test/acct".to_string(),
            "acct".to_string(),
            &healthy_probe_headers(),
            None,
            refused.clone(),
        );
        assert_eq!(row.error, None);
        assert_eq!(row.usage_endpoint, Some(refused));
        assert!(!probe_result_exhausted(&row));
        assert_eq!(row.source.as_deref(), Some("probe"));
        assert!((row.utilization - 0.25).abs() < 1e-9);
    }

    /// Cooling is the same: an endpoint holding back a 429'd token says
    /// nothing about the account's own quota.
    #[test]
    fn a_cooling_endpoint_leaves_the_account_unexhausted() {
        let cooling = UsageEndpointInfo {
            state: UsageEndpointState::Cooling,
            token_kind: None,
            retry_at: Some(1_800_000_300),
        };
        let row = header_probe_row(
            "/test/acct".to_string(),
            "acct".to_string(),
            &healthy_probe_headers(),
            None,
            cooling,
        );
        assert_eq!(row.error, None);
        assert!(!probe_result_exhausted(&row));
    }

    // ── D1: the session window, from literal header maps ──────────────────

    fn headers(pairs: &[(&str, &str)]) -> reqwest::header::HeaderMap {
        let mut map = reqwest::header::HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        map
    }

    /// Why 4's first set: a hashed Max 20x account (`.claude-paktis`). The
    /// whole unified family, a seven-day representative claim, and no
    /// `-unified-5h-*` header at all.
    const MAX_20X: &[(&str, &str)] = &[
        ("anthropic-ratelimit-unified-status", "allowed"),
        ("anthropic-ratelimit-unified-reset", "1800300000"),
        (
            "anthropic-ratelimit-unified-representative-claim",
            "seven_day",
        ),
        ("anthropic-ratelimit-unified-fallback-percentage", "0.5"),
        ("anthropic-ratelimit-unified-overage-status", "rejected"),
        (
            "anthropic-ratelimit-unified-overage-disabled-reason",
            "org_level_disabled",
        ),
        ("anthropic-ratelimit-unified-7d-utilization", "0.41"),
        ("anthropic-ratelimit-unified-7d-status", "allowed"),
        ("anthropic-ratelimit-unified-7d-reset", "1800300000"),
    ];

    /// Why 4's second set: an unhashed account (`.claude-peter`). The same
    /// family plus the three five-hour headers, claim `five_hour`.
    const FIVE_HOUR: &[(&str, &str)] = &[
        ("anthropic-ratelimit-unified-status", "allowed"),
        ("anthropic-ratelimit-unified-reset", "1800010000"),
        (
            "anthropic-ratelimit-unified-representative-claim",
            "five_hour",
        ),
        ("anthropic-ratelimit-unified-fallback-percentage", "0.5"),
        ("anthropic-ratelimit-unified-overage-status", "rejected"),
        ("anthropic-ratelimit-unified-5h-utilization", "0.12"),
        ("anthropic-ratelimit-unified-5h-status", "allowed"),
        ("anthropic-ratelimit-unified-5h-reset", "1800010000"),
        ("anthropic-ratelimit-unified-7d-utilization", "0.33"),
        ("anthropic-ratelimit-unified-7d-status", "allowed"),
        ("anthropic-ratelimit-unified-7d-reset", "1800300000"),
    ];

    fn row(pairs: &[(&str, &str)], error: Option<&str>) -> AccountUsageInfo {
        header_probe_row(
            "/test/acct".to_string(),
            "acct".to_string(),
            &headers(pairs),
            error.map(str::to_string),
            UsageEndpointInfo::bare(UsageEndpointState::ScopeRefused),
        )
    }

    #[test]
    fn a_max_20x_header_set_reads_the_session_window_absent() {
        let r = row(MAX_20X, None);
        assert_eq!(r.session_window, SessionWindow::Absent);
        assert_eq!(r.session_utilization, None);
        assert_eq!(r.session_resets_at, None);
        assert_eq!(r.representative_claim.as_deref(), Some("seven_day"));
    }

    #[test]
    fn a_five_hour_header_set_reads_reported_with_its_reset() {
        let r = row(FIVE_HOUR, None);
        assert_eq!(r.session_window, SessionWindow::Reported);
        assert_eq!(r.session_utilization, Some(0.12));
        assert_eq!(r.session_resets_at, Some(1_800_010_000));
        assert_eq!(r.representative_claim.as_deref(), Some("five_hour"));
    }

    /// Without `-unified-status` the family is incomplete: no evidence of
    /// absence, so UNKNOWN.
    #[test]
    fn a_response_without_the_unified_status_is_unread() {
        let partial: Vec<(&str, &str)> = MAX_20X
            .iter()
            .copied()
            .filter(|(n, _)| *n != "anthropic-ratelimit-unified-status")
            .collect();
        assert_eq!(row(&partial, None).session_window, SessionWindow::Unread);
        assert_eq!(row(&[], None).session_window, SessionWindow::Unread);
    }

    /// The classification reads the headers, not the status: a 429 that
    /// carries the unified family is an observation of which windows exist.
    #[test]
    fn a_429_carrying_the_unified_family_is_absent() {
        let r = row(MAX_20X, Some("API error (429): rate limited"));
        assert!(r.error.is_some());
        assert_eq!(r.session_window, SessionWindow::Absent);
    }

    /// A five-hour header with no readable utilization: the window exists,
    /// its figure did not come.
    #[test]
    fn a_five_hour_header_without_a_utilization_is_unread() {
        let r = row(
            &[
                ("anthropic-ratelimit-unified-status", "allowed"),
                (
                    "anthropic-ratelimit-unified-representative-claim",
                    "five_hour",
                ),
                ("anthropic-ratelimit-unified-7d-utilization", "0.33"),
                ("anthropic-ratelimit-unified-5h-status", "allowed"),
            ],
            None,
        );
        assert_eq!(r.session_window, SessionWindow::Unread);
    }

    // ── D1's latch ────────────────────────────────────────────────────────

    fn reading(
        dir: &str,
        hash: Option<&str>,
        source: &str,
        window: SessionWindow,
    ) -> AccountUsageInfo {
        AccountUsageInfo {
            config_dir: dir.to_string(),
            account_hash: hash.map(str::to_string),
            source: Some(source.to_string()),
            session_window: window,
            ..Default::default()
        }
    }

    /// A hashed account: the OAuth source reported a five-hour window under
    /// one dir, so a probe `absent` for the SAME account under another dir
    /// reads `unread`; a different account is untouched.
    #[test]
    fn the_latch_turns_a_hashed_accounts_probe_absent_unread() {
        let mut seen = HashSet::new();
        let mut oauth = reading("/a", Some("h1"), "oauth_usage", SessionWindow::Reported);
        apply_five_hour_latch(&mut seen, &mut oauth);
        assert_eq!(oauth.session_window, SessionWindow::Reported);
        let mut same = reading("/b", Some("h1"), "probe", SessionWindow::Absent);
        apply_five_hour_latch(&mut seen, &mut same);
        assert_eq!(same.session_window, SessionWindow::Unread);
        let mut other = reading("/c", Some("h2"), "probe", SessionWindow::Absent);
        apply_five_hour_latch(&mut seen, &mut other);
        assert_eq!(other.session_window, SessionWindow::Absent);
    }

    /// An unhashed dir has no identity, so the latch keys on its config dir:
    /// it flips that dir only, never another unhashed one.
    #[test]
    fn the_latch_turns_an_unhashed_dirs_probe_absent_unread() {
        let mut seen = HashSet::new();
        let mut oauth = reading("/peter", None, "oauth_usage", SessionWindow::Reported);
        apply_five_hour_latch(&mut seen, &mut oauth);
        let mut same = reading("/peter", None, "probe", SessionWindow::Absent);
        apply_five_hour_latch(&mut seen, &mut same);
        assert_eq!(same.session_window, SessionWindow::Unread);
        let mut other = reading("/other", None, "probe", SessionWindow::Absent);
        apply_five_hour_latch(&mut seen, &mut other);
        assert_eq!(other.session_window, SessionWindow::Absent);
    }

    /// Before the OAuth source has shown a window, a probe `absent` stands;
    /// an OAuth `absent` latches nothing.
    #[test]
    fn without_an_oauth_five_hour_window_the_probe_absent_stands() {
        let mut seen = HashSet::new();
        let mut oauth = reading("/a", Some("h1"), "oauth_usage", SessionWindow::Absent);
        apply_five_hour_latch(&mut seen, &mut oauth);
        let mut probe = reading("/a", Some("h1"), "probe", SessionWindow::Absent);
        apply_five_hour_latch(&mut seen, &mut probe);
        assert_eq!(probe.session_window, SessionWindow::Absent);
        assert!(seen.is_empty());
    }

    #[test]
    fn the_logged_header_names_are_the_rate_limit_family_sorted() {
        let h = headers(&[
            ("anthropic-ratelimit-unified-status", "allowed"),
            ("x-request-id", "req"),
            ("anthropic-ratelimit-unified-7d-utilization", "0.4"),
            ("anthropic-ratelimit-unified-5h-reset", "1"),
        ]);
        assert_eq!(
            rate_limit_header_names(&h),
            vec![
                "anthropic-ratelimit-unified-5h-reset".to_string(),
                "anthropic-ratelimit-unified-7d-utilization".to_string(),
                "anthropic-ratelimit-unified-status".to_string(),
            ]
        );
    }

    // ── D7: the probe's per-call max age, at production scale ─────────────

    #[test]
    fn the_production_ages_are_270_60_and_a_60_second_failure() {
        assert_eq!(USAGE_PROBE_POLL_MAX_AGE, Duration::from_secs(270));
        assert_eq!(USAGE_PROBE_CONFIRM_MAX_AGE, Duration::from_secs(60));
        assert_eq!(USAGE_PROBE_FAILURE_TTL, Duration::from_secs(60));
    }

    /// One key, one 100-second-old success: a poller (270 s) is served it, a
    /// confirming caller (60 s) is not and probes again. The dir has no
    /// credentials, so the re-probe answers from the filesystem and reaches
    /// no provider.
    #[tokio::test]
    async fn a_100_second_old_success_serves_a_poller_but_not_a_confirming_caller() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = dir.path().to_string_lossy().into_owned();
        let seeded = AccountUsageInfo {
            config_dir: key.clone(),
            label: "seeded".to_string(),
            utilization: 0.25,
            ..Default::default()
        };
        USAGE_PROBE_CACHE.insert_aged(&key, seeded, Duration::from_secs(100));
        let polled = probe_account_usage(key.clone(), USAGE_PROBE_POLL_MAX_AGE).await;
        assert_eq!(polled.label, "seeded");
        let confirmed = probe_account_usage(key.clone(), USAGE_PROBE_CONFIRM_MAX_AGE).await;
        assert_ne!(
            confirmed.label, "seeded",
            "a confirming caller must re-probe"
        );
        assert!(confirmed.error.is_some(), "no credentials: an error row");
    }

    /// A failure is reused for the failure TTL only, even for a poller.
    #[tokio::test]
    async fn a_61_second_old_failure_is_not_served_even_to_a_poller() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = dir.path().to_string_lossy().into_owned();
        let seeded = AccountUsageInfo {
            config_dir: key.clone(),
            label: "seeded".to_string(),
            error: Some("API error (429): seeded".to_string()),
            ..Default::default()
        };
        USAGE_PROBE_CACHE.insert_aged(&key, seeded, Duration::from_secs(61));
        let polled = probe_account_usage(key.clone(), USAGE_PROBE_POLL_MAX_AGE).await;
        assert_ne!(polled.label, "seeded");
    }
}
