//! AI Verification Agent module
//!
//! This module implements an AI-driven verification system that explores
//! application states using the state machine structure and verifies that
//! reality matches the described expectations.
//!
//! # Architecture
//!
//! The verification agent:
//! 1. Loads a configuration file with states, transitions, and descriptions
//! 2. Uses exploration strategies to traverse the state machine
//! 3. At each state, captures screenshots and logs
//! 4. Compares actual state to expected descriptions
//! 5. Reports discrepancies for AI analysis
//!
//! # Exploration Strategies
//!
//! - **Exhaustive**: Visit every state and transition
//! - **SmokeTest**: Quick path through critical states
//! - **Regression**: Focus on previously-failed areas
//! - **RandomWalk**: Discover unexpected behaviors
//! - **Targeted**: Verify specific states/transitions

mod exploration;
mod report;
mod types;
mod verification_task;

// Re-export types that are used by commands and other modules
pub use exploration::{ExplorationStrategy, StateExplorer, StateMachineGraph};
pub use types::VerificationTaskConfig;
pub use verification_task::VerificationTask;

// These exports are available for external use but not currently used internally
#[allow(unused_imports)]
pub use exploration::{ExplorationPath, StateVisit};
#[allow(unused_imports)]
pub use report::{
    DiscrepancyType, StateVerificationReport, TransitionVerificationReport, VerificationReport,
};
#[allow(unused_imports)]
pub use types::{
    StateVerification, TransitionVerification, VerificationConfig, VerificationPriority,
    VerificationStatus,
};
