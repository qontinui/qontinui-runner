//! Layered Graph Engine
//!
//! Inspired by Dify's layered architecture, this module implements a middleware
//! pattern for the orchestrator that separates cross-cutting concerns:
//!
//! - **Persistence**: State checkpointing and recovery
//! - **Observability**: Event logging, metrics, tracing
//! - **Execution Limits**: Rate limiting, timeouts, quotas
//! - **Debug**: Development-time logging and inspection
//!
//! # Design Goals
//!
//! 1. **Separation of Concerns**: Each layer handles one responsibility
//! 2. **Composability**: Layers can be added/removed independently
//! 3. **Testability**: Layers can be tested in isolation
//! 4. **Configurability**: Different layer stacks for prod vs dev
//!
//! # Usage
//!
//! ```rust,ignore
//! let engine = LayeredEngine::new()
//!     .with_layer(PersistenceLayer::new(db))
//!     .with_layer(ObservabilityLayer::new())
//!     .with_layer(ExecutionLimitsLayer::new(config));
//!
//! engine.emit_event(event);
//! ```

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info, warn};

// ============================================================================
// Events
// ============================================================================

/// Events emitted by the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum OrchestratorEvent {
    /// Task run started.
    TaskStarted {
        task_id: String,
        goal: String,
        timestamp: String,
    },

    /// State transition occurred.
    StateTransition {
        task_id: String,
        from_state: String,
        to_state: String,
        reason: Option<String>,
        timestamp: String,
    },

    /// Iteration started.
    IterationStarted {
        task_id: String,
        iteration: u32,
        timestamp: String,
    },

    /// Iteration completed.
    IterationCompleted {
        task_id: String,
        iteration: u32,
        duration_ms: u64,
        verification_passed: bool,
        timestamp: String,
    },

    /// Worker output received.
    WorkerOutput {
        task_id: String,
        iteration: u32,
        signal: Option<String>,
        findings_count: usize,
        timestamp: String,
    },

    /// Verification started.
    VerificationStarted {
        task_id: String,
        iteration: u32,
        criterion_count: usize,
        timestamp: String,
    },

    /// Verification completed.
    VerificationCompleted {
        task_id: String,
        iteration: u32,
        passed: bool,
        criteria_passed: usize,
        criteria_total: usize,
        timestamp: String,
    },

    /// Knowledge recorded.
    KnowledgeRecorded {
        task_id: String,
        category: String,
        content_preview: String,
        timestamp: String,
    },

    /// Error occurred.
    Error {
        task_id: String,
        error_type: String,
        message: String,
        recoverable: bool,
        timestamp: String,
    },

    /// Task completed.
    TaskCompleted {
        task_id: String,
        success: bool,
        iterations: u32,
        duration_ms: u64,
        timestamp: String,
    },

    /// Stall detected.
    StallDetected {
        task_id: String,
        reason: String,
        iterations_without_progress: u32,
        timestamp: String,
    },

    /// Replan triggered.
    ReplanTriggered {
        task_id: String,
        reason: String,
        plan_version: u32,
        timestamp: String,
    },
}

impl OrchestratorEvent {
    /// Get the task ID from any event.
    pub fn task_id(&self) -> &str {
        match self {
            Self::TaskStarted { task_id, .. } => task_id,
            Self::StateTransition { task_id, .. } => task_id,
            Self::IterationStarted { task_id, .. } => task_id,
            Self::IterationCompleted { task_id, .. } => task_id,
            Self::WorkerOutput { task_id, .. } => task_id,
            Self::VerificationStarted { task_id, .. } => task_id,
            Self::VerificationCompleted { task_id, .. } => task_id,
            Self::KnowledgeRecorded { task_id, .. } => task_id,
            Self::Error { task_id, .. } => task_id,
            Self::TaskCompleted { task_id, .. } => task_id,
            Self::StallDetected { task_id, .. } => task_id,
            Self::ReplanTriggered { task_id, .. } => task_id,
        }
    }

