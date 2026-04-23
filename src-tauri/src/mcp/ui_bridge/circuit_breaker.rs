//! Circuit breaker for the UI Bridge transport.
//!
//! Uses a rolling-window failure counter to prevent cascading failures when
//! the webview is unresponsive. Extracted from the original monolithic
//! ui_bridge.rs.

use tauri::Emitter;
use tracing::{error, info, warn};

/// Circuit breaker states for UI Bridge
#[derive(Debug, Clone, PartialEq)]
pub enum CircuitBreakerState {
    Closed,
    Open,
    HalfOpen,
}

/// Circuit breaker to prevent cascading failures when the webview is unresponsive.
///
/// Uses a rolling-window failure counter instead of a simple consecutive counter.
/// Failures older than `window_ms` are pruned automatically.
pub struct UiBridgeCircuitBreaker {
    state: tokio::sync::Mutex<CircuitBreakerState>,
    /// Rolling window of failure timestamps (epoch ms)
    failure_timestamps: tokio::sync::Mutex<Vec<u64>>,
    last_failure_time: std::sync::atomic::AtomicU64,
    /// Threshold: failures within the rolling window before opening
    threshold: u32,
    /// Cooldown in ms before transitioning from Open to HalfOpen
    cooldown_ms: u64,
    /// Rolling window size in ms — failures older than this are pruned
    window_ms: u64,
    /// Counts recovery attempts since last success to prevent infinite loops
    recovery_attempts: std::sync::atomic::AtomicU32,
    /// Timestamp of the last recovery attempt in ms
    last_recovery_time: std::sync::atomic::AtomicU64,
}

impl UiBridgeCircuitBreaker {
    pub fn new() -> Self {
        Self {
            state: tokio::sync::Mutex::new(CircuitBreakerState::Closed),
            failure_timestamps: tokio::sync::Mutex::new(Vec::new()),
            last_failure_time: std::sync::atomic::AtomicU64::new(0),
            threshold: 5,
            cooldown_ms: 15000,
            window_ms: 30000,
            recovery_attempts: std::sync::atomic::AtomicU32::new(0),
            last_recovery_time: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Check if a request should be allowed through
    pub async fn check(&self) -> Result<(), String> {
        let mut state = self.state.lock().await;
        match *state {
            CircuitBreakerState::Closed => Ok(()),
            CircuitBreakerState::Open => {
                let last_failure = self
                    .last_failure_time
                    .load(std::sync::atomic::Ordering::Relaxed);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                if now - last_failure >= self.cooldown_ms {
                    *state = CircuitBreakerState::HalfOpen;
                    info!("UI Bridge circuit breaker: Open -> HalfOpen (cooldown elapsed)");
                    Ok(())
                } else {
                    Err("UI Bridge temporarily unavailable (circuit breaker open)".to_string())
                }
            }
            CircuitBreakerState::HalfOpen => Ok(()),
        }
    }

    /// Record a successful request
    pub async fn record_success(&self) {
        // Clear the rolling window on success
        {
            let mut timestamps = self.failure_timestamps.lock().await;
            timestamps.clear();
        }
        self.recovery_attempts
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let mut state = self.state.lock().await;
        if *state != CircuitBreakerState::Closed {
            info!(
                "UI Bridge circuit breaker: {:?} -> Closed (success)",
                *state
            );
            *state = CircuitBreakerState::Closed;
        }
    }

    /// Record a failed request (timeout).
    ///
    /// Uses a rolling window: only failures within the last `window_ms` count
    /// towards the threshold.
    pub async fn record_failure(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.last_failure_time
            .store(now, std::sync::atomic::Ordering::Relaxed);

        let count = {
            let mut timestamps = self.failure_timestamps.lock().await;
            // Add current failure
            timestamps.push(now);
            // Prune entries older than the rolling window
            let cutoff = now.saturating_sub(self.window_ms);
            timestamps.retain(|&ts| ts >= cutoff);
            timestamps.len() as u32
        };

        if count >= self.threshold {
            let mut state = self.state.lock().await;
            if *state != CircuitBreakerState::Open {
                warn!(
                    "UI Bridge circuit breaker: {:?} -> Open ({} failures in {}s window)",
                    *state,
                    count,
                    self.window_ms / 1000
                );
                *state = CircuitBreakerState::Open;
            }
        }
    }

    /// Attempt recovery by emitting an event instead of destructively navigating.
    ///
    /// The frontend can listen for `ui-bridge-circuit-open` and show a toast or
    /// attempt reconnection without losing page state.
    pub fn attempt_recovery(&self, app_handle: &tauri::AppHandle) {
        let attempts = self
            .recovery_attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.last_recovery_time
            .store(now, std::sync::atomic::Ordering::Relaxed);

        warn!(
            "UI Bridge: Emitting circuit-open event (attempt {})",
            attempts
        );
        if let Err(e) = app_handle.emit(
            "ui-bridge-circuit-open",
            serde_json::json!({
                "recovery_attempt": attempts,
                "timestamp": now,
            }),
        ) {
            error!("UI Bridge: Failed to emit circuit-open event: {}", e);
        }
    }

    /// Manually reset the circuit breaker to Closed state.
    ///
    /// Clears failure timestamps and recovery attempt counters.
    pub async fn reset(&self) {
        {
            let mut timestamps = self.failure_timestamps.lock().await;
            timestamps.clear();
        }
        self.recovery_attempts
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.last_recovery_time
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.last_failure_time
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let mut state = self.state.lock().await;
        info!(
            "UI Bridge circuit breaker: {:?} -> Closed (manual reset)",
            *state
        );
        *state = CircuitBreakerState::Closed;
    }

    /// Get current state for diagnostics
    pub async fn get_state(&self) -> CircuitBreakerState {
        self.state.lock().await.clone()
    }

    /// Get failure count within the rolling window
    pub async fn get_failure_count(&self) -> u32 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let timestamps = self.failure_timestamps.lock().await;
        let cutoff = now.saturating_sub(self.window_ms);
        timestamps.iter().filter(|&&ts| ts >= cutoff).count() as u32
    }

    /// Get the configured failure threshold.
    pub fn get_threshold(&self) -> u32 {
        self.threshold
    }

    /// Get the configured cooldown period in milliseconds.
    pub fn get_cooldown_ms(&self) -> u64 {
        self.cooldown_ms
    }
}

impl Default for UiBridgeCircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}
