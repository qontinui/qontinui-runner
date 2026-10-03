use super::circuit_breaker;
use super::types::AiResponse;
use crate::session::failure::{category_for, kind_of_error_text, policy_for};
use qontinui_types::cli_session::{FailureCategory, FailureKind, RecoveryPolicy};
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Maximum number of retry attempts for AI API calls (non-rate-limit errors).
pub(super) const MAX_AI_RETRIES: u32 = 3;

/// Base backoff delay in milliseconds (doubles each retry: 2s, 4s, 8s).
pub(super) const BASE_BACKOFF_MS: u64 = 2000;

/// Maximum number of account-capacity wait cycles before giving up entirely.
/// Each cycle waits for the earliest account cooldown to expire, so this
/// caps total wait time at roughly MAX_ACCOUNT_WAITS * cooldown_duration.
const MAX_ACCOUNT_WAITS: u32 = 6;

/// The session-failure kind an error text names — a thin call into the one
/// classifier (`session::failure`, plan
/// `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`
/// Phase 7). This module no longer keeps a failure vocabulary of its own.
fn failure_kind(error_msg: &str) -> Option<FailureKind> {
    kind_of_error_text(error_msg)
}

/// Whether the recovery table answers `migrate_account` for this error — an
/// exhausted quota or spend budget, which another account's capacity fixes
/// (the Claude CLI's "You've hit your monthly spend limit" included, so a
/// mid-request hit rotates instead of failing the call).
///
/// A rate limit or an overload is NOT in this set: its policy is a backoff on
/// the same account. Until Phase 7 `"overloaded"` sat in the old
/// `is_rate_limit_error` set and rotated accounts on a provider overload — a
/// fault no account switch fixes.
fn migrates_account(kind: Option<FailureKind>) -> bool {
    kind.is_some_and(|k| policy_for(k, false) == RecoveryPolicy::MigrateAccount)
}

/// Whether the error is an account-capacity signal (a rate, quota or budget
/// limit) rather than a provider fault. Capacity signals are kept out of the
/// circuit breaker, which measures provider health.
fn is_capacity_signal(kind: Option<FailureKind>) -> bool {
    kind.is_some_and(|k| category_for(k) == FailureCategory::Limit)
}

/// Determine whether an AI error response represents a transient/retryable failure.
///
/// Retryable errors include:
/// - Network timeouts and connection errors
/// - HTTP 429 (rate limit)
/// - HTTP 500, 502, 503, 504 (server errors)
/// - CLI process failures that look transient (e.g., overloaded)
///
/// Permanent (non-retryable) errors include:
/// - HTTP 400 (bad request)
/// - HTTP 401 (authentication — note: 403 IS retried as it can be transient token refresh)
/// - Deserialization / JSON parse errors
/// - Missing API key configuration
/// - Client construction failures
pub(super) fn is_retryable_error(error_msg: &str) -> bool {
    // Every capacity limit and every overload is retryable. The account-move
    // kinds must be, because the rotation branch is gated behind this function
    // returning true; keeping this first makes that set a true subset, so
    // phrasings the explicit list below omits ("token limit", "usage limit",
    // "spend limit") still reach it. An overload is retried too — on the same
    // account, by the plain backoff.
    let kind = failure_kind(error_msg);
    if is_capacity_signal(kind) || kind == Some(FailureKind::Overloaded) {
        return true;
    }

    let lower = error_msg.to_lowercase();

    // HTTP status code checks (from API error messages like "API error (429): ...")
    // Retryable status codes
    if lower.contains("(429)")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
    {
        return true;
    }
    if lower.contains("(500)")
        || lower.contains("(502)")
        || lower.contains("(503)")
        || lower.contains("(504)")
    {
        return true;
    }
    // "overloaded" is a common API error message for 529/overloaded status
    if lower.contains("overloaded") {
        return true;
    }

    // Network-level errors (reqwest error messages)
    if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("connection closed")
        || lower.contains("broken pipe")
        || lower.contains("dns error")
        || lower.contains("name resolution")
    {
        return true;
    }

    // reqwest "request failed" without a clear permanent cause is usually transient
    // but we need to be careful not to catch permanent errors here
    if lower.contains("request failed")
        && !lower.contains("(400)")
        && !lower.contains("(401)")
        && !lower.contains("(403)")
        && !lower.contains("(404)")
    {
        return true;
    }

    // HTTP 403 from Claude API can be a transient auth token refresh issue
    // (e.g., subscription token expired mid-workflow). Retry with backoff
    // so we don't waste an entire workflow iteration on a temporary auth failure.
    if lower.contains("(403)") || lower.contains("forbidden") {
        return true;
    }

    // Permanent errors — return false explicitly for clarity
    // HTTP 400, 401 are validation/credential errors (not transient)
    if lower.contains("(400)") || lower.contains("(401)") {
        return false;
    }
    // Missing configuration or client errors
    if lower.contains("no claude api key")
        || lower.contains("no gemini api key")
        || lower.contains("failed to retrieve api key")
        || lower.contains("failed to create http client")
        || lower.contains("failed to parse")
    {
        return false;
    }

    // Default: not retryable (conservative — only retry what we know is transient)
    false
}