    /// Get the event type name.
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::TaskStarted { .. } => "task_started",
            Self::StateTransition { .. } => "state_transition",
            Self::IterationStarted { .. } => "iteration_started",
            Self::IterationCompleted { .. } => "iteration_completed",
            Self::WorkerOutput { .. } => "worker_output",
            Self::VerificationStarted { .. } => "verification_started",
            Self::VerificationCompleted { .. } => "verification_completed",
            Self::KnowledgeRecorded { .. } => "knowledge_recorded",
            Self::Error { .. } => "error",
            Self::TaskCompleted { .. } => "task_completed",
            Self::StallDetected { .. } => "stall_detected",
            Self::ReplanTriggered { .. } => "replan_triggered",
        }
    }

    /// Create a timestamp for events.
    pub fn now() -> String {
        chrono::Utc::now().to_rfc3339()
    }
}

// ============================================================================
// Commands
// ============================================================================

/// Commands that can be issued to the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command_type", rename_all = "snake_case")]
pub enum OrchestratorCommand {
    /// Pause execution.
    Pause { reason: String },

    /// Resume execution.
    Resume,

    /// Stop execution.
    Stop { reason: String },

    /// Trigger replanning.
    Replan { reason: String },

    /// Extend iterations.
    ExtendIterations { additional: u32 },

    /// Skip to verification.
    SkipToVerification,

    /// Override a criterion.
    OverrideCriterion {
        criterion_id: String,
        justification: String,
    },
}

// ============================================================================
// Layer Trait
// ============================================================================

/// A layer that intercepts orchestrator events and commands.
///
/// Layers are called in order for events, and in reverse order for commands.
/// Each layer can:
/// - Observe events for logging/metrics
/// - Modify events (with caution)
/// - Issue commands in response to events
/// - Block commands (by returning false from on_command)
pub trait OrchestratorLayer: Send + Sync {
    /// Called when an event is emitted.
    ///
    /// Returns any commands to issue in response.
    fn on_event(&self, event: &OrchestratorEvent, state: &LayerState) -> Vec<OrchestratorCommand>;

    /// Called before a command is executed.
    ///
    /// Return false to block the command.
    fn on_command(&self, command: &OrchestratorCommand, state: &LayerState) -> bool;

    /// Get the layer's name for debugging.
    fn name(&self) -> &'static str;
}

/// State available to layers.
#[derive(Debug, Clone, Default)]
pub struct LayerState {
    /// Current task ID.
    pub task_id: Option<String>,

    /// Current iteration.
    pub iteration: u32,

    /// Current orchestrator phase.
    pub phase: String,

    /// Whether the task is paused.
    pub is_paused: bool,

    /// Custom state data (JSON).
    pub custom: std::collections::HashMap<String, serde_json::Value>,
}

impl LayerState {
    /// Create a new layer state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the task ID.
    pub fn with_task_id(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    /// Set the iteration.
    pub fn with_iteration(mut self, iteration: u32) -> Self {
        self.iteration = iteration;
        self
    }

    /// Set custom data.
    pub fn set_custom(&mut self, key: impl Into<String>, value: serde_json::Value) {
        self.custom.insert(key.into(), value);
    }

    /// Get custom data.
    pub fn get_custom(&self, key: &str) -> Option<&serde_json::Value> {
        self.custom.get(key)
    }
}

// ============================================================================
// Layered Engine
// ============================================================================

/// Engine that processes events through a stack of layers.
pub struct LayeredEngine {
    layers: Vec<Box<dyn OrchestratorLayer>>,
    state: LayerState,
}

impl LayeredEngine {
    /// Create a new layered engine.
    pub fn new() -> Self {
        Self {
            layers: Vec::new(),
            state: LayerState::new(),
        }
    }

    /// Add a layer to the engine.
    pub fn with_layer<L: OrchestratorLayer + 'static>(mut self, layer: L) -> Self {
        info!("Adding layer: {}", layer.name());
        self.layers.push(Box::new(layer));
        self
    }

    /// Set the engine state.
    pub fn with_state(mut self, state: LayerState) -> Self {
        self.state = state;
        self
    }

    /// Update the state.
    pub fn update_state(&mut self, state: LayerState) {
        self.state = state;
    }

    /// Emit an event through all layers.
    ///
    /// Returns any commands generated by layers.
    pub fn emit_event(&self, event: &OrchestratorEvent) -> Vec<OrchestratorCommand> {
        let mut commands = Vec::new();

        for layer in &self.layers {
            let layer_commands = layer.on_event(event, &self.state);
            if !layer_commands.is_empty() {
                debug!(
                    "Layer {} generated {} commands",
                    layer.name(),
                    layer_commands.len()
                );
            }
            commands.extend(layer_commands);
        }

        commands
    }

