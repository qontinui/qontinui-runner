//! Account usage via Anthropic's OAuth usage endpoint.
//!
//! `GET https://api.anthropic.com/api/oauth/usage` (Bearer = the account's
//! `sk-ant-oat*` token from `.credentials.json`) returns the exact numbers the
//! Claude Code `/usage` screen shows: 5-hour session-window utilization +
//! reset time, 7-day utilization + reset time, and per-model scoped weekly
//! limits. Unlike the Haiku header probe (`commands::ai_settings::
//! probe_account_usage`'s fallback), this endpoint costs zero tokens and
//! cannot itself consume the quota it measures — so it is the preferred
//! stats source, with the header probe kept as fallback for when this
//! endpoint errors or rate-limits.
//!
//! Utilizations arrive as percentages (`8.0` = 8%) and are normalized to the
//! 0.0–1.0 fractions the rest of the account-selection code uses.
//! `resets_at` arrives as RFC3339 and is normalized to unix seconds.
//!
//! The endpoint reads only with a token granted the [`PROFILE_SCOPE`]. A
//! long-lived `claude setup-token` token is inference-only and is refused
//! with a 403 naming that scope, which [`UsageFetchError::scope_refused`]
//! reports. WHICH token asks, and how often a refused or rate-limited one is
//! asked again, is `super::oauth_usage_gate`'s decision, not this module's.

use serde::Deserialize;
use std::time::Duration;
use tracing::debug;

const OAUTH_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// The OAuth scope the usage endpoint requires of the token that asks it.
pub(crate) const PROFILE_SCOPE: &str = "user:profile";

/// Why one ask of the usage endpoint produced no snapshot. Carries no token,
/// so it is safe to log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageFetchError {
    /// The HTTP status, or `None` when no response arrived (a transport
    /// failure, or a request that was never sent).
    pub status: Option<u16>,
    /// The response's `Retry-After`, when it sent one.
    pub retry_after: Option<Duration>,
    /// A 403 whose `error.type` is `permission_error` and whose body names
    /// [`PROFILE_SCOPE`]: this token can never read the endpoint, however
    /// often it is asked.
    pub scope_refused: bool,
    /// What went wrong, for a log line.
    pub detail: String,
}

impl std::fmt::Display for UsageFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl UsageFetchError {
    /// A failure with no HTTP response behind it.
    fn transport(detail: String) -> Self {
        Self {
            status: None,
            retry_after: None,
            scope_refused: false,
            detail,
        }
    }
}

/// Whether a 403 body is the endpoint refusing this token's scope: its
/// `error.type` is `permission_error` and its text names [`PROFILE_SCOPE`]
/// (measured body: `OAuth token does not meet scope requirement
/// user:profile`). Any other 403 is not a scope verdict.
fn is_scope_refusal(status: u16, body: &str) -> bool {
    if status != 403 || !body.contains(PROFILE_SCOPE) {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")?
                .get("type")?
                .as_str()
                .map(|t| t == "permission_error")
        })
        .unwrap_or(false)
}

/// The typed error for a non-2xx answer. `retry_after_secs` is the
/// response's `Retry-After`, parsed as the Messages API path parses it
/// (`super::claude_api::retry_after_secs`).
fn error_from_response(status: u16, retry_after_secs: Option<u64>, body: &str) -> UsageFetchError {
    UsageFetchError {
        status: Some(status),
        retry_after: retry_after_secs.map(Duration::from_secs),
        scope_refused: is_scope_refusal(status, body),
        detail: format!(
            "usage endpoint returned {status}: {}",
            body.chars().take(200).collect::<String>()
        ),
    }
}

/// One usage window (the 5-hour session window or the 7-day week).
#[derive(Debug, Clone, PartialEq)]
pub struct UsageWindow {
    /// Fraction used, 0.0–1.0.
    pub utilization: f64,
    /// Unix seconds when this window resets.
    pub resets_at: Option<u64>,
}

/// A per-model scoped weekly limit (e.g. the Fable-only weekly cap).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelLimit {
    pub model: String,
    /// Fraction used, 0.0–1.0.
    pub utilization: f64,
    pub resets_at: Option<u64>,
}

/// Parsed snapshot of one account's usage windows.
#[derive(Debug, Clone, Default)]
pub struct OauthUsageSnapshot {
    pub five_hour: Option<UsageWindow>,
    pub seven_day: Option<UsageWindow>,
    pub model_limits: Vec<ModelLimit>,
    /// True when the server flags any limit with a blocking severity
    /// (reject/block/exceed/at_limit). Utilization thresholds remain the
    /// primary exhaustion signal; this catches server-side enforcement that
    /// fires below 100%.
    pub blocking_severity: bool,
}

