//! Circuit Breaker for AI Providers
//!
//! Implements the circuit breaker pattern to track provider health and prevent
//! repeated calls to unhealthy providers. Inspired by cc-switch's provider
//! failover proxy.
//!
//! # States
//! - **Closed**: Provider is healthy, all requests pass through
//! - **Open**: Provider has failed too many times, requests fail fast
//! - **HalfOpen**: After a timeout, allows a single probe request to test recovery
//!
//! # Usage
//! The global `PROVIDER_CIRCUIT_BREAKERS` registry tracks per-provider state.
//! Call `record_success`/`record_failure` after each AI call, and check
//! `is_available` before dispatching.

use dashmap::DashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

// ============================================================================
// Event Broadcasting
// ============================================================================

/// Event emitted on every circuit breaker state transition.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CircuitBreakerEvent {
    pub provider_key: String,
    pub state: String,
    pub available: bool,
}

/// Canonical representation of a provider's circuit breaker state,
/// shared between Tauri commands, MCP health endpoint, and event payloads.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProviderCircuitState {
    pub provider_key: String,
    pub state: String,
    pub available: bool,
}

static EVENT_TX: OnceLock<broadcast::Sender<CircuitBreakerEvent>> = OnceLock::new();

fn event_sender() -> &'static broadcast::Sender<CircuitBreakerEvent> {
    EVENT_TX.get_or_init(|| broadcast::channel(16).0)
}

/// Subscribe to circuit breaker state-change events.
pub fn event_receiver() -> broadcast::Receiver<CircuitBreakerEvent> {
    event_sender().subscribe()
}

/// Fire-and-forget: send a state-change event if anyone is listening.
fn emit_state_change(provider_key: &str, state: CircuitState) {
    let _ = event_sender().send(CircuitBreakerEvent {
        provider_key: provider_key.to_string(),
        state: state.to_string(),
        available: state != CircuitState::Open,
    });
}

// ============================================================================
// Configuration
// ============================================================================

/// Circuit breaker configuration with sensible defaults for AI providers.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures before opening the circuit.
    pub failure_threshold: u32,
    /// Number of consecutive successes in HalfOpen before closing the circuit.
    pub success_threshold: u32,
    /// How long the circuit stays open before transitioning to HalfOpen.
    pub open_timeout: Duration,
    /// Maximum number of concurrent probe requests in HalfOpen state.
    pub half_open_max_probes: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 4,
            success_threshold: 2,
            open_timeout: Duration::from_secs(60),
            half_open_max_probes: 1,
        }
    }
}

// ============================================================================
// Circuit State
// ============================================================================

/// The three states of a circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation — requests pass through.
    Closed,
    /// Provider is unhealthy — requests fail fast.
    Open,
    /// Testing recovery — limited probe requests allowed.
    HalfOpen,
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitState::Closed => write!(f, "Closed"),
            CircuitState::Open => write!(f, "Open"),
            CircuitState::HalfOpen => write!(f, "HalfOpen"),
        }
    }
}

// ============================================================================
// Circuit Breaker
// ============================================================================

/// Circuit breaker for a single AI provider.
pub struct CircuitBreaker {
    /// Current state of the circuit.
    state: std::sync::RwLock<CircuitState>,
    /// Number of consecutive failures (in Closed state).
    consecutive_failures: AtomicU32,
    /// Number of consecutive successes (in HalfOpen state).
    consecutive_successes: AtomicU32,
    /// Number of active probe requests (in HalfOpen state).
    active_probes: AtomicU32,
    /// When the circuit transitioned to Open state.
    opened_at: std::sync::RwLock<Option<Instant>>,
    /// Configuration.
    config: CircuitBreakerConfig,
    /// Provider identifier (for logging).
    provider_name: String,
}

impl CircuitBreaker {
    /// Create a new circuit breaker for the given provider.
    pub fn new(provider_name: String, config: CircuitBreakerConfig) -> Self {
        Self {
            state: std::sync::RwLock::new(CircuitState::Closed),
            consecutive_failures: AtomicU32::new(0),
            consecutive_successes: AtomicU32::new(0),
            active_probes: AtomicU32::new(0),
            opened_at: std::sync::RwLock::new(None),
            config,
            provider_name,
        }
    }