    /// Execute a command through all layers.
    ///
    /// Returns false if any layer blocks the command.
    pub fn execute_command(&self, command: &OrchestratorCommand) -> bool {
        // Commands go through layers in reverse order
        for layer in self.layers.iter().rev() {
            if !layer.on_command(command, &self.state) {
                warn!("Layer {} blocked command: {:?}", layer.name(), command);
                return false;
            }
        }
        true
    }

    /// Get layer names.
    pub fn layer_names(&self) -> Vec<&'static str> {
        self.layers.iter().map(|l| l.name()).collect()
    }
}

impl Default for LayeredEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Built-in Layers
// ============================================================================

/// Layer that logs all events for observability.
pub struct ObservabilityLayer {
    /// Whether to log events to tracing.
    pub log_events: bool,

    /// Event handler callback.
    pub event_handler: Option<Arc<dyn Fn(&OrchestratorEvent) + Send + Sync>>,
}

impl ObservabilityLayer {
    /// Create a new observability layer.
    pub fn new() -> Self {
        Self {
            log_events: true,
            event_handler: None,
        }
    }

    /// Set event handler callback.
    pub fn with_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(&OrchestratorEvent) + Send + Sync + 'static,
    {
        self.event_handler = Some(Arc::new(handler));
        self
    }
}

impl Default for ObservabilityLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl OrchestratorLayer for ObservabilityLayer {
    fn on_event(&self, event: &OrchestratorEvent, _state: &LayerState) -> Vec<OrchestratorCommand> {
        if self.log_events {
            match event {
                OrchestratorEvent::TaskStarted { task_id, goal, .. } => {
                    info!(task_id = %task_id, "Task started: {}", goal);
                }
                OrchestratorEvent::StateTransition {
                    from_state,
                    to_state,
                    reason,
                    ..
                } => {
                    info!(
                        from = %from_state,
                        to = %to_state,
                        reason = ?reason,
                        "State transition"
                    );
                }
                OrchestratorEvent::IterationCompleted {
                    iteration,
                    verification_passed,
                    duration_ms,
                    ..
                } => {
                    info!(
                        iteration = %iteration,
                        passed = %verification_passed,
                        duration_ms = %duration_ms,
                        "Iteration completed"
                    );
                }
                OrchestratorEvent::Error {
                    error_type,
                    message,
                    recoverable,
                    ..
                } => {
                    if *recoverable {
                        warn!(error_type = %error_type, "Recoverable error: {}", message);
                    } else {
                        error!(error_type = %error_type, "Unrecoverable error: {}", message);
                    }
                }
                OrchestratorEvent::StallDetected { reason, .. } => {
                    warn!("Stall detected: {}", reason);
                }
                OrchestratorEvent::TaskCompleted {
                    success,
                    iterations,
                    duration_ms,
                    ..
                } => {
                    info!(
                        success = %success,
                        iterations = %iterations,
                        duration_ms = %duration_ms,
                        "Task completed"
                    );
                }
                _ => {
                    debug!(event_type = %event.event_type(), "Event: {:?}", event);
                }
            }
        }

        if let Some(handler) = &self.event_handler {
            handler(event);
        }

        Vec::new()
    }

    fn on_command(&self, command: &OrchestratorCommand, _state: &LayerState) -> bool {
        debug!(command = ?command, "Command received");
        true // Don't block any commands
    }

    fn name(&self) -> &'static str {
        "observability"
    }
}

/// Layer that enforces execution limits (rate limiting, timeouts, quotas).
pub struct ExecutionLimitsLayer {
    /// Maximum iterations allowed.
    pub max_iterations: u32,

    /// Timeout per iteration (seconds).
    pub iteration_timeout_secs: u64,

    /// Minimum delay between iterations (milliseconds).
    pub min_iteration_delay_ms: u64,

    /// Maximum consecutive errors before stopping.
    pub max_consecutive_errors: u32,

    /// Track consecutive errors.
    consecutive_errors: std::sync::atomic::AtomicU32,

