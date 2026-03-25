//! Retry with Feedback Injection for the task orchestrator.
//!
//! Implements automatic retry on AI session failure with error context
//! injection into retry prompts. Inspired by agenticSeek's retry loop
//! with feedback pattern.
//!
//! Key features:
//! - Exponential backoff with jitter
//! - Error pattern matching for retryable errors
//! - Feedback injection with error history
//! - Configurable retry limits

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for retry behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Whether retry is enabled
    #[serde(default = "default_retry_enabled")]
    pub enabled: bool,

    /// Maximum number of retry attempts
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    /// Base delay in milliseconds before first retry
    #[serde(default = "default_base_delay_ms")]
    pub base_delay_ms: u64,

    /// Maximum delay in milliseconds (caps exponential growth)
    #[serde(default = "default_max_delay_ms")]
    pub max_delay_ms: u64,

    /// Base for exponential backoff (delay = base_delay * exponential_base^attempt)
    #[serde(default = "default_exponential_base")]
    pub exponential_base: f32,

    /// Whether to add random jitter to delays
    #[serde(default = "default_jitter")]
    pub jitter: bool,

    /// Whether to inject error feedback into retry prompts
    #[serde(default = "default_feedback_injection")]
    pub feedback_injection: bool,

    /// Error patterns that should trigger a retry
    #[serde(default = "default_retryable_errors")]
    pub retryable_errors: Vec<String>,
}

fn default_retry_enabled() -> bool {
    true
}

fn default_max_retries() -> u32 {
    3
}

fn default_base_delay_ms() -> u64 {
    1000 // 1 second
}

fn default_max_delay_ms() -> u64 {
    30000 // 30 seconds
}

fn default_exponential_base() -> f32 {
    2.0
}

fn default_jitter() -> bool {
    true
}

fn default_feedback_injection() -> bool {
    true
}

fn default_retryable_errors() -> Vec<String> {
    vec![
        "timeout".to_string(),
        "rate_limit".to_string(),
        "rate limit".to_string(),
        "connection refused".to_string(),
        "connection reset".to_string(),
        "network error".to_string(),
        "temporary failure".to_string(),
        "service unavailable".to_string(),
        "503".to_string(),
        "504".to_string(),
        "429".to_string(),
        "ETIMEDOUT".to_string(),
        "ECONNRESET".to_string(),
        "ECONNREFUSED".to_string(),
        "exited during initialization".to_string(),
    ]
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            enabled: default_retry_enabled(),
            max_retries: default_max_retries(),
            base_delay_ms: default_base_delay_ms(),
            max_delay_ms: default_max_delay_ms(),
            exponential_base: default_exponential_base(),
            jitter: default_jitter(),
            feedback_injection: default_feedback_injection(),
            retryable_errors: default_retryable_errors(),
        }
    }
}

// ============================================================================
// Retry State
// ============================================================================

/// State tracking for retries within a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryState {
    /// Current attempt number (0 = initial attempt, 1 = first retry, etc.)
    pub attempt: u32,

    /// Last error encountered
    pub last_error: Option<String>,

    /// Timestamp of last attempt
    pub last_attempt_at: Option<String>,

    /// Total delay accumulated across all retries (ms)
    pub total_delay_ms: u64,

    /// History of all retry attempts
    pub error_history: Vec<RetryAttempt>,
}

impl Default for RetryState {
    fn default() -> Self {
        Self::new()
    }
}

impl RetryState {
    /// Create a new retry state.
    pub fn new() -> Self {
        Self {
            attempt: 0,
            last_error: None,
            last_attempt_at: None,
            total_delay_ms: 0,
            error_history: Vec::new(),
        }
    }

    /// Record a new retry attempt.
    pub fn record_attempt(&mut self, error: &str, delay_ms: u64, feedback_injected: bool) {
        self.attempt += 1;
        self.last_error = Some(error.to_string());
        self.last_attempt_at = Some(chrono::Utc::now().to_rfc3339());
        self.total_delay_ms += delay_ms;

        self.error_history.push(RetryAttempt {
            attempt_number: self.attempt,
            error: error.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            delay_ms,
            feedback_injected,
        });
    }

