//! Shared Workflow State Management
//!
//! This module provides a unified state machine infrastructure that can be used
//! by multiple workflow types (Unified Workflow, Orchestrator, GUI Automation).
//!
//! ## Key Components
//!
//! - `WorkflowState` trait: Defines common behavior for workflow states
//! - `StateMachine<S>`: Generic state machine that works with any WorkflowState
//! - `CheckpointManager`: Manages step-level checkpointing for resume capability
//!
//! ## Usage
//!
//! ```ignore
//! use workflow_state::{WorkflowState, StateMachine, CheckpointManager};
//!
//! // Define your workflow states by implementing WorkflowState
//! #[derive(Clone, Serialize, Deserialize)]
//! enum MyWorkflowState {
//!     Created,
//!     Running,
//!     Completed,
//!     Failed { reason: String },
//! }
//!
//! impl WorkflowState for MyWorkflowState {
//!     fn name(&self) -> &'static str { ... }
//!     fn is_terminal(&self) -> bool { ... }
//!     fn is_resumable(&self) -> bool { ... }
//! }
//!
//! // Use the state machine
//! let mut sm = StateMachine::new(db, execution_id, MyWorkflowState::Created);
//! sm.transition_to(MyWorkflowState::Running)?;
//! sm.persist()?;
//! ```

mod checkpoint;
mod progress;
mod progress_parser;
mod transitions;

pub use checkpoint::{CheckpointManager, StepCheckpoint, StepCheckpointStatus};
pub use progress::StepProgressMarker;
pub use progress_parser::{ParsedProgress, ProgressParser};
pub mod progress_markers {
    //! Re-export of marker type constants.
    pub use super::progress::marker_types::*;
}
pub use transitions::{StateTransition, StateTransitionError};

use serde::{de::DeserializeOwned, Serialize};
use std::sync::Arc;
use tracing::info;

use crate::database::CheckpointDb;

/// Trait for workflow states that can be used with the generic StateMachine.
///
/// Each workflow type (Unified, Orchestrator, GUI) defines its own state enum
/// that implements this trait.
pub trait WorkflowState: Send + Sync + Clone + Serialize + DeserializeOwned + std::fmt::Debug {
    /// Get the name of the current state (for logging and persistence).
    fn name(&self) -> &'static str;

    /// Check if this is a terminal state (workflow complete, failed, or stopped).
    fn is_terminal(&self) -> bool;

    /// Check if the workflow can be resumed from this state after a restart.
    fn is_resumable(&self) -> bool;

    /// Get the phase name if applicable (for unified workflows).
    fn phase(&self) -> Option<&'static str> {
        None
    }

    /// Get the iteration number if applicable.
    fn iteration(&self) -> Option<u32> {
        None
    }
}

/// Generic state machine that manages workflow state transitions.
///
/// The state machine:
/// - Tracks the current state
/// - Records transition history
/// - Persists state to the database
/// - Supports restoration from a saved state
pub struct StateMachine<S: WorkflowState> {
    /// Unique execution ID (task_run_id)
    execution_id: String,
    /// Workflow type identifier
    workflow_type: String,
    /// Current state
    current_state: S,
    /// History of state transitions
    transition_history: Vec<StateTransition<S>>,
    /// Database connection for persistence
    checkpoint_db: Arc<CheckpointDb>,
}

impl<S: WorkflowState> std::fmt::Debug for StateMachine<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateMachine")
            .field("execution_id", &self.execution_id)
            .field("workflow_type", &self.workflow_type)
            .field("current_state", &self.current_state)
            .field("transition_history_len", &self.transition_history.len())
            .field("checkpoint_db", &"<CheckpointDb>")
            .finish()
    }
}

impl<S: WorkflowState> StateMachine<S> {
    /// Create a new state machine with an initial state.
    pub fn new(
        checkpoint_db: Arc<CheckpointDb>,
        execution_id: impl Into<String>,
        workflow_type: impl Into<String>,
        initial_state: S,
    ) -> Self {
        Self {
            execution_id: execution_id.into(),
            workflow_type: workflow_type.into(),
            current_state: initial_state,
            transition_history: Vec::new(),
            checkpoint_db,
        }
    }

    /// Get the current state.
    pub fn current_state(&self) -> &S {
        &self.current_state
    }

    /// Get the execution ID.
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    /// Get the workflow type.
    pub fn workflow_type(&self) -> &str {
        &self.workflow_type
    }

    /// Get the transition history.
    pub fn transition_history(&self) -> &[StateTransition<S>] {
        &self.transition_history
    }

    /// Transition to a new state.
    ///
    /// This records the transition in history but does NOT persist to the database.
    /// Call `persist()` after transitioning to save the state.
    pub fn transition_to(&mut self, new_state: S) -> Result<(), StateTransitionError> {
        let old_state_name = self.current_state.name();
        let new_state_name = new_state.name();

        // Record the transition
        let transition = StateTransition {
            from_state: self.current_state.clone(),
            to_state: new_state.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            reason: None,
        };

        info!(
            execution_id = %self.execution_id,
            from = %old_state_name,
            to = %new_state_name,
            "Workflow state transition"
        );

        self.transition_history.push(transition);
        self.current_state = new_state;

        Ok(())
    }