    /// Track last iteration time.
    last_iteration: std::sync::Mutex<Option<Instant>>,
}

impl ExecutionLimitsLayer {
    /// Create with default limits.
    pub fn new() -> Self {
        Self {
            max_iterations: 20,
            iteration_timeout_secs: 300, // 5 minutes
            min_iteration_delay_ms: 1000, // 1 second
            max_consecutive_errors: 3,
            consecutive_errors: std::sync::atomic::AtomicU32::new(0),
            last_iteration: std::sync::Mutex::new(None),
        }
    }

    /// Set max iterations.
    pub fn with_max_iterations(mut self, max: u32) -> Self {
        self.max_iterations = max;
        self
    }

    /// Set iteration timeout.
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.iteration_timeout_secs = secs;
        self
    }
}

impl Default for ExecutionLimitsLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl OrchestratorLayer for ExecutionLimitsLayer {
    fn on_event(&self, event: &OrchestratorEvent, _state: &LayerState) -> Vec<OrchestratorCommand> {
        match event {
            OrchestratorEvent::IterationStarted { iteration, .. } => {
                // Check iteration limit
                if *iteration >= self.max_iterations {
                    warn!(
                        "Max iterations ({}) reached at iteration {}",
                        self.max_iterations, iteration
                    );
                    return vec![OrchestratorCommand::Pause {
                        reason: format!(
                            "Maximum iterations ({}) reached",
                            self.max_iterations
                        ),
                    }];
                }

                // Track iteration start time
                *self.last_iteration.lock().unwrap() = Some(Instant::now());

                // Reset consecutive errors on new iteration
                self.consecutive_errors
                    .store(0, std::sync::atomic::Ordering::SeqCst);
            }

            OrchestratorEvent::IterationCompleted { duration_ms, .. } => {
                // Check for timeout
                if *duration_ms > self.iteration_timeout_secs * 1000 {
                    warn!(
                        "Iteration exceeded timeout ({} > {} secs)",
                        duration_ms / 1000,
                        self.iteration_timeout_secs
                    );
                }
            }

            OrchestratorEvent::Error { recoverable, .. } => {
                if !recoverable {
                    return vec![OrchestratorCommand::Stop {
                        reason: "Unrecoverable error".to_string(),
                    }];
                }

                let errors = self
                    .consecutive_errors
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;

                if errors >= self.max_consecutive_errors {
                    warn!(
                        "Max consecutive errors ({}) reached",
                        self.max_consecutive_errors
                    );
                    return vec![OrchestratorCommand::Stop {
                        reason: format!(
                            "Too many consecutive errors ({})",
                            self.max_consecutive_errors
                        ),
                    }];
                }
            }

            _ => {}
        }

        Vec::new()
    }

    fn on_command(&self, command: &OrchestratorCommand, state: &LayerState) -> bool {
        match command {
            OrchestratorCommand::ExtendIterations { additional } => {
                let new_max = state.iteration + additional;
                if new_max > 100 {
                    warn!("Cannot extend beyond 100 iterations");
                    return false;
                }
            }
            _ => {}
        }
        true
    }

    fn name(&self) -> &'static str {
        "execution_limits"
    }
}

/// Layer for development-time debugging.
pub struct DebugLayer {
    /// Whether debug mode is enabled.
    pub enabled: bool,

    /// Whether to emit verbose logs.
    pub verbose: bool,

    /// Events to capture for later inspection.
    pub captured_events: std::sync::Mutex<Vec<OrchestratorEvent>>,

    /// Maximum events to capture.
    pub max_captured: usize,
}

impl DebugLayer {
    /// Create a new debug layer.
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            verbose: false,
            captured_events: std::sync::Mutex::new(Vec::new()),
            max_captured: 1000,
        }
    }

    /// Enable verbose logging.
    pub fn verbose(mut self) -> Self {
        self.verbose = true;
        self
    }

    /// Get captured events.
    pub fn get_events(&self) -> Vec<OrchestratorEvent> {
        self.captured_events.lock().unwrap().clone()
    }

    /// Clear captured events.
    pub fn clear_events(&self) {
        self.captured_events.lock().unwrap().clear();
    }
}