    /// Check if max retries have been exhausted.
    pub fn exhausted(&self, max_retries: u32) -> bool {
        self.attempt >= max_retries
    }

    /// Get the current attempt number (1-indexed for display).
    pub fn current_attempt_display(&self) -> u32 {
        self.attempt + 1
    }

    /// Reset the retry state.
    pub fn reset(&mut self) {
        self.attempt = 0;
        self.last_error = None;
        self.last_attempt_at = None;
        self.total_delay_ms = 0;
        self.error_history.clear();
    }
}

/// Record of a single retry attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryAttempt {
    /// Attempt number (1-indexed)
    pub attempt_number: u32,

    /// Error that triggered this retry
    pub error: String,

    /// When the retry was attempted
    pub timestamp: String,

    /// Delay before this retry (ms)
    pub delay_ms: u64,

    /// Whether feedback was injected for this retry
    pub feedback_injected: bool,
}

// ============================================================================
// Retry Decision
// ============================================================================

/// Decision about whether to retry an operation.
#[derive(Debug, Clone)]
pub enum RetryDecision {
    /// Retry the operation after the specified delay
    Retry {
        /// Delay before retry in milliseconds
        delay_ms: u64,
        /// Feedback to inject into the retry prompt
        feedback: String,
    },

    /// Do not retry, give up
    GiveUp {
        /// Reason for giving up
        reason: String,
    },
}

// ============================================================================
// Retry Service
// ============================================================================

/// Service for managing retry logic.
pub struct RetryService {
    config: RetryConfig,
}

impl RetryService {
    /// Create a new retry service with the given configuration.
    pub fn new(config: RetryConfig) -> Self {
        Self { config }
    }

