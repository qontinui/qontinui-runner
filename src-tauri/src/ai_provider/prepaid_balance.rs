//! Prepaid (pay-as-you-go credit) balance probing.
//!
//! Subscription accounts surfaced by [`super::account_usage`] report a rolling
//! utilization percentage. Prepaid providers like DeepSeek instead expose a
//! remaining money balance that shrinks as you spend — a fundamentally
//! different shape. This module probes those balances so the mobile dashboard's
//! "Credit Balance" card can show remaining funds instead of a usage bar.
//!
//! Only DeepSeek is wired today (the default OpenAI-compatible endpoint), but
//! the public surface returns a `Vec<PrepaidBalanceInfo>` so additional prepaid
//! providers plug in without a schema change.
//!
//! API key resolution mirrors the OpenAI-compatible chat path
//! (`ai_provider::openai_compat`) so the balance probe and the subagent client
//! read the same credential:
//! 1. OS keychain slot `openai_compatible`
//! 2. `DEEPSEEK_API_KEY` env var
//! 3. `OPENAI_COMPATIBLE_API_KEY` env var

use crate::config_facade::{ai_keychain, keychain_keys};
use crate::settings;
use serde::Serialize;
use tracing::{info, warn};

/// DeepSeek's default API host, used when no `openai_compatible.base_url` is
/// configured. The balance endpoint lives at `{base}/user/balance` and DeepSeek
/// accepts it both at the root and under `/v1`, so appending the suffix to the
/// configured base URL works regardless of whether it carries a `/v1` segment.
const DEEPSEEK_DEFAULT_BASE_URL: &str = "https://api.deepseek.com";

/// A single prepaid provider's remaining credit.
///
/// On a successful probe the numeric fields carry the reported balance and
/// `error` is `None`. On failure (auth / network / parse) `error` is `Some` and
/// the numeric fields are best-effort zeros — the mobile card renders the error
/// string rather than the numbers in that case.
#[derive(Debug, Clone, Serialize)]
pub struct PrepaidBalanceInfo {
    /// Stable provider id, e.g. `"deepseek"`.
    pub provider: String,
    /// Human-facing label, e.g. `"DeepSeek"`.
    pub label: String,
    /// ISO currency code reported by the provider, e.g. `"USD"`.
    pub currency: String,
    /// Total remaining balance (granted + topped-up), in `currency` units.
    pub balance: f64,
    /// Portion of the balance that is granted (free) credit.
    pub granted_balance: f64,
    /// Portion of the balance that was topped up (paid) credit.
    pub topped_up_balance: f64,
    /// Whether the provider considers the account usable (enough balance).
    pub is_available: bool,
    /// Populated when the probe failed; the numeric fields are unknown then.
    pub error: Option<String>,
}

impl PrepaidBalanceInfo {
    fn error(provider: &str, label: &str, error: String) -> Self {
        Self {
            provider: provider.to_string(),
            label: label.to_string(),
            currency: String::new(),
            balance: 0.0,
            granted_balance: 0.0,
            topped_up_balance: 0.0,
            is_available: false,
            error: Some(error),
        }
    }
}

/// Resolve the OpenAI-compatible API key: keychain slot first, then env
/// fallbacks. Returns `None` when no key is configured anywhere — the caller
/// then skips the prepaid probe entirely (no key = nothing to report).
fn resolve_api_key() -> Option<String> {
    match ai_keychain().get(keychain_keys::OPENAI_COMPATIBLE_API_KEY) {
        Ok(Some(key)) if !key.trim().is_empty() => return Some(key),
        Ok(_) => {}
        Err(e) => {
            warn!(
                "Failed to read openai_compatible key from keychain (falling back to env): {}",
                e
            );
        }
    }

    for var in ["DEEPSEEK_API_KEY", "OPENAI_COMPATIBLE_API_KEY"] {
        if let Ok(val) = std::env::var(var) {
            if !val.trim().is_empty() {
                return Some(val);
            }
        }
    }

    None
}

/// Probe every configured prepaid provider for its remaining balance.
///
/// Currently probes DeepSeek only, and only when:
/// - an OpenAI-compatible API key resolves (keychain or env), AND
/// - the configured `openai_compatible.base_url` is empty (defaults to
///   DeepSeek) or points at a DeepSeek host — a non-DeepSeek OpenAI-compatible
///   endpoint (vLLM, LM Studio, …) has no `/user/balance` concept, so it is
///   skipped rather than probed and errored.
///
/// Returns an empty vec when no prepaid provider is configured — the mobile
/// card then renders nothing, exactly like the account-usage card with no
/// Claude accounts.
pub async fn get_prepaid_balances() -> Vec<PrepaidBalanceInfo> {
    let Some(api_key) = resolve_api_key() else {
        return Vec::new();
    };

    let configured = settings::get_ai_settings().openai_compatible.base_url;
    let configured = configured.trim();
    let base_url = if configured.is_empty() {
        DEEPSEEK_DEFAULT_BASE_URL.to_string()
    } else {
        configured.to_string()
    };

    // Prepaid balance is a DeepSeek-only concept for now. A base URL that was
    // repointed at a non-DeepSeek OpenAI-compatible server has no balance
    // endpoint, so don't probe it.
    if !base_url.contains("deepseek") {
        return Vec::new();
    }

    vec![probe_deepseek_balance(&base_url, &api_key).await]
}

