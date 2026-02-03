//! Execution Events
//!
//! Provides event types for execution monitoring and UI updates.
//! These events are emitted during flow and workflow execution.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Events emitted during flow execution for UI updates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum ExecutionEvent {
    // ========================================================================
    // Flow-Level Events
    // ========================================================================
    /// Flow execution started.
    FlowStarted {
        instance_id: String,
        flow_id: String,
        flow_name: String,
        timestamp: String,
    },
    /// Flow execution completed.
    FlowCompleted {
        instance_id: String,
        flow_id: String,
        success: bool,
        error: Option<String>,
        total_steps: usize,
        duration_ms: u64,
        timestamp: String,
    },
    /// Flow was paused.
    FlowPaused {
        instance_id: String,
        flow_id: String,
        step_id: String,
        reason: String,
        timestamp: String,
    },
    /// Flow was resumed.
    FlowResumed {
        instance_id: String,
        flow_id: String,
        step_id: String,
        timestamp: String,
    },
    /// Flow was cancelled.
    FlowCancelled {
        instance_id: String,
        flow_id: String,
        reason: String,
        timestamp: String,
    },

    // ========================================================================
    // Step-Level Events
    // ========================================================================
    /// A step is about to execute.
    StepStarted {
        instance_id: String,
        step_id: String,
        step_name: String,
        step_type: String,
        timestamp: String,
    },
    /// A step completed.
    StepCompleted {
        instance_id: String,
        step_id: String,
        success: bool,
        outputs: HashMap<String, serde_json::Value>,
        error: Option<String>,
        duration_ms: u64,
        timestamp: String,
    },
    /// A step is being retried.
    StepRetrying {
        instance_id: String,
        step_id: String,
        attempt: u32,
        max_retries: u32,
        error: String,
        timestamp: String,
    },
    /// A step timed out.
    StepTimedOut {
        instance_id: String,
        step_id: String,
        timeout_secs: u64,
        timestamp: String,
    },
    /// A step was skipped (condition not met).
    StepSkipped {
        instance_id: String,
        step_id: String,
        reason: String,
        timestamp: String,
    },

    // ========================================================================
    // Human Input Events
    // ========================================================================
    /// Waiting for human input.
    WaitingForInput {
        instance_id: String,
        step_id: String,
        prompt: String,
        options: Vec<String>,
        timestamp: String,
    },
    /// Human input was received.
    InputReceived {
        instance_id: String,
        step_id: String,
        input: serde_json::Value,
        timestamp: String,
    },

    // ========================================================================
    // Parallel Execution Events
    // ========================================================================
    /// Parallel execution started.
    ParallelStarted {
        instance_id: String,
        step_id: String,
        branch_count: usize,
        timestamp: String,
    },
    /// Progress update during parallel execution.
    ParallelProgress {
        instance_id: String,
        step_id: String,
        completed: usize,
        total: usize,
        successful: usize,
        failed: usize,
        timestamp: String,
    },
    /// A parallel branch started.
    BranchStarted {
        instance_id: String,
        step_id: String,
        branch_id: String,
        timestamp: String,
    },
    /// A parallel branch completed.
    BranchCompleted {
        instance_id: String,
        step_id: String,
        branch_id: String,
        success: bool,
        outputs: HashMap<String, serde_json::Value>,
        error: Option<String>,
        duration_ms: u64,
        timestamp: String,
    },
    /// Parallel execution completed.
    ParallelCompleted {
        instance_id: String,
        step_id: String,
        success: bool,
        branch_results: HashMap<String, bool>,
        duration_ms: u64,
        timestamp: String,
    },

    // ========================================================================
    // Loop Events
    // ========================================================================
    /// Loop started.
    LoopStarted {
        instance_id: String,
        step_id: String,
        total_iterations: usize,
        timestamp: String,
    },
    /// Loop iteration started.
    LoopIterationStarted {
        instance_id: String,
        step_id: String,
        index: usize,
        total: usize,
        timestamp: String,
    },
    /// Loop iteration completed.
    LoopIterationCompleted {
        instance_id: String,
        step_id: String,
        index: usize,
        success: bool,
        outputs: HashMap<String, serde_json::Value>,
        timestamp: String,
    },
    /// Loop completed.
    LoopCompleted {
        instance_id: String,
        step_id: String,
        iterations_completed: usize,
        success: bool,
        timestamp: String,
    },
    /// Loop break triggered.
    LoopBreak {
        instance_id: String,
        step_id: String,
        index: usize,
        reason: String,
        timestamp: String,
    },

    // ========================================================================
    // Context Events
    // ========================================================================
    /// Context was updated.
    ContextUpdated {
        instance_id: String,
        step_id: String,
        updates: HashMap<String, serde_json::Value>,
        timestamp: String,
    },
}