    /// Create a retry service with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(RetryConfig::default())
    }

    /// Decide whether to retry based on the error and current state.
    pub fn should_retry(&self, error: &str, state: &RetryState) -> RetryDecision {
        // Check if retry is enabled
        if !self.config.enabled {
            return RetryDecision::GiveUp {
                reason: "Retry is disabled".to_string(),
            };
        }

        // Check if max retries exceeded
        if state.exhausted(self.config.max_retries) {
            return RetryDecision::GiveUp {
                reason: format!("Max retries ({}) exceeded", self.config.max_retries),
            };
        }

        // Check if error is retryable
        if !self.is_retryable_error(error) {
            return RetryDecision::GiveUp {
                reason: format!("Error is not retryable: {}", truncate_error(error, 100)),
            };
        }

        // Calculate delay
        let delay_ms = self.calculate_delay(state.attempt);

        // Build feedback if enabled
        let feedback = if self.config.feedback_injection {
            self.build_feedback_prompt(error, &state.error_history)
        } else {
            String::new()
        };

        info!(
            "Retry decision: attempt {} of {}, delay {}ms",
            state.current_attempt_display(),
            self.config.max_retries,
            delay_ms
        );

        RetryDecision::Retry { delay_ms, feedback }
    }

    /// Check if an error is retryable based on configured patterns.
    pub fn is_retryable_error(&self, error: &str) -> bool {
        let error_lower = error.to_lowercase();

        for pattern in &self.config.retryable_errors {
            if error_lower.contains(&pattern.to_lowercase()) {
                debug!(
                    "Error matches retryable pattern '{}': {}",
                    pattern,
                    truncate_error(error, 50)
                );
                return true;
            }
        }

        debug!(
            "Error does not match any retryable patterns: {}",
            truncate_error(error, 50)
        );
        false
    }

    /// Calculate the delay before the next retry attempt.
    ///
    /// Uses exponential backoff: delay = base_delay * exponential_base^attempt
    /// Optionally adds jitter to prevent thundering herd.
    pub fn calculate_delay(&self, attempt: u32) -> u64 {
        // Calculate base exponential delay
        let exponential_delay = self.config.base_delay_ms as f64
            * (self.config.exponential_base as f64).powi(attempt as i32);

        // Cap at max delay
        let capped_delay = exponential_delay.min(self.config.max_delay_ms as f64);

        // Add jitter if enabled (random value between 0 and 25% of delay)
        let final_delay = if self.config.jitter {
            let jitter_factor = 0.25;
            let jitter = capped_delay * jitter_factor * rand_factor();
            capped_delay + jitter
        } else {
            capped_delay
        };

        final_delay as u64
    }

    /// Build a feedback prompt to inject into the retry.
    pub fn build_feedback_prompt(&self, error: &str, history: &[RetryAttempt]) -> String {
        let mut feedback = String::new();

        feedback.push_str("## Previous Attempt Failed\n\n");
        feedback.push_str(&format!(
            "**Attempt**: {} of {}\n",
            history.len() + 1,
            self.config.max_retries
        ));
        feedback.push_str(&format!("**Error**: {}\n\n", truncate_error(error, 500)));

        // Add error history if there are previous attempts
        if !history.is_empty() {
            feedback.push_str("**Error History**:\n");
            for attempt in history.iter().rev().take(3) {
                feedback.push_str(&format!(
                    "- Attempt {}: {}\n",
                    attempt.attempt_number,
                    truncate_error(&attempt.error, 100)
                ));
            }
            feedback.push('\n');
        }

        feedback.push_str("**Instructions**: Please address the error above. ");
        feedback.push_str("Adjust your approach to avoid repeating this error.\n\n");

        // Add specific guidance based on error patterns
        if let Some(guidance) = self.get_error_guidance(error) {
            feedback.push_str(&format!("**Suggestion**: {}\n\n", guidance));
        }

        feedback
    }

    /// Get specific guidance based on the error type.
    fn get_error_guidance(&self, error: &str) -> Option<String> {
        let error_lower = error.to_lowercase();

        if error_lower.contains("timeout") {
            Some("The operation timed out. Consider breaking the task into smaller steps or checking for blocking operations.".to_string())
        } else if error_lower.contains("rate limit") || error_lower.contains("429") {
            Some(
                "Rate limit encountered. The system will wait before retrying. No action needed."
                    .to_string(),
            )
        } else if error_lower.contains("connection") {
            Some(
                "Network connectivity issue. Check if external services are accessible."
                    .to_string(),
            )
        } else if error_lower.contains("permission") || error_lower.contains("access denied") {
            Some("Permission error. Check file/resource permissions.".to_string())
        } else if error_lower.contains("not found") || error_lower.contains("404") {
            Some("Resource not found. Verify the path or URL is correct.".to_string())
        } else {
            None
        }
    }

    /// Get the delay as a Duration.
    pub fn get_delay_duration(&self, attempt: u32) -> Duration {
        Duration::from_millis(self.calculate_delay(attempt))
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Generate a random factor between 0 and 1 for jitter.
fn rand_factor() -> f64 {
    use rand::Rng;
    rand::rng().random::<f64>()
}

/// Truncate an error message to a maximum length.
fn truncate_error(error: &str, max_len: usize) -> String {
    if error.len() <= max_len {
        error.to_string()
    } else {
        let mut end = max_len;
        while end > 0 && !error.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &error[..end])
    }
}

// ============================================================================
// Non-Retriable Error Detection
// ============================================================================

/// Error patterns that should never be retried (deterministic failures).
const NON_RETRIABLE_PATTERNS: &[&str] = &[
    "syntax error",
    "type error",
    "compilation failed",
    "invalid configuration",
    "authentication failed",
    "permission denied",
    "resource not found",
    "schema validation",
    "missing required field",
];

/// Check if an error is definitively non-retriable.
///
/// Non-retriable errors are deterministic failures that will not resolve
/// with retries (e.g., syntax errors, auth failures, missing resources).
pub fn is_non_retriable(error: &str) -> bool {
    let error_lower = error.to_lowercase();
    NON_RETRIABLE_PATTERNS
        .iter()
        .any(|pattern| error_lower.contains(pattern))
}

// ============================================================================
// Circuit Breaker
// ============================================================================

/// State of the circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Circuit is closed — requests flow normally.
    Closed,
    /// Circuit is open — requests are rejected immediately.
    Open,
    /// Circuit is half-open — a single probe request is allowed through.
    HalfOpen,
}

