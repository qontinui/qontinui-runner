//! Trigger evaluation: debounce, throttle, and condition checking.

use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use super::types::{TriggerCondition, TriggerEvent, WorkflowTrigger};

/// Maximum allowed chain depth to prevent circular trigger chains.
const MAX_CHAIN_DEPTH: u32 = 5;

/// Tracks debounce and throttle state for all triggers.
pub struct TriggerEvaluator {
    /// Last event timestamp per trigger (for debounce)
    last_event_times: RwLock<HashMap<String, std::time::Instant>>,
    /// Last execution timestamp per trigger (for cooldown)
    last_execution_times: RwLock<HashMap<String, std::time::Instant>>,
    /// Currently running execution count per trigger
    active_executions: RwLock<HashMap<String, u32>>,
}

/// Result of evaluating a trigger event.
#[derive(Debug, Clone)]
pub enum EvalResult {
    /// Event should trigger workflow execution
    Execute,
    /// Event was debounced (too soon after last event)
    Debounced,
    /// Event was throttled (cooldown not elapsed or max concurrent reached)
    Throttled { reason: String },
    /// Conditions not met
    ConditionFailed { reason: String },
    /// Chain depth exceeded
    ChainDepthExceeded,
    /// Trigger is disabled
    Disabled,
}

impl EvalResult {
    pub fn action_name(&self) -> &str {
        match self {
            EvalResult::Execute => "executed",
            EvalResult::Debounced => "debounced",
            EvalResult::Throttled { .. } => "throttled",
            EvalResult::ConditionFailed { .. } => "condition_failed",
            EvalResult::ChainDepthExceeded => "chain_depth_exceeded",
            EvalResult::Disabled => "disabled",
        }
    }

    pub fn should_execute(&self) -> bool {
        matches!(self, EvalResult::Execute)
    }
}

impl TriggerEvaluator {
    pub fn new() -> Self {
        Self {
            last_event_times: RwLock::new(HashMap::new()),
            last_execution_times: RwLock::new(HashMap::new()),
            active_executions: RwLock::new(HashMap::new()),
        }
    }

    /// Evaluate whether a trigger event should result in workflow execution.
    pub async fn evaluate(&self, trigger: &WorkflowTrigger, event: &TriggerEvent) -> EvalResult {
        // 1. Check enabled
        if !trigger.enabled {
            return EvalResult::Disabled;
        }

        // 2. Check chain depth
        if event.chain_depth > MAX_CHAIN_DEPTH {
            warn!(
                "Chain depth {} exceeded max {} for trigger '{}'",
                event.chain_depth, MAX_CHAIN_DEPTH, trigger.name
            );
            return EvalResult::ChainDepthExceeded;
        }

        // 3. Check debounce
        {
            let mut last_events = self.last_event_times.write().await;
            let now = std::time::Instant::now();

            if let Some(last_time) = last_events.get(&trigger.id) {
                let elapsed_ms = now.duration_since(*last_time).as_millis() as u64;
                if elapsed_ms < trigger.debounce_ms {
                    debug!(
                        "Trigger '{}' debounced ({}ms < {}ms window)",
                        trigger.name, elapsed_ms, trigger.debounce_ms
                    );
                    // Update the time so the debounce window resets
                    last_events.insert(trigger.id.clone(), now);
                    return EvalResult::Debounced;
                }
            }

            last_events.insert(trigger.id.clone(), now);
        }

        // 4. Check cooldown
        {
            let last_execs = self.last_execution_times.read().await;
            if let Some(last_exec) = last_execs.get(&trigger.id) {
                let elapsed_secs = last_exec.elapsed().as_secs();
                if elapsed_secs < trigger.cooldown_seconds {
                    let remaining = trigger.cooldown_seconds - elapsed_secs;
                    info!(
                        "Trigger '{}' throttled: {}s remaining in cooldown",
                        trigger.name, remaining
                    );
                    return EvalResult::Throttled {
                        reason: format!("Cooldown: {}s remaining", remaining),
                    };
                }
            }
        }

        // 5. Check max concurrent
        {
            let active = self.active_executions.read().await;
            let count = active.get(&trigger.id).copied().unwrap_or(0);
            if count >= trigger.max_concurrent {
                info!(
                    "Trigger '{}' throttled: {} active (max {})",
                    trigger.name, count, trigger.max_concurrent
                );
                return EvalResult::Throttled {
                    reason: format!(
                        "Max concurrent: {} active (limit {})",
                        count, trigger.max_concurrent
                    ),
                };
            }
        }

        // 6. Check conditions
        for condition in &trigger.conditions {
            if !self.check_condition(condition, event).await {
                return EvalResult::ConditionFailed {
                    reason: format!("Condition '{}' not met", condition.condition_type),
                };
            }
        }

        EvalResult::Execute
    }

