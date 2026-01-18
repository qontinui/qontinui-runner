//! Event Bus with Subscribers Module
//!
//! Implements a decoupled event handling system inspired by CrewAI and Dify.
//! Events are emitted to a central bus, and handlers subscribe to specific
//! event types.
//!
//! ## Key Features
//!
//! - Type-safe event definitions
//! - Multiple handlers per event type
//! - Async and sync handler support
//! - Built-in handlers for common use cases
//! - Plugin architecture for extensions
//!
//! ## Example
//!
//! ```rust
//! use qontinui_runner::orchestrator::event_bus::*;
//!
//! let mut bus = EventBus::new();
//! bus.subscribe(EventType::WorkerStarted, LoggingHandler::new());
//! bus.emit(OrchestratorEvent::worker_started("worker-1"));
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::{debug, info, warn};

// ============================================================================
// Event Types
// ============================================================================

/// Categories of orchestrator events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    // Lifecycle events
    TaskCreated,
    TaskStarted,
    TaskCompleted,
    TaskFailed,
    TaskCancelled,

    // Worker events
    WorkerStarted,
    WorkerCompleted,
    WorkerFailed,
    WorkerSignal,

    // Iteration events
    IterationStarted,
    IterationCompleted,
    IterationFailed,

    // Verification events
    VerificationStarted,
    VerificationPassed,
    VerificationFailed,
    CriterionEvaluated,

    // State events
    StateChanged,
    InterruptRaised,
    InterruptResolved,

    // Progress events
    ProgressUpdated,
    StallDetected,
    ReplanTriggered,

    // Error events
    ErrorOccurred,
    WarningRaised,

    // Custom events
    Custom,
}

impl EventType {
    /// Get all event types for comprehensive subscription.
    pub fn all() -> Vec<EventType> {
        vec![
            Self::TaskCreated,
            Self::TaskStarted,
            Self::TaskCompleted,
            Self::TaskFailed,
            Self::TaskCancelled,
            Self::WorkerStarted,
            Self::WorkerCompleted,
            Self::WorkerFailed,
            Self::WorkerSignal,
            Self::IterationStarted,
            Self::IterationCompleted,
            Self::IterationFailed,
            Self::VerificationStarted,
            Self::VerificationPassed,
            Self::VerificationFailed,
            Self::CriterionEvaluated,
            Self::StateChanged,
            Self::InterruptRaised,
            Self::InterruptResolved,
            Self::ProgressUpdated,
            Self::StallDetected,
            Self::ReplanTriggered,
            Self::ErrorOccurred,
            Self::WarningRaised,
            Self::Custom,
        ]
    }
}

// ============================================================================
// Event Payloads
// ============================================================================

/// An orchestrator event with its payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorEvent {
    /// Unique event ID.
    pub id: String,
    /// Event type.
    pub event_type: EventType,
    /// When the event occurred (RFC3339).
    pub timestamp: String,
    /// Task ID this event relates to.
    pub task_id: Option<String>,
    /// Worker ID this event relates to.
    pub worker_id: Option<String>,
    /// Iteration number.
    pub iteration: Option<u32>,
    /// Event payload.
    pub payload: EventPayload,
    /// Additional metadata.
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Payload variants for different event types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    /// No additional data.
    Empty,
    /// Text message.
    Message { message: String },
    /// Progress update.
    Progress {
        current: u32,
        total: u32,
        percent: f32,
    },
    /// State transition.
    StateChange { from: String, to: String },
    /// Error information.
    Error {
        code: String,
        message: String,
        details: Option<String>,
    },
    /// Verification result.
    VerificationResult {
        criterion_id: String,
        passed: bool,
        reason: Option<String>,
    },
    /// Worker signal.
    Signal {
        signal_type: String,
        data: serde_json::Value,
    },
    /// Finding from analysis.
    Finding {
        category: String,
        severity: String,
        description: String,
        location: Option<String>,
    },
    /// Generic JSON payload.
    Json(serde_json::Value),
}

