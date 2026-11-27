//! Per-phase timeout configuration for unified workflows.
//!
//! A workflow stores its timeouts as `phase_timeouts_json`.
//! `LoopConfig::from_workflow()` (`unified_workflow_executor/types.rs`) parses
//! that into [`PhaseTimeouts`] and keeps `execution_ms` as the loop's
//! `phase_timeout_ms`, which `handle_check_preconditions()`
//! (`unified_workflow_executor/loop_handlers.rs`) enforces.
//!
//! This file used to hold the orchestrator's task state machine
//! (`TaskOrchestrator`, `TaskState`, `TransitionReason`). No production path
//! constructed it, so it was deleted and only the timeout config remains.

use serde::{Deserialize, Serialize};

/// Per-phase timeout configuration (milliseconds), in the shape a workflow's
/// `phase_timeouts_json` stores it.
///
/// Only `execution_ms` is enforced: `LoopConfig::from_workflow()` keeps it as
/// `phase_timeout_ms`, and the unified loop completes with
/// `CompletionReason::PhaseTimeout` once the agentic phase runs past it. The
/// other fields are parsed and not read.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PhaseTimeouts {
    /// Timeout for the Setup phase (ms).
    pub setup_ms: Option<u64>,
    /// Timeout for the Execution (agentic) phase (ms).
    pub execution_ms: Option<u64>,
    /// Timeout for the DeterministicVerification phase (ms).
    pub verification_ms: Option<u64>,
    /// Timeout for the AiVerification phase (ms).
    pub ai_verification_ms: Option<u64>,
    /// Timeout for the Completion phase (ms).
    pub completion_ms: Option<u64>,
}