/// Probe DeepSeek's `GET {base}/user/balance` endpoint.
///
/// The response shape is:
/// ```json
/// {"is_available": true, "balance_infos": [
///    {"currency": "USD", "total_balance": "19.28",
///     "granted_balance": "0.00", "topped_up_balance": "19.28"}]}
/// ```
/// Balance values are JSON *strings*, so they are parsed to `f64`. When
/// multiple currencies are present the first `balance_infos` entry is used
/// (DeepSeek reports a single currency per account in practice).
async fn probe_deepseek_balance(base_url: &str, api_key: &str) -> PrepaidBalanceInfo {
    const PROVIDER: &str = "deepseek";
    const LABEL: &str = "DeepSeek";

    let url = format!("{}/user/balance", base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .bearer_auth(api_key)
        .header("accept", "application/json")
        .send()
        .await;

    let resp = match response {
        Ok(r) => r,
        Err(e) => {
            return PrepaidBalanceInfo::error(PROVIDER, LABEL, format!("Network error: {}", e));
        }
    };

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return PrepaidBalanceInfo::error(
            PROVIDER,
            LABEL,
            format!("API error ({}): {}", status, truncate(&body, 200)),
        );
    }

    let json = match resp.json::<serde_json::Value>().await {
        Ok(j) => j,
        Err(e) => {
            return PrepaidBalanceInfo::error(
                PROVIDER,
                LABEL,
                format!("Failed to parse balance response: {}", e),
            );
        }
    };

    parse_deepseek_balance(&json, PROVIDER, LABEL)
}

/// Pure parse of DeepSeek's balance JSON into a [`PrepaidBalanceInfo`]. Split
/// out so it can be unit-tested without a live endpoint.
fn parse_deepseek_balance(
    json: &serde_json::Value,
    provider: &str,
    label: &str,
) -> PrepaidBalanceInfo {
    let is_available = json["is_available"].as_bool().unwrap_or(false);

    let Some(info) = json["balance_infos"].as_array().and_then(|a| a.first()) else {
        return PrepaidBalanceInfo::error(
            provider,
            label,
            "Balance response had no balance_infos entries".to_string(),
        );
    };

    // DeepSeek reports the amounts as JSON strings (e.g. "19.28").
    let parse_amount = |key: &str| -> f64 {
        info[key]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .or_else(|| info[key].as_f64())
            .unwrap_or(0.0)
    };

    let currency = info["currency"].as_str().unwrap_or("").to_string();
    let balance = parse_amount("total_balance");
    let granted_balance = parse_amount("granted_balance");
    let topped_up_balance = parse_amount("topped_up_balance");

    info!(
        "Prepaid balance '{}': {} {:.2} (available={})",
        label, currency, balance, is_available
    );

    PrepaidBalanceInfo {
        provider: provider.to_string(),
        label: label.to_string(),
        currency,
        balance,
        granted_balance,
        topped_up_balance,
        is_available,
        error: None,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_deepseek_string_amounts() {
        let body = json!({
            "is_available": true,
            "balance_infos": [{
                "currency": "USD",
                "total_balance": "19.28",
                "granted_balance": "0.00",
                "topped_up_balance": "19.28"
            }]
        });

        let info = parse_deepseek_balance(&body, "deepseek", "DeepSeek");
        assert!(info.error.is_none(), "expected success: {:?}", info.error);
        assert_eq!(info.currency, "USD");
        assert_eq!(info.balance, 19.28);
        assert_eq!(info.granted_balance, 0.0);
        assert_eq!(info.topped_up_balance, 19.28);
        assert!(info.is_available);
    }

    #[test]
    fn tolerates_numeric_amounts() {
        // Defensive: if a provider ever reports numbers instead of strings.
        let body = json!({
            "is_available": true,
            "balance_infos": [{
                "currency": "USD",
                "total_balance": 5.5,
                "granted_balance": 1.0,
                "topped_up_balance": 4.5
            }]
        });

        let info = parse_deepseek_balance(&body, "deepseek", "DeepSeek");
        assert_eq!(info.balance, 5.5);
        assert_eq!(info.granted_balance, 1.0);
    }

    #[test]
    fn empty_balance_infos_is_an_error() {
        let body = json!({ "is_available": false, "balance_infos": [] });
        let info = parse_deepseek_balance(&body, "deepseek", "DeepSeek");
        assert!(info.error.is_some());
        assert!(!info.is_available);
    }

    #[test]
    fn unavailable_account_still_parses_balance() {
        // A depleted-but-nonzero account: is_available=false but numbers present.
        let body = json!({
            "is_available": false,
            "balance_infos": [{
                "currency": "USD",
                "total_balance": "0.00",
                "granted_balance": "0.00",
                "topped_up_balance": "0.00"
            }]
        });
        let info = parse_deepseek_balance(&body, "deepseek", "DeepSeek");
        assert!(info.error.is_none());
        assert_eq!(info.balance, 0.0);
        assert!(!info.is_available);
    }
}