    /// Check if the provider is available for requests.
    ///
    /// Returns `true` if the circuit is Closed or if a HalfOpen probe is allowed.
    /// Returns `false` if the circuit is Open (fail fast).
    ///
    /// Holds the write lock for the entire check-transition-probe sequence to
    /// prevent TOCTOU races where multiple threads could simultaneously
    /// transition from Open to HalfOpen.
    pub fn is_available(&self) -> bool {
        let mut state = self.state.write().unwrap();
        match *state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // Check if the open timeout has elapsed
                if self.should_transition_to_half_open_inner() {
                    info!(
                        "Circuit breaker HALF-OPEN for '{}': timeout elapsed, allowing probe",
                        self.provider_name
                    );
                    *state = CircuitState::HalfOpen;
                    self.consecutive_successes.store(0, Ordering::Relaxed);
                    self.active_probes.store(0, Ordering::Relaxed);
                    emit_state_change(&self.provider_name, CircuitState::HalfOpen);
                    // Allow one probe
                    self.try_acquire_probe()
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => self.try_acquire_probe(),
        }
    }

    /// Get the current circuit state.
    pub fn state(&self) -> CircuitState {
        *self.state.read().unwrap()
    }

    /// Record a successful request. Transitions HalfOpen → Closed if enough
    /// consecutive successes are recorded.
    pub fn record_success(&self) {
        let mut state = self.state.write().unwrap();
        match *state {
            CircuitState::Closed => {
                // Reset failure counter on success
                self.consecutive_failures.store(0, Ordering::Relaxed);
            }
            CircuitState::HalfOpen => {
                self.release_probe();
                let successes = self.consecutive_successes.fetch_add(1, Ordering::SeqCst) + 1;
                if successes >= self.config.success_threshold {
                    info!(
                        "Circuit breaker CLOSED for '{}': {} consecutive successes in HalfOpen",
                        self.provider_name, self.config.success_threshold
                    );
                    *state = CircuitState::Closed;
                    self.consecutive_failures.store(0, Ordering::Relaxed);
                    self.consecutive_successes.store(0, Ordering::Relaxed);
                    self.active_probes.store(0, Ordering::Relaxed);
                    *self.opened_at.write().unwrap() = None;
                    emit_state_change(&self.provider_name, CircuitState::Closed);
                }
            }
            CircuitState::Open => {}
        }
    }

    /// Record a failed request. May transition Closed → Open or HalfOpen → Open.
    pub fn record_failure(&self, error_msg: &str) {
        let mut state = self.state.write().unwrap();
        match *state {
            CircuitState::Closed => {
                let failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
                if failures >= self.config.failure_threshold {
                    warn!(
                        "Circuit breaker OPENING for '{}': {} consecutive failures (threshold: {}). Last error: {}",
                        self.provider_name, failures, self.config.failure_threshold, error_msg
                    );
                    *state = CircuitState::Open;
                    *self.opened_at.write().unwrap() = Some(Instant::now());
                    self.consecutive_successes.store(0, Ordering::Relaxed);
                    self.active_probes.store(0, Ordering::Relaxed);
                    emit_state_change(&self.provider_name, CircuitState::Open);
                } else {
                    debug!(
                        "Circuit breaker '{}': failure {}/{} — {}",
                        self.provider_name, failures, self.config.failure_threshold, error_msg
                    );
                }
            }
            CircuitState::HalfOpen => {
                self.release_probe();
                warn!(
                    "Circuit breaker RE-OPENING for '{}': probe failed in HalfOpen — {}",
                    self.provider_name, error_msg
                );
                *state = CircuitState::Open;
                *self.opened_at.write().unwrap() = Some(Instant::now());
                self.consecutive_successes.store(0, Ordering::Relaxed);
                self.active_probes.store(0, Ordering::Relaxed);
                emit_state_change(&self.provider_name, CircuitState::Open);
            }
            CircuitState::Open => {}
        }
    }

