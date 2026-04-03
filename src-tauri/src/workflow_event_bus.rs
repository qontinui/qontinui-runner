//! Workflow event bus for inter-workflow communication (Inngest-inspired).
//!
//! Provides an in-process pub/sub system that enables:
//! - **Event-driven chaining**: Workflow completion/failure triggers downstream workflows
//! - **Fan-out**: One event can trigger multiple independent workflows
//! - **waitForEvent**: A running workflow can pause until a specific event arrives
//! - **Decoupled orchestration**: Workflows don't need to know about each other
//!
//! # Architecture
//!
//! The event bus is a singleton (`get_workflow_event_bus()`) that manages:
//! - **Subscriptions**: Registered handlers that fire when matching events arrive
//! - **Waiters**: One-shot channels for workflows blocked on `waitForEvent`
//! - **History**: Recent events for debugging and late-subscriber replay
//!
//! # Event Types
//!
//! Events follow a dotted naming convention (like Inngest):
//! - `workflow.completed` — A workflow finished successfully
//! - `workflow.failed` — A workflow failed
//! - `workflow.stopped` — A workflow was stopped by user
//! - `step.completed` — A workflow step/phase completed
//! - `verification.passed` — Verification checks passed
//! - `verification.failed` — Verification checks failed
//! - `trigger.fired` — A trigger condition was met
//! - Custom events via `emit("app/custom.event", data)`

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};
use tracing::{debug, info, warn};

// =============================================================================
// Event Types
// =============================================================================

/// A workflow event that can be emitted and subscribed to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEvent {
    /// Event name using dotted convention (e.g., "workflow.completed").
    pub name: String,
    /// Structured event data.
    pub data: serde_json::Value,
    /// ISO-8601 timestamp of when the event was emitted.
    pub timestamp: String,
    /// Optional idempotency key for deduplication.
    pub idempotency_key: Option<String>,
    /// Source context: which workflow/task produced this event.
    pub source: EventSource,
}

/// Identifies the source of an event.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EventSource {
    /// Task run ID that produced the event (if any).
    pub task_run_id: Option<String>,
    /// Workflow ID that produced the event (if any).
    pub workflow_id: Option<String>,
    /// Workflow name for logging.
    pub workflow_name: Option<String>,
}

/// A subscription that triggers when a matching event is emitted.
#[derive(Debug, Clone)]
pub struct EventSubscription {
    /// Unique subscription ID.
    pub id: String,
    /// Event name pattern to match. Supports exact match and prefix glob:
    /// - `"workflow.completed"` — exact match
    /// - `"workflow.*"` — matches any event starting with "workflow."
    /// - `"*"` — matches all events
    pub event_pattern: String,
    /// Optional condition expression (JSONPath-like) on event data.
    /// e.g., `"data.workflow_id == 'abc'"` — not yet implemented, reserved.
    pub condition: Option<String>,
    /// Workflow ID to trigger when the event matches.
    pub target_workflow_id: Option<String>,
    /// Whether this subscription fires once and then removes itself.
    pub once: bool,
}

/// Result of emitting an event: how many subscribers were notified.
#[derive(Debug)]
pub struct EmitResult {
    /// Number of broadcast subscribers that received the event.
    pub broadcast_count: usize,
    /// Number of one-shot waiters that were woken.
    pub waiters_woken: usize,
    /// Subscription IDs that matched (for trigger execution).
    pub matched_subscriptions: Vec<String>,
}

// =============================================================================
// Workflow Event Bus
// =============================================================================

/// In-process pub/sub event bus for workflow orchestration.
pub struct WorkflowEventBus {
    /// Broadcast channel for all events (subscribers get all events and filter).
    broadcast_tx: broadcast::Sender<WorkflowEvent>,
    /// Registered subscriptions.
    subscriptions: RwLock<Vec<EventSubscription>>,
    /// One-shot waiters: event_pattern → list of (waiter_id, oneshot_sender).
    waiters: Mutex<HashMap<String, Vec<(String, tokio::sync::oneshot::Sender<WorkflowEvent>)>>>,
    /// Recent event history for debugging (bounded circular buffer).
    history: Mutex<std::collections::VecDeque<WorkflowEvent>>,
    /// Maximum history size.
    max_history: usize,
}

impl WorkflowEventBus {
    /// Create a new event bus.
    pub fn new() -> Self {
        let (broadcast_tx, _) = broadcast::channel(256);
        info!("Workflow event bus initialized");
        Self {
            broadcast_tx,
            subscriptions: RwLock::new(Vec::new()),
            waiters: Mutex::new(HashMap::new()),
            history: Mutex::new(std::collections::VecDeque::new()),
            max_history: 500,
        }
    }

