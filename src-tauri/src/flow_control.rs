//! Declarative per-workflow flow control (Inngest-inspired).
//!
//! Provides configurable flow control primitives that can be attached to
//! individual workflow definitions:
//!
//! - **Concurrency**: Limit concurrent runs, optionally scoped by key
//! - **Throttle**: Limit throughput over time (e.g., 3 runs per minute)
//! - **Debounce**: Sliding window deduplication (only run the latest)
//! - **Rate Limit**: Hard cap that drops excess requests
//! - **Priority**: Dynamic priority based on workflow context
//!
//! # Usage
//!
//! Flow control is configured per-workflow in `unified_workflows` and
//! enforced by the `WorkflowQueue` on dequeue.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info};

// =============================================================================
// Flow Control Configuration
// =============================================================================

/// Flow control settings for a workflow definition.
///
/// All fields are optional — if unset, no flow control is applied for
/// that dimension.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlowControl {
    /// Concurrency limit: max simultaneous runs of this workflow.
    pub concurrency: Option<ConcurrencyLimit>,
    /// Throttle: max N runs within a time window.
    pub throttle: Option<ThrottleConfig>,
    /// Debounce: sliding window, only execute the latest trigger.
    pub debounce: Option<DebounceConfig>,
    /// Rate limit: hard cap, skip excess (unlike throttle which queues).
    pub rate_limit: Option<RateLimitConfig>,
    /// Priority configuration for dynamic queue ordering.
    pub priority: Option<PriorityConfig>,
}

/// Concurrency limit configuration.
///
/// Inspired by Inngest's `concurrency` option which can scope limits by key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurrencyLimit {
    /// Maximum concurrent executions.
    pub limit: u32,
    /// Optional key expression to scope concurrency.
    /// e.g., `"workflow_id"` means per-workflow, `"repo_path"` means per-repo.
    pub key: Option<String>,
}

/// Throttle configuration: limits throughput over time.
///
/// Unlike rate limiting, throttled requests queue and wait rather than being dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThrottleConfig {
    /// Maximum number of runs allowed in the period.
    pub limit: u32,
    /// Time window in seconds.
    pub period_seconds: u64,
    /// Optional scoping key (like concurrency).
    pub key: Option<String>,
}

/// Debounce configuration: sliding window deduplication.
///
/// Only the latest trigger within the window actually executes.
/// Earlier triggers in the same window are cancelled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebounceConfig {
    /// Debounce window in milliseconds.
    pub window_ms: u64,
    /// Optional key to scope debounce (e.g., per file path).
    pub key: Option<String>,
}

/// Rate limit configuration: hard cap that drops excess.
///
/// Unlike throttle, rate-limited requests are immediately rejected/dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Maximum number of runs allowed in the period.
    pub limit: u32,
    /// Time window in seconds.
    pub period_seconds: u64,
    /// Optional scoping key.
    pub key: Option<String>,
}

/// Priority configuration for dynamic queue ordering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorityConfig {
    /// Static priority offset (-100 to 100). Higher = more priority.
    pub base_priority: i32,
}

// =============================================================================
// Flow Control Enforcer
// =============================================================================

/// Enforces flow control policies at dequeue time.
///
/// Maintains runtime state for throttle windows, concurrency counts,
/// debounce timers, and rate limit buckets.
pub struct FlowControlEnforcer {
    /// Per-key concurrency counters: (workflow_id|scope_key) → current_count.
    concurrency_counts: Mutex<HashMap<String, u32>>,
    /// Per-key throttle windows: (workflow_id|scope_key) → Vec<timestamp_ms>.
    throttle_windows: Mutex<HashMap<String, Vec<u64>>>,
    /// Per-key rate limit windows: (workflow_id|scope_key) → Vec<timestamp_ms>.
    rate_limit_windows: Mutex<HashMap<String, Vec<u64>>>,
    /// Per-key debounce timers: (workflow_id|scope_key) → latest_trigger_ms.
    debounce_timers: Mutex<HashMap<String, u64>>,
}

/// Result of a flow control check.
#[derive(Debug, Clone)]
pub enum FlowControlDecision {
    /// Proceed with execution.
    Allow,
    /// Wait and retry later (throttled).
    Wait {
        /// How long to wait in milliseconds.
        wait_ms: u64,
        reason: String,
    },
    /// Skip this execution entirely (rate limited or debounced).
    Skip { reason: String },
}