    /// Manually reset the circuit to Closed state.
    pub fn reset(&self) {
        info!(
            "Circuit breaker RESET for '{}': manually closed",
            self.provider_name
        );
        let mut state = self.state.write().unwrap();
        *state = CircuitState::Closed;
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.consecutive_successes.store(0, Ordering::Relaxed);
        self.active_probes.store(0, Ordering::Relaxed);
        *self.opened_at.write().unwrap() = None;
        emit_state_change(&self.provider_name, CircuitState::Closed);
    }

    // --- Private helpers ---

    /// Check if the open timeout has elapsed. Safe to call while holding
    /// the state write lock (only acquires `opened_at` read lock).
    fn should_transition_to_half_open_inner(&self) -> bool {
        if let Some(opened) = *self.opened_at.read().unwrap() {
            opened.elapsed() >= self.config.open_timeout
        } else {
            false
        }
    }

    fn try_acquire_probe(&self) -> bool {
        let current = self.active_probes.load(Ordering::SeqCst);
        if current >= self.config.half_open_max_probes {
            return false;
        }
        // CAS to avoid over-acquiring
        self.active_probes
            .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
    }

    fn release_probe(&self) {
        // Use fetch_update to avoid atomic underflow wrapping to u32::MAX
        let _ = self.active_probes.fetch_update(
            Ordering::SeqCst,
            Ordering::SeqCst,
            |current| {
                if current > 0 {
                    Some(current - 1)
                } else {
                    None // Don't decrement below zero
                }
            },
        );
    }
}

// ============================================================================
// Global Provider Registry
// ============================================================================

/// Global registry of circuit breakers, keyed by provider name string.
///
/// Uses DashMap for lock-free concurrent access across threads.
static PROVIDER_BREAKERS: OnceLock<DashMap<String, CircuitBreaker>> = OnceLock::new();

fn breakers() -> &'static DashMap<String, CircuitBreaker> {
    PROVIDER_BREAKERS.get_or_init(DashMap::new)
}

/// Get or create a circuit breaker for a provider.
fn get_or_create(provider_key: &str) -> dashmap::mapref::one::Ref<'static, String, CircuitBreaker> {
    let map = breakers();
    if !map.contains_key(provider_key) {
        map.entry(provider_key.to_string())
            .or_insert_with(|| {
                CircuitBreaker::new(provider_key.to_string(), CircuitBreakerConfig::default())
            });
    }
    map.get(provider_key).unwrap()
}

/// Check if a provider is available (circuit not open).
pub fn is_provider_available(provider_key: &str) -> bool {
    get_or_create(provider_key).is_available()
}

/// Get the circuit state for a provider.
pub fn provider_state(provider_key: &str) -> CircuitState {
    get_or_create(provider_key).state()
}

/// Record a successful AI call for the provider.
pub fn record_provider_success(provider_key: &str) {
    get_or_create(provider_key).record_success();
}

/// Record a failed AI call for the provider.
pub fn record_provider_failure(provider_key: &str, error_msg: &str) {
    get_or_create(provider_key).record_failure(error_msg);
}

/// Reset a provider's circuit breaker to Closed.
pub fn reset_provider(provider_key: &str) {
    get_or_create(provider_key).reset();
}

/// Get a snapshot of all provider circuit breaker states.
pub fn all_provider_states() -> Vec<(String, CircuitState)> {
    breakers()
        .iter()
        .map(|entry| (entry.key().clone(), entry.value().state()))
        .collect()
}

/// Get a snapshot of all provider circuit breaker states as serializable structs.
pub fn all_provider_circuit_states() -> Vec<ProviderCircuitState> {
    breakers()
        .iter()
        .map(|entry| ProviderCircuitState {
            provider_key: entry.key().clone(),
            state: entry.value().state().to_string(),
            available: entry.value().is_available(),
        })
        .collect()
}

/// Convert an AiProvider enum to a circuit breaker key string.
pub fn provider_key(provider: &crate::settings::AiProvider) -> String {
    match provider {
        crate::settings::AiProvider::ClaudeCli => "claude_cli".to_string(),
        crate::settings::AiProvider::ClaudeApi => "claude_api".to_string(),
        crate::settings::AiProvider::GeminiCli => "gemini_cli".to_string(),
        crate::settings::AiProvider::GeminiApi => "gemini_api".to_string(),
    }
}