impl OrchestratorEvent {
    /// Create a new event.
    pub fn new(event_type: EventType, payload: EventPayload) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            event_type,
            timestamp: chrono::Utc::now().to_rfc3339(),
            task_id: None,
            worker_id: None,
            iteration: None,
            payload,
            metadata: HashMap::new(),
        }
    }

    /// Set the task ID.
    pub fn with_task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    /// Set the worker ID.
    pub fn with_worker(mut self, worker_id: impl Into<String>) -> Self {
        self.worker_id = Some(worker_id.into());
        self
    }

    /// Set the iteration number.
    pub fn with_iteration(mut self, iteration: u32) -> Self {
        self.iteration = Some(iteration);
        self
    }

    /// Add metadata.
    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    // Convenience constructors

    /// Create a task created event.
    pub fn task_created(task_id: &str) -> Self {
        Self::new(EventType::TaskCreated, EventPayload::Empty).with_task(task_id)
    }

    /// Create a task started event.
    pub fn task_started(task_id: &str) -> Self {
        Self::new(EventType::TaskStarted, EventPayload::Empty).with_task(task_id)
    }

    /// Create a task completed event.
    pub fn task_completed(task_id: &str) -> Self {
        Self::new(EventType::TaskCompleted, EventPayload::Empty).with_task(task_id)
    }

    /// Create a task failed event.
    pub fn task_failed(task_id: &str, error: &str) -> Self {
        Self::new(
            EventType::TaskFailed,
            EventPayload::Error {
                code: "task_failed".to_string(),
                message: error.to_string(),
                details: None,
            },
        )
        .with_task(task_id)
    }

    /// Create a worker started event.
    pub fn worker_started(worker_id: &str) -> Self {
        Self::new(EventType::WorkerStarted, EventPayload::Empty).with_worker(worker_id)
    }

    /// Create a worker completed event.
    pub fn worker_completed(worker_id: &str) -> Self {
        Self::new(EventType::WorkerCompleted, EventPayload::Empty).with_worker(worker_id)
    }

    /// Create a progress updated event.
    pub fn progress_updated(current: u32, total: u32) -> Self {
        let percent = if total > 0 {
            (current as f32 / total as f32) * 100.0
        } else {
            0.0
        };
        Self::new(
            EventType::ProgressUpdated,
            EventPayload::Progress {
                current,
                total,
                percent,
            },
        )
    }

    /// Create a state changed event.
    pub fn state_changed(from: &str, to: &str) -> Self {
        Self::new(
            EventType::StateChanged,
            EventPayload::StateChange {
                from: from.to_string(),
                to: to.to_string(),
            },
        )
    }

    /// Create an error occurred event.
    pub fn error_occurred(code: &str, message: &str) -> Self {
        Self::new(
            EventType::ErrorOccurred,
            EventPayload::Error {
                code: code.to_string(),
                message: message.to_string(),
                details: None,
            },
        )
    }

    /// Create a verification result event.
    pub fn verification_result(criterion_id: &str, passed: bool, reason: Option<&str>) -> Self {
        Self::new(
            EventType::CriterionEvaluated,
            EventPayload::VerificationResult {
                criterion_id: criterion_id.to_string(),
                passed,
                reason: reason.map(|s| s.to_string()),
            },
        )
    }

    /// Create a stall detected event.
    pub fn stall_detected(message: &str) -> Self {
        Self::new(
            EventType::StallDetected,
            EventPayload::Message {
                message: message.to_string(),
            },
        )
    }

    /// Create an interrupt raised event.
    pub fn interrupt_raised(reason: &str) -> Self {
        Self::new(
            EventType::InterruptRaised,
            EventPayload::Message {
                message: reason.to_string(),
            },
        )
    }
}

// ============================================================================
// Event Handler Trait
// ============================================================================

/// Handler for orchestrator events.
pub trait EventHandler: Send + Sync {
    /// Handle an event. Returns true if the event was handled successfully.
    fn handle(&self, event: &OrchestratorEvent) -> bool;

    /// Get the handler's name for logging.
    fn name(&self) -> &str;

    /// Whether this handler should receive events even if earlier handlers failed.
    fn continue_on_prior_failure(&self) -> bool {
        true
    }
}

// ============================================================================
// Event Bus
// ============================================================================

/// Central event bus for the orchestrator.
pub struct EventBus {
    /// Subscribers per event type.
    subscribers: RwLock<HashMap<EventType, Vec<Arc<dyn EventHandler>>>>,
    /// Global subscribers that receive all events.
    global_subscribers: RwLock<Vec<Arc<dyn EventHandler>>>,
    /// Event history for replay/debugging.
    history: RwLock<Vec<OrchestratorEvent>>,
    /// Maximum history size.
    max_history: usize,
    /// Whether to keep history.
    keep_history: bool,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    /// Create a new event bus.
    pub fn new() -> Self {
        Self {
            subscribers: RwLock::new(HashMap::new()),
            global_subscribers: RwLock::new(Vec::new()),
            history: RwLock::new(Vec::new()),
            max_history: 1000,
            keep_history: true,
        }
    }