impl FlowControlEnforcer {
    /// Create a new flow control enforcer.
    pub fn new() -> Self {
        Self {
            concurrency_counts: Mutex::new(HashMap::new()),
            throttle_windows: Mutex::new(HashMap::new()),
            rate_limit_windows: Mutex::new(HashMap::new()),
            debounce_timers: Mutex::new(HashMap::new()),
        }
    }

    /// Check if a workflow is allowed to execute under its flow control rules.
    pub async fn check(
        &self,
        workflow_id: &str,
        flow_control: &FlowControl,
    ) -> FlowControlDecision {
        let now_ms = current_time_ms();

        // 1. Check concurrency limit
        if let Some(ref concurrency) = flow_control.concurrency {
            let key = self.make_key(workflow_id, concurrency.key.as_deref());
            let counts = self.concurrency_counts.lock().await;
            let current = counts.get(&key).copied().unwrap_or(0);
            if current >= concurrency.limit {
                return FlowControlDecision::Wait {
                    wait_ms: 1000, // Check again in 1s
                    reason: format!(
                        "Concurrency limit reached ({}/{}) for key '{}'",
                        current, concurrency.limit, key
                    ),
                };
            }
        }

        // 2. Check rate limit (hard cap — skip if exceeded)
        if let Some(ref rate_limit) = flow_control.rate_limit {
            let key = self.make_key(workflow_id, rate_limit.key.as_deref());
            let mut windows = self.rate_limit_windows.lock().await;
            let window = windows.entry(key.clone()).or_default();
            let window_start = now_ms.saturating_sub(rate_limit.period_seconds * 1000);
            window.retain(|&ts| ts >= window_start);

            if window.len() >= rate_limit.limit as usize {
                return FlowControlDecision::Skip {
                    reason: format!(
                        "Rate limit exceeded ({}/{} in {}s) for key '{}'",
                        window.len(),
                        rate_limit.limit,
                        rate_limit.period_seconds,
                        key
                    ),
                };
            }
            window.push(now_ms);
        }

        // 3. Check throttle (queue if exceeded)
        if let Some(ref throttle) = flow_control.throttle {
            let key = self.make_key(workflow_id, throttle.key.as_deref());
            let mut windows = self.throttle_windows.lock().await;
            let window = windows.entry(key.clone()).or_default();
            let window_start = now_ms.saturating_sub(throttle.period_seconds * 1000);
            window.retain(|&ts| ts >= window_start);

            if window.len() >= throttle.limit as usize {
                // Calculate how long until the oldest entry falls out of the window
                if let Some(&oldest) = window.first() {
                    let wait_until = oldest + throttle.period_seconds * 1000;
                    let wait_ms = wait_until.saturating_sub(now_ms);
                    return FlowControlDecision::Wait {
                        wait_ms,
                        reason: format!(
                            "Throttled ({}/{} in {}s) for key '{}', wait {}ms",
                            window.len(),
                            throttle.limit,
                            throttle.period_seconds,
                            key,
                            wait_ms
                        ),
                    };
                }
            }
            window.push(now_ms);
        }

        // 4. Check debounce
        if let Some(ref debounce) = flow_control.debounce {
            let key = self.make_key(workflow_id, debounce.key.as_deref());
            let timers = self.debounce_timers.lock().await;
            if let Some(&last_trigger) = timers.get(&key) {
                if now_ms - last_trigger < debounce.window_ms {
                    return FlowControlDecision::Skip {
                        reason: format!(
                            "Debounced (within {}ms window) for key '{}'",
                            debounce.window_ms, key
                        ),
                    };
                }
            }
        }

        FlowControlDecision::Allow
    }

    /// Record that a workflow started executing (for concurrency tracking).
    pub async fn record_start(&self, workflow_id: &str, concurrency_key: Option<&str>) {
        let key = self.make_key(workflow_id, concurrency_key);
        let mut counts = self.concurrency_counts.lock().await;
        *counts.entry(key.clone()).or_insert(0) += 1;
        debug!("Flow control: concurrency +1 for '{}'", key);
    }

