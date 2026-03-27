//! Step Event Logger Facade
//!
//! Provides a unified interface for logging step events.
//! This facade combines the tracker, policy, and event builder
//! to ensure consistent, deduplicated logging.

use std::sync::Arc;
use tracing::warn;

use crate::database::pg::PgDb;
use crate::database::CheckpointDb;
use crate::step_event_builder::StepEventBuilder;
use crate::step_metadata::{StepDetails, StepMetadata};
use crate::unified_workflow_executor::get_parent_task_id;

use super::definitions::StepEventKind;
use super::policy::{PolicyViolation, StepKey};
use super::tracker::TrackerHandle;

/// Error type for step event logging.
#[derive(Debug)]
pub enum LogError {
    /// Logging policy was violated (duplicate event, missing start, etc.)
    PolicyViolation(PolicyViolation),
    /// Database error while creating the event
    DatabaseError(String),
}

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogError::PolicyViolation(v) => write!(f, "Policy violation: {}", v),
            LogError::DatabaseError(e) => write!(f, "Database error: {}", e),
        }
    }
}

impl std::error::Error for LogError {}

impl From<PolicyViolation> for LogError {
    fn from(v: PolicyViolation) -> Self {
        LogError::PolicyViolation(v)
    }
}

/// Unified facade for step event logging.
///
/// This is the single entry point for all step event logging in the
/// unified workflow executor. It ensures:
/// - Policy validation (no duplicates, proper sequencing)
/// - Consistent event format (via StepEventBuilder)
/// - Event tracking (via TrackerHandle)
#[derive(Clone)]
pub struct StepEventLogger {
    /// Handle to the execution tracker
    tracker: TrackerHandle,
    /// Database for persisting events
    checkpoint_db: Arc<CheckpointDb>,
    /// PostgreSQL database (PG-primary, SQLite fallback)
    pg_db: Arc<PgDb>,
    /// Workflow name for event context
    workflow_name: String,
}

impl std::fmt::Debug for StepEventLogger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StepEventLogger")
            .field("tracker", &self.tracker)
            .field("checkpoint_db", &"<CheckpointDb>")
            .field("pg_db", &"<PgDb>")
            .field("workflow_name", &self.workflow_name)
            .finish()
    }
}

impl StepEventLogger {
    /// Create a new step event logger.
    ///
    /// Note: For composed run children (e.g., composed-run-X-workflow-N),
    /// the execution_id is automatically remapped to the parent task ID because
    /// only parent IDs exist in task_runs (required by foreign key constraint).
    pub fn new(
        checkpoint_db: Arc<CheckpointDb>,
        pg_db: Arc<PgDb>,
        execution_id: impl Into<String>,
        workflow_name: impl Into<String>,
    ) -> Self {
        // For composed run children, remap to parent ID to satisfy FK constraint
        let exec_id = execution_id.into();
        let parent_id = get_parent_task_id(&exec_id);

        Self {
            tracker: TrackerHandle::new(parent_id),
            checkpoint_db,
            pg_db,
            workflow_name: workflow_name.into(),
        }
    }

    /// Create a no-op logger that doesn't persist events.
    ///
    /// This is useful for the Executor trait implementations where
    /// we don't have access to a borrowed logger. The events won't
    /// be persisted to the database, but the execution will still work.
    ///
    /// For full logging support, use the direct run_* methods with a
    /// proper StepEventLogger instance.
    pub fn noop(pg_db: Arc<PgDb>) -> Self {
        // Use a minimal in-memory database that won't persist anything
        // This is a workaround - events logged to this logger are silently discarded
        // because we use an in-memory database that's not shared.
        let db = CheckpointDb::new_in_memory().expect("Failed to create in-memory db");
        Self {
            tracker: TrackerHandle::new("noop"),
            checkpoint_db: Arc::new(db),
            pg_db,
            workflow_name: String::new(),
        }
    }

    /// Get the execution ID this logger is for.
    pub fn execution_id(&self) -> String {
        self.tracker.execution_id()
    }

    /// Log a start event for a step.
    ///
    /// This method:
    /// 1. Validates that the step hasn't been started already
    /// 2. Creates the event using StepEventBuilder
    /// 3. Persists the event to the database
    /// 4. Records the start in the tracker
    ///
    /// If the policy is violated, it logs a warning and returns an error
    /// but does NOT crash the workflow.
    pub fn log_start(
        &self,
        _kind: StepEventKind,
        metadata: StepMetadata,
        details: StepDetails,
    ) -> Result<(), LogError> {
        let key = StepKey::from_metadata(&metadata);

        // Check policy
        if let Err(violation) = self.tracker.can_log_start(&key) {
            warn!(
                "Step event logging policy violation: {} - event not logged",
                violation
            );
            return Err(LogError::PolicyViolation(violation));
        }

        // Build and log the event
        let builder = StepEventBuilder::new(self.tracker.execution_id(), metadata)
            .with_details(details)
            .with_workflow_name(&self.workflow_name);

        let event = builder.build_start();

        // PG-primary: fire-and-forget async write to PostgreSQL
        {
            let pg = self.pg_db.clone();
            let event_clone = event.clone();
            tokio::spawn(async move {
                if let Err(e) = pg.create_task_run_event(&event_clone).await {
                    tracing::warn!("PG step start event write failed: {}", e);
                }
            });
        }
        if let Err(e) = self.checkpoint_db.create_task_run_event(&event) {
            warn!("Failed to log step start event: {}", e);
            return Err(LogError::DatabaseError(e.to_string()));
        }

        // Record successful logging
        self.tracker.record_start(&key);
        Ok(())
    }