    /// Create an event bus without history.
    pub fn without_history() -> Self {
        Self {
            keep_history: false,
            ..Self::new()
        }
    }

    /// Set the maximum history size.
    pub fn with_max_history(mut self, max: usize) -> Self {
        self.max_history = max;
        self
    }

    /// Subscribe to a specific event type.
    pub fn subscribe(&self, event_type: EventType, handler: impl EventHandler + 'static) {
        let mut subscribers = self.subscribers.write().unwrap();
        subscribers
            .entry(event_type)
            .or_insert_with(Vec::new)
            .push(Arc::new(handler));
    }

    /// Subscribe to all events.
    pub fn subscribe_all(&self, handler: impl EventHandler + 'static) {
        let mut global = self.global_subscribers.write().unwrap();
        global.push(Arc::new(handler));
    }

    /// Subscribe to multiple event types.
    pub fn subscribe_many(
        &self,
        event_types: &[EventType],
        handler: impl EventHandler + Clone + 'static,
    ) {
        for event_type in event_types {
            self.subscribe(*event_type, handler.clone());
        }
    }

    /// Emit an event to all subscribers.
    pub fn emit(&self, event: OrchestratorEvent) {
        debug!(
            event_type = ?event.event_type,
            event_id = %event.id,
            "Emitting event"
        );

        // Store in history
        if self.keep_history {
            let mut history = self.history.write().unwrap();
            history.push(event.clone());
            if history.len() > self.max_history {
                history.remove(0);
            }
        }

        // Notify global subscribers first
        {
            let global = self.global_subscribers.read().unwrap();
            for handler in global.iter() {
                if !handler.handle(&event) {
                    warn!(
                        handler = handler.name(),
                        event_id = %event.id,
                        "Global handler failed to process event"
                    );
                }
            }
        }

        // Notify type-specific subscribers
        {
            let subscribers = self.subscribers.read().unwrap();
            if let Some(handlers) = subscribers.get(&event.event_type) {
                for handler in handlers {
                    if !handler.handle(&event) {
                        warn!(
                            handler = handler.name(),
                            event_id = %event.id,
                            "Handler failed to process event"
                        );
                    }
                }
            }
        }
    }

    /// Get event history.
    pub fn history(&self) -> Vec<OrchestratorEvent> {
        self.history.read().unwrap().clone()
    }

    /// Get events of a specific type from history.
    pub fn history_by_type(&self, event_type: EventType) -> Vec<OrchestratorEvent> {
        self.history
            .read()
            .unwrap()
            .iter()
            .filter(|e| e.event_type == event_type)
            .cloned()
            .collect()
    }

    /// Get events for a specific task from history.
    pub fn history_by_task(&self, task_id: &str) -> Vec<OrchestratorEvent> {
        self.history
            .read()
            .unwrap()
            .iter()
            .filter(|e| e.task_id.as_deref() == Some(task_id))
            .cloned()
            .collect()
    }

    /// Clear event history.
    pub fn clear_history(&self) {
        self.history.write().unwrap().clear();
    }

    /// Get subscriber count for an event type.
    pub fn subscriber_count(&self, event_type: EventType) -> usize {
        self.subscribers
            .read()
            .unwrap()
            .get(&event_type)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// Get global subscriber count.
    pub fn global_subscriber_count(&self) -> usize {
        self.global_subscribers.read().unwrap().len()
    }
}

// ============================================================================
// Built-in Handlers
// ============================================================================

/// Handler that logs all events.
#[derive(Debug, Clone)]
pub struct LoggingHandler {
    level: LogLevel,
}

#[derive(Debug, Clone, Copy)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
}

impl LoggingHandler {
    pub fn new() -> Self {
        Self {
            level: LogLevel::Info,
        }
    }

    pub fn with_level(level: LogLevel) -> Self {
        Self { level }
    }
}

impl Default for LoggingHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl EventHandler for LoggingHandler {
    fn handle(&self, event: &OrchestratorEvent) -> bool {
        let msg = format!(
            "Event: {:?} | Task: {:?} | Worker: {:?}",
            event.event_type, event.task_id, event.worker_id
        );

        match self.level {
            LogLevel::Debug => debug!("{}", msg),
            LogLevel::Info => info!("{}", msg),
            LogLevel::Warn => warn!("{}", msg),
        }

        true
    }

    fn name(&self) -> &str {
        "LoggingHandler"
    }
}

/// Handler that collects events into a vector.
#[derive(Debug)]
pub struct CollectorHandler {
    events: Arc<RwLock<Vec<OrchestratorEvent>>>,
}