/// Execute an AI operation with exponential backoff retry.
///
/// Calls `operation` up to `MAX_AI_RETRIES + 1` times (1 initial + retries).
/// On each failed attempt, if the error is retryable, waits with exponential
/// backoff before the next attempt. Permanent errors return immediately.
///
/// Records success/failure to the circuit breaker for the given provider.
///
/// # Arguments
/// * `operation_name` - Human-readable label for log messages (e.g., "Claude API")
/// * `operation` - Closure that performs the AI call and returns an `AiResponse`
pub(super) fn retry_with_backoff<F>(operation_name: &str, operation: F) -> AiResponse
where
    F: Fn() -> AiResponse,
{
    retry_with_backoff_tracked(operation_name, None, operation)
}

/// Like `retry_with_backoff`, but also records results to the circuit breaker
/// for the specified provider key.
pub(super) fn retry_with_backoff_tracked<F>(
    operation_name: &str,
    provider_key: Option<&str>,
    operation: F,
) -> AiResponse
where
    F: Fn() -> AiResponse,
{
    // Check circuit breaker before first attempt
    if let Some(key) = provider_key {
        if !circuit_breaker::is_provider_available(key) {
            let state = circuit_breaker::provider_state(key);
            warn!(
                "{}: circuit breaker is {} for '{}', failing fast",
                operation_name, state, key
            );
            return AiResponse::error(format!(
                "Provider '{}' circuit breaker is {} — too many recent failures",
                key, state
            ));
        }
    }

    let mut attempt: u32 = 0;
    let mut account_waits: u32 = 0;

    loop {
        let response = operation();

        if response.success {
            if let Some(key) = provider_key {
                circuit_breaker::record_provider_success(key);
            }
            return response;
        }

        // Extract error message for retryability check
        let error_msg = response.error.as_deref().unwrap_or("");

        if !is_retryable_error(error_msg) {
            if attempt > 0 {
                debug!(
                    "{} permanent error after {} retries, not retrying: {}",
                    operation_name, attempt, error_msg
                );
            }
            return response;
        }

        let kind = failure_kind(error_msg);

        // Record retryable failure to circuit breaker — but NOT capacity
        // signals (rate, quota, budget limits), which say nothing about the
        // provider's health. An overload does.
        if !is_capacity_signal(kind) {
            if let Some(key) = provider_key {
                circuit_breaker::record_provider_failure(key, error_msg);
            }
        }

        // The `migrate_account` kinds: try rotating to another account. A rate
        // limit or an overload falls through to the plain backoff below, on
        // the same account.
        if migrates_account(kind) {
            account_waits += 1;

            // Safety cap: don't spin forever if every account is persistently exhausted
            if account_waits > MAX_ACCOUNT_WAITS {
                error!(
                    "{}: exceeded max account waits ({}) across all accounts, giving up: {}",
                    operation_name, MAX_ACCOUNT_WAITS, error_msg
                );
                return response;
            }

            if super::config::rotate_account_on_rate_limit() {
                let new_account = super::config::get_resolved_config_dir()
                    .unwrap_or_else(|| "unknown".to_string());
                let label = std::path::Path::new(&new_account)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&new_account);
                info!(
                    "{}: account capacity exhausted ({:?}, cycle {}/{}), switched to account '{}' for retry",
                    operation_name, kind, account_waits, MAX_ACCOUNT_WAITS, label
                );
                // Reset attempt counter — new account gets fresh retries
                attempt = 0;
                // Brief pause before hitting the new account
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }

            // Rotation failed — all accounts are rate-limited.
            // Wait for the earliest cooldown to expire and try again.
            if let Some(wait_duration) = super::config::time_until_next_account_available() {
                let wait_secs = wait_duration.as_secs();

                // Calculate which account will be used and when
                let resume_time =
                    chrono::Local::now() + chrono::Duration::seconds(wait_secs as i64);
                let resume_str = resume_time.format("%H:%M:%S").to_string();

                warn!(
                    "{}: all accounts exhausted (cycle {}/{}). \
                     Waiting {}s — will retry at {} with next available account.",
                    operation_name, account_waits, MAX_ACCOUNT_WAITS, wait_secs, resume_str,
                );

                std::thread::sleep(wait_duration);

                // Unlock the account whose cooldown just expired
                super::config::force_unlock_earliest_account();

                let new_account = super::config::get_resolved_config_dir()
                    .unwrap_or_else(|| "unknown".to_string());
                let label = std::path::Path::new(&new_account)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&new_account);
                info!(
                    "{}: cooldown expired, resuming with account '{}'",
                    operation_name, label
                );

                attempt = 0;
                continue;
            }
        }

        // Every other retryable error — a rate limit and an overload included —
        // gets the standard exponential backoff on the same account.
        if attempt >= MAX_AI_RETRIES {
            error!(
                "{} failed after {} retries and {} account waits: {}",
                operation_name, attempt, account_waits, error_msg
            );
            return response;
        }

        let backoff_ms = BASE_BACKOFF_MS * 2u64.pow(attempt);
        let backoff_secs = backoff_ms as f64 / 1000.0;

        warn!(
            "AI API retry {}/{}: {}, backing off {}s",
            attempt + 1,
            MAX_AI_RETRIES,
            error_msg,
            backoff_secs
        );

        std::thread::sleep(Duration::from_millis(backoff_ms));
        attempt += 1;
    }
}