// ── Wire shapes (subset — unknown fields ignored) ───────────────────────────

#[derive(Deserialize)]
struct WireWindow {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct WireScopeModel {
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct WireScope {
    model: Option<WireScopeModel>,
}

#[derive(Deserialize)]
struct WireLimit {
    kind: Option<String>,
    percent: Option<f64>,
    severity: Option<String>,
    resets_at: Option<String>,
    scope: Option<WireScope>,
}

#[derive(Deserialize)]
struct WireUsage {
    five_hour: Option<WireWindow>,
    seven_day: Option<WireWindow>,
    #[serde(default)]
    limits: Vec<WireLimit>,
}

fn rfc3339_to_unix(s: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp().max(0) as u64)
}

fn window_from_wire(w: &WireWindow) -> Option<UsageWindow> {
    let utilization = w.utilization? / 100.0;
    Some(UsageWindow {
        utilization,
        resets_at: w.resets_at.as_deref().and_then(rfc3339_to_unix),
    })
}

fn severity_is_blocking(severity: &str) -> bool {
    let s = severity.to_ascii_lowercase();
    s.contains("reject") || s.contains("block") || s.contains("exceed") || s.contains("at_limit")
}

fn snapshot_from_wire(wire: WireUsage) -> OauthUsageSnapshot {
    let model_limits = wire
        .limits
        .iter()
        .filter(|l| l.kind.as_deref() == Some("weekly_scoped"))
        .filter_map(|l| {
            let model = l
                .scope
                .as_ref()
                .and_then(|s| s.model.as_ref())
                .and_then(|m| m.display_name.clone())?;
            Some(ModelLimit {
                model,
                utilization: l.percent.unwrap_or(0.0) / 100.0,
                resets_at: l.resets_at.as_deref().and_then(rfc3339_to_unix),
            })
        })
        .collect();
    let blocking_severity = wire
        .limits
        .iter()
        .filter_map(|l| l.severity.as_deref())
        .any(severity_is_blocking);
    OauthUsageSnapshot {
        five_hour: wire.five_hour.as_ref().and_then(window_from_wire),
        seven_day: wire.seven_day.as_ref().and_then(window_from_wire),
        model_limits,
        blocking_severity,
    }
}

/// Fetch the usage snapshot for one account's OAuth token. `Err` on any
/// transport / auth / rate-limit / parse failure, typed so the caller can
/// tell a scope refusal from a rate limit from a dead network — the caller
/// then falls back to the header probe. Only meaningful for subscription
/// OAuth tokens (`sk-ant-oat*`); API keys have no usage windows, so callers
/// should skip straight to the probe for those.
pub async fn fetch(token: &str) -> Result<OauthUsageSnapshot, UsageFetchError> {
    // A subscription-only endpoint: never called while a model gateway is
    // declared (subscription accounts are off, `crate::model_gateway`), and
    // never routed to the gateway, which would hand it a subscription token.
    if crate::model_gateway::gateway_declared() {
        return Err(UsageFetchError::transport(
            crate::model_gateway::SUBSCRIPTION_OFF_REASON.to_string(),
        ));
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| UsageFetchError::transport(format!("client build failed: {e}")))?;
    let resp = client
        .get(OAUTH_USAGE_URL)
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await
        .map_err(|e| UsageFetchError::transport(format!("request failed: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let retry_after = super::claude_api::retry_after_secs(resp.headers());
        let body = resp.text().await.unwrap_or_default();
        return Err(error_from_response(status.as_u16(), retry_after, &body));
    }
    let answered = |detail: String| UsageFetchError {
        status: Some(status.as_u16()),
        retry_after: None,
        scope_refused: false,
        detail,
    };
    let body = resp
        .text()
        .await
        .map_err(|e| answered(format!("body read failed: {e}")))?;
    let wire: WireUsage = serde_json::from_str(&body)
        .map_err(|e| answered(format!("unexpected response shape: {e}")))?;
    let snap = snapshot_from_wire(wire);
    debug!(
        five_hour = ?snap.five_hour,
        seven_day = ?snap.seven_day,
        "oauth usage snapshot fetched"
    );
    Ok(snap)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed real response shape (verified live 2026-07-17).
    const SAMPLE: &str = r#"{
        "five_hour": {"utilization": 16.0, "resets_at": "2026-07-17T04:50:00.118700+00:00"},
        "seven_day": {"utilization": 17.0, "resets_at": "2026-07-18T03:00:00.118723+00:00"},
        "seven_day_opus": null,
        "extra_usage": {"is_enabled": false},
        "limits": [
            {"kind": "session", "group": "session", "percent": 16, "severity": "normal",
             "resets_at": "2026-07-17T04:50:00.118700+00:00", "scope": null, "is_active": false},
            {"kind": "weekly_all", "group": "weekly", "percent": 17, "severity": "normal",
             "resets_at": "2026-07-18T03:00:00.118723+00:00", "scope": null, "is_active": false},
            {"kind": "weekly_scoped", "group": "weekly", "percent": 34, "severity": "normal",
             "resets_at": "2026-07-18T03:00:00.118973+00:00",
             "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null},
             "is_active": true}
        ]
    }"#;

    #[test]
    fn parses_real_shape() {
        let wire: WireUsage = serde_json::from_str(SAMPLE).unwrap();
        let snap = snapshot_from_wire(wire);
        let fh = snap.five_hour.expect("five_hour");
        assert!((fh.utilization - 0.16).abs() < 1e-9);
        assert!(fh.resets_at.is_some());
        let sd = snap.seven_day.expect("seven_day");
        assert!((sd.utilization - 0.17).abs() < 1e-9);
        assert_eq!(snap.model_limits.len(), 1);
        assert_eq!(snap.model_limits[0].model, "Fable");
        assert!((snap.model_limits[0].utilization - 0.34).abs() < 1e-9);
        assert!(!snap.blocking_severity);
    }

    #[test]
    fn is_active_alone_is_not_blocking() {
        // `is_active` marks the currently-binding limit, NOT exhaustion — a
        // healthy account still has one active limit. Only severity blocks.
        let wire: WireUsage = serde_json::from_str(SAMPLE).unwrap();
        let snap = snapshot_from_wire(wire);
        assert!(!snap.blocking_severity);
    }

    #[test]
    fn blocking_severity_detected() {
        let s = SAMPLE.replace(
            "\"severity\": \"normal\",\n             \"resets_at\": \"2026-07-17",
            "\"severity\": \"rejected\",\n             \"resets_at\": \"2026-07-17",
        );
        let wire: WireUsage = serde_json::from_str(&s).unwrap();
        assert!(snapshot_from_wire(wire).blocking_severity);
    }

    #[test]
    fn missing_windows_tolerated() {
        let wire: WireUsage = serde_json::from_str(r#"{"limits": []}"#).unwrap();
        let snap = snapshot_from_wire(wire);
        assert!(snap.five_hour.is_none());
        assert!(snap.seven_day.is_none());
        assert!(snap.model_limits.is_empty());
    }

    /// The measured refusal of an inference-only token, verbatim in shape.
    const SCOPE_403: &str = r#"{"type":"error","error":{"type":"permission_error","message":"OAuth token does not meet scope requirement user:profile"},"request_id":"req_test"}"#;

    #[test]
    fn a_403_permission_error_naming_the_profile_scope_is_a_scope_refusal() {
        let e = error_from_response(403, None, SCOPE_403);
        assert_eq!(e.status, Some(403));
        assert!(e.scope_refused);
        assert_eq!(e.retry_after, None);
    }

    /// Only that exact shape latches a token as unable to read the endpoint:
    /// a 403 of another type, a permission error naming another scope, and a
    /// non-403 naming the scope all stay ordinary failures.
    #[test]
    fn any_other_refusal_is_not_a_scope_refusal() {
        let other_type = SCOPE_403.replace("permission_error", "forbidden");
        assert!(!error_from_response(403, None, &other_type).scope_refused);
        let other_scope = SCOPE_403.replace("user:profile", "user:inference");
        assert!(!error_from_response(403, None, &other_scope).scope_refused);
        assert!(!error_from_response(401, None, SCOPE_403).scope_refused);
        assert!(!error_from_response(403, None, "user:profile").scope_refused);
    }

    #[test]
    fn a_429_carries_its_retry_after_and_none_when_absent() {
        let body =
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"Rate limited"}}"#;
        let with = error_from_response(429, Some(1800), body);
        assert_eq!(with.status, Some(429));
        assert_eq!(with.retry_after, Some(Duration::from_secs(1800)));
        assert!(!with.scope_refused);
        let without = error_from_response(429, None, body);
        assert_eq!(without.retry_after, None);
    }

    #[test]
    fn rfc3339_parses() {
        assert!(rfc3339_to_unix("2026-07-17T04:50:00.118700+00:00").unwrap() > 1_700_000_000);
        assert!(rfc3339_to_unix("garbage").is_none());
    }
}