impl CollectorHandler {
    pub fn new() -> Self {
        Self {
            events: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn events(&self) -> Vec<OrchestratorEvent> {
        self.events.read().unwrap().clone()
    }

    pub fn clear(&self) {
        self.events.write().unwrap().clear();
    }

    pub fn count(&self) -> usize {
        self.events.read().unwrap().len()
    }
}

impl Default for CollectorHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for CollectorHandler {
    fn clone(&self) -> Self {
        Self {
            events: Arc::clone(&self.events),
        }
    }
}

impl EventHandler for CollectorHandler {
    fn handle(&self, event: &OrchestratorEvent) -> bool {
        self.events.write().unwrap().push(event.clone());
        true
    }

    fn name(&self) -> &str {
        "CollectorHandler"
    }
}

/// Handler that filters events before passing to inner handler.
pub struct FilterHandler<F>
where
    F: Fn(&OrchestratorEvent) -> bool + Send + Sync,
{
    filter: F,
    inner: Arc<dyn EventHandler>,
    name: String,
}

impl<F> FilterHandler<F>
where
    F: Fn(&OrchestratorEvent) -> bool + Send + Sync,
{
    pub fn new(filter: F, inner: impl EventHandler + 'static, name: impl Into<String>) -> Self {
        Self {
            filter,
            inner: Arc::new(inner),
            name: name.into(),
        }
    }
}

impl<F> EventHandler for FilterHandler<F>
where
    F: Fn(&OrchestratorEvent) -> bool + Send + Sync,
{
    fn handle(&self, event: &OrchestratorEvent) -> bool {
        if (self.filter)(event) {
            self.inner.handle(event)
        } else {
            true // Filtered out events are considered "handled"
        }
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Handler that calls a callback function.
pub struct CallbackHandler<F>
where
    F: Fn(&OrchestratorEvent) + Send + Sync,
{
    callback: F,
    name: String,
}

impl<F> CallbackHandler<F>
where
    F: Fn(&OrchestratorEvent) + Send + Sync,
{
    pub fn new(callback: F, name: impl Into<String>) -> Self {
        Self {
            callback,
            name: name.into(),
        }
    }
}

impl<F> EventHandler for CallbackHandler<F>
where
    F: Fn(&OrchestratorEvent) + Send + Sync,
{
    fn handle(&self, event: &OrchestratorEvent) -> bool {
        (self.callback)(event);
        true
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Handler that tracks metrics/counters.
#[derive(Debug)]
pub struct MetricsHandler {
    counters: Arc<RwLock<HashMap<EventType, u64>>>,
}

impl MetricsHandler {
    pub fn new() -> Self {
        Self {
            counters: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn get_count(&self, event_type: EventType) -> u64 {
        *self.counters.read().unwrap().get(&event_type).unwrap_or(&0)
    }

    pub fn get_all_counts(&self) -> HashMap<EventType, u64> {
        self.counters.read().unwrap().clone()
    }

    pub fn reset(&self) {
        self.counters.write().unwrap().clear();
    }
}

impl Default for MetricsHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for MetricsHandler {
    fn clone(&self) -> Self {
        Self {
            counters: Arc::clone(&self.counters),
        }
    }
}

impl EventHandler for MetricsHandler {
    fn handle(&self, event: &OrchestratorEvent) -> bool {
        let mut counters = self.counters.write().unwrap();
        *counters.entry(event.event_type).or_insert(0) += 1;
        true
    }

    fn name(&self) -> &str {
        "MetricsHandler"
    }
}

// ============================================================================
// Event Bus Builder
// ============================================================================

/// Builder for configuring an event bus with common handlers.
pub struct EventBusBuilder {
    bus: EventBus,
}

impl EventBusBuilder {
    pub fn new() -> Self {
        Self {
            bus: EventBus::new(),
        }
    }

    /// Add logging for all events.
    pub fn with_logging(self) -> Self {
        self.bus.subscribe_all(LoggingHandler::new());
        self
    }

    /// Add logging at a specific level.
    pub fn with_logging_level(self, level: LogLevel) -> Self {
        self.bus.subscribe_all(LoggingHandler::with_level(level));
        self
    }

    /// Add metrics collection.
    pub fn with_metrics(self, handler: MetricsHandler) -> Self {
        self.bus.subscribe_all(handler);
        self
    }

    /// Add a collector for specific event types.
    pub fn with_collector(self, event_types: &[EventType], collector: CollectorHandler) -> Self {
        self.bus.subscribe_many(event_types, collector);
        self
    }

    /// Disable history.
    pub fn without_history(mut self) -> Self {
        self.bus.keep_history = false;
        self
    }

    /// Set max history size.
    pub fn with_max_history(mut self, max: usize) -> Self {
        self.bus.max_history = max;
        self
    }

    /// Build the event bus.
    pub fn build(self) -> EventBus {
        self.bus
    }
}

impl Default for EventBusBuilder {
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

    #[test]
    fn test_event_creation() {
        let event = OrchestratorEvent::task_started("task-1");
        assert_eq!(event.event_type, EventType::TaskStarted);
        assert_eq!(event.task_id, Some("task-1".to_string()));
    }

    #[test]
    fn test_event_with_metadata() {
        let event = OrchestratorEvent::worker_started("worker-1")
            .with_task("task-1")
            .with_iteration(5)
            .with_metadata("custom", serde_json::json!("value"));

        assert_eq!(event.worker_id, Some("worker-1".to_string()));
        assert_eq!(event.task_id, Some("task-1".to_string()));
        assert_eq!(event.iteration, Some(5));
        assert!(event.metadata.contains_key("custom"));
    }

    #[test]
    fn test_event_bus_subscribe_and_emit() {
        let bus = EventBus::new();
        let collector = CollectorHandler::new();

        bus.subscribe(EventType::TaskStarted, collector.clone());
        bus.emit(OrchestratorEvent::task_started("task-1"));

        assert_eq!(collector.count(), 1);
    }

    #[test]
    fn test_event_bus_global_subscriber() {
        let bus = EventBus::new();
        let collector = CollectorHandler::new();

        bus.subscribe_all(collector.clone());
        bus.emit(OrchestratorEvent::task_started("task-1"));
        bus.emit(OrchestratorEvent::task_completed("task-1"));

        assert_eq!(collector.count(), 2);
    }

    #[test]
    fn test_event_bus_history() {
        let bus = EventBus::new();
        bus.emit(OrchestratorEvent::task_started("task-1"));
        bus.emit(OrchestratorEvent::task_completed("task-1"));

        let history = bus.history();
        assert_eq!(history.len(), 2);

        let by_type = bus.history_by_type(EventType::TaskStarted);
        assert_eq!(by_type.len(), 1);

        let by_task = bus.history_by_task("task-1");
        assert_eq!(by_task.len(), 2);
    }

    #[test]
    fn test_metrics_handler() {
        let bus = EventBus::new();
        let metrics = MetricsHandler::new();

        bus.subscribe_all(metrics.clone());
        bus.emit(OrchestratorEvent::task_started("task-1"));
        bus.emit(OrchestratorEvent::task_started("task-2"));
        bus.emit(OrchestratorEvent::task_completed("task-1"));

        assert_eq!(metrics.get_count(EventType::TaskStarted), 2);
        assert_eq!(metrics.get_count(EventType::TaskCompleted), 1);
    }

    #[test]
    fn test_event_bus_builder() {
        let metrics = MetricsHandler::new();
        let bus = EventBusBuilder::new()
            .with_logging_level(LogLevel::Debug)
            .with_metrics(metrics.clone())
            .with_max_history(100)
            .build();

        bus.emit(OrchestratorEvent::task_started("task-1"));

        assert_eq!(metrics.get_count(EventType::TaskStarted), 1);
        assert!(bus.global_subscriber_count() >= 2); // logging + metrics
    }

    #[test]
    fn test_subscriber_count() {
        let bus = EventBus::new();
        let collector = CollectorHandler::new();

        bus.subscribe(EventType::TaskStarted, collector.clone());
        bus.subscribe(EventType::TaskStarted, collector.clone());

        assert_eq!(bus.subscriber_count(EventType::TaskStarted), 2);
        assert_eq!(bus.subscriber_count(EventType::TaskCompleted), 0);
    }

    #[test]
    fn test_progress_event() {
        let event = OrchestratorEvent::progress_updated(50, 100);
        if let EventPayload::Progress {
            current,
            total,
            percent,
        } = event.payload
        {
            assert_eq!(current, 50);
            assert_eq!(total, 100);
            assert!((percent - 50.0).abs() < 0.01);
        } else {
            panic!("Expected Progress payload");
        }
    }

    #[test]
    fn test_state_change_event() {
        let event = OrchestratorEvent::state_changed("Planning", "Executing");
        if let EventPayload::StateChange { from, to } = event.payload {
            assert_eq!(from, "Planning");
            assert_eq!(to, "Executing");
        } else {
            panic!("Expected StateChange payload");
        }
    }
}
