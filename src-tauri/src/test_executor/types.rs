//! Core types for test execution
//!
//! Defines the data structures used across all test executors.

use crate::test_executor::diagnostics::collect_diagnostics;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Test type enumeration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestType {
    /// Browser DOM assertions via CDP
    PlaywrightCdp,
    /// Screenshot-based vision verification
    QontinuiVision,
    /// User-defined Python scripts
    PythonScript,
    /// Repository-based tests (pytest, Jest, etc.)
    RepositoryTest,
}

/// Test category for organization
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestCategory {
    Visual,
    Dom,
    Network,
    Data,
    Log,
    Layout,
    Unit,
    Integration,
    Custom,
}

/// Test execution status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    Pending,
    Running,
    Passed,
    Failed,
    Skipped,
    Error,
    Timeout,
}

/// Configuration for vision-based assertions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionConfig {
    /// List of vision assertions to run
    pub assertions: Vec<VisionAssertion>,
    /// Screenshot source: "capture_new" or "use_last"
    #[serde(default = "default_screenshot_source")]
    pub screenshot_source: String,
}

fn default_screenshot_source() -> String {
    "capture_new".to_string()
}

/// A single vision assertion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionAssertion {
    /// Assertion type (element_within_bounds, text_present, visual_regression)
    #[serde(rename = "type")]
    pub assertion_type: String,
    /// Human-readable description
    pub description: Option<String>,
    /// Assertion-specific parameters
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Configuration for repository-based tests
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoTestConfig {
    /// Test command to execute (e.g., "pytest tests/ -v")
    pub command: String,
    /// Working directory (supports ${PROJECT_ROOT})
    #[serde(default = "default_working_directory")]
    pub working_directory: String,
    /// Environment variables
    #[serde(default)]
    pub environment: HashMap<String, String>,
    /// Result parsing format
    #[serde(default = "default_parse_format")]
    pub parse_format: ParseFormat,
    /// Whether to capture coverage data
    #[serde(default)]
    pub capture_coverage: bool,
    /// Minimum coverage threshold (0-100)
    pub coverage_threshold: Option<f64>,
}

fn default_working_directory() -> String {
    "${PROJECT_ROOT}".to_string()
}

fn default_parse_format() -> ParseFormat {
    ParseFormat::Generic
}

/// Test result parsing format
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ParseFormat {
    PytestJson,
    JestJson,
    VitestJson,
    CargoTest,
    GoTest,
    #[default]
    Generic,
}

/// Test definition from the database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestDefinition {
    /// Unique test ID (UUID)
    pub id: String,
    /// Test name
    pub name: String,
    /// Test type
    pub test_type: TestType,
    /// Test category
    pub category: TestCategory,
    /// Playwright code (for PlaywrightCdp)
    pub playwright_code: Option<String>,
    /// Vision configuration (for QontinuiVision)
    pub vision_config: Option<VisionConfig>,
    /// Python script code (for PythonScript)
    pub python_code: Option<String>,
    /// Repository test configuration (for RepositoryTest)
    pub repo_test_config: Option<RepoTestConfig>,
    /// Timeout in seconds
    pub timeout_seconds: u32,
    /// Whether test failure should fail the entire workflow
    pub is_critical: bool,
    /// Additional test configuration
    #[serde(default)]
    pub config: serde_json::Value,
}

/// Result of a single assertion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionResult {
    /// Assertion name/identifier
    pub name: String,
    /// Whether assertion passed
    pub passed: bool,
    /// Assertion message
    pub message: Option<String>,
    /// Expected value
    pub expected: Option<serde_json::Value>,
    /// Actual value
    pub actual: Option<serde_json::Value>,
    /// Duration in milliseconds
    pub duration_ms: Option<u64>,
    /// Additional metadata
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Error details for a failed test
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestError {
    /// Error type (e.g., AssertionError, TypeError)
    pub error_type: String,
    /// Error message
    pub message: String,
    /// Full stack trace
    pub traceback: Option<String>,
    /// The failing line of code
    pub relevant_code: Option<String>,
    /// Assertion diff (expected vs actual)
    pub diff: Option<String>,
}