/// Circuit breaker to prevent cascading failures.
///
/// Tracks consecutive failures and opens the circuit when the threshold
/// is reached. After a cooldown period, allows a single probe request
/// through (half-open). If the probe succeeds, the circuit closes;
/// if it fails, the circuit reopens.
pub struct CircuitBreaker {
    /// Current state of the circuit.
    state: std::sync::Mutex<CircuitState>,
    /// Number of consecutive failures.
    failure_count: std::sync::Mutex<u32>,
    /// Threshold of consecutive failures before the circuit opens.
    failure_threshold: u32,
    /// When the circuit was last opened (for cooldown calculation).
    last_opened: std::sync::Mutex<Option<std::time::Instant>>,
    /// Cooldown duration before transitioning from Open to HalfOpen.
    cooldown: Duration,
}

impl CircuitBreaker {
    /// Create a new circuit breaker.
    ///
    /// - `failure_threshold`: number of consecutive failures to trip the circuit
    /// - `cooldown`: how long to wait before allowing a probe request
    pub fn new(failure_threshold: u32, cooldown: Duration) -> Arc<Self> {
        Arc::new(Self {
            state: std::sync::Mutex::new(CircuitState::Closed),
            failure_count: std::sync::Mutex::new(0),
            failure_threshold,
            last_opened: std::sync::Mutex::new(None),
            cooldown,
        })
    }

    /// Check if a request is allowed through the circuit.
    ///
    /// Returns `true` if the request should proceed, `false` if rejected.
    pub fn allow_request(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        match *state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // Check if cooldown has elapsed
                let last_opened = self.last_opened.lock().unwrap();
                if let Some(opened_at) = *last_opened {
                    if opened_at.elapsed() >= self.cooldown {
                        info!("Circuit breaker: cooldown elapsed, transitioning to HalfOpen");
                        *state = CircuitState::HalfOpen;
                        true
                    } else {
                        debug!(
                            "Circuit breaker: open, rejecting request ({}ms remaining)",
                            (self.cooldown - opened_at.elapsed()).as_millis()
                        );
                        false
                    }
                } else {
                    // No timestamp — shouldn't happen, but allow through
                    *state = CircuitState::HalfOpen;
                    true
                }
            }
            CircuitState::HalfOpen => {
                // Only one probe request allowed; reject additional ones
                // The probe is already in-flight
                false
            }
        }
    }

    /// Record a successful operation.
    pub fn record_success(&self) {
        let mut state = self.state.lock().unwrap();
        let mut count = self.failure_count.lock().unwrap();
        *count = 0;
        if *state == CircuitState::HalfOpen {
            info!("Circuit breaker: probe succeeded, closing circuit");
        }
        *state = CircuitState::Closed;
    }

    /// Record a failed operation.
    pub fn record_failure(&self) {
        let mut state = self.state.lock().unwrap();
        let mut count = self.failure_count.lock().unwrap();
        *count += 1;

        match *state {
            CircuitState::Closed => {
                if *count >= self.failure_threshold {
                    warn!(
                        "Circuit breaker: {} consecutive failures (threshold={}), opening circuit",
                        *count, self.failure_threshold
                    );
                    *state = CircuitState::Open;
                    *self.last_opened.lock().unwrap() = Some(std::time::Instant::now());
                }
            }
            CircuitState::HalfOpen => {
                warn!("Circuit breaker: probe failed, reopening circuit");
                *state = CircuitState::Open;
                *self.last_opened.lock().unwrap() = Some(std::time::Instant::now());
            }
            CircuitState::Open => {
                // Already open, just count
            }
        }
    }

    /// Get the current circuit state.
    pub fn state(&self) -> CircuitState {
        *self.state.lock().unwrap()
    }

    /// Get the current consecutive failure count.
    pub fn failure_count(&self) -> u32 {
        *self.failure_count.lock().unwrap()
    }
}

// ============================================================================
// Retry Wrapper
// ============================================================================

