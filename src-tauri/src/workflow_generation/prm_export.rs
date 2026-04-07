//! PRM Training Data Export types.
//!
//! The export implementation lives on `PgDb::export_prm_training_data` in
//! `database/pg/tiered_info.rs`. This module retains only the shared
//! result types consumed by the MCP prm_export route.

use serde::{Deserialize, Serialize};

/// A single step definition as it appeared in the workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepDefinition {
    pub step_type: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(default)]
    pub parameters: serde_json::Value,
}

/// Execution result for a single step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionResult {
    Passed,
    Failed { error: String },
    Skipped,
    Timeout,
}

/// Final outcome of the workflow containing this step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalOutcome {
    WorkflowPassed,
    WorkflowFailed,
    StepFixedLater,
}

/// One training example for PRM training.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrmTrainingExample {
    pub run_id: String,
    pub workflow_id: Option<String>,
    pub step_index: usize,
    pub step_definition: StepDefinition,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub criterion: Option<CriterionContext>,
    pub workflow_context: String,
    pub execution_result: ExecutionResult,
    pub iteration: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixer_diff: Option<String>,
    pub final_outcome: FinalOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

/// Context about the acceptance criterion this step verifies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionContext {
    pub id: String,
    pub description: String,
    pub method: String,
    pub priority: String,
}

/// Statistics from a PRM data export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrmExportStats {
    pub total_examples: usize,
    pub passed_count: usize,
    pub failed_count: usize,
    pub fixed_count: usize,
    pub runs_processed: usize,
    pub domains: Vec<String>,
}

/// Serialize training examples to JSONL format.
pub fn examples_to_jsonl(examples: &[PrmTrainingExample]) -> String {
    examples
        .iter()
        .filter_map(|e| serde_json::to_string(e).ok())
        .collect::<Vec<_>>()
        .join("\n")
}