impl OrchestratorLayer for DebugLayer {
    fn on_event(&self, event: &OrchestratorEvent, _state: &LayerState) -> Vec<OrchestratorCommand> {
        if !self.enabled {
            return Vec::new();
        }

        // Capture event
        let mut events = self.captured_events.lock().unwrap();
        if events.len() < self.max_captured {
            events.push(event.clone());
        }

        // Verbose logging
        if self.verbose {
            debug!(
                event_type = %event.event_type(),
                task_id = %event.task_id(),
                "DEBUG: {:?}",
                event
            );
        }

        Vec::new()
    }

    fn on_command(&self, command: &OrchestratorCommand, _state: &LayerState) -> bool {
        if self.enabled && self.verbose {
            debug!("DEBUG command: {:?}", command);
        }
        true
    }

    fn name(&self) -> &'static str {
        "debug"
    }
}

/// Layer that detects stalls and triggers replanning.
pub struct StallDetectionLayer {
    /// Iterations without progress before triggering.
    pub threshold: u32,

    /// Counter for iterations without progress.
    iterations_without_progress: std::sync::atomic::AtomicU32,

    /// Last seen criteria passing count.
    last_criteria_passing: std::sync::atomic::AtomicU32,
}

impl StallDetectionLayer {
    /// Create a new stall detection layer.
    pub fn new(threshold: u32) -> Self {
        Self {
            threshold,
            iterations_without_progress: std::sync::atomic::AtomicU32::new(0),
            last_criteria_passing: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Reset the stall counter.
    pub fn reset(&self) {
        self.iterations_without_progress
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }
}

impl OrchestratorLayer for StallDetectionLayer {
    fn on_event(&self, event: &OrchestratorEvent, _state: &LayerState) -> Vec<OrchestratorCommand> {
        match event {
            OrchestratorEvent::VerificationCompleted {
                criteria_passed, ..
            } => {
                let last = self
                    .last_criteria_passing
                    .load(std::sync::atomic::Ordering::SeqCst);

                if *criteria_passed as u32 <= last {
                    // No progress
                    let count = self
                        .iterations_without_progress
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        + 1;

                    if count >= self.threshold {
                        warn!(
                            "Stall detected: {} iterations without progress",
                            count
                        );
                        self.reset();
                        return vec![OrchestratorCommand::Replan {
                            reason: format!(
                                "No progress for {} iterations (stuck at {} criteria)",
                                count, last
                            ),
                        }];
                    }
                } else {
                    // Progress made
                    self.iterations_without_progress
                        .store(0, std::sync::atomic::Ordering::SeqCst);
                }

                self.last_criteria_passing
                    .store(*criteria_passed as u32, std::sync::atomic::Ordering::SeqCst);
            }

            OrchestratorEvent::ReplanTriggered { .. } => {
                // Reset on replan
                self.reset();
            }

            _ => {}
        }

        Vec::new()
    }

    fn on_command(&self, _command: &OrchestratorCommand, _state: &LayerState) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "stall_detection"
    }
}

// ============================================================================
// Layer Builder
// ============================================================================

/// Builder for creating a layered engine with common configurations.
pub struct LayerStackBuilder {
    engine: LayeredEngine,
}

impl LayerStackBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self {
            engine: LayeredEngine::new(),
        }
    }

    /// Add observability layer.
    pub fn with_observability(mut self) -> Self {
        self.engine = self.engine.with_layer(ObservabilityLayer::new());
        self
    }

    /// Add execution limits layer.
    pub fn with_limits(mut self, max_iterations: u32) -> Self {
        self.engine = self.engine.with_layer(
            ExecutionLimitsLayer::new().with_max_iterations(max_iterations),
        );
        self
    }

    /// Add stall detection layer.
    pub fn with_stall_detection(mut self, threshold: u32) -> Self {
        self.engine = self.engine.with_layer(StallDetectionLayer::new(threshold));
        self
    }

    /// Add debug layer.
    pub fn with_debug(mut self, enabled: bool) -> Self {
        self.engine = self.engine.with_layer(DebugLayer::new(enabled));
        self
    }

    /// Build the production layer stack.
    pub fn production(max_iterations: u32) -> LayeredEngine {
        Self::new()
            .with_observability()
            .with_limits(max_iterations)
            .with_stall_detection(3)
            .build()
    }

    /// Build the development layer stack.
    pub fn development(max_iterations: u32) -> LayeredEngine {
        Self::new()
            .with_observability()
            .with_limits(max_iterations)
            .with_stall_detection(3)
            .with_debug(true)
            .build()
    }

    /// Build the engine.
    pub fn build(self) -> LayeredEngine {
        self.engine
    }
}

