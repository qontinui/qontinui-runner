//! Test Executor Module
//!
//! This module provides execution infrastructure for verification tests.
//! It supports four test types:
//!
//! 1. **Playwright CDP** - Browser DOM assertions via Chrome DevTools Protocol
//! 2. **Qontinui Vision** - Screenshot-based ML verification using qontinui vision
//! 3. **Python Scripts** - User-defined custom verification scripts
//! 4. **Repository Tests** - Project-based tests (pytest, Jest, cargo test)
//!
//! # Module Structure
//!
//! - `types` - Core types (TestDefinition, TestResult, TestStatus)
//! - `diagnostics` - Timeout diagnostic information collection
//! - `playwright_runner` - Playwright CDP test execution
//! - `python_runner` - Python verification script execution
//! - `repo_test_runner` - Repository test execution and result parsing
//!
//! # Usage
//!
//! ```rust,ignore
//! use test_executor::{execute_test, TestDefinition, TestType};
//!
//! let test = TestDefinition {
//!     id: "test-1".to_string(),
//!     name: "My Test".to_string(),
//!     test_type: TestType::PythonScript,
//!     python_code: Some("assertion('check', True)".to_string()),
//!     ..Default::default()
//! };
//!
//! let result = execute_test(&test);
//! println!("Test passed: {}", result.status == TestStatus::Passed);
//! ```

pub mod diagnostics;
mod types;

pub mod playwright_runner;
pub mod python_runner;
pub mod repo_test_runner;
pub mod task_integration;

// Re-export public types
pub use types::{
    RepoTestConfig, TestCategory, TestDefinition, TestExecutionResult, TestStatus,
    TestSuiteSummary, TestType, VisionConfig,
};

// Re-export runner functions

// Re-export task integration
pub use task_integration::{
    create_findings_for_failures, execute_tests_for_trigger, format_results_for_ai,
    TriggerTestsResult,
};

/// Execute a verification test based on its type
///
/// This is the main entry point for running any verification test.
/// It dispatches to the appropriate runner based on the test type.
pub fn execute_test(test_def: &TestDefinition) -> TestExecutionResult {
    match test_def.test_type {
        TestType::PlaywrightCdp => playwright_runner::execute_playwright_test(test_def),
        TestType::PythonScript => python_runner::execute_python_script(test_def),
        TestType::RepositoryTest => repo_test_runner::execute_repo_test(test_def),
        TestType::QontinuiVision => {
            // Vision tests require Python bridge integration
            // For now, return an error - will be implemented via Python bridge
            TestExecutionResult::error(
                test_def.id.clone(),
                test_def.name.clone(),
                "QontinuiVision tests must be executed via Python bridge".to_string(),
            )
        }
    }
}

/// Execute multiple tests and aggregate results
pub fn execute_test_suite(
    tests: &[TestDefinition],
    parallel: bool,
) -> (Vec<TestExecutionResult>, TestSuiteSummary) {
    let results: Vec<TestExecutionResult> = if parallel {
        // For now, sequential execution - can add rayon for parallelism later
        tests.iter().map(execute_test).collect()
    } else {
        tests.iter().map(execute_test).collect()
    };

    let mut summary = TestSuiteSummary::default();
    for result in &results {
        summary.add_result(result);
    }

    (results, summary)
}
