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

use std::time::Duration;

use once_cell::sync::Lazy;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
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

/// How long a probe result — success OR failure — is reused before the
/// account is asked again.
///
/// Sized to swallow the burst: the four independent callers
/// (`check_accounts_usage`, `/analytics/account-usage`, the 10-minute
/// snapshot timer, `account_migration`'s confirmation) no longer each cost a
/// request, and a 429'd account is not immediately re-probed into another
/// 429. A minute is well inside the resolution of the weekly/5-hour windows
/// these numbers describe, so nothing downstream reads a materially staler
/// figure than before.
const USAGE_PROBE_TTL: Duration = Duration::from_secs(60);

/// Shared single-flight + TTL cache in front of the live probe. Keyed by
/// `config_dir`, which is exactly the per-account identity the probe and the
/// selection snapshot already use.
pub(crate) static USAGE_PROBE_CACHE: Lazy<
    crate::commands::usage_probe_cache::CoalescingCache<AccountUsageInfo>,
> = Lazy::new(|| crate::commands::usage_probe_cache::CoalescingCache::new(USAGE_PROBE_TTL));

/// Probe a single account for its weekly rate limit utilization.
///
/// Reads through a shared single-flight + TTL cache
/// ([`USAGE_PROBE_CACHE`]): concurrent callers for the same account await ONE
/// upstream request, and a result is reused for [`USAGE_PROBE_TTL`].
///
/// This is not an optimization. The probe has four independent callers that
/// each fanned out one request per configured account with no coalescing and
/// no TTL — measured at 25 usage checks per 5-minute tick and **18,981 HTTP
/// 429s per day** in the dev logs. Because the probe consumes the same
/// per-account quota the CLI does, that stampede both competed with real work
/// and then reported the 429s it earned as account exhaustion.
///
/// See [`crate::commands::usage_probe_cache`] for the mechanism.
pub async fn probe_account_usage(config_dir: String) -> AccountUsageInfo {
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
        .get_or_fetch(&key, move || async move {
            // Identity is read here, after the probe, so every return path of
            // the probe (token error, probe error, success) carries it: an
            // errored row is exactly the one a consumer must collapse onto its
            // healthy twin on another device.
            let hash_dir = config_dir.clone();
            let mut info = probe_account_usage_uncached(config_dir).await;
            info.account_hash = account_hash_for(&hash_dir, dirs::home_dir().as_deref());
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
    // Also grab 5-hour utilization for context
    let utilization_5h =
        header("anthropic-ratelimit-unified-5h-utilization").and_then(|v| v.parse::<f64>().ok());

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
        source: Some("probe".to_string()),
        usage_endpoint: Some(endpoint),
        ..Default::default()
    }
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
}
