//! Prepaid (pay-as-you-go credit) balance probing.
//!
//! Subscription accounts surfaced by [`super::account_usage`] report a rolling
//! utilization percentage. Prepaid providers like DeepSeek instead expose a
//! remaining money balance that shrinks as you spend — a fundamentally
//! different shape. This module probes those balances so the mobile dashboard's
//! "Credit Balance" card can show remaining funds instead of a usage bar.
//!
//! Two consumers: the local `/analytics/prepaid-balance` route (on demand), and
//! the 10-minute fleet report (`commands::ai_settings::refresh_fleet_usage_report`),
//! which mirrors the balances to coord's `coord.prepaid_balances` so the card
//! still has a reading when this machine is down.
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
/// On a successful probe the money fields carry the reported balance and
/// `error` is `None`. On failure (auth / network / parse) `error` is `Some` and
/// every measured field is `None` — UNKNOWN, never a best-effort zero. A real
/// `Some(0.0)` means "out of credit", and a `0.0` standing in for "we could not
/// read it" is a false alarm about money (plan
/// `2026-09-12-prepaid-balance-is-a-fleet-fact-with-no-ingest`, Phase 3).
///
/// The `f64` fields are for DISPLAY on the local `/analytics/prepaid-balance`
/// route. The `*_micros` twins are what leaves the machine: they are parsed
/// exactly from the provider's decimal string, never derived from the `f64`,
/// and are not serialized on the local route.
#[derive(Debug, Clone, Serialize)]
pub struct PrepaidBalanceInfo {
    /// Stable provider id, e.g. `"deepseek"`.
    pub provider: String,
    /// Human-facing label, e.g. `"DeepSeek"`.
    pub label: String,
    /// ISO currency code reported by the provider, e.g. `"USD"`.
    pub currency: Option<String>,
    /// Total remaining balance (granted + topped-up), in `currency` units.
    pub balance: Option<f64>,
    /// Portion of the balance that is granted (free) credit.
    pub granted_balance: Option<f64>,
    /// Portion of the balance that was topped up (paid) credit.
    pub topped_up_balance: Option<f64>,
    /// Whether the provider considers the account usable (enough balance).
    pub is_available: Option<bool>,
    /// Populated when the probe failed; the measured fields are `None` then.
    pub error: Option<String>,
    /// `balance` in 1e-6 `currency` units, exact from the provider's string.
    #[serde(skip)]
    pub balance_micros: Option<i64>,
    #[serde(skip)]
    pub granted_micros: Option<i64>,
    #[serde(skip)]
    pub topped_up_micros: Option<i64>,
}

impl PrepaidBalanceInfo {
    pub(crate) fn error(provider: &str, label: &str, error: String) -> Self {
        Self {
            provider: provider.to_string(),
            label: label.to_string(),
            currency: None,
            balance: None,
            granted_balance: None,
            topped_up_balance: None,
            is_available: None,
            error: Some(error),
            balance_micros: None,
            granted_micros: None,
            topped_up_micros: None,
        }
    }
}

