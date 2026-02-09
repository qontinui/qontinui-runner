//! Session state machine for interactive Claude CLI sessions.
//!
//! States: Created → Initializing → Ready ⇄ Processing → Ready
//!                                        ↘ Interrupting → Ready
//! Any state → Closing → Closed

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::{Arc, Mutex};

/// Session lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Session created but CLI not yet spawned.
    Created,
    /// CLI spawned, waiting for init handshake.
    Initializing,
    /// Ready to accept user messages.
    Ready,
    /// Processing a turn (waiting for result).
    Processing,
    /// Interrupt requested, waiting for CLI to acknowledge.
    Interrupting,
    /// Graceful shutdown in progress.
    Closing,
    /// Session fully closed.
    Closed,
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionState::Created => write!(f, "created"),
            SessionState::Initializing => write!(f, "initializing"),
            SessionState::Ready => write!(f, "ready"),
            SessionState::Processing => write!(f, "processing"),
            SessionState::Interrupting => write!(f, "interrupting"),
            SessionState::Closing => write!(f, "closing"),
            SessionState::Closed => write!(f, "closed"),
        }
    }
}

impl SessionState {
    /// Whether the session can accept a new user message.
    pub fn can_send_message(&self) -> bool {
        matches!(self, SessionState::Ready | SessionState::Processing)
    }

    /// Whether the session can be interrupted.
    pub fn can_interrupt(&self) -> bool {
        matches!(self, SessionState::Processing)
    }

    /// Whether the session is terminal.
    pub fn is_terminal(&self) -> bool {
        matches!(self, SessionState::Closed)
    }

    /// Whether the session is active (not closed/closing).
    pub fn is_active(&self) -> bool {
        !matches!(self, SessionState::Closing | SessionState::Closed)
    }

    /// Convert to the string used in frontend events.
    pub fn as_event_str(&self) -> &str {
        match self {
            SessionState::Created => "created",
            SessionState::Initializing => "initializing",
            SessionState::Ready => "ready",
            SessionState::Processing => "processing",
            SessionState::Interrupting => "interrupting",
            SessionState::Closing => "closing",
            SessionState::Closed => "closed",
        }
    }
}

/// Thread-safe state tracker with transition validation.
#[derive(Debug, Clone)]
pub struct SessionStateTracker {
    state: Arc<Mutex<SessionState>>,
}

impl SessionStateTracker {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(SessionState::Created)),
        }
    }

    /// Get the current state.
    pub fn get(&self) -> SessionState {
        *self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Attempt a state transition. Returns Ok(new_state) or Err with reason.
    pub fn transition(&self, to: SessionState) -> Result<SessionState, String> {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let from = *guard;

        if is_valid_transition(from, to) {
            *guard = to;
            Ok(to)
        } else {
            Err(format!("Invalid state transition: {} -> {}", from, to))
        }
    }

    /// Force transition to Closing/Closed (always valid from any state).
    pub fn force_close(&self) -> SessionState {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if *guard != SessionState::Closed {
            *guard = SessionState::Closed;
        }
        SessionState::Closed
    }
}

/// Check if a state transition is valid.
fn is_valid_transition(from: SessionState, to: SessionState) -> bool {
    use SessionState::*;
    matches!(
        (from, to),
        // Normal flow
        (Created, Initializing)
            | (Initializing, Ready)
            | (Ready, Processing)
            | (Processing, Ready)
            | (Interrupting, Ready)
            // Interrupt
            | (Processing, Interrupting)
            // Closing from any active state
            | (Created, Closing)
            | (Initializing, Closing)
            | (Ready, Closing)
            | (Processing, Closing)
            | (Interrupting, Closing)
            | (Closing, Closed)
            // Direct close from any state (force_close)
            | (Created, Closed)
            | (Initializing, Closed)
            | (Ready, Closed)
            | (Processing, Closed)
            | (Interrupting, Closed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_lifecycle() {
        let tracker = SessionStateTracker::new();
        assert_eq!(tracker.get(), SessionState::Created);

        tracker.transition(SessionState::Initializing).unwrap();
        assert_eq!(tracker.get(), SessionState::Initializing);

        tracker.transition(SessionState::Ready).unwrap();
        assert_eq!(tracker.get(), SessionState::Ready);

        tracker.transition(SessionState::Processing).unwrap();
        assert_eq!(tracker.get(), SessionState::Processing);

        tracker.transition(SessionState::Ready).unwrap();
        assert_eq!(tracker.get(), SessionState::Ready);
    }

    #[test]
    fn test_interrupt_flow() {
        let tracker = SessionStateTracker::new();
        tracker.transition(SessionState::Initializing).unwrap();
        tracker.transition(SessionState::Ready).unwrap();
        tracker.transition(SessionState::Processing).unwrap();
        tracker.transition(SessionState::Interrupting).unwrap();
        assert_eq!(tracker.get(), SessionState::Interrupting);
        tracker.transition(SessionState::Ready).unwrap();
        assert_eq!(tracker.get(), SessionState::Ready);
    }

    #[test]
    fn test_invalid_transition() {
        let tracker = SessionStateTracker::new();
        assert!(tracker.transition(SessionState::Ready).is_err());
        assert!(tracker.transition(SessionState::Processing).is_err());
    }

    #[test]
    fn test_force_close() {
        let tracker = SessionStateTracker::new();
        tracker.transition(SessionState::Initializing).unwrap();
        tracker.transition(SessionState::Ready).unwrap();
        tracker.transition(SessionState::Processing).unwrap();
        let state = tracker.force_close();
        assert_eq!(state, SessionState::Closed);
        assert_eq!(tracker.get(), SessionState::Closed);
    }

    #[test]
    fn test_close_from_any_state() {
        // Can close from any state
        for initial in &[
            SessionState::Created,
            SessionState::Initializing,
            SessionState::Ready,
            SessionState::Processing,
            SessionState::Interrupting,
        ] {
            let tracker = SessionStateTracker::new();
            // Get to the desired state
            match initial {
                SessionState::Created => {}
                SessionState::Initializing => {
                    tracker.transition(SessionState::Initializing).unwrap();
                }
                SessionState::Ready => {
                    tracker.transition(SessionState::Initializing).unwrap();
                    tracker.transition(SessionState::Ready).unwrap();
                }
                SessionState::Processing => {
                    tracker.transition(SessionState::Initializing).unwrap();
                    tracker.transition(SessionState::Ready).unwrap();
                    tracker.transition(SessionState::Processing).unwrap();
                }
                SessionState::Interrupting => {
                    tracker.transition(SessionState::Initializing).unwrap();
                    tracker.transition(SessionState::Ready).unwrap();
                    tracker.transition(SessionState::Processing).unwrap();
                    tracker.transition(SessionState::Interrupting).unwrap();
                }
                _ => {}
            }
            assert!(tracker.transition(SessionState::Closed).is_ok());
        }
    }
}
