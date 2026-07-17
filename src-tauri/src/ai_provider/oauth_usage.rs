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

use serde::Deserialize;
use tracing::debug;

const OAUTH_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

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
/// transport / auth / rate-limit / parse failure — the caller falls back to
/// the header probe. Only meaningful for subscription OAuth tokens
/// (`sk-ant-oat*`); API keys have no usage windows, so callers should skip
/// straight to the probe for those.
pub async fn fetch(token: &str) -> Result<OauthUsageSnapshot, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("client build failed: {e}"))?;
    let resp = client
        .get(OAUTH_USAGE_URL)
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "usage endpoint returned {status}: {}",
            body.chars().take(200).collect::<String>()
        ));
    }
    let body = resp
        .text()
        .await
        .map_err(|e| format!("body read failed: {e}"))?;
    let wire: WireUsage =
        serde_json::from_str(&body).map_err(|e| format!("unexpected response shape: {e}"))?;
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

    #[test]
    fn rfc3339_parses() {
        assert!(rfc3339_to_unix("2026-07-17T04:50:00.118700+00:00").unwrap() > 1_700_000_000);
        assert!(rfc3339_to_unix("garbage").is_none());
    }
}
