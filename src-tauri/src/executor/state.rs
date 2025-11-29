use serde::{Deserialize, Serialize};
use std::fmt;

/// Represents the lifecycle state of the Python executor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum ExecutorState {
    /// Executor is being initialized, Python process is starting
    Initializing {
        /// Timestamp when initialization started
        started_at: u64,
    },

    /// Executor has sent READY signal and is ready to accept commands
    Ready {
        /// Timestamp when ready state was entered
        ready_at: u64,
    },

    /// Executor is currently processing commands
    Running {
        /// Timestamp when current execution started
        started_at: u64,
    },

    /// Executor has failed and cannot process commands
    Failed {
        /// Error message describing the failure
        error: String,
        /// Timestamp when failure occurred
        failed_at: u64,
    },

    /// Executor is shutting down gracefully
    Shutdown {
        /// Timestamp when shutdown was initiated
        shutdown_at: u64,
    },
}

impl ExecutorState {
    /// Creates a new Initializing state with current timestamp
    #[allow(dead_code)]
    pub fn initializing() -> Self {
        Self::Initializing {
            started_at: current_timestamp(),
        }
    }

    /// Creates a new Ready state with current timestamp
    pub fn ready() -> Self {
        Self::Ready {
            ready_at: current_timestamp(),
        }
    }

    /// Creates a new Running state with current timestamp
    #[allow(dead_code)]
    pub fn running() -> Self {
        Self::Running {
            started_at: current_timestamp(),
        }
    }

    /// Creates a new Failed state with error message and current timestamp
    pub fn failed(error: String) -> Self {
        Self::Failed {
            error,
            failed_at: current_timestamp(),
        }
    }

    /// Creates a new Shutdown state with current timestamp
    pub fn shutdown() -> Self {
        Self::Shutdown {
            shutdown_at: current_timestamp(),
        }
    }

    /// Returns true if the executor can accept commands in this state
    pub fn can_accept_commands(&self) -> bool {
        matches!(
            self,
            ExecutorState::Ready { .. } | ExecutorState::Running { .. }
        )
    }

    /// Returns true if the executor is in a terminal state (Failed or Shutdown)
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ExecutorState::Failed { .. } | ExecutorState::Shutdown { .. }
        )
    }

    /// Returns true if the executor is initializing
    pub fn is_initializing(&self) -> bool {
        matches!(self, ExecutorState::Initializing { .. })
    }

    /// Returns true if the executor is ready
    pub fn is_ready(&self) -> bool {
        matches!(self, ExecutorState::Ready { .. })
    }

    /// Returns true if the executor is running
    #[allow(dead_code)]
    pub fn is_running(&self) -> bool {
        matches!(self, ExecutorState::Running { .. })
    }

    /// Returns a human-readable name for the state
    pub fn name(&self) -> &'static str {
        match self {
            ExecutorState::Initializing { .. } => "Initializing",
            ExecutorState::Ready { .. } => "Ready",
            ExecutorState::Running { .. } => "Running",
            ExecutorState::Failed { .. } => "Failed",
            ExecutorState::Shutdown { .. } => "Shutdown",
        }
    }

    /// Validates state transition and returns error if invalid
    pub fn can_transition_to(&self, next: &ExecutorState) -> Result<(), String> {
        use ExecutorState::*;

        match (self, next) {
            // From Initializing
            (Initializing { .. }, Ready { .. }) => Ok(()),
            (Initializing { .. }, Failed { .. }) => Ok(()),
            (Initializing { .. }, Shutdown { .. }) => Ok(()),

            // From Ready
            (Ready { .. }, Running { .. }) => Ok(()),
            (Ready { .. }, Failed { .. }) => Ok(()),
            (Ready { .. }, Shutdown { .. }) => Ok(()),

            // From Running
            (Running { .. }, Ready { .. }) => Ok(()),
            (Running { .. }, Failed { .. }) => Ok(()),
            (Running { .. }, Shutdown { .. }) => Ok(()),

            // From Failed (can only shutdown or restart via new initialization)
            (Failed { .. }, Shutdown { .. }) => Ok(()),
            (Failed { .. }, Initializing { .. }) => Ok(()), // Allow restart

            // From Shutdown (terminal, no transitions allowed except restart)
            (Shutdown { .. }, Initializing { .. }) => Ok(()), // Allow restart

            // All other transitions are invalid
            _ => Err(format!(
                "Invalid state transition: {} -> {}",
                self.name(),
                next.name()
            )),
        }
    }
}

impl fmt::Display for ExecutorState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecutorState::Initializing { started_at } => {
                write!(f, "Initializing (started at {})", started_at)
            }
            ExecutorState::Ready { ready_at } => {
                write!(f, "Ready (ready at {})", ready_at)
            }
            ExecutorState::Running { started_at } => {
                write!(f, "Running (started at {})", started_at)
            }
            ExecutorState::Failed { error, failed_at } => {
                write!(f, "Failed at {}: {}", failed_at, error)
            }
            ExecutorState::Shutdown { shutdown_at } => {
                write!(f, "Shutdown (at {})", shutdown_at)
            }
        }
    }
}

/// Returns current Unix timestamp in milliseconds
fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("System time before Unix epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_creation() {
        let state = ExecutorState::initializing();
        assert!(state.is_initializing());
        assert!(!state.can_accept_commands());
    }

    #[test]
    fn test_valid_transitions() {
        let init = ExecutorState::initializing();
        let ready = ExecutorState::ready();

        assert!(init.can_transition_to(&ready).is_ok());
        assert!(ready.can_transition_to(&ExecutorState::running()).is_ok());
    }

    #[test]
    fn test_invalid_transitions() {
        let init = ExecutorState::initializing();
        let running = ExecutorState::running();

        // Cannot go directly from Initializing to Running
        assert!(init.can_transition_to(&running).is_err());
    }

    #[test]
    fn test_terminal_states() {
        assert!(ExecutorState::failed("error".to_string()).is_terminal());
        assert!(ExecutorState::shutdown().is_terminal());
        assert!(!ExecutorState::ready().is_terminal());
    }

    #[test]
    fn test_command_acceptance() {
        assert!(!ExecutorState::initializing().can_accept_commands());
        assert!(ExecutorState::ready().can_accept_commands());
        assert!(ExecutorState::running().can_accept_commands());
        assert!(!ExecutorState::failed("error".to_string()).can_accept_commands());
        assert!(!ExecutorState::shutdown().can_accept_commands());
    }
}