/// Parse a plain decimal amount (`"19.28"`, `"-0.5"`, `"7"`) into integer
/// micros (1e-6 units) EXACTLY, by string arithmetic — never through `f64`,
/// where a truncating `(x * 1e6) as i64` is how a cent gets lost.
///
/// Returns `None` for anything that is not a plain decimal (exponents, blanks,
/// garbage), for precision finer than a micro that is not all zeros (rounding
/// money silently is the defect this exists to avoid), and on overflow.
pub(crate) fn decimal_str_to_micros(s: &str) -> Option<i64> {
    let s = s.trim();
    let (negative, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (int_part, frac_part) = match digits.split_once('.') {
        Some((i, f)) => (i, f),
        None => (digits, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    // Precision beyond 1e-6 is accepted only when it carries no value.
    let (kept, dropped) = frac_part.split_at(frac_part.len().min(6));
    if dropped.bytes().any(|b| b != b'0') {
        return None;
    }
    let int_val: i64 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().ok()?
    };
    let mut frac_val: i64 = if kept.is_empty() {
        0
    } else {
        kept.parse().ok()?
    };
    for _ in kept.len()..6 {
        frac_val = frac_val.checked_mul(10)?;
    }
    let micros = int_val.checked_mul(1_000_000)?.checked_add(frac_val)?;
    Some(if negative { -micros } else { micros })
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
    // The keychain read is SYNCHRONOUS and can block indefinitely (a locked
    // Linux secret-service waiting on an unlock prompt nobody answers). This
    // runs on the periodic fleet-usage loop, where one hung call would stop
    // every later tick — so it goes to the blocking pool, bounded. A timeout
    // means "we could not tell whether a key exists", which is not a provider
    // error to report, so it reports nothing this cycle.
    //
    // A timeout stops the WAIT, not the thread: the blocked read keeps its
    // blocking-pool thread. So reads are single-flight — while one is still
    // outstanding, later callers skip instead of stranding another thread, and
    // a keyring that stays locked costs one thread total, not one per tick
    // (the pool is shared with `pick_best_account` and the OAuth refresh).
    use std::sync::atomic::{AtomicBool, Ordering};
    static KEY_READ_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
    const KEY_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
    if KEY_READ_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        warn!("Prepaid balance: a previous API key read is still blocked — skipping this cycle");
        return Vec::new();
    }
    // Cleared when the read actually RETURNS (not when the caller stops
    // waiting), on a panic, and — because the guard is CAPTURED by the closure
    // rather than created inside it — even if the task is dropped unrun.
    struct ClearInFlight;
    impl Drop for ClearInFlight {
        fn drop(&mut self) {
            KEY_READ_IN_FLIGHT.store(false, Ordering::Release);
        }
    }
    let clear = ClearInFlight;
    let api_key = match tokio::time::timeout(
        KEY_READ_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            let _clear = clear;
            resolve_api_key()
        }),
    )
    .await
    {
        Ok(Ok(key)) => key,
        Ok(Err(e)) => {
            warn!("Prepaid balance: API key resolution task failed: {}", e);
            None
        }
        Err(_) => {
            warn!(
                "Prepaid balance: API key read exceeded {:?} (keychain locked?) — skipping this cycle",
                KEY_READ_TIMEOUT
            );
            None
        }
    };
    let Some(api_key) = api_key else {
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
/// Balance values are JSON *strings*; each is parsed twice from the same text —
/// to `f64` for display and to exact integer micros for the coord wire (see
/// [`parse_deepseek_balance`]). When
/// multiple currencies are present the first `balance_infos` entry is used
/// (DeepSeek reports a single currency per account in practice).
async fn probe_deepseek_balance(base_url: &str, api_key: &str) -> PrepaidBalanceInfo {
    const PROVIDER: &str = "deepseek";
    const LABEL: &str = "DeepSeek";

    let url = format!("{}/user/balance", base_url.trim_end_matches('/'));
    // Bounded: this probe now also runs on the periodic fleet-usage report, and
    // an unbounded request to a hung provider would pin that loop forever.
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return PrepaidBalanceInfo::error(PROVIDER, LABEL, format!("HTTP client error: {}", e));
        }
    };
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
    let is_available = json["is_available"].as_bool();

    let Some(info) = json["balance_infos"].as_array().and_then(|a| a.first()) else {
        return PrepaidBalanceInfo::error(
            provider,
            label,
            "Balance response had no balance_infos entries".to_string(),
        );
    };

    // DeepSeek reports the amounts as JSON strings (e.g. "19.28"); a number is
    // tolerated defensively. Each amount yields (display f64, exact micros),
    // both from the SAME text, and `None` when absent or unparseable — never
    // a default of 0.
    let parse_amount = |key: &str| -> Option<(f64, i64)> {
        let text = match &info[key] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => return None,
        };
        let micros = decimal_str_to_micros(&text)?;
        let display = text.trim().parse::<f64>().ok()?;
        Some((display, micros))
    };

    let currency = info["currency"]
        .as_str()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    let total = parse_amount("total_balance");
    let granted = parse_amount("granted_balance");
    let topped_up = parse_amount("topped_up_balance");

    // Without a total there is no reading at all. Say so as an ERROR rather
    // than returning an error-free row whose balance someone would read as 0.
    let Some((balance, balance_micros)) = total else {
        return PrepaidBalanceInfo::error(
            provider,
            label,
            "Balance response had no parseable total_balance".to_string(),
        );
    };

    info!(
        "Prepaid balance '{}': {} {:.2} (available={:?})",
        label,
        currency.as_deref().unwrap_or("?"),
        balance,
        is_available
    );

    PrepaidBalanceInfo {
        provider: provider.to_string(),
        label: label.to_string(),
        currency,
        balance: Some(balance),
        granted_balance: granted.map(|(f, _)| f),
        topped_up_balance: topped_up.map(|(f, _)| f),
        is_available,
        error: None,
        balance_micros: Some(balance_micros),
        granted_micros: granted.map(|(_, m)| m),
        topped_up_micros: topped_up.map(|(_, m)| m),
    }
}