impl ExecutionEvent {
    /// Get the instance ID for this event.
    pub fn instance_id(&self) -> &str {
        match self {
            Self::FlowStarted { instance_id, .. }
            | Self::FlowCompleted { instance_id, .. }
            | Self::FlowPaused { instance_id, .. }
            | Self::FlowResumed { instance_id, .. }
            | Self::FlowCancelled { instance_id, .. }
            | Self::StepStarted { instance_id, .. }
            | Self::StepCompleted { instance_id, .. }
            | Self::StepRetrying { instance_id, .. }
            | Self::StepTimedOut { instance_id, .. }
            | Self::StepSkipped { instance_id, .. }
            | Self::WaitingForInput { instance_id, .. }
            | Self::InputReceived { instance_id, .. }
            | Self::ParallelStarted { instance_id, .. }
            | Self::ParallelProgress { instance_id, .. }
            | Self::BranchStarted { instance_id, .. }
            | Self::BranchCompleted { instance_id, .. }
            | Self::ParallelCompleted { instance_id, .. }
            | Self::LoopStarted { instance_id, .. }
            | Self::LoopIterationStarted { instance_id, .. }
            | Self::LoopIterationCompleted { instance_id, .. }
            | Self::LoopCompleted { instance_id, .. }
            | Self::LoopBreak { instance_id, .. }
            | Self::ContextUpdated { instance_id, .. } => instance_id,
        }
    }

    /// Get the step ID for this event (if applicable).
    pub fn step_id(&self) -> Option<&str> {
        match self {
            Self::FlowStarted { .. }
            | Self::FlowCompleted { .. }
            | Self::FlowCancelled { .. } => None,

            Self::FlowPaused { step_id, .. }
            | Self::FlowResumed { step_id, .. }
            | Self::StepStarted { step_id, .. }
            | Self::StepCompleted { step_id, .. }
            | Self::StepRetrying { step_id, .. }
            | Self::StepTimedOut { step_id, .. }
            | Self::StepSkipped { step_id, .. }
            | Self::WaitingForInput { step_id, .. }
            | Self::InputReceived { step_id, .. }
            | Self::ParallelStarted { step_id, .. }
            | Self::ParallelProgress { step_id, .. }
            | Self::BranchStarted { step_id, .. }
            | Self::BranchCompleted { step_id, .. }
            | Self::ParallelCompleted { step_id, .. }
            | Self::LoopStarted { step_id, .. }
            | Self::LoopIterationStarted { step_id, .. }
            | Self::LoopIterationCompleted { step_id, .. }
            | Self::LoopCompleted { step_id, .. }
            | Self::LoopBreak { step_id, .. }
            | Self::ContextUpdated { step_id, .. } => Some(step_id),
        }
    }

    /// Get the timestamp for this event.
    pub fn timestamp(&self) -> &str {
        match self {
            Self::FlowStarted { timestamp, .. }
            | Self::FlowCompleted { timestamp, .. }
            | Self::FlowPaused { timestamp, .. }
            | Self::FlowResumed { timestamp, .. }
            | Self::FlowCancelled { timestamp, .. }
            | Self::StepStarted { timestamp, .. }
            | Self::StepCompleted { timestamp, .. }
            | Self::StepRetrying { timestamp, .. }
            | Self::StepTimedOut { timestamp, .. }
            | Self::StepSkipped { timestamp, .. }
            | Self::WaitingForInput { timestamp, .. }
            | Self::InputReceived { timestamp, .. }
            | Self::ParallelStarted { timestamp, .. }
            | Self::ParallelProgress { timestamp, .. }
            | Self::BranchStarted { timestamp, .. }
            | Self::BranchCompleted { timestamp, .. }
            | Self::ParallelCompleted { timestamp, .. }
            | Self::LoopStarted { timestamp, .. }
            | Self::LoopIterationStarted { timestamp, .. }
            | Self::LoopIterationCompleted { timestamp, .. }
            | Self::LoopCompleted { timestamp, .. }
            | Self::LoopBreak { timestamp, .. }
            | Self::ContextUpdated { timestamp, .. } => timestamp,
        }
    }

