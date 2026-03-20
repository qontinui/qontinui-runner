#![allow(dead_code)]

use super::types::AiResponse;
use std::time::Duration;
use tracing::{debug, error, warn};

/// Maximum number of retry attempts for AI API calls.
pub(super) const MAX_AI_RETRIES: u32 = 3;

/// Base backoff delay in milliseconds (doubles each retry: 2s, 4s, 8s).
pub(super) const BASE_BACKOFF_MS: u64 = 2000;

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
/// - HTTP 401, 403 (authentication/authorization)
/// - Deserialization / JSON parse errors
/// - Missing API key configuration
/// - Client construction failures
pub(super) fn is_retryable_error(error_msg: &str) -> bool {
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
/// # Arguments
/// * `operation_name` - Human-readable label for log messages (e.g., "Claude API")
/// * `operation` - Closure that performs the AI call and returns an `AiResponse`
pub(super) fn retry_with_backoff<F>(operation_name: &str, operation: F) -> AiResponse
where
    F: Fn() -> AiResponse,
{
    for attempt in 0..=MAX_AI_RETRIES {
        let response = operation();

        if response.success {
            return response;
        }

        // Extract error message for retryability check
        let error_msg = response.error.as_deref().unwrap_or("");

        if !is_retryable_error(error_msg) {
            // Permanent error — return immediately, no retry
            if attempt > 0 {
                debug!(
                    "{} permanent error after {} retries, not retrying: {}",
                    operation_name, attempt, error_msg
                );
            }
            return response;
        }

        if attempt == MAX_AI_RETRIES {
            // Final attempt failed — log and return
            error!(
                "{} failed after {} retries: {}",
                operation_name, MAX_AI_RETRIES, error_msg
            );
            return response;
        }

        // Calculate backoff: BASE_BACKOFF_MS * 2^attempt (2s, 4s, 8s for base=2000)
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
    }

    // Unreachable, but satisfy the compiler
    AiResponse::error(format!(
        "{} failed after exhausting retries",
        operation_name
    ))
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
    let response = primary();
    if response.success {
        return response;
    }

    if let Some(fb) = fallback {
        let error_msg = response.error.as_deref().unwrap_or("");
        if is_retryable_error(error_msg) {
            warn!(
                "{}: primary failed with retryable error, trying fallback model",
                operation_name
            );
            return fb();
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
