//! Type definitions for the Test Orchestrator system

use crate::api_request::types::{HttpMethod, VariableExtraction};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// User input for creating a test orchestration plan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestOrchestrationRequest {
    /// IDs of saved API requests to include in the orchestration
    pub request_ids: Vec<String>,
    /// User's description of what the test should verify
    pub test_description: String,
    /// Optional additional context or constraints
    pub context: Option<String>,
}

/// AI-generated execution plan for the test orchestration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestOrchestrationPlan {
    /// Unique identifier for this plan
    pub id: String,
    /// Original user input
    pub request: TestOrchestrationRequest,
    /// Ordered steps to execute
    pub steps: Vec<OrchestrationStep>,
    /// Variables that will be extracted and passed between steps
    pub variable_mappings: Vec<VariableMappingInfo>,
    /// What the AI suggests to verify in the final response
    pub verification_suggestions: Vec<VerificationSuggestion>,
    /// AI's explanation of the plan
    pub explanation: String,
    /// Timestamp when the plan was created
    pub created_at: String,
}

/// A single step in the orchestration plan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationStep {
    /// Step index (0-based)
    pub step_index: usize,
    /// Human-readable name for this step
    pub name: String,
    /// ID of the saved API request to execute
    pub request_id: String,
    /// Variables to extract from this step's response
    pub extractions: Vec<VariableExtraction>,
    /// IDs of steps that must complete before this one
    pub depends_on: Vec<usize>,
    /// Description of what this step does in the context of the test
    pub purpose: String,
    /// URL template with variables (for display)
    pub url_template: String,
    /// Body template with variables (for display, if applicable)
    pub body_template: Option<String>,
}

/// Information about how a variable is passed between steps
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableMappingInfo {
    /// Name of the variable
    pub variable_name: String,
    /// Step index that produces this variable
    pub source_step: usize,
    /// JSON path used to extract the variable
    pub json_path: String,
    /// Step indices that use this variable
    pub used_in_steps: Vec<usize>,
    /// Description of what this variable represents
    pub description: String,
}

/// AI's suggestion for what to verify in the test
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationSuggestion {
    /// What to verify (e.g., "response.states.length > 3")
    pub condition: String,
    /// Human-readable description
    pub description: String,
    /// JSON path to the value to check (if applicable)
    pub json_path: Option<String>,
    /// The step index whose response to check
    pub step_index: usize,
}

/// Result of executing a single step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecutionResult {
    /// Step index
    pub step_index: usize,
    /// Step name
    pub step_name: String,
    /// The full HTTP request that was made
    pub request: ExecutedRequest,
    /// The HTTP response received
    pub response: ExecutedResponse,
    /// Variables extracted from this step
    pub extracted_variables: HashMap<String, serde_json::Value>,
    /// Whether the step completed successfully
    pub success: bool,
    /// Error message if step failed
    pub error: Option<String>,
    /// Duration in milliseconds
    pub duration_ms: u64,
}

/// Details of the executed HTTP request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutedRequest {
    /// HTTP method
    pub method: HttpMethod,
    /// The actual URL (after variable substitution)
    pub url: String,
    /// Headers sent
    pub headers: HashMap<String, String>,
    /// Body sent (if any)
    pub body: Option<String>,
}

/// Details of the HTTP response received
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutedResponse {
    /// HTTP status code
    pub status_code: u16,
    /// Response headers
    pub headers: HashMap<String, String>,
    /// Response body (as JSON if possible, otherwise raw string)
    pub body: serde_json::Value,
    /// Content type
    pub content_type: Option<String>,
    /// Response size in bytes
    pub size_bytes: usize,
}

/// Full result of executing an orchestration plan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationExecutionResult {
    /// ID of the plan that was executed
    pub plan_id: String,
    /// Results of each step in order
    pub step_results: Vec<StepExecutionResult>,
    /// All variables that were extracted during execution
    pub all_variables: HashMap<String, serde_json::Value>,
    /// Whether all steps completed successfully
    pub success: bool,
    /// Index of the first step that failed (if any)
    pub failed_at_step: Option<usize>,
    /// Total execution time in milliseconds
    pub total_duration_ms: u64,
    /// Timestamp when execution started
    pub started_at: String,
    /// Timestamp when execution completed
    pub completed_at: String,
}

/// Request to generate a test from orchestration results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateTestRequest {
    /// The execution result to generate a test from
    pub execution_result: OrchestrationExecutionResult,
    /// The original plan (for context)
    pub plan: TestOrchestrationPlan,
    /// Type of test to generate
    pub test_type: String,
    /// Additional instructions for the AI
    pub additional_instructions: Option<String>,
}

/// A single step in the generated test (for preview)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestStep {
    /// Step number (1-based)
    pub step_number: usize,
    /// What this step does
    pub action: String,
    /// Expected outcome
    pub expected: String,
    /// Type of step: setup, request, assertion, cleanup
    pub step_type: String,
}

/// Generated test code from the orchestration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedTest {
    /// Name for the test
    pub name: String,
    /// Description of what the test verifies
    pub description: String,
    /// The generated test code
    pub code: String,
    /// Type of test (e.g., "python_script", "playwright_cdp")
    pub test_type: String,
    /// AI's explanation of the generated test
    pub explanation: String,
    /// Structured preview of test steps
    pub steps: Vec<TestStep>,
}

/// Status of an orchestration plan (for UI updates)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum OrchestrationStatus {
    /// Plan is being created by AI
    Planning { message: String },
    /// Plan created, ready for review
    PlanReady { plan: TestOrchestrationPlan },
    /// Plan is being executed
    Executing {
        current_step: usize,
        total_steps: usize,
        step_name: String,
    },
    /// Step completed during execution
    StepCompleted {
        step_index: usize,
        step_result: StepExecutionResult,
    },
    /// Execution complete
    ExecutionComplete {
        result: OrchestrationExecutionResult,
    },
    /// Test code is being generated
    Generating { message: String },
    /// Test generation complete
    Complete { test: GeneratedTest },
    /// An error occurred
    Error { message: String, phase: String },
}

/// Saved orchestration plan (for persistence/reuse)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedOrchestrationPlan {
    /// The plan itself
    pub plan: TestOrchestrationPlan,
    /// Number of times this plan has been executed
    pub execution_count: u32,
    /// Last execution result (if any)
    pub last_execution: Option<OrchestrationExecutionResult>,
    /// Whether this plan is marked as favorite
    pub is_favorite: bool,
    /// User-defined tags
    pub tags: Vec<String>,
}
