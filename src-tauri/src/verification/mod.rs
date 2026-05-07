//! Runtime verification signals for the agentic loop.
//!
//! Houses the VLM-based World State Verifier (WSM) and its supporting
//! types. The existing text-based verifier agent in
//! [`crate::agentic_verification`] remains the fallback path and is
//! used exclusively when WSM is disabled or errors out.

pub mod mode;
pub mod retry_policy;
pub mod world_state_verifier;

pub use mode::{parse_mode, WsvConfig, WsvMode};
pub use retry_policy::{RefusalRetryTracker, RetryDecision};
pub use world_state_verifier::{TerminalContext, VerifierError, WorldStateVerifier};