    /// Record that a workflow finished executing (for concurrency tracking).
    pub async fn record_end(&self, workflow_id: &str, concurrency_key: Option<&str>) {
        let key = self.make_key(workflow_id, concurrency_key);
        let mut counts = self.concurrency_counts.lock().await;
        if let Some(count) = counts.get_mut(&key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&key);
            }
            debug!("Flow control: concurrency -1 for '{}'", key);
        }
    }

    /// Record a debounce trigger timestamp.
    pub async fn record_debounce_trigger(&self, workflow_id: &str, debounce_key: Option<&str>) {
        let key = self.make_key(workflow_id, debounce_key);
        let mut timers = self.debounce_timers.lock().await;
        timers.insert(key, current_time_ms());
    }

    /// Build a scoping key from workflow_id and optional scope key.
    fn make_key(&self, workflow_id: &str, scope_key: Option<&str>) -> String {
        match scope_key {
            Some(key) => format!("{}:{}", workflow_id, key),
            None => workflow_id.to_string(),
        }
    }
}

// =============================================================================
// Helpers
// =============================================================================

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// =============================================================================
// Global Singleton
// =============================================================================

use once_cell::sync::Lazy;

static FLOW_CONTROL_ENFORCER: Lazy<Arc<FlowControlEnforcer>> =
    Lazy::new(|| Arc::new(FlowControlEnforcer::new()));

/// Get the global flow control enforcer.
pub fn get_flow_control_enforcer() -> &'static Arc<FlowControlEnforcer> {
    &FLOW_CONTROL_ENFORCER
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_no_flow_control_allows() {
        let enforcer = FlowControlEnforcer::new();
        let fc = FlowControl::default();

        let decision = enforcer.check("wf-1", &fc).await;
        assert!(matches!(decision, FlowControlDecision::Allow));
    }

    #[tokio::test]
    async fn test_concurrency_limit() {
        let enforcer = FlowControlEnforcer::new();
        let fc = FlowControl {
            concurrency: Some(ConcurrencyLimit {
                limit: 1,
                key: None,
            }),
            ..Default::default()
        };

        // First should be allowed
        let d1 = enforcer.check("wf-1", &fc).await;
        assert!(matches!(d1, FlowControlDecision::Allow));

        // Record a start
        enforcer.record_start("wf-1", None).await;

        // Second should be throttled
        let d2 = enforcer.check("wf-1", &fc).await;
        assert!(matches!(d2, FlowControlDecision::Wait { .. }));

        // After end, should be allowed again
        enforcer.record_end("wf-1", None).await;
        let d3 = enforcer.check("wf-1", &fc).await;
        assert!(matches!(d3, FlowControlDecision::Allow));
    }

    #[tokio::test]
    async fn test_rate_limit_skip() {
        let enforcer = FlowControlEnforcer::new();
        let fc = FlowControl {
            rate_limit: Some(RateLimitConfig {
                limit: 2,
                period_seconds: 60,
                key: None,
            }),
            ..Default::default()
        };

        // First two should be allowed (they also record in the window)
        let d1 = enforcer.check("wf-1", &fc).await;
        assert!(matches!(d1, FlowControlDecision::Allow));
        let d2 = enforcer.check("wf-1", &fc).await;
        assert!(matches!(d2, FlowControlDecision::Allow));

        // Third should be skipped
        let d3 = enforcer.check("wf-1", &fc).await;
        assert!(matches!(d3, FlowControlDecision::Skip { .. }));
    }

    #[tokio::test]
    async fn test_throttle_wait() {
        let enforcer = FlowControlEnforcer::new();
        let fc = FlowControl {
            throttle: Some(ThrottleConfig {
                limit: 1,
                period_seconds: 60,
                key: None,
            }),
            ..Default::default()
        };

        // First should be allowed (and recorded)
        let d1 = enforcer.check("wf-1", &fc).await;
        assert!(matches!(d1, FlowControlDecision::Allow));

        // Second should wait
        let d2 = enforcer.check("wf-1", &fc).await;
        assert!(matches!(d2, FlowControlDecision::Wait { .. }));
    }

    #[tokio::test]
    async fn test_scoped_concurrency() {
        let enforcer = FlowControlEnforcer::new();
        let fc = FlowControl {
            concurrency: Some(ConcurrencyLimit {
                limit: 1,
                key: Some("repo".to_string()),
            }),
            ..Default::default()
        };

        enforcer.record_start("wf-1", Some("repo")).await;

        // Same workflow + same key → blocked
        let d1 = enforcer.check("wf-1", &fc).await;
        assert!(matches!(d1, FlowControlDecision::Wait { .. }));

        // Different workflow_id but checking key = "repo" won't block
        // because make_key produces "wf-2:repo" vs "wf-1:repo"
        let d2 = enforcer.check("wf-2", &fc).await;
        assert!(matches!(d2, FlowControlDecision::Allow));
    }
}
