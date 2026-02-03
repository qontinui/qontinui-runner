//! State Transition Types
//!
//! Defines the structures for tracking state transitions in workflows.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A state transition record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTransition<S> {
    /// The state before the transition.
    pub from_state: S,
    /// The state after the transition.
    pub to_state: S,
    /// When the transition occurred (RFC3339 timestamp).
    pub timestamp: String,
    /// Optional reason for the transition.
    pub reason: Option<String>,
}

impl<S: fmt::Debug> fmt::Display for StateTransition<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(reason) = &self.reason {
            write!(
                f,
                "{:?} -> {:?} at {} ({})",
                self.from_state, self.to_state, self.timestamp, reason
            )
        } else {
            write!(
                f,
                "{:?} -> {:?} at {}",
                self.from_state, self.to_state, self.timestamp
            )
        }
    }
}

/// Error type for state transition failures.
#[derive(Debug, Clone)]
pub enum StateTransitionError {
    /// The transition is not allowed from the current state.
    InvalidTransition {
        from: String,
        to: String,
        reason: String,
    },
    /// The workflow is in a terminal state and cannot transition.
    TerminalState { state: String },
    /// Database error during persistence.
    PersistenceError(String),
}

impl fmt::Display for StateTransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateTransitionError::InvalidTransition { from, to, reason } => {
                write!(
                    f,
                    "Invalid transition from '{}' to '{}': {}",
                    from, to, reason
                )
            }
            StateTransitionError::TerminalState { state } => {
                write!(f, "Cannot transition from terminal state '{}'", state)
            }
            StateTransitionError::PersistenceError(msg) => {
                write!(f, "Failed to persist state transition: {}", msg)
            }
        }
    }
}

impl std::error::Error for StateTransitionError {}