/// Result of an individual test case (for repository tests)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndividualTestResult {
    /// Test name
    pub name: String,
    /// Test file path
    pub file_path: Option<String>,
    /// Line number in test file
    pub line_number: Option<u32>,
    /// Test status
    pub status: String,
    /// Duration in milliseconds
    pub duration_ms: Option<u64>,
    /// Error details if failed
    pub error: Option<TestError>,
    /// Test stdout
    pub stdout: Option<String>,
    /// Test stderr
    pub stderr: Option<String>,
    /// Additional metadata
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Coverage report from repository tests
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageReport {
    /// Total coverage percentage
    pub total_percentage: f64,
    /// Coverage by file
    #[serde(default)]
    pub files: HashMap<String, f64>,
    /// Uncovered lines by file
    #[serde(default)]
    pub uncovered_lines: HashMap<String, Vec<u32>>,
}

/// Test execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestExecutionResult {
    /// Test ID
    pub test_id: String,
    /// Test name
    pub test_name: String,
    /// Execution status
    pub status: TestStatus,
    /// Duration in milliseconds
    pub duration_ms: u64,
    /// Test output (stdout/stderr)
    pub output: String,
    /// Error message if failed
    pub error: Option<String>,
    /// Screenshots captured during test
    #[serde(default)]
    pub screenshots: Vec<String>,
    /// Individual assertion results
    #[serde(default)]
    pub assertions: Vec<AssertionResult>,
    /// Number of assertions passed
    pub assertions_passed: u32,
    /// Number of assertions failed
    pub assertions_failed: u32,
    /// Structured output (test-type specific)
    pub structured_output: Option<serde_json::Value>,
    /// Individual test results (for repository tests)
    pub individual_tests: Option<Vec<IndividualTestResult>>,
    /// Coverage report (for repository tests)
    pub coverage: Option<CoverageReport>,
    /// Process exit code (for repository tests)
    pub exit_code: Option<i32>,
    /// Timestamp when execution started
    pub started_at: String,
    /// Timestamp when execution completed
    pub completed_at: String,
}

