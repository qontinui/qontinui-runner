//! The phase a workflow step or AI session runs in.
//!
//! Lives beside the step event registry, its main consumer (`StepEventKind`,
//! `StepKey`, `StepMetadata`, `ExecutionContext`).

use serde::{Deserialize, Serialize};

/// Phase of workflow execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPhase {
    Setup,
    Verification,
    Agentic,
    Completion,
}

impl WorkflowPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkflowPhase::Setup => "setup",
            WorkflowPhase::Verification => "verification",
            WorkflowPhase::Agentic => "agentic",
            WorkflowPhase::Completion => "completion",
        }
    }
}