#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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
        assert_eq!(info.currency.as_deref(), Some("USD"));
        assert_eq!(info.balance, Some(19.28));
        assert_eq!(info.granted_balance, Some(0.0));
        assert_eq!(info.topped_up_balance, Some(19.28));
        assert_eq!(info.is_available, Some(true));
        // Exact micros from the decimal STRING.
        assert_eq!(info.balance_micros, Some(19_280_000));
        assert_eq!(info.granted_micros, Some(0));
        assert_eq!(info.topped_up_micros, Some(19_280_000));
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
        assert_eq!(info.balance, Some(5.5));
        assert_eq!(info.granted_balance, Some(1.0));
        assert_eq!(info.balance_micros, Some(5_500_000));
        assert_eq!(info.granted_micros, Some(1_000_000));
    }

    #[test]
    fn empty_balance_infos_is_an_error() {
        let body = json!({ "is_available": false, "balance_infos": [] });
        let info = parse_deepseek_balance(&body, "deepseek", "DeepSeek");
        assert!(info.error.is_some());
        // An errored probe knows nothing — every measured field is None.
        assert_eq!(info.is_available, None);
        assert_eq!(info.balance, None);
        assert_eq!(info.balance_micros, None);
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
        // A real zero is a READING and must stay Some(0), distinct from None.
        assert_eq!(info.balance, Some(0.0));
        assert_eq!(info.balance_micros, Some(0));
        assert_eq!(info.is_available, Some(false));
    }

    /// The false alarm Phase 3 removes: a missing / unparseable total used to
    /// land as `balance: 0.0, error: None` — "out of credit". It must now be an
    /// error row with no numbers.
    #[test]
    fn missing_total_is_an_error_not_a_zero() {
        for total in [json!(null), json!("n/a"), json!(""), json!("1e3")] {
            let body = json!({
                "is_available": true,
                "balance_infos": [{"currency": "USD", "total_balance": total,
                                   "granted_balance": "0.00", "topped_up_balance": "0.00"}]
            });
            let info = parse_deepseek_balance(&body, "deepseek", "DeepSeek");
            assert!(info.error.is_some(), "total {total:?} must be an error row");
            assert_eq!(info.balance, None);
            assert_eq!(info.balance_micros, None);
        }
    }

    /// A missing SUB-amount is unknown for that field only; the total is still
    /// a reading.
    #[test]
    fn missing_sub_amount_is_unknown_not_zero() {
        let body = json!({
            "is_available": true,
            "balance_infos": [{"currency": "USD", "total_balance": "3.10"}]
        });
        let info = parse_deepseek_balance(&body, "deepseek", "DeepSeek");
        assert!(info.error.is_none());
        assert_eq!(info.balance_micros, Some(3_100_000));
        assert_eq!(info.granted_balance, None);
        assert_eq!(info.granted_micros, None);
        assert_eq!(info.topped_up_micros, None);
    }

    #[test]
    fn decimal_to_micros_is_exact() {
        assert_eq!(decimal_str_to_micros("19.28"), Some(19_280_000));
        // 8.2 * 1e6 in f64 is 8199999.999999999, so the truncating cast
        // `(x * 1e6) as i64` yields 8_199_999 — a lost micro. The string path
        // must not.
        assert_eq!(decimal_str_to_micros("8.2"), Some(8_200_000));
        assert_ne!(
            (8.2_f64 * 1e6) as i64,
            8_200_000,
            "premise: the f64 path is lossy"
        );
        assert_eq!(decimal_str_to_micros("4.35"), Some(4_350_000));
        assert_eq!(decimal_str_to_micros("7"), Some(7_000_000));
        assert_eq!(decimal_str_to_micros(".5"), Some(500_000));
        assert_eq!(decimal_str_to_micros(" 1.000001 "), Some(1_000_001));
        assert_eq!(decimal_str_to_micros("-2.5"), Some(-2_500_000));
        assert_eq!(decimal_str_to_micros("1.2300000"), Some(1_230_000));
        // Sub-micro value, garbage, exponents, blanks, overflow: refused.
        assert_eq!(decimal_str_to_micros("1.0000001"), None);
        assert_eq!(decimal_str_to_micros("1e3"), None);
        assert_eq!(decimal_str_to_micros("abc"), None);
        assert_eq!(decimal_str_to_micros(""), None);
        assert_eq!(decimal_str_to_micros("."), None);
        assert_eq!(decimal_str_to_micros("1.2.3"), None);
        assert_eq!(decimal_str_to_micros("99999999999999999"), None);
    }

    /// The local route's JSON: unknowns serialize as `null`, and the micros
    /// twins stay off it (they are the coord wire's, not this route's).
    #[test]
    fn error_row_serializes_nulls_and_hides_micros() {
        let v = serde_json::to_value(PrepaidBalanceInfo::error(
            "deepseek",
            "DeepSeek",
            "x".into(),
        ))
        .expect("serializes");
        assert!(v["balance"].is_null());
        assert!(v["currency"].is_null());
        assert!(v["is_available"].is_null());
        assert!(v.get("balance_micros").is_none());
    }
}
