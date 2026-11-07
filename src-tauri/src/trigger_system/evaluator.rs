//! Trigger evaluation: debounce, throttle, and condition checking.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::commands::AppState;

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
    /// AppState for port-aware URL construction (optional for tests)
    app_state: Option<Arc<AppState>>,
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
            app_state: None,
        }
    }

    /// Create a new evaluator with AppState for port-aware URL construction.
    pub fn with_app_state(app_state: Arc<AppState>) -> Self {
        Self {
            last_event_times: RwLock::new(HashMap::new()),
            last_execution_times: RwLock::new(HashMap::new()),
            active_executions: RwLock::new(HashMap::new()),
            app_state: Some(app_state),
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

    /// Check if the runner is idle (no workflows running), through the shared
    /// typed `/status` reader (`crate::runner_idle`): idle only on a `data`
    /// that reads `Ready` with no AI analysis running; anything unreadable
    /// fails the condition.
    async fn check_idle_condition(&self) -> bool {
        let base = crate::runner_idle::self_status_base_url(self.app_state.as_deref());
        let verdict =
            crate::runner_idle::check_runner_idle(&base, crate::runner_idle::STATUS_PROBE_TIMEOUT)
                .await;
        crate::runner_idle::log_verdict("trigger require_idle", &verdict);
        verdict.is_idle()
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
            // Evaluator tests don't exercise workflow execution.
            workflow_id: None,
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
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
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

    // ---- `require_idle`, entered through `evaluate` against a stub /status ----
    //
    // The condition used to read `/status` at the top level, outside its `data`
    // envelope, so it never saw `Ready` and an idle-gated trigger never executed
    // (coord finding `89c8c10e`; plan
    // `2026-10-24-runner-idle-gate-reads-status-outside-its-envelope` Phase 2).
    // With no `AppState` the evaluator reaches its own API through
    // `QONTINUI_PORT`, which the test points at the stub.

    /// The `/status` writer's idle answer.
    const READY_AND_IDLE: &str =
        r#"{"success":true,"data":{"executor_state":"Ready","ai_analysis_running":false}}"#;

    /// Every answer that must fail the condition: (case, HTTP status, body).
    const NOT_IDLE: [(&str, u16, &str); 8] = [
        (
            "Ready with AI analysis running",
            200,
            r#"{"success":true,"data":{"executor_state":"Ready","ai_analysis_running":true}}"#,
        ),
        (
            "executor Running",
            200,
            r#"{"success":true,"data":{"executor_state":"Running","ai_analysis_running":false}}"#,
        ),
        (
            "executor not_started (no bridge)",
            200,
            r#"{"success":true,"data":{"executor_state":"not_started","ai_analysis_running":false}}"#,
        ),
        (
            "ai_analysis_running null",
            200,
            r#"{"success":true,"data":{"executor_state":"Ready","ai_analysis_running":null}}"#,
        ),
        (
            "ai_analysis_running absent",
            200,
            r#"{"success":true,"data":{"executor_state":"Ready"}}"#,
        ),
        (
            "success: false",
            200,
            r#"{"success":false,"error":"Internal error"}"#,
        ),
        ("HTTP 500 with an idle body", 500, READY_AND_IDLE),
        (
            "the old un-enveloped idle shape",
            200,
            r#"{"executor_state":"Ready","ai_analysis_running":false}"#,
        ),
    ];

    /// What the stub `/status` answers: (HTTP status, body). Swapped per case.
    type StubReply = Arc<std::sync::Mutex<(u16, &'static str)>>;

    /// A `/status` stub on an ephemeral loopback port. Returns its port and
    /// the handle that sets its answer.
    async fn spawn_status_stub() -> (u16, StubReply) {
        use axum::{extract::State, http::StatusCode, routing::get, Router};

        async fn status(State(reply): State<StubReply>) -> (StatusCode, &'static str) {
            let (code, body) = *reply.lock().unwrap();
            (StatusCode::from_u16(code).unwrap(), body)
        }

        let reply: StubReply = Arc::new(std::sync::Mutex::new((200, "")));
        let app = Router::new()
            .route("/status", get(status))
            .with_state(reply.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (port, reply)
    }

    /// A loopback port that refuses connections for as long as the returned
    /// socket lives: bound, never listening, so no parallel test can take it.
    fn closed_port() -> (u16, tokio::net::TcpSocket) {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let port = socket.local_addr().unwrap().port();
        (port, socket)
    }

    /// An enabled webhook trigger with no debounce and no cooldown whose
    /// conditions are `require_idle` followed by `extra`.
    fn idle_gated_trigger(extra: Option<TriggerCondition>) -> WorkflowTrigger {
        let mut trigger = make_trigger("idle-gated", 0, 0, 10);
        trigger.conditions = vec![TriggerCondition {
            condition_type: "require_idle".to_string(),
            config: serde_json::json!({}),
        }];
        trigger.conditions.extend(extra);
        trigger
    }

    /// One test walks the whole matrix so `QONTINUI_PORT` is mutated, under
    /// the env lock, for one short window. The idle case is FIRST: it is the
    /// one the envelope defect starved. Each evaluation uses a fresh evaluator,
    /// so no debounce or cooldown state carries between cases.
    #[tokio::test]
    async fn trigger_idle_gate_fires_only_when_status_data_reads_ready_and_idle() {
        let _lock = crate::test_env::env_lock();
        let _env = crate::test_env::EnvVarRestore::capture(&["QONTINUI_PORT"]);
        let (port, reply) = spawn_status_stub().await;
        std::env::set_var("QONTINUI_PORT", port.to_string());

        let trigger = idle_gated_trigger(None);
        let event = make_event(&trigger.id);
        let not_met = "Condition 'require_idle' not met";

        // Idle executes — twice, so the verdict is stable rather than a first-read fluke.
        *reply.lock().unwrap() = (200, READY_AND_IDLE);
        for round in 1..=2 {
            let result =
                TriggerEvaluator::evaluate(&TriggerEvaluator::new(), &trigger, &event).await;
            assert!(
                matches!(result, EvalResult::Execute),
                "round {round}: /status data reads Ready with no AI analysis, so the event must execute; got {result:?}"
            );
        }

        for (case, code, body) in NOT_IDLE {
            *reply.lock().unwrap() = (code, body);
            match TriggerEvaluator::evaluate(&TriggerEvaluator::new(), &trigger, &event).await {
                EvalResult::ConditionFailed { reason } => assert_eq!(reason, not_met, "{case}"),
                other => panic!("{case}: the event must not execute, got {other:?}"),
            }
        }

        // The conditions stay a conjunction: idle, but outside a time window
        // that admits no hour, does not execute — and it is the window that fails.
        *reply.lock().unwrap() = (200, READY_AND_IDLE);
        let windowed = idle_gated_trigger(Some(TriggerCondition {
            condition_type: "time_window".to_string(),
            config: serde_json::json!({"timezone": "UTC", "start_hour": 0, "end_hour": 0}),
        }));
        match TriggerEvaluator::evaluate(&TriggerEvaluator::new(), &windowed, &event).await {
            EvalResult::ConditionFailed { reason } => {
                assert_eq!(reason, "Condition 'time_window' not met")
            }
            other => panic!("idle but outside the time window must not execute, got {other:?}"),
        }

        // Nothing listening: an unreachable /status is not idle either.
        let (closed, _held) = closed_port();
        std::env::set_var("QONTINUI_PORT", closed.to_string());
        match TriggerEvaluator::evaluate(&TriggerEvaluator::new(), &trigger, &event).await {
            EvalResult::ConditionFailed { reason } => assert_eq!(reason, not_met, "closed port"),
            other => panic!("closed port: the event must not execute, got {other:?}"),
        }
    }
}
