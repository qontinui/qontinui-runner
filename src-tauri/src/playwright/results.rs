//! Playwright result types
//!
//! Contains all result-related structs for test execution output.

use serde::{Deserialize, Serialize};

/// A single test spec result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSpec {
    /// Test title
    pub title: String,
    /// Source file
    pub file: String,
    /// Test status: passed, failed, skipped
    pub status: String,
    /// Duration in milliseconds
    pub duration_ms: u64,
    /// Error message if failed
    #[serde(default)]
    pub error: Option<String>,
    /// Retry count
    #[serde(default)]
    pub retry: u32,
}

/// Structured output for AI analysis
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StructuredTestOutput {
    /// Individual test results
    #[serde(default)]
    pub specs: Vec<TestSpec>,
    /// Console output lines
    #[serde(default)]
    pub console_output: Vec<String>,
    /// Network requests (optional)
    #[serde(default)]
    pub network_requests: Option<Vec<NetworkRequest>>,
    /// Page snapshot from error-context.md (YAML showing all elements on page)
    #[serde(default)]
    pub page_snapshot: Option<String>,
}

/// Network request info (optional tracing)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkRequest {
    pub url: String,
    pub method: String,
    pub status: u16,
    pub duration_ms: u64,
}

/// Result of verifying a single success criterion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriteriaResult {
    /// The criterion text
    pub criterion: String,
    /// Whether it was verified as met
    pub verified: bool,
    /// Evidence supporting the verification
    #[serde(default)]
    pub evidence: Option<String>,
}

/// Workflow verification status - tracks whether the workflow objective was achieved
/// (separate from whether the script executed successfully)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkflowStatus {
    /// The workflow objective (copied from script for reference)
    #[serde(default)]
    pub objective: Option<String>,

    /// Did the script execute without errors? (For automation, expected to be true)
    #[serde(default)]
    pub script_passed: bool,

    /// Explanatory note about script_passed for AI consumption
    #[serde(default)]
    pub script_passed_note: Option<String>,

    /// Was the workflow objective verified as successful?
    /// - None = not yet verified (pending)
    /// - Some(true) = objective confirmed achieved
    /// - Some(false) = objective confirmed NOT achieved
    #[serde(default)]
    pub objective_verified: Option<bool>,

    /// How was the objective verified?
    #[serde(default)]
    pub verification_method: Option<String>, // "automatic", "ai_analysis", "manual", "pending"

    /// Notes from verification
    #[serde(default)]
    pub verification_notes: Option<String>,

    /// Success criteria from the script
    #[serde(default)]
    pub success_criteria: Vec<String>,

    /// Which criteria were confirmed (if verification was performed)
    #[serde(default)]
    pub criteria_results: Vec<CriteriaResult>,

    /// Hints for manual/AI verification
    #[serde(default)]
    pub verification_hints: Vec<String>,
}

/// Result of a Playwright test execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaywrightResult {
    /// Whether all tests passed
    pub passed: bool,
    /// Number of tests that passed
    pub tests_passed: u32,
    /// Number of tests that failed
    pub tests_failed: u32,
    /// Number of tests skipped
    #[serde(default)]
    pub tests_skipped: u32,
    /// Total duration in milliseconds
    pub duration_ms: u64,
    /// Error message if execution failed
    #[serde(default)]
    pub error: Option<String>,
    /// Path to HTML report
    #[serde(default)]
    pub report_path: Option<String>,
    /// Paths to screenshots taken
    #[serde(default)]
    pub screenshots: Vec<String>,
    /// Paths to video recordings
    #[serde(default)]
    pub video_paths: Vec<String>,
    /// Path to trace file (if enabled)
    #[serde(default)]
    pub trace_path: Option<String>,
    /// ISO 8601 timestamp of execution
    pub executed_at: String,
    /// Structured output for AI analysis
    #[serde(default)]
    pub structured_output: Option<StructuredTestOutput>,
    /// Workflow verification status (for AI workflows)
    /// Separates "script passed" from "workflow objective achieved"
    #[serde(default)]
    pub workflow_status: Option<WorkflowStatus>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_test_spec_creation() {
        let spec = TestSpec {
            title: "Test Login".to_string(),
            file: "login.spec.ts".to_string(),
            status: "passed".to_string(),
            duration_ms: 1500,
            error: None,
            retry: 0,
        };
        assert_eq!(spec.title, "Test Login");
        assert_eq!(spec.status, "passed");
    }

    #[test]
    fn test_structured_output_default() {
        let output = StructuredTestOutput::default();
        assert!(output.specs.is_empty());
        assert!(output.console_output.is_empty());
        assert!(output.network_requests.is_none());
    }

    #[test]
    fn test_workflow_status_default() {
        let status = WorkflowStatus::default();
        assert!(status.objective.is_none());
        assert!(!status.script_passed);
        assert!(status.success_criteria.is_empty());
    }
}
