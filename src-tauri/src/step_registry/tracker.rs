//! Execution Tracker Module
//!
//! Tracks step events per execution to enforce logging policy.
//! This is the stateful component that remembers which steps have
//! been started and ended.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use super::policy::{LoggingPolicy, PolicyViolation, StepKey};

/// Tracks step events for a single workflow execution.
///
/// This tracker maintains sets of started and ended steps to
/// enforce the logging policy:
/// - No duplicate starts
/// - Require start before end
/// - No duplicate ends
#[derive(Debug)]
pub struct ExecutionTracker {
    /// The execution ID this tracker is for
    execution_id: String,
    /// Steps that have been started (received start event)
    started_steps: HashSet<StepKey>,
    /// Steps that have ended (received complete or error event)
    ended_steps: HashSet<StepKey>,
}

impl ExecutionTracker {
    /// Create a new tracker for the given execution ID.
    pub fn new(execution_id: impl Into<String>) -> Self {
        Self {
            execution_id: execution_id.into(),
            started_steps: HashSet::new(),
            ended_steps: HashSet::new(),
        }
    }

    /// Get the execution ID this tracker is for.
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    /// Check if a start event can be logged for this step.
    pub fn can_log_start(&self, key: &StepKey) -> Result<(), PolicyViolation> {
        let has_started = self.started_steps.contains(key);
        LoggingPolicy::validate_start(has_started, key)
    }

    /// Check if an end event can be logged for this step.
    pub fn can_log_end(&self, key: &StepKey) -> Result<(), PolicyViolation> {
        let has_started = self.started_steps.contains(key);
        let has_ended = self.ended_steps.contains(key);
        LoggingPolicy::validate_end(has_started, has_ended, key)
    }

    /// Record that a start event was logged for this step.
    ///
    /// This should be called AFTER the event is successfully logged.
    pub fn record_start(&mut self, key: &StepKey) {
        self.started_steps.insert(key.clone());
    }

    /// Record that an end event was logged for this step.
    ///
    /// This should be called AFTER the event is successfully logged.
    pub fn record_end(&mut self, key: &StepKey) {
        self.ended_steps.insert(key.clone());
    }

    /// Get the number of started steps.
    #[allow(dead_code)]
    pub fn started_count(&self) -> usize {
        self.started_steps.len()
    }

    /// Get the number of ended steps.
    #[allow(dead_code)]
    pub fn ended_count(&self) -> usize {
        self.ended_steps.len()
    }

    /// Get the number of steps that are currently in progress (started but not ended).
    #[allow(dead_code)]
    pub fn in_progress_count(&self) -> usize {
        self.started_steps.difference(&self.ended_steps).count()
    }
}

/// Thread-safe handle to an ExecutionTracker.
///
/// This wrapper allows the tracker to be shared between phase executors
/// while maintaining thread safety. It uses an Arc<RwLock<>> internally.
#[derive(Debug, Clone)]
pub struct TrackerHandle {
    inner: Arc<RwLock<ExecutionTracker>>,
}

impl TrackerHandle {
    /// Create a new tracker handle for the given execution ID.
    pub fn new(execution_id: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(ExecutionTracker::new(execution_id))),
        }
    }

    /// Get the execution ID this tracker is for.
    pub fn execution_id(&self) -> String {
        self.inner
            .read()
            .expect("TrackerHandle lock poisoned")
            .execution_id()
            .to_string()
    }

    /// Check if a start event can be logged for this step.
    pub fn can_log_start(&self, key: &StepKey) -> Result<(), PolicyViolation> {
        self.inner
            .read()
            .expect("TrackerHandle lock poisoned")
            .can_log_start(key)
    }

    /// Check if an end event can be logged for this step.
    pub fn can_log_end(&self, key: &StepKey) -> Result<(), PolicyViolation> {
        self.inner
            .read()
            .expect("TrackerHandle lock poisoned")
            .can_log_end(key)
    }

    /// Record that a start event was logged for this step.
    pub fn record_start(&self, key: &StepKey) {
        self.inner
            .write()
            .expect("TrackerHandle lock poisoned")
            .record_start(key)
    }

    /// Record that an end event was logged for this step.
    pub fn record_end(&self, key: &StepKey) {
        self.inner
            .write()
            .expect("TrackerHandle lock poisoned")
            .record_end(key)
    }

    /// Get the number of started steps.
    #[allow(dead_code)]
    pub fn started_count(&self) -> usize {
        self.inner
            .read()
            .expect("TrackerHandle lock poisoned")
            .started_count()
    }

    /// Get the number of ended steps.
    #[allow(dead_code)]
    pub fn ended_count(&self) -> usize {
        self.inner
            .read()
            .expect("TrackerHandle lock poisoned")
            .ended_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step_types::StepType;
    use crate::unified_workflow_executor::WorkflowPhase;

    #[test]
    fn test_tracker_lifecycle() {
        let mut tracker = ExecutionTracker::new("test-exec-1");

        let key = StepKey {
            phase: WorkflowPhase::Setup,
            step_type: StepType::Prompt,
            step_index: 0,
            iteration: None,
        };

        // Step starts
        assert!(tracker.can_log_start(&key).is_ok());
        tracker.record_start(&key);
        assert_eq!(tracker.started_count(), 1);
        assert_eq!(tracker.in_progress_count(), 1);

        // Duplicate start fails
        assert!(tracker.can_log_start(&key).is_err());

        // Step ends
        assert!(tracker.can_log_end(&key).is_ok());
        tracker.record_end(&key);
        assert_eq!(tracker.ended_count(), 1);
        assert_eq!(tracker.in_progress_count(), 0);

        // Duplicate end fails
        assert!(tracker.can_log_end(&key).is_err());
    }

    #[test]
    fn test_tracker_multiple_steps() {
        let mut tracker = ExecutionTracker::new("test-exec-2");

        let key1 = StepKey::setup(StepType::ShellCommand, 0);
        let key2 = StepKey::setup(StepType::Prompt, 1);

        // Start both
        tracker.record_start(&key1);
        tracker.record_start(&key2);
        assert_eq!(tracker.started_count(), 2);
        assert_eq!(tracker.in_progress_count(), 2);

        // End first
        tracker.record_end(&key1);
        assert_eq!(tracker.ended_count(), 1);
        assert_eq!(tracker.in_progress_count(), 1);

        // End second
        tracker.record_end(&key2);
        assert_eq!(tracker.ended_count(), 2);
        assert_eq!(tracker.in_progress_count(), 0);
    }

    #[test]
    fn test_tracker_handle_thread_safety() {
        use std::thread;

        let handle = TrackerHandle::new("test-exec-3");
        let key = StepKey::setup(StepType::Prompt, 0);

        // Clone handle for another thread
        let handle_clone = handle.clone();
        let key_clone = key.clone();

        // Start in main thread
        assert!(handle.can_log_start(&key).is_ok());
        handle.record_start(&key);

        // Try to start again in another thread (should fail)
        let result = thread::spawn(move || handle_clone.can_log_start(&key_clone))
            .join()
            .unwrap();

        assert!(result.is_err());
    }
}