    /// Emit an event to all matching subscribers and waiters.
    pub async fn emit(&self, event: WorkflowEvent) -> EmitResult {
        let event_name = event.name.clone();
        debug!("Event bus: emitting '{}'", event_name);

        // 1. Add to history
        {
            let mut history = self.history.lock().await;
            if history.len() >= self.max_history {
                history.pop_front();
            }
            history.push_back(event.clone());
        }

        // 2. Broadcast to all subscribers
        let broadcast_count = self.broadcast_tx.send(event.clone()).unwrap_or(0);

        // 3. Wake any one-shot waiters matching this event
        let waiters_woken = self.wake_waiters(&event).await;

        // 4. Find matching subscriptions and remove once-only matches atomically
        let matched_subscriptions = {
            let mut subs = self.subscriptions.write().await;
            let matched: Vec<String> = subs
                .iter()
                .filter(|s| event_matches(&event_name, &s.event_pattern))
                .map(|s| s.id.clone())
                .collect();
            // Remove once-only subscriptions that matched
            if !matched.is_empty() {
                subs.retain(|s| !s.once || !matched.contains(&s.id));
            }
            matched
        };

        info!(
            "Event '{}': {} broadcast, {} waiters woken, {} subscriptions matched",
            event_name,
            broadcast_count,
            waiters_woken,
            matched_subscriptions.len()
        );

        EmitResult {
            broadcast_count,
            waiters_woken,
            matched_subscriptions,
        }
    }

    /// Convenience: emit a workflow lifecycle event.
    pub async fn emit_workflow_event(
        &self,
        event_name: &str,
        task_run_id: &str,
        workflow_id: Option<&str>,
        workflow_name: Option<&str>,
        data: serde_json::Value,
    ) -> EmitResult {
        let event = WorkflowEvent {
            name: event_name.to_string(),
            data,
            timestamp: chrono::Utc::now().to_rfc3339(),
            idempotency_key: Some(format!("{}:{}", event_name, task_run_id)),
            source: EventSource {
                task_run_id: Some(task_run_id.to_string()),
                workflow_id: workflow_id.map(|s| s.to_string()),
                workflow_name: workflow_name.map(|s| s.to_string()),
            },
        };
        self.emit(event).await
    }

    /// Register a subscription.
    pub async fn subscribe(&self, subscription: EventSubscription) {
        info!(
            "Event bus: new subscription '{}' for pattern '{}'",
            subscription.id, subscription.event_pattern
        );
        let mut subs = self.subscriptions.write().await;
        subs.push(subscription);
    }

    /// Remove a subscription by ID.
    pub async fn unsubscribe(&self, subscription_id: &str) -> bool {
        let mut subs = self.subscriptions.write().await;
        let initial_len = subs.len();
        subs.retain(|s| s.id != subscription_id);
        let removed = subs.len() < initial_len;
        if removed {
            info!("Event bus: removed subscription '{}'", subscription_id);
        }
        removed
    }

    /// Wait for a specific event, with timeout.
    ///
    /// Returns the matching event if one arrives before the timeout, or None.
    /// This is the Inngest `step.waitForEvent()` equivalent.
    pub async fn wait_for_event(
        &self,
        waiter_id: &str,
        event_pattern: &str,
        timeout: std::time::Duration,
    ) -> Option<WorkflowEvent> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        // Register the waiter
        {
            let mut waiters = self.waiters.lock().await;
            waiters
                .entry(event_pattern.to_string())
                .or_default()
                .push((waiter_id.to_string(), tx));
        }

        info!(
            "Event bus: waiter '{}' waiting for '{}' (timeout: {:?})",
            waiter_id, event_pattern, timeout
        );

        // Wait with timeout
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(event)) => {
                info!(
                    "Event bus: waiter '{}' received event '{}'",
                    waiter_id, event.name
                );
                Some(event)
            }
            Ok(Err(_)) => {
                // Sender was dropped (bus shutting down)
                warn!("Event bus: waiter '{}' sender dropped", waiter_id);
                None
            }
            Err(_) => {
                // Timeout — remove the waiter
                let mut waiters = self.waiters.lock().await;
                if let Some(list) = waiters.get_mut(event_pattern) {
                    list.retain(|(id, _)| id != waiter_id);
                    if list.is_empty() {
                        waiters.remove(event_pattern);
                    }
                }
                info!(
                    "Event bus: waiter '{}' timed out waiting for '{}'",
                    waiter_id, event_pattern
                );
                None
            }
        }
    }

    /// Get a broadcast receiver for streaming all events.
    pub fn subscribe_broadcast(&self) -> broadcast::Receiver<WorkflowEvent> {
        self.broadcast_tx.subscribe()
    }

    /// Get recent event history.
    pub async fn history(&self, limit: usize) -> Vec<WorkflowEvent> {
        let history = self.history.lock().await;
        history.iter().rev().take(limit).rev().cloned().collect()
    }

    /// List active subscriptions.
    pub async fn list_subscriptions(&self) -> Vec<EventSubscription> {
        let subs = self.subscriptions.read().await;
        subs.clone()
    }

    // =========================================================================
    // Internal helpers
    // =========================================================================

    /// Wake any one-shot waiters whose pattern matches the event name.
    async fn wake_waiters(&self, event: &WorkflowEvent) -> usize {
        let mut waiters = self.waiters.lock().await;
        let mut woken = 0;

        // Collect matching patterns
        let patterns: Vec<String> = waiters.keys().cloned().collect();
        for pattern in patterns {
            if event_matches(&event.name, &pattern) {
                if let Some(waiter_list) = waiters.remove(&pattern) {
                    for (waiter_id, tx) in waiter_list {
                        debug!("Waking waiter '{}' for pattern '{}'", waiter_id, pattern);
                        let _ = tx.send(event.clone());
                        woken += 1;
                    }
                }
            }
        }

        woken
    }

    /// Find subscription IDs whose patterns match the event name.
    async fn find_matching_subscriptions(&self, event_name: &str) -> Vec<String> {
        let subs = self.subscriptions.read().await;
        subs.iter()
            .filter(|s| event_matches(event_name, &s.event_pattern))
            .map(|s| s.id.clone())
            .collect()
    }
}