    /// Transition to a new state with a reason.
    pub fn transition_to_with_reason(
        &mut self,
        new_state: S,
        reason: impl Into<String>,
    ) -> Result<(), StateTransitionError> {
        let old_state_name = self.current_state.name();
        let new_state_name = new_state.name();
        let reason_str = reason.into();

        let transition = StateTransition {
            from_state: self.current_state.clone(),
            to_state: new_state.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            reason: Some(reason_str.clone()),
        };

        info!(
            execution_id = %self.execution_id,
            from = %old_state_name,
            to = %new_state_name,
            reason = %reason_str,
            "Workflow state transition"
        );

        self.transition_history.push(transition);
        self.current_state = new_state;

        Ok(())
    }

    /// Persist the current state to the database.
    pub fn persist(&self) -> Result<(), String> {
        let state_name = self.current_state.name();
        let state_data = serde_json::to_string(&self.current_state)
            .map_err(|e| format!("Failed to serialize state: {}", e))?;

        self.checkpoint_db.save_workflow_execution_state(
            &self.execution_id,
            &self.workflow_type,
            state_name,
            Some(&state_data),
            self.current_state.phase(),
            self.current_state.iteration(),
        )?;

        Ok(())
    }

    /// Restore a state machine from the database.
    ///
    /// Returns None if no saved state exists for the given execution_id.
    pub fn restore(
        checkpoint_db: Arc<CheckpointDb>,
        execution_id: &str,
        workflow_type: &str,
    ) -> Result<Option<Self>, String> {
        let saved = checkpoint_db.get_workflow_execution_state(execution_id)?;

        match saved {
            Some(state_record) => {
                // Verify workflow type matches
                if state_record.workflow_type != workflow_type {
                    return Err(format!(
                        "Workflow type mismatch: expected '{}', found '{}'",
                        workflow_type, state_record.workflow_type
                    ));
                }

                // Deserialize the state
                let state: S = if let Some(data) = state_record.state_data {
                    serde_json::from_str(&data)
                        .map_err(|e| format!("Failed to deserialize state: {}", e))?
                } else {
                    return Err("State data is missing".to_string());
                };

                Ok(Some(Self {
                    execution_id: execution_id.to_string(),
                    workflow_type: workflow_type.to_string(),
                    current_state: state,
                    transition_history: Vec::new(), // History not restored
                    checkpoint_db,
                }))
            }
            None => Ok(None),
        }
    }

    /// Check if the current state is terminal.
    pub fn is_terminal(&self) -> bool {
        self.current_state.is_terminal()
    }

    /// Check if the workflow can be resumed from the current state.
    pub fn is_resumable(&self) -> bool {
        self.current_state.is_resumable()
    }
}

/// Record stored in the database for workflow execution state.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowExecutionStateRecord {
    pub execution_id: String,
    pub workflow_type: String,
    pub state_name: String,
    pub state_data: Option<String>,
    pub phase: Option<String>,
    pub iteration: Option<u32>,
    pub updated_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    enum TestState {
        Created,
        Running,
        Completed,
        Failed { reason: String },
    }

    impl WorkflowState for TestState {
        fn name(&self) -> &'static str {
            match self {
                TestState::Created => "created",
                TestState::Running => "running",
                TestState::Completed => "completed",
                TestState::Failed { .. } => "failed",
            }
        }

        fn is_terminal(&self) -> bool {
            matches!(self, TestState::Completed | TestState::Failed { .. })
        }

        fn is_resumable(&self) -> bool {
            matches!(self, TestState::Running)
        }
    }

    #[test]
    fn test_state_machine_transitions() {
        let db = CheckpointDb::new_in_memory().expect("Failed to create in-memory db");
        let mut sm = StateMachine::new(
            Arc::new(db),
            "test-exec-1",
            "test",
            TestState::Created,
        );

        assert_eq!(sm.current_state().name(), "created");
        assert!(!sm.is_terminal());

        sm.transition_to(TestState::Running).unwrap();
        assert_eq!(sm.current_state().name(), "running");
        assert!(sm.is_resumable());

        sm.transition_to(TestState::Completed).unwrap();
        assert_eq!(sm.current_state().name(), "completed");
        assert!(sm.is_terminal());
        assert!(!sm.is_resumable());

        assert_eq!(sm.transition_history().len(), 2);
    }

    #[test]
    fn test_state_machine_with_reason() {
        let db = CheckpointDb::new_in_memory().expect("Failed to create in-memory db");
        let mut sm = StateMachine::new(
            Arc::new(db),
            "test-exec-2",
            "test",
            TestState::Created,
        );

        sm.transition_to_with_reason(
            TestState::Failed {
                reason: "Test failure".to_string(),
            },
            "Intentional test failure",
        )
        .unwrap();

        assert!(sm.is_terminal());
        let last_transition = sm.transition_history().last().unwrap();
        assert_eq!(last_transition.reason, Some("Intentional test failure".to_string()));
    }
}