    /// Record that an execution started (for concurrent tracking).
    pub async fn record_execution_start(&self, trigger_id: &str) {
        let mut active = self.active_executions.write().await;
        let count = active.entry(trigger_id.to_string()).or_insert(0);
        *count += 1;

        let mut last_execs = self.last_execution_times.write().await;
        last_execs.insert(trigger_id.to_string(), std::time::Instant::now());
    }

    /// Record that an execution completed.
    pub async fn record_execution_end(&self, trigger_id: &str) {
        let mut active = self.active_executions.write().await;
        if let Some(count) = active.get_mut(trigger_id) {
            *count = count.saturating_sub(1);
        }
    }

    /// Get active execution count for a trigger.
    pub async fn get_active_count(&self, trigger_id: &str) -> u32 {
        let active = self.active_executions.read().await;
        active.get(trigger_id).copied().unwrap_or(0)
    }

    /// Get total active executions across all triggers.
    pub async fn get_total_active(&self) -> u64 {
        let active = self.active_executions.read().await;
        active.values().map(|v| *v as u64).sum()
    }

    /// Clear all evaluator state for a trigger (debounce, cooldown, active count).
    pub async fn clear_trigger_state(&self, trigger_id: &str) {
        let mut last_events = self.last_event_times.write().await;
        last_events.remove(trigger_id);

        let mut last_execs = self.last_execution_times.write().await;
        last_execs.remove(trigger_id);

        let mut active = self.active_executions.write().await;
        active.remove(trigger_id);
    }

    /// Check a single condition.
    async fn check_condition(&self, condition: &TriggerCondition, event: &TriggerEvent) -> bool {
        match condition.condition_type.as_str() {
            "require_idle" => self.check_idle_condition().await,
            "branch_filter" => {
                if let Some(pattern) = condition.config.as_str() {
                    if let Some(branch) = event.variables.get("branch") {
                        match regex::Regex::new(pattern) {
                            Ok(re) => re.is_match(branch),
                            Err(e) => {
                                warn!("Invalid branch filter regex '{}': {}", pattern, e);
                                false
                            }
                        }
                    } else {
                        // No branch variable -- condition passes (not applicable)
                        true
                    }
                } else {
                    true
                }
            }
            "time_window" => self.check_time_window_condition(condition),
            _ => {
                debug!(
                    "Unknown condition type '{}', passing",
                    condition.condition_type
                );
                true
            }
        }
    }