// =============================================================================
// Pattern Matching
// =============================================================================

/// Check if an event name matches a subscription pattern.
///
/// Supports:
/// - Exact match: `"workflow.completed"` matches `"workflow.completed"`
/// - Wildcard suffix: `"workflow.*"` matches `"workflow.completed"`, `"workflow.failed"`
/// - Global wildcard: `"*"` matches everything
fn event_matches(event_name: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if pattern.ends_with(".*") {
        let prefix = &pattern[..pattern.len() - 2];
        return event_name.starts_with(prefix)
            && event_name.len() > prefix.len()
            && event_name.as_bytes()[prefix.len()] == b'.';
    }
    event_name == pattern
}

// =============================================================================
// Global Singleton
// =============================================================================

static WORKFLOW_EVENT_BUS: Lazy<Arc<WorkflowEventBus>> =
    Lazy::new(|| Arc::new(WorkflowEventBus::new()));

/// Get the global workflow event bus.
pub fn get_workflow_event_bus() -> &'static Arc<WorkflowEventBus> {
    &WORKFLOW_EVENT_BUS
}

// =============================================================================
// Well-Known Event Names
// =============================================================================

/// Standard workflow lifecycle events.
pub mod events {
    /// A workflow completed successfully.
    pub const WORKFLOW_COMPLETED: &str = "workflow.completed";
    /// A workflow failed.
    pub const WORKFLOW_FAILED: &str = "workflow.failed";
    /// A workflow was stopped by the user.
    pub const WORKFLOW_STOPPED: &str = "workflow.stopped";
    /// A workflow step/phase completed.
    pub const STEP_COMPLETED: &str = "step.completed";
    /// A workflow step/phase failed.
    pub const STEP_FAILED: &str = "step.failed";
    /// Verification checks passed.
    pub const VERIFICATION_PASSED: &str = "verification.passed";
    /// Verification checks failed.
    pub const VERIFICATION_FAILED: &str = "verification.failed";
    /// A trigger condition was met.
    pub const TRIGGER_FIRED: &str = "trigger.fired";
    /// A workflow was queued.
    pub const WORKFLOW_QUEUED: &str = "workflow.queued";
    /// A workflow started executing.
    pub const WORKFLOW_STARTED: &str = "workflow.started";
    /// A procedural skill was auto-extracted from a successful run.
    pub const SKILL_CREATED: &str = "skill.created";
    /// A procedural skill was updated (revision via self-repair).
    pub const SKILL_UPDATED: &str = "skill.updated";
    /// A skill-informed iteration failed (triggers self-repair evaluation).
    pub const SKILL_FAILED: &str = "skill.failed";
    /// The knowledge graph cache was invalidated (finding/fix/rule/observation mutation).
    pub const GRAPH_CACHE_INVALIDATED: &str = "graph.cache_invalidated";
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_matches_exact() {
        assert!(event_matches("workflow.completed", "workflow.completed"));
        assert!(!event_matches("workflow.failed", "workflow.completed"));
    }

    #[test]
    fn test_event_matches_wildcard() {
        assert!(event_matches("workflow.completed", "workflow.*"));
        assert!(event_matches("workflow.failed", "workflow.*"));
        assert!(!event_matches("step.completed", "workflow.*"));
        // Should not match bare prefix (no dot after)
        assert!(!event_matches("workflow", "workflow.*"));
    }

    #[test]
    fn test_event_matches_global() {
        assert!(event_matches("anything", "*"));
        assert!(event_matches("workflow.completed", "*"));
    }

