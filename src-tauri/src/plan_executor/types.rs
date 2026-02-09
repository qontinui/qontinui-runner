//! Plan Executor Data Types
//!
//! Types for structured plan execution where each phase runs as a separate AI session.

use serde::{Deserialize, Serialize};

/// Configuration for a plan execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanConfig {
    /// Name of the plan.
    pub plan_name: String,
    /// Overview/description of the plan.
    pub plan_overview: String,
    /// Ordered list of phases to execute.
    pub phases: Vec<PlanPhase>,
    /// Whether to run a "next steps sweep" after all phases complete.
    pub next_steps_sweep: bool,
    /// Maximum number of sweep iterations before stopping (default 5).
    pub max_next_steps_iterations: u32,
    /// Optional timeout in seconds for each AI session.
    pub timeout_seconds: Option<u64>,
    /// Unique execution ID for this plan run.
    pub execution_id: String,
}

/// A single phase in a plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanPhase {
    /// Human-readable name for this phase.
    pub name: String,
    /// The prompt/instructions for this phase.
    pub prompt: String,
}

/// Result of executing a single phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanPhaseResult {
    /// Zero-based index of this phase.
    pub phase_index: usize,
    /// Name of this phase.
    pub phase_name: String,
    /// Whether this phase succeeded.
    pub success: bool,
    /// AI output from this phase.
    pub output: String,
    /// Duration in milliseconds.
    pub duration_ms: i64,
}

/// Final result of an entire plan execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanResult {
    /// Whether the entire plan succeeded.
    pub success: bool,
    /// Total number of phases in the plan.
    pub total_phases: usize,
    /// Number of phases that completed (successfully or not).
    pub completed_phases: usize,
    /// If a phase failed, which index it failed at (zero-based).
    pub failed_at_phase: Option<usize>,
    /// Whether the next steps sweep ran.
    pub sweep_ran: bool,
    /// How many sweep iterations ran.
    pub sweep_iterations: u32,
    /// Results for each phase.
    pub phase_results: Vec<PlanPhaseResult>,
    /// Results of sweep iterations (empty if sweep didn't run).
    pub sweep_results: Vec<PlanPhaseResult>,
    /// Total duration in milliseconds.
    pub total_duration_ms: u64,
}