    /// Check if the runner is idle (no workflows running).
    async fn check_idle_condition(&self) -> bool {
        let client = reqwest::Client::new();
        let status_url = format!("{}/status", crate::mcp::types::get_self_base_url_from_env());
        match client
            .get(&status_url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    let executor_state = json
                        .get("executor_state")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let ai_running = json
                        .get("ai_analysis_running")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    executor_state == "Ready" && !ai_running
                } else {
                    false
                }
            }
            Err(e) => {
                debug!("Idle condition check failed: {}", e);
                false
            }
        }
    }

    /// Check a time window condition.
    ///
    /// Config format: `{"days": [1,2,3,4,5], "start_hour": 9, "end_hour": 17, "timezone": "Local"}`
    /// - days: 0=Sunday, 1=Monday, ..., 6=Saturday (defaults to all days)
    /// - start_hour: start of window (inclusive, 0-23, defaults to 0)
    /// - end_hour: end of window (exclusive, 0-23, defaults to 24)
    /// - timezone: "UTC" or "Local" (defaults to "Local")
    fn check_time_window_condition(&self, condition: &TriggerCondition) -> bool {
        let config = &condition.config;

        let use_utc = config
            .get("timezone")
            .and_then(|v| v.as_str())
            .map(|s| s.eq_ignore_ascii_case("utc"))
            .unwrap_or(false);

        let (hour, weekday_number) = if use_utc {
            let now = chrono::Utc::now();
            (
                chrono::Timelike::hour(&now),
                chrono::Datelike::weekday(&now).num_days_from_sunday(),
            )
        } else {
            let now = chrono::Local::now();
            (
                chrono::Timelike::hour(&now),
                chrono::Datelike::weekday(&now).num_days_from_sunday(),
            )
        };

        // Check day-of-week filter
        if let Some(days) = config.get("days").and_then(|v| v.as_array()) {
            let allowed_days: Vec<u32> = days
                .iter()
                .filter_map(|d| d.as_u64().map(|n| n as u32))
                .collect();
            if !allowed_days.is_empty() && !allowed_days.contains(&weekday_number) {
                debug!(
                    "Time window: weekday {} not in allowed days {:?}",
                    weekday_number, allowed_days
                );
                return false;
            }
        }

        // Check hour range
        let start_hour = config
            .get("start_hour")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let end_hour = config
            .get("end_hour")
            .and_then(|v| v.as_u64())
            .unwrap_or(24) as u32;

        if hour < start_hour || hour >= end_hour {
            debug!(
                "Time window: hour {} outside [{}, {})",
                hour, start_hour, end_hour
            );
            return false;
        }

        true
    }
}

impl Default for TriggerEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_trigger(
        id: &str,
        debounce_ms: u64,
        cooldown: u64,
        max_concurrent: u32,
    ) -> WorkflowTrigger {
        WorkflowTrigger {
            id: id.to_string(),
            name: format!("Test {}", id),
            description: None,
            trigger_type: "webhook".to_string(),
            trigger_config: super::super::types::TriggerConfig::Webhook {
                secret: None,
                payload_filter: None,
                variable_mapping: HashMap::new(),
            },
            workflow_id: "wf-1".to_string(),
            workflow_overrides: None,
            conditions: vec![],
            debounce_ms,
            cooldown_seconds: cooldown,
            max_concurrent,
            retry_count: 0,
            retry_delay_seconds: 30,
            enabled: true,
            last_triggered_at: None,
            last_execution_id: None,
            trigger_count: 0,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn make_event(trigger_id: &str) -> TriggerEvent {
        TriggerEvent {
            trigger_id: trigger_id.to_string(),
            event_type: "test".to_string(),
            event_data: serde_json::Value::Null,
            variables: HashMap::new(),
            chain_depth: 0,
        }
    }

    #[tokio::test]
    async fn test_disabled_trigger() {
        let eval = TriggerEvaluator::new();
        let mut trigger = make_trigger("t1", 0, 0, 10);
        trigger.enabled = false;
        let event = make_event("t1");

        let result = eval.evaluate(&trigger, &event).await;
        assert!(!result.should_execute());
        assert_eq!(result.action_name(), "disabled");
    }

    #[tokio::test]
    async fn test_chain_depth_exceeded() {
        let eval = TriggerEvaluator::new();
        let trigger = make_trigger("t1", 0, 0, 10);
        let mut event = make_event("t1");
        event.chain_depth = 6;

        let result = eval.evaluate(&trigger, &event).await;
        assert!(!result.should_execute());
    }

    #[tokio::test]
    async fn test_max_concurrent_throttle() {
        let eval = TriggerEvaluator::new();
        let trigger = make_trigger("t1", 0, 0, 1);
        let event = make_event("t1");

        // First execution should pass
        let result = eval.evaluate(&trigger, &event).await;
        assert!(result.should_execute());

        // Record execution start
        eval.record_execution_start("t1").await;

        // Second should be throttled (max concurrent = 1)
        // Need fresh debounce: wait or use 0 debounce
        let trigger_no_cooldown = make_trigger("t1", 0, 0, 1);
        let result = eval.evaluate(&trigger_no_cooldown, &event).await;
        assert!(!result.should_execute());

        // Record execution end
        eval.record_execution_end("t1").await;
        assert_eq!(eval.get_active_count("t1").await, 0);
    }
}