/// Execute an async operation with retry logic.
///
/// This is a helper function that wraps an operation with automatic retry.
pub async fn with_retry<F, Fut, T, E>(service: &RetryService, mut operation: F) -> Result<T, E>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let mut state = RetryState::new();

    loop {
        // Get feedback from previous attempt (if any)
        let feedback = if state.attempt > 0 {
            state
                .last_error
                .as_ref()
                .map(|e| service.build_feedback_prompt(e, &state.error_history))
        } else {
            None
        };

        // Execute the operation
        match operation(feedback).await {
            Ok(result) => return Ok(result),
            Err(error) => {
                let error_str = error.to_string();

                match service.should_retry(&error_str, &state) {
                    RetryDecision::Retry {
                        delay_ms,
                        feedback: _,
                    } => {
                        warn!(
                            "Operation failed, retrying in {}ms: {}",
                            delay_ms,
                            truncate_error(&error_str, 100)
                        );

                        state.record_attempt(
                            &error_str,
                            delay_ms,
                            service.config.feedback_injection,
                        );

                        // Wait before retry
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    }
                    RetryDecision::GiveUp { reason } => {
                        warn!("Giving up after {} attempts: {}", state.attempt, reason);
                        return Err(error);
                    }
                }
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = RetryConfig::default();
        assert!(config.enabled);
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.base_delay_ms, 1000);
        assert!(config.feedback_injection);
    }

    #[test]
    fn test_retry_state_recording() {
        let mut state = RetryState::new();
        assert_eq!(state.attempt, 0);
        assert!(state.error_history.is_empty());

        state.record_attempt("timeout error", 1000, true);
        assert_eq!(state.attempt, 1);
        assert_eq!(state.error_history.len(), 1);
        assert_eq!(state.last_error, Some("timeout error".to_string()));

        state.record_attempt("rate limit", 2000, true);
        assert_eq!(state.attempt, 2);
        assert_eq!(state.error_history.len(), 2);
        assert_eq!(state.total_delay_ms, 3000);
    }

    #[test]
    fn test_retry_exhausted() {
        let mut state = RetryState::new();

        assert!(!state.exhausted(3));

        state.record_attempt("error 1", 1000, false);
        assert!(!state.exhausted(3));

        state.record_attempt("error 2", 1000, false);
        assert!(!state.exhausted(3));

        state.record_attempt("error 3", 1000, false);
        assert!(state.exhausted(3));
    }

    #[test]
    fn test_is_retryable_error() {
        let service = RetryService::with_defaults();

        assert!(service.is_retryable_error("Connection timeout occurred"));
        assert!(service.is_retryable_error("Rate limit exceeded"));
        assert!(service.is_retryable_error("ETIMEDOUT"));
        assert!(service.is_retryable_error("HTTP 429"));
        assert!(service.is_retryable_error("Service Unavailable"));

        assert!(!service.is_retryable_error("Syntax error in code"));
        assert!(!service.is_retryable_error("File not found"));
        assert!(!service.is_retryable_error("Invalid input"));
    }

    #[test]
    fn test_calculate_delay_exponential() {
        let mut config = RetryConfig::default();
        config.jitter = false; // Disable jitter for predictable testing
        let service = RetryService::new(config);

        // base_delay * 2^attempt
        assert_eq!(service.calculate_delay(0), 1000); // 1000 * 2^0 = 1000
        assert_eq!(service.calculate_delay(1), 2000); // 1000 * 2^1 = 2000
        assert_eq!(service.calculate_delay(2), 4000); // 1000 * 2^2 = 4000
        assert_eq!(service.calculate_delay(3), 8000); // 1000 * 2^3 = 8000
    }

    #[test]
    fn test_calculate_delay_capped() {
        let mut config = RetryConfig::default();
        config.jitter = false;
        config.max_delay_ms = 5000; // Cap at 5 seconds
        let service = RetryService::new(config);

        // Should be capped at max_delay
        assert_eq!(service.calculate_delay(5), 5000); // Would be 32000, capped to 5000
    }

    #[test]
    fn test_should_retry_disabled() {
        let mut config = RetryConfig::default();
        config.enabled = false;
        let service = RetryService::new(config);
        let state = RetryState::new();

        let decision = service.should_retry("timeout", &state);
        assert!(matches!(decision, RetryDecision::GiveUp { .. }));
    }

    #[test]
    fn test_should_retry_exhausted() {
        let config = RetryConfig::default();
        let service = RetryService::new(config);

        let mut state = RetryState::new();
        state.record_attempt("e1", 1000, false);
        state.record_attempt("e2", 1000, false);
        state.record_attempt("e3", 1000, false);

        let decision = service.should_retry("timeout", &state);
        assert!(matches!(decision, RetryDecision::GiveUp { .. }));
    }

    #[test]
    fn test_should_retry_success() {
        let config = RetryConfig::default();
        let service = RetryService::new(config);
        let state = RetryState::new();

        let decision = service.should_retry("Connection timeout", &state);
        assert!(matches!(decision, RetryDecision::Retry { .. }));

        if let RetryDecision::Retry { delay_ms, feedback } = decision {
            assert!(delay_ms > 0);
            assert!(feedback.contains("Previous Attempt Failed"));
        }
    }

    #[test]
    fn test_build_feedback_prompt() {
        let service = RetryService::with_defaults();
        let history = vec![RetryAttempt {
            attempt_number: 1,
            error: "First error".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            delay_ms: 1000,
            feedback_injected: true,
        }];

        let feedback = service.build_feedback_prompt("Second error", &history);

        assert!(feedback.contains("Previous Attempt Failed"));
        assert!(feedback.contains("Second error"));
        assert!(feedback.contains("Error History"));
        assert!(feedback.contains("First error"));
    }

    #[test]
    fn test_truncate_error() {
        assert_eq!(truncate_error("short", 10), "short");
        assert_eq!(
            truncate_error("this is a longer error message", 10),
            "this is a ..."
        );
    }

    #[test]
    fn test_is_non_retriable() {
        assert!(is_non_retriable("Syntax error on line 42"));
        assert!(is_non_retriable("Authentication failed: invalid token"));
        assert!(is_non_retriable("compilation failed with 3 errors"));
        assert!(is_non_retriable("Permission Denied for resource"));
        assert!(is_non_retriable("schema validation: missing field 'name'"));

        // These ARE retriable
        assert!(!is_non_retriable("Connection timeout"));
        assert!(!is_non_retriable("Rate limit exceeded"));
        assert!(!is_non_retriable("Service unavailable"));
    }

    #[test]
    fn test_circuit_breaker_closed_allows_requests() {
        let cb = CircuitBreaker::new(3, Duration::from_millis(100));
        assert!(cb.allow_request());
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_opens_after_threshold() {
        let cb = CircuitBreaker::new(3, Duration::from_millis(100));
        cb.record_failure();
        cb.record_failure();
        assert!(cb.allow_request()); // Still closed
        cb.record_failure(); // 3rd failure = threshold
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allow_request()); // Rejected
    }

    #[test]
    fn test_circuit_breaker_success_resets() {
        let cb = CircuitBreaker::new(3, Duration::from_millis(100));
        cb.record_failure();
        cb.record_failure();
        cb.record_success(); // Reset
        assert_eq!(cb.failure_count(), 0);
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_half_open_on_cooldown() {
        let cb = CircuitBreaker::new(2, Duration::from_millis(10));
        cb.record_failure();
        cb.record_failure(); // Opens
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for cooldown
        std::thread::sleep(Duration::from_millis(15));
        assert!(cb.allow_request()); // Transitions to HalfOpen
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn test_circuit_breaker_half_open_success_closes() {
        let cb = CircuitBreaker::new(2, Duration::from_millis(10));
        cb.record_failure();
        cb.record_failure(); // Opens

        std::thread::sleep(Duration::from_millis(15));
        cb.allow_request(); // HalfOpen
        cb.record_success(); // Close
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_half_open_failure_reopens() {
        let cb = CircuitBreaker::new(2, Duration::from_millis(10));
        cb.record_failure();
        cb.record_failure(); // Opens

        std::thread::sleep(Duration::from_millis(15));
        cb.allow_request(); // HalfOpen
        cb.record_failure(); // Reopen
        assert_eq!(cb.state(), CircuitState::Open);
    }
}
