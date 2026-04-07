//! Tiered Information Model for automation runs.
//!
//! This module implements a tiered information architecture for debugging context:
//!
//! - **Tier 1 (run_details)**: Detailed run data - comprehensive info about each run
//! - **Tier 2 (anomalies)**: Unusual events that deviate from normal patterns
//! - **Tier 3 (derived insights)**: Computed patterns and correlations (in-memory)
//! - **Tier 4 (config_statistics)**: Aggregated statistics per config
//!
//! The AI receives Tier 4 summary first, then can request Tier 1 details for specific runs.
//! This prevents context overflow while providing rich debugging information.
//!
//! ## Recording Module
//!
//! The `recording` module provides automatic run recording via `RunRecorder`.
//! Create a recorder when a workflow starts, feed it events during execution,
//! and call `finish_*` when the workflow completes.
//!
//! ## Event Handler
//!
//! The `event_handler` module provides `RunRecordingHandler` for processing
//! executor events and feeding them to the RunRecorder automatically.

pub mod event_handler;
pub mod recording;
pub mod types;

pub use event_handler::*;
pub use recording::*;
pub use types::*;