impl TestExecutionResult {
    /// Create a new result for a passed test
    pub fn passed(test_id: String, test_name: String, duration_ms: u64, output: String) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            test_id,
            test_name,
            status: TestStatus::Passed,
            duration_ms,
            output,
            error: None,
            screenshots: vec![],
            assertions: vec![],
            assertions_passed: 0,
            assertions_failed: 0,
            structured_output: None,
            individual_tests: None,
            coverage: None,
            exit_code: Some(0),
            started_at: now.clone(),
            completed_at: now,
        }
    }

    /// Create a new result for a failed test
    pub fn failed(
        test_id: String,
        test_name: String,
        duration_ms: u64,
        output: String,
        error: String,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            test_id,
            test_name,
            status: TestStatus::Failed,
            duration_ms,
            output,
            error: Some(error),
            screenshots: vec![],
            assertions: vec![],
            assertions_passed: 0,
            assertions_failed: 0,
            structured_output: None,
            individual_tests: None,
            coverage: None,
            exit_code: Some(1),
            started_at: now.clone(),
            completed_at: now,
        }
    }

    /// Create a new result for an errored test
    pub fn error(test_id: String, test_name: String, error: String) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            test_id,
            test_name,
            status: TestStatus::Error,
            duration_ms: 0,
            output: String::new(),
            error: Some(error),
            screenshots: vec![],
            assertions: vec![],
            assertions_passed: 0,
            assertions_failed: 0,
            structured_output: None,
            individual_tests: None,
            coverage: None,
            exit_code: None,
            started_at: now.clone(),
            completed_at: now,
        }
    }

    /// Create a new result for a timed out test
    pub fn timeout(test_id: String, test_name: String, timeout_seconds: u32) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            test_id,
            test_name,
            status: TestStatus::Timeout,
            duration_ms: (timeout_seconds * 1000) as u64,
            output: String::new(),
            error: Some(format!("Test timed out after {} seconds", timeout_seconds)),
            screenshots: vec![],
            assertions: vec![],
            assertions_passed: 0,
            assertions_failed: 0,
            structured_output: None,
            individual_tests: None,
            coverage: None,
            exit_code: None,
            started_at: now.clone(),
            completed_at: now,
        }
    }

    /// Create a new result for a timed out test with diagnostic information
    ///
    /// This collects diagnostic information about the extension and browser state
    /// to help AI agents debug why the test timed out.
    pub fn timeout_with_diagnostics(
        test_id: String,
        test_name: String,
        timeout_seconds: u32,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();

        // Collect diagnostic information
        let diagnostics = collect_diagnostics();
        let diagnostic_text = diagnostics.format_for_ai();

        // Build detailed error message with diagnostics
        let error_message = format!(
            "Test timed out after {} seconds\n\n{}",
            timeout_seconds, diagnostic_text
        );

        // Store diagnostics in structured output for programmatic access
        let structured_diagnostics = serde_json::json!({
            "timeout_seconds": timeout_seconds,
            "diagnostics": {
                "extension_reachable": diagnostics.extension_reachable,
                "extension_error": diagnostics.extension_error,
                "browser_process_running": diagnostics.browser_process_running,
                "last_successful_operation": diagnostics.last_successful_operation,
                "collected_at": diagnostics.collected_at,
            }
        });

        Self {
            test_id,
            test_name,
            status: TestStatus::Timeout,
            duration_ms: (timeout_seconds * 1000) as u64,
            output: diagnostic_text.clone(),
            error: Some(error_message),
            screenshots: vec![],
            assertions: vec![],
            assertions_passed: 0,
            assertions_failed: 0,
            structured_output: Some(structured_diagnostics),
            individual_tests: None,
            coverage: None,
            exit_code: None,
            started_at: now.clone(),
            completed_at: now,
        }
    }
}

/// Summary statistics for a test suite execution
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TestSuiteSummary {
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub error: u32,
    pub timeout: u32,
    pub duration_ms: u64,
}

impl TestSuiteSummary {
    /// Calculate pass rate as percentage
    pub fn pass_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            (self.passed as f64 / self.total as f64) * 100.0
        }
    }

    /// Check if all tests passed
    pub fn all_passed(&self) -> bool {
        self.failed == 0 && self.error == 0 && self.timeout == 0
    }

    /// Update summary from a test result
    pub fn add_result(&mut self, result: &TestExecutionResult) {
        self.total += 1;
        self.duration_ms += result.duration_ms;
        match result.status {
            TestStatus::Passed => self.passed += 1,
            TestStatus::Failed => self.failed += 1,
            TestStatus::Skipped => self.skipped += 1,
            TestStatus::Error => self.error += 1,
            TestStatus::Timeout => self.timeout += 1,
            TestStatus::Pending | TestStatus::Running => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_result_passed() {
        let result = TestExecutionResult::passed(
            "test-1".to_string(),
            "Test One".to_string(),
            1000,
            "All good".to_string(),
        );
        assert_eq!(result.status, TestStatus::Passed);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_execution_result_failed() {
        let result = TestExecutionResult::failed(
            "test-1".to_string(),
            "Test One".to_string(),
            1000,
            "Output".to_string(),
            "Assertion failed".to_string(),
        );
        assert_eq!(result.status, TestStatus::Failed);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_suite_summary() {
        let mut summary = TestSuiteSummary::default();

        let passed = TestExecutionResult::passed(
            "t1".to_string(),
            "Test 1".to_string(),
            100,
            "".to_string(),
        );
        summary.add_result(&passed);

        let failed = TestExecutionResult::failed(
            "t2".to_string(),
            "Test 2".to_string(),
            200,
            "".to_string(),
            "error".to_string(),
        );
        summary.add_result(&failed);

        assert_eq!(summary.total, 2);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.duration_ms, 300);
        assert_eq!(summary.pass_rate(), 50.0);
        assert!(!summary.all_passed());
    }
}