    /// Check if this is a terminal event (flow ended).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::FlowCompleted { .. } | Self::FlowCancelled { .. }
        )
    }

    /// Check if this is an error event.
    pub fn is_error(&self) -> bool {
        match self {
            Self::FlowCompleted { success, .. } => !success,
            Self::StepCompleted { success, .. } => !success,
            Self::StepTimedOut { .. } => true,
            Self::BranchCompleted { success, .. } => !success,
            Self::ParallelCompleted { success, .. } => !success,
            Self::LoopIterationCompleted { success, .. } => !success,
            Self::LoopCompleted { success, .. } => !success,
            _ => false,
        }
    }
}

/// Get the current timestamp in RFC3339 format.
pub fn now_timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Event emitter for execution events.
pub struct EventEmitter {
    /// Callback function for emitting events.
    callback: Option<Box<dyn Fn(ExecutionEvent) + Send + Sync>>,
    /// Whether events are enabled.
    enabled: bool,
}

impl EventEmitter {
    /// Create a new event emitter.
    pub fn new() -> Self {
        Self {
            callback: None,
            enabled: true,
        }
    }

    /// Set the callback function.
    pub fn with_callback<F>(mut self, callback: F) -> Self
    where
        F: Fn(ExecutionEvent) + Send + Sync + 'static,
    {
        self.callback = Some(Box::new(callback));
        self
    }

    /// Enable or disable events.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Emit an event.
    pub fn emit(&self, event: ExecutionEvent) {
        if self.enabled {
            if let Some(callback) = &self.callback {
                callback(event);
            }
        }
    }
}

impl Default for EventEmitter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_instance_id() {
        let event = ExecutionEvent::FlowStarted {
            instance_id: "test-123".to_string(),
            flow_id: "flow-1".to_string(),
            flow_name: "Test Flow".to_string(),
            timestamp: now_timestamp(),
        };

        assert_eq!(event.instance_id(), "test-123");
    }

    #[test]
    fn test_event_step_id() {
        let event = ExecutionEvent::StepStarted {
            instance_id: "test-123".to_string(),
            step_id: "step-1".to_string(),
            step_name: "First Step".to_string(),
            step_type: "agent".to_string(),
            timestamp: now_timestamp(),
        };

        assert_eq!(event.step_id(), Some("step-1"));
    }

    #[test]
    fn test_event_is_terminal() {
        let completed = ExecutionEvent::FlowCompleted {
            instance_id: "test".to_string(),
            flow_id: "flow".to_string(),
            success: true,
            error: None,
            total_steps: 5,
            duration_ms: 1000,
            timestamp: now_timestamp(),
        };
        assert!(completed.is_terminal());

        let started = ExecutionEvent::FlowStarted {
            instance_id: "test".to_string(),
            flow_id: "flow".to_string(),
            flow_name: "Test".to_string(),
            timestamp: now_timestamp(),
        };
        assert!(!started.is_terminal());
    }

    #[test]
    fn test_event_is_error() {
        let success = ExecutionEvent::StepCompleted {
            instance_id: "test".to_string(),
            step_id: "step".to_string(),
            success: true,
            outputs: HashMap::new(),
            error: None,
            duration_ms: 100,
            timestamp: now_timestamp(),
        };
        assert!(!success.is_error());

        let failure = ExecutionEvent::StepCompleted {
            instance_id: "test".to_string(),
            step_id: "step".to_string(),
            success: false,
            outputs: HashMap::new(),
            error: Some("Failed".to_string()),
            duration_ms: 100,
            timestamp: now_timestamp(),
        };
        assert!(failure.is_error());
    }

    #[test]
    fn test_event_emitter() {
        use std::sync::{Arc, Mutex};

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        let emitter = EventEmitter::new().with_callback(move |event| {
            events_clone.lock().unwrap().push(event);
        });

        emitter.emit(ExecutionEvent::FlowStarted {
            instance_id: "test".to_string(),
            flow_id: "flow".to_string(),
            flow_name: "Test".to_string(),
            timestamp: now_timestamp(),
        });

        assert_eq!(events.lock().unwrap().len(), 1);
    }
}