/// Try the primary operation first; if it fails with a retryable error and a fallback
/// is provided, try the fallback instead.
pub(crate) fn retry_with_fallback<F, G>(
    operation_name: &str,
    primary: F,
    fallback: Option<G>,
) -> AiResponse
where
    F: Fn() -> AiResponse,
    G: Fn() -> AiResponse,
{
    retry_with_fallback_tracked(operation_name, None, None, primary, fallback)
}

/// Like `retry_with_fallback`, but also records results to circuit breakers.
pub(crate) fn retry_with_fallback_tracked<F, G>(
    operation_name: &str,
    primary_provider_key: Option<&str>,
    fallback_provider_key: Option<&str>,
    primary: F,
    fallback: Option<G>,
) -> AiResponse
where
    F: Fn() -> AiResponse,
    G: Fn() -> AiResponse,
{
    // Check if primary provider circuit is open — skip directly to fallback
    if let Some(key) = primary_provider_key {
        if !circuit_breaker::is_provider_available(key) {
            let state = circuit_breaker::provider_state(key);
            warn!(
                "{}: primary provider '{}' circuit is {}, skipping to fallback",
                operation_name, key, state
            );
            if let Some(fb) = fallback {
                return fb();
            }
            return AiResponse::error(format!(
                "Provider '{}' circuit breaker is {} and no fallback configured",
                key, state
            ));
        }
    }

    let response = primary();
    if response.success {
        if let Some(key) = primary_provider_key {
            circuit_breaker::record_provider_success(key);
        }
        return response;
    }

    let error_msg = response.error.as_deref().unwrap_or("");

    // Record failure to circuit breaker (only for retryable errors)
    if is_retryable_error(error_msg) {
        if let Some(key) = primary_provider_key {
            circuit_breaker::record_provider_failure(key, error_msg);
        }
    }

    if let Some(fb) = fallback {
        if is_retryable_error(error_msg) {
            // Rotate account before trying the fallback when the failure is
            // the account's capacity (never on a rate limit or an overload).
            if migrates_account(failure_kind(error_msg))
                && super::config::rotate_account_on_rate_limit()
            {
                warn!(
                    "{}: account capacity exhausted on primary, rotated account before fallback",
                    operation_name
                );
            }
            warn!(
                "{}: primary failed with retryable error, trying fallback model",
                operation_name
            );
            let fb_response = fb();
            if let Some(key) = fallback_provider_key {
                if fb_response.success {
                    circuit_breaker::record_provider_success(key);
                } else {
                    let fb_err = fb_response.error.as_deref().unwrap_or("");
                    if is_retryable_error(fb_err) {
                        circuit_breaker::record_provider_failure(key, fb_err);
                    }
                }
            }
            return fb_response;
        }
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retryable_rate_limit() {
        assert!(is_retryable_error(
            "Claude API error (429): rate limit exceeded"
        ));
        assert!(is_retryable_error("Too Many Requests"));
        assert!(is_retryable_error("rate limit reached, please slow down"));
    }

    #[test]
    fn test_spend_limit_migrates_the_account() {
        // The Claude CLI's monthly-spend-limit message must reach the account
        // rotation so a mid-request hit moves to another account.
        let msg = "Claude CLI failed (exit 1): stdout: You've hit your monthly \
                   spend limit . raise it at claude.ai/settings/usage";
        assert_eq!(failure_kind(msg), Some(FailureKind::BudgetExhausted));
        assert!(migrates_account(failure_kind(msg)));
        assert!(is_retryable_error(msg));
    }

    /// The Phase 7 regression: `"overloaded"` used to sit in the rate-limit
    /// set, so a provider overload rotated accounts. It is its own kind now,
    /// still retried (on the same account), never rotated, and — unlike a
    /// capacity limit — counted against the provider's circuit breaker.
    #[test]
    fn test_overloaded_retries_but_never_rotates() {
        for msg in [
            "Claude API error (529): overloaded",
            "The API is currently overloaded",
        ] {
            let kind = failure_kind(msg);
            assert_eq!(kind, Some(FailureKind::Overloaded), "{msg}");
            assert!(is_retryable_error(msg), "{msg}");
            assert!(!migrates_account(kind), "{msg}");
            assert!(!is_capacity_signal(kind), "{msg}");
        }
    }

    /// A rate limit is retried with backoff on the same account; only quota
    /// and budget exhaustion rotate.
    #[test]
    fn test_only_account_capacity_rotates() {
        for msg in [
            "Claude API error (429): rate limit exceeded",
            "Too Many Requests",
        ] {
            let kind = failure_kind(msg);
            assert_eq!(kind, Some(FailureKind::RateLimited), "{msg}");
            assert!(!migrates_account(kind), "{msg}");
            assert!(is_capacity_signal(kind), "{msg}");
        }
        for msg in [
            "Claude AI usage limit reached|1790727000",
            "token limit exceeded",
        ] {
            let kind = failure_kind(msg);
            assert_eq!(kind, Some(FailureKind::QuotaExhausted), "{msg}");
            assert!(migrates_account(kind), "{msg}");
            assert!(is_retryable_error(msg), "{msg}");
        }
    }

    #[test]
    fn test_retryable_server_errors() {
        assert!(is_retryable_error(
            "Claude API error (500): internal server error"
        ));
        assert!(is_retryable_error("Gemini API error (502): bad gateway"));
        assert!(is_retryable_error("API error (503): service unavailable"));
        assert!(is_retryable_error("API error (504): gateway timeout"));
    }

    #[test]
    fn test_retryable_overloaded() {
        assert!(is_retryable_error("Claude API error (529): overloaded"));
        assert!(is_retryable_error("The API is currently overloaded"));
    }

    #[test]
    fn test_retryable_network_errors() {
        assert!(is_retryable_error("connection timed out"));
        assert!(is_retryable_error("request timeout after 30s"));
        assert!(is_retryable_error("connection reset by peer"));
        assert!(is_retryable_error("connection refused"));
        assert!(is_retryable_error("dns error: failed to resolve"));
        assert!(is_retryable_error("broken pipe"));
    }

    #[test]
    fn test_retryable_403_auth_errors() {
        // 403 errors are retryable because they can be transient token refresh issues
        assert!(is_retryable_error("Claude API error (403): forbidden"));
        assert!(is_retryable_error("Access forbidden"));
    }

    #[test]
    fn test_not_retryable_auth_errors() {
        assert!(!is_retryable_error("Claude API error (401): unauthorized"));
        assert!(!is_retryable_error("Claude API error (400): bad request"));
    }

    #[test]
    fn test_not_retryable_config_errors() {
        assert!(!is_retryable_error(
            "No Claude API key configured. Please set your API key in Settings."
        ));
        assert!(!is_retryable_error(
            "No Gemini API key configured. Please set your API key in Settings."
        ));
        assert!(!is_retryable_error(
            "Failed to retrieve API key: keychain error"
        ));
        assert!(!is_retryable_error(
            "Failed to create HTTP client: TLS error"
        ));
    }

    #[test]
    fn test_not_retryable_parse_errors() {
        assert!(!is_retryable_error(
            "Failed to parse API response: expected value at line 1"
        ));
    }

    #[test]
    fn test_not_retryable_unknown_errors() {
        // Unknown errors default to not-retryable (conservative approach)
        assert!(!is_retryable_error("something unexpected happened"));
    }

    #[test]
    fn test_retry_returns_success_immediately() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let call_count = AtomicU32::new(0);

        let response = retry_with_backoff("test", || {
            call_count.fetch_add(1, Ordering::SeqCst);
            AiResponse::success("ok".to_string())
        });

        assert!(response.success);
        assert_eq!(response.output, "ok");
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_retry_returns_permanent_error_immediately() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let call_count = AtomicU32::new(0);

        let response = retry_with_backoff("test", || {
            call_count.fetch_add(1, Ordering::SeqCst);
            AiResponse::error("Claude API error (401): unauthorized".to_string())
        });

        assert!(!response.success);
        // Should only be called once — permanent errors are not retried
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_retry_succeeds_on_second_attempt() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let call_count = AtomicU32::new(0);

        // Override backoff for testing — the real retry_with_backoff uses the consts,
        // but we can test the logic by having the closure succeed on second call.
        // Note: This test will incur a ~2s sleep for the first retry backoff.
        // For CI, we test the retryability logic separately (above tests) and
        // only do a minimal integration test here.
        let response = retry_with_backoff("test", || {
            let count = call_count.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                AiResponse::error("Claude API error (429): rate limit".to_string())
            } else {
                AiResponse::success("recovered".to_string())
            }
        });

        assert!(response.success);
        assert_eq!(response.output, "recovered");
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }
}