// ============================================================================
// Failover Switch Deduplication
// ============================================================================

/// Guards against concurrent provider failover switches.
///
/// When multiple concurrent AI sessions detect the same provider failure,
/// this prevents a "failover storm" where all sessions independently try
/// to switch providers at the same time.
static PENDING_SWITCHES: OnceLock<DashMap<String, ()>> = OnceLock::new();

fn pending_switches() -> &'static DashMap<String, ()> {
    PENDING_SWITCHES.get_or_init(DashMap::new)
}

/// Attempt to acquire a failover switch lock for a provider.
/// Returns `true` if this caller should perform the switch, `false` if
/// another caller is already switching.
pub fn try_acquire_failover_switch(provider_key: &str) -> bool {
    use dashmap::mapref::entry::Entry;
    match pending_switches().entry(provider_key.to_string()) {
        Entry::Occupied(_) => {
            debug!(
                "Failover switch already in progress for '{}', skipping",
                provider_key
            );
            false
        }
        Entry::Vacant(v) => {
            v.insert(());
            true
        }
    }
}

/// Release the failover switch lock for a provider.
pub fn release_failover_switch(provider_key: &str) {
    pending_switches().remove(provider_key);
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_breaker(failure_threshold: u32, success_threshold: u32) -> CircuitBreaker {
        CircuitBreaker::new(
            "test_provider".to_string(),
            CircuitBreakerConfig {
                failure_threshold,
                success_threshold,
                open_timeout: Duration::from_millis(50),
                half_open_max_probes: 1,
            },
        )
    }

    #[test]
    fn test_starts_closed() {
        let cb = make_breaker(3, 2);
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.is_available());
    }

    #[test]
    fn test_opens_after_threshold_failures() {
        let cb = make_breaker(3, 2);

        cb.record_failure("error 1");
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure("error 2");
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure("error 3");
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.is_available());
    }

    #[test]
    fn test_success_resets_failure_count() {
        let cb = make_breaker(3, 2);

        cb.record_failure("error 1");
        cb.record_failure("error 2");
        cb.record_success(); // reset
        cb.record_failure("error 1 again");
        cb.record_failure("error 2 again");

        // Still closed — success reset the counter
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_transitions_to_half_open_after_timeout() {
        let cb = make_breaker(2, 1);

        cb.record_failure("error 1");
        cb.record_failure("error 2");
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(60));

        // is_available should trigger transition to HalfOpen
        assert!(cb.is_available());
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn test_half_open_closes_on_success() {
        let cb = make_breaker(2, 1);

        // Open the circuit
        cb.record_failure("e1");
        cb.record_failure("e2");

        // Wait and probe
        std::thread::sleep(Duration::from_millis(60));
        assert!(cb.is_available());
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Success closes the circuit
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_half_open_reopens_on_failure() {
        let cb = make_breaker(2, 2);

        // Open the circuit
        cb.record_failure("e1");
        cb.record_failure("e2");

        // Wait and probe
        std::thread::sleep(Duration::from_millis(60));
        assert!(cb.is_available());
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Failure re-opens
        cb.record_failure("probe failed");
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_reset() {
        let cb = make_breaker(2, 2);

        cb.record_failure("e1");
        cb.record_failure("e2");
        assert_eq!(cb.state(), CircuitState::Open);

        cb.reset();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.is_available());
    }

    #[test]
    fn test_half_open_limits_probes() {
        let cb = make_breaker(1, 2);

        // Open the circuit
        cb.record_failure("e1");

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(60));

        // First probe should succeed
        assert!(cb.is_available());
        // Second probe should be rejected (max 1)
        assert!(!cb.is_available());
    }

    #[test]
    fn test_failover_deduplication() {
        assert!(try_acquire_failover_switch("test_dedup"));
        assert!(!try_acquire_failover_switch("test_dedup"));

        release_failover_switch("test_dedup");
        assert!(try_acquire_failover_switch("test_dedup"));
        release_failover_switch("test_dedup");
    }

    #[test]
    fn test_provider_key_mapping() {
        use crate::settings::AiProvider;
        assert_eq!(provider_key(&AiProvider::ClaudeCli), "claude_cli");
        assert_eq!(provider_key(&AiProvider::ClaudeApi), "claude_api");
        assert_eq!(provider_key(&AiProvider::GeminiCli), "gemini_cli");
        assert_eq!(provider_key(&AiProvider::GeminiApi), "gemini_api");
    }

    // ---- Registry tests ----

    #[test]
    fn test_registry_returns_same_breaker_for_same_key() {
        // Use a unique key to avoid interference from other tests
        let key = "registry_same_key_test";
        // Record a failure through the public API so the breaker exists
        record_provider_failure(key, "first error");

        // The state should be Closed (only 1 failure, threshold is 4)
        assert_eq!(provider_state(key), CircuitState::Closed);

        // Record more failures through the same key — they accumulate
        record_provider_failure(key, "second error");
        record_provider_failure(key, "third error");
        record_provider_failure(key, "fourth error");

        // Now it should be Open (4 failures = default threshold)
        assert_eq!(provider_state(key), CircuitState::Open);
    }

    #[test]
    fn test_registry_returns_different_breaker_for_different_keys() {
        let key_a = "registry_diff_key_a";
        let key_b = "registry_diff_key_b";

        // Trip breaker A
        for _ in 0..4 {
            record_provider_failure(key_a, "error");
        }
        assert_eq!(provider_state(key_a), CircuitState::Open);

        // Breaker B should still be Closed
        assert_eq!(provider_state(key_b), CircuitState::Closed);
        assert!(is_provider_available(key_b));
    }

    // ---- Probe mechanics ----

    #[test]
    fn test_release_probe_handles_underflow() {
        let cb = make_breaker(2, 1);
        // active_probes starts at 0 — releasing should not panic or wrap
        cb.release_probe();
        assert_eq!(cb.active_probes.load(Ordering::SeqCst), 0);

        // Double release after a single acquire should also be safe
        cb.active_probes.store(1, Ordering::SeqCst);
        cb.release_probe();
        assert_eq!(cb.active_probes.load(Ordering::SeqCst), 0);
        cb.release_probe();
        assert_eq!(cb.active_probes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_half_open_multiple_successes_required() {
        // Require 3 successes to close from HalfOpen
        let cb = CircuitBreaker::new(
            "test_multi_success".to_string(),
            CircuitBreakerConfig {
                failure_threshold: 1,
                success_threshold: 3,
                open_timeout: Duration::from_millis(50),
                half_open_max_probes: 1,
            },
        );

        // Open the circuit
        cb.record_failure("boom");
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(60));

        // First probe + success: still HalfOpen (1 of 3)
        assert!(cb.is_available());
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Second probe + success: still HalfOpen (2 of 3)
        assert!(cb.is_available());
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Third probe + success: now Closed (3 of 3)
        assert!(cb.is_available());
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    // ---- Failover dedup ----

    #[test]
    fn test_failover_dedup_independent_providers() {
        let key_x = "failover_indep_x";
        let key_y = "failover_indep_y";

        assert!(try_acquire_failover_switch(key_x));
        // Different provider should still acquire independently
        assert!(try_acquire_failover_switch(key_y));
        // Same provider blocked
        assert!(!try_acquire_failover_switch(key_x));
        assert!(!try_acquire_failover_switch(key_y));

        release_failover_switch(key_x);
        release_failover_switch(key_y);
    }

    #[test]
    fn test_release_failover_switch_is_idempotent() {
        let key = "failover_idempotent";
        assert!(try_acquire_failover_switch(key));
        release_failover_switch(key);
        // Double release should not panic
        release_failover_switch(key);
        // Should be acquirable again
        assert!(try_acquire_failover_switch(key));
        release_failover_switch(key);
    }

    // ---- Broadcast ----

    #[test]
    fn test_broadcast_emits_on_state_change() {
        let mut rx = event_receiver();
        // Drain stale/lagged messages from other parallel tests
        while rx.try_recv().is_ok() {}

        let cb = make_breaker(2, 1);

        // Open the circuit (Closed -> Open)
        cb.record_failure("e1");
        cb.record_failure("e2");
        assert_eq!(cb.state(), CircuitState::Open);

        // Find our Open event (skip events from other tests)
        let mut found = false;
        for _ in 0..20 {
            match rx.try_recv() {
                Ok(ev) if ev.provider_key == "test_provider" && ev.state == "Open" => {
                    assert!(!ev.available);
                    found = true;
                    break;
                }
                Ok(_) => continue,
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
        assert!(found, "should receive Open event for test_provider");
    }

    #[test]
    fn test_broadcast_emits_on_reset() {
        let mut rx = event_receiver();
        // Drain stale/lagged messages from other parallel tests
        while rx.try_recv().is_ok() {}

        let cb = make_breaker(1, 1);

        // Open then reset
        cb.record_failure("e1");
        // Drain all until quiet
        while rx.try_recv().is_ok() {}

        cb.reset();

        let mut found = false;
        for _ in 0..20 {
            match rx.try_recv() {
                Ok(ev) if ev.provider_key == "test_provider" && ev.state == "Closed" => {
                    assert!(ev.available);
                    found = true;
                    break;
                }
                Ok(_) => continue,
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
        assert!(found, "should receive Closed event on reset");
    }

    #[test]
    fn test_broadcast_emits_half_open_and_closed() {
        // Subscribe fresh, then drain any lagged/stale messages from other tests
        let mut rx = event_receiver();
        while rx.try_recv().is_ok() {}

        let cb = make_breaker(1, 1);

        // Open the circuit
        cb.record_failure("e1");

        // Drain until we find our Open event (other tests may have emitted too)
        let mut found_open = false;
        for _ in 0..20 {
            match rx.try_recv() {
                Ok(ev) if ev.provider_key == "test_provider" && ev.state == "Open" => {
                    found_open = true;
                    break;
                }
                Ok(_) => continue, // event from another test
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
        assert!(found_open, "should receive Open event for test_provider");

        // Wait for timeout, trigger HalfOpen via is_available
        std::thread::sleep(Duration::from_millis(60));
        assert!(cb.is_available());

        let mut found_half_open = false;
        for _ in 0..20 {
            match rx.try_recv() {
                Ok(ev) if ev.provider_key == "test_provider" && ev.state == "HalfOpen" => {
                    assert!(ev.available);
                    found_half_open = true;
                    break;
                }
                Ok(_) => continue,
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
        assert!(found_half_open, "should receive HalfOpen event for test_provider");

        // Success closes the circuit
        cb.record_success();

        let mut found_closed = false;
        for _ in 0..20 {
            match rx.try_recv() {
                Ok(ev) if ev.provider_key == "test_provider" && ev.state == "Closed" => {
                    assert!(ev.available);
                    found_closed = true;
                    break;
                }
                Ok(_) => continue,
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
        assert!(found_closed, "should receive Closed event for test_provider");
    }

    // ---- Edge cases ----

    #[test]
    fn test_record_failure_in_open_state_is_noop() {
        let cb = make_breaker(2, 1);
        cb.record_failure("e1");
        cb.record_failure("e2");
        assert_eq!(cb.state(), CircuitState::Open);

        // Additional failures in Open state should not panic or change state
        cb.record_failure("e3");
        cb.record_failure("e4");
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_record_success_in_open_state_is_noop() {
        let cb = make_breaker(2, 1);
        cb.record_failure("e1");
        cb.record_failure("e2");
        assert_eq!(cb.state(), CircuitState::Open);

        // Success in Open state should not close it
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_open_not_available_before_timeout() {
        let cb = CircuitBreaker::new(
            "test_no_early_halfopen".to_string(),
            CircuitBreakerConfig {
                failure_threshold: 1,
                success_threshold: 1,
                open_timeout: Duration::from_secs(60), // very long
                half_open_max_probes: 1,
            },
        );

        cb.record_failure("e1");
        assert_eq!(cb.state(), CircuitState::Open);

        // Should still be unavailable — timeout hasn't elapsed
        assert!(!cb.is_available());
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_circuit_state_display() {
        assert_eq!(CircuitState::Closed.to_string(), "Closed");
        assert_eq!(CircuitState::Open.to_string(), "Open");
        assert_eq!(CircuitState::HalfOpen.to_string(), "HalfOpen");
    }
}