    #[tokio::test]
    async fn test_emit_and_wait() {
        let bus = Arc::new(WorkflowEventBus::new());

        // Start a waiter in a background task
        let bus_clone = bus.clone();
        let waiter = tokio::spawn(async move {
            bus_clone
                .wait_for_event(
                    "test-waiter",
                    "test.event",
                    std::time::Duration::from_secs(5),
                )
                .await
        });

        // Give the waiter a moment to register
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Emit on the same bus
        bus.emit(WorkflowEvent {
            name: "test.event".to_string(),
            data: serde_json::json!({"key": "value"}),
            timestamp: chrono::Utc::now().to_rfc3339(),
            idempotency_key: None,
            source: EventSource::default(),
        })
        .await;

        // Waiter should receive the event
        let result = waiter.await.unwrap();
        assert!(result.is_some(), "Waiter should have received the event");
        assert_eq!(result.unwrap().name, "test.event");
    }

    #[tokio::test]
    async fn test_wait_for_event_timeout() {
        let bus = WorkflowEventBus::new();

        // Wait for an event that never arrives, with a short timeout
        let result = bus
            .wait_for_event(
                "timeout-waiter",
                "never.arrives",
                std::time::Duration::from_millis(50),
            )
            .await;

        assert!(result.is_none(), "Should have timed out");
    }

    #[tokio::test]
    async fn test_unsubscribe() {
        let bus = WorkflowEventBus::new();

        bus.subscribe(EventSubscription {
            id: "unsub-test".to_string(),
            event_pattern: "test.*".to_string(),
            condition: None,
            target_workflow_id: None,
            once: false,
        })
        .await;

        // Should match before unsubscribe
        let r1 = bus
            .emit(WorkflowEvent {
                name: "test.event".to_string(),
                data: serde_json::json!({}),
                timestamp: chrono::Utc::now().to_rfc3339(),
                idempotency_key: None,
                source: EventSource::default(),
            })
            .await;
        assert_eq!(r1.matched_subscriptions.len(), 1);

        // Unsubscribe
        assert!(bus.unsubscribe("unsub-test").await);
        assert!(!bus.unsubscribe("unsub-test").await); // Already removed

        // Should NOT match after unsubscribe
        let r2 = bus
            .emit(WorkflowEvent {
                name: "test.event".to_string(),
                data: serde_json::json!({}),
                timestamp: chrono::Utc::now().to_rfc3339(),
                idempotency_key: None,
                source: EventSource::default(),
            })
            .await;
        assert_eq!(r2.matched_subscriptions.len(), 0);
    }

    #[tokio::test]
    async fn test_subscribe_and_match() {
        let bus = WorkflowEventBus::new();

        bus.subscribe(EventSubscription {
            id: "sub-1".to_string(),
            event_pattern: "workflow.*".to_string(),
            condition: None,
            target_workflow_id: Some("wf-downstream".to_string()),
            once: false,
        })
        .await;

        let result = bus
            .emit(WorkflowEvent {
                name: "workflow.completed".to_string(),
                data: serde_json::json!({}),
                timestamp: chrono::Utc::now().to_rfc3339(),
                idempotency_key: None,
                source: EventSource::default(),
            })
            .await;

        assert_eq!(result.matched_subscriptions, vec!["sub-1"]);
    }

    #[tokio::test]
    async fn test_once_subscription_auto_removes() {
        let bus = WorkflowEventBus::new();

        bus.subscribe(EventSubscription {
            id: "once-sub".to_string(),
            event_pattern: "test.event".to_string(),
            condition: None,
            target_workflow_id: None,
            once: true,
        })
        .await;

        // First emit should match
        let r1 = bus
            .emit(WorkflowEvent {
                name: "test.event".to_string(),
                data: serde_json::json!({}),
                timestamp: chrono::Utc::now().to_rfc3339(),
                idempotency_key: None,
                source: EventSource::default(),
            })
            .await;
        assert_eq!(r1.matched_subscriptions.len(), 1);

        // Second emit should NOT match (subscription removed)
        let r2 = bus
            .emit(WorkflowEvent {
                name: "test.event".to_string(),
                data: serde_json::json!({}),
                timestamp: chrono::Utc::now().to_rfc3339(),
                idempotency_key: None,
                source: EventSource::default(),
            })
            .await;
        assert_eq!(r2.matched_subscriptions.len(), 0);
    }

    #[tokio::test]
    async fn test_history() {
        let bus = WorkflowEventBus::new();

        for i in 0..5 {
            bus.emit(WorkflowEvent {
                name: format!("event.{}", i),
                data: serde_json::json!({}),
                timestamp: chrono::Utc::now().to_rfc3339(),
                idempotency_key: None,
                source: EventSource::default(),
            })
            .await;
        }

        let history = bus.history(3).await;
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].name, "event.2");
        assert_eq!(history[2].name, "event.4");
    }
}