impl Default for LayerStackBuilder {
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
    fn test_layered_engine_creation() {
        let engine = LayeredEngine::new()
            .with_layer(ObservabilityLayer::new())
            .with_layer(DebugLayer::new(true));

        assert_eq!(engine.layer_names().len(), 2);
    }

    #[test]
    fn test_event_emission() {
        let engine = LayeredEngine::new().with_layer(DebugLayer::new(true));

        let event = OrchestratorEvent::TaskStarted {
            task_id: "test".to_string(),
            goal: "Test task".to_string(),
            timestamp: OrchestratorEvent::now(),
        };

        let commands = engine.emit_event(&event);
        assert!(commands.is_empty());
    }

    #[test]
    fn test_execution_limits_max_iterations() {
        let engine = LayeredEngine::new().with_layer(ExecutionLimitsLayer::new().with_max_iterations(5));

        let event = OrchestratorEvent::IterationStarted {
            task_id: "test".to_string(),
            iteration: 5,
            timestamp: OrchestratorEvent::now(),
        };

        let commands = engine.emit_event(&event);
        assert!(!commands.is_empty());
        assert!(matches!(commands[0], OrchestratorCommand::Pause { .. }));
    }

    #[test]
    fn test_stall_detection() {
        let layer = StallDetectionLayer::new(2);
        let state = LayerState::new();

        // Simulate no progress
        let event1 = OrchestratorEvent::VerificationCompleted {
            task_id: "test".to_string(),
            iteration: 1,
            passed: false,
            criteria_passed: 0,
            criteria_total: 5,
            timestamp: OrchestratorEvent::now(),
        };

        let cmds1 = layer.on_event(&event1, &state);
        assert!(cmds1.is_empty()); // First iteration, no stall yet

        let event2 = OrchestratorEvent::VerificationCompleted {
            task_id: "test".to_string(),
            iteration: 2,
            passed: false,
            criteria_passed: 0,
            criteria_total: 5,
            timestamp: OrchestratorEvent::now(),
        };

        let cmds2 = layer.on_event(&event2, &state);
        assert!(!cmds2.is_empty()); // Should trigger replan
        assert!(matches!(cmds2[0], OrchestratorCommand::Replan { .. }));
    }

    #[test]
    fn test_debug_layer_captures_events() {
        let layer = DebugLayer::new(true);

        let event = OrchestratorEvent::TaskStarted {
            task_id: "test".to_string(),
            goal: "Test".to_string(),
            timestamp: OrchestratorEvent::now(),
        };

        let state = LayerState::new();
        layer.on_event(&event, &state);

        let captured = layer.get_events();
        assert_eq!(captured.len(), 1);
    }

    #[test]
    fn test_layer_stack_builder() {
        let engine = LayerStackBuilder::production(10);
        assert!(engine.layer_names().contains(&"observability"));
        assert!(engine.layer_names().contains(&"execution_limits"));
        assert!(engine.layer_names().contains(&"stall_detection"));
    }

    #[test]
    fn test_command_blocking() {
        struct BlockingLayer;

        impl OrchestratorLayer for BlockingLayer {
            fn on_event(
                &self,
                _event: &OrchestratorEvent,
                _state: &LayerState,
            ) -> Vec<OrchestratorCommand> {
                Vec::new()
            }

            fn on_command(&self, command: &OrchestratorCommand, _state: &LayerState) -> bool {
                // Block all stop commands
                !matches!(command, OrchestratorCommand::Stop { .. })
            }

            fn name(&self) -> &'static str {
                "blocking"
            }
        }

        let engine = LayeredEngine::new().with_layer(BlockingLayer);

        let allowed = engine.execute_command(&OrchestratorCommand::Pause {
            reason: "test".to_string(),
        });
        assert!(allowed);

        let blocked = engine.execute_command(&OrchestratorCommand::Stop {
            reason: "test".to_string(),
        });
        assert!(!blocked);
    }
}