    /// Log a complete event for a step.
    ///
    /// This method:
    /// 1. Validates that the step was started and hasn't ended already
    /// 2. Creates the event using StepEventBuilder
    /// 3. Persists the event to the database
    /// 4. Records the end in the tracker
    pub fn log_complete(
        &self,
        _kind: StepEventKind,
        metadata: StepMetadata,
        details: StepDetails,
        duration_ms: i64,
    ) -> Result<(), LogError> {
        let key = StepKey::from_metadata(&metadata);

        // Check policy
        if let Err(violation) = self.tracker.can_log_end(&key) {
            warn!(
                "Step event logging policy violation: {} - event not logged",
                violation
            );
            return Err(LogError::PolicyViolation(violation));
        }

        // Build and log the event
        let builder = StepEventBuilder::new(self.tracker.execution_id(), metadata)
            .with_details(details)
            .with_workflow_name(&self.workflow_name);

        let event = builder.build_complete(duration_ms);

        // PG-primary: fire-and-forget async write to PostgreSQL
        {
            let pg = self.pg_db.clone();
            let event_clone = event.clone();
            tokio::spawn(async move {
                if let Err(e) = pg.create_task_run_event(&event_clone).await {
                    tracing::warn!("PG step complete event write failed: {}", e);
                }
            });
        }
        if let Err(e) = self.checkpoint_db.create_task_run_event(&event) {
            warn!("Failed to log step complete event: {}", e);
            return Err(LogError::DatabaseError(e.to_string()));
        }

        // Record successful logging
        self.tracker.record_end(&key);
        Ok(())
    }

    /// Log an error event for a step.
    ///
    /// This method:
    /// 1. Validates that the step was started and hasn't ended already
    /// 2. Creates the event using StepEventBuilder
    /// 3. Persists the event to the database
    /// 4. Records the end in the tracker
    pub fn log_error(
        &self,
        _kind: StepEventKind,
        metadata: StepMetadata,
        details: StepDetails,
        duration_ms: i64,
        error: Option<&str>,
    ) -> Result<(), LogError> {
        let key = StepKey::from_metadata(&metadata);

        // Check policy
        if let Err(violation) = self.tracker.can_log_end(&key) {
            warn!(
                "Step event logging policy violation: {} - event not logged",
                violation
            );
            return Err(LogError::PolicyViolation(violation));
        }

        // Build and log the event
        let builder = StepEventBuilder::new(self.tracker.execution_id(), metadata)
            .with_details(details)
            .with_workflow_name(&self.workflow_name);

        let event = builder.build_error(duration_ms, error);

        // PG-primary: fire-and-forget async write to PostgreSQL
        {
            let pg = self.pg_db.clone();
            let event_clone = event.clone();
            tokio::spawn(async move {
                if let Err(e) = pg.create_task_run_event(&event_clone).await {
                    tracing::warn!("PG step error event write failed: {}", e);
                }
            });
        }
        if let Err(e) = self.checkpoint_db.create_task_run_event(&event) {
            warn!("Failed to log step error event: {}", e);
            return Err(LogError::DatabaseError(e.to_string()));
        }

        // Record successful logging
        self.tracker.record_end(&key);
        Ok(())
    }

    /// Log a complete or error event based on success flag.
    ///
    /// Convenience method for the common pattern of logging either
    /// a complete or error event based on a success boolean.
    pub fn log_end(
        &self,
        kind: StepEventKind,
        metadata: StepMetadata,
        details: StepDetails,
        duration_ms: i64,
        success: bool,
        error: Option<&str>,
    ) -> Result<(), LogError> {
        if success {
            self.log_complete(kind, metadata, details, duration_ms)
        } else {
            self.log_error(kind, metadata, details, duration_ms, error)
        }
    }

    /// Get the tracker for this logger (for testing/debugging).
    #[allow(dead_code)]
    pub(crate) fn tracker(&self) -> &TrackerHandle {
        &self.tracker
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step_types::StepType;
    use crate::unified_workflow_executor::WorkflowPhase;

    // Note: These tests would require mocking the CheckpointDb.
    // For now, we test the policy enforcement logic.

    #[test]
    fn test_step_key_from_metadata() {
        let metadata =
            StepMetadata::verification("task-123", StepType::Playwright, "Test Step", 3, 2);

        let key = StepKey::from_metadata(&metadata);
        assert_eq!(key.phase, WorkflowPhase::Verification);
        assert_eq!(key.step_type, StepType::Playwright);
        assert_eq!(key.step_index, 3);
        assert_eq!(key.iteration, Some(2));
    }
}
