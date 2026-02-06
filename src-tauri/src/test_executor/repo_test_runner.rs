//! Repository Test Runner
//!
//! Executes tests from the user's project repository (pytest, Jest, cargo test, etc.)
//! with rich result parsing for AI debugging.

use super::types::{
    CoverageReport, IndividualTestResult, ParseFormat, TestDefinition, TestError,
    TestExecutionResult, TestStatus, TestSuiteSummary, TestType,
};
use std::fs;
use std::process::Command;
use tracing::info;

/// Execute a repository test command and parse results
pub fn execute_repo_test(test_def: &TestDefinition) -> TestExecutionResult {
    let start_time = std::time::Instant::now();

    // Validate test type
    if test_def.test_type != TestType::RepositoryTest {
        return TestExecutionResult::error(
            test_def.id.clone(),
            test_def.name.clone(),
            "Invalid test type: expected RepositoryTest".to_string(),
        );
    }

    // Get repo test config
    let config = match &test_def.repo_test_config {
        Some(config) => config.clone(),
        None => {
            return TestExecutionResult::error(
                test_def.id.clone(),
                test_def.name.clone(),
                "No repository test configuration provided".to_string(),
            );
        }
    };

    // Substitute variables in command and working directory
    let command = substitute_variables(&config.command);
    let working_dir = substitute_variables(&config.working_directory);

    // Validate working directory exists
    let working_path = std::path::Path::new(&working_dir);
    if !working_path.exists() {
        return TestExecutionResult::error(
            test_def.id.clone(),
            test_def.name.clone(),
            format!("Working directory does not exist: {}", working_dir),
        );
    }

    info!(
        "Executing repository test: {} (command: {}, parse_format: {:?})",
        test_def.name, command, config.parse_format
    );

    // Execute the test command
    let output = if cfg!(target_os = "windows") {
        Command::new("cmd.exe")
            .args(["/c", &command])
            .current_dir(&working_dir)
            .envs(&config.environment)
            .output()
    } else {
        Command::new("sh")
            .args(["-c", &command])
            .current_dir(&working_dir)
            .envs(&config.environment)
            .output()
    };

    let duration_ms = start_time.elapsed().as_millis() as u64;

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);

            // Parse results based on format
            let parsed = match config.parse_format {
                ParseFormat::PytestJson => parse_pytest_json(&stdout, &stderr, &working_dir),
                ParseFormat::JestJson => parse_jest_json(&stdout, &stderr, &working_dir),
                ParseFormat::VitestJson => parse_vitest_json(&stdout, &stderr, &working_dir),
                ParseFormat::CargoTest => parse_cargo_test(&stdout, &stderr),
                ParseFormat::GoTest => parse_go_test(&stdout, &stderr),
                ParseFormat::Generic => parse_generic(&stdout, &stderr, exit_code),
            };

            let passed = exit_code == 0;
            let _status = if passed {
                TestStatus::Passed
            } else {
                TestStatus::Failed
            };

            let mut result = if passed {
                TestExecutionResult::passed(
                    test_def.id.clone(),
                    test_def.name.clone(),
                    duration_ms,
                    format!("STDOUT:\n{}\n\nSTDERR:\n{}", stdout, stderr),
                )
            } else {
                let error_summary = parsed
                    .tests
                    .iter()
                    .filter(|t| t.status == "failed")
                    .map(|t| {
                        t.error
                            .as_ref()
                            .map(|e| format!("{}: {}", t.name, e.message))
                            .unwrap_or_else(|| format!("{}: failed", t.name))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                TestExecutionResult::failed(
                    test_def.id.clone(),
                    test_def.name.clone(),
                    duration_ms,
                    format!("STDOUT:\n{}\n\nSTDERR:\n{}", stdout, stderr),
                    if error_summary.is_empty() {
                        "Tests failed".to_string()
                    } else {
                        error_summary
                    },
                )
            };

            result.exit_code = Some(exit_code);
            result.individual_tests = Some(parsed.tests);
            result.coverage = parsed.coverage;
            result.assertions_passed = parsed.summary.passed;
            result.assertions_failed = parsed.summary.failed;

            // Add structured output with summary
            result.structured_output = Some(serde_json::json!({
                "summary": {
                    "total": parsed.summary.total,
                    "passed": parsed.summary.passed,
                    "failed": parsed.summary.failed,
                    "skipped": parsed.summary.skipped,
                    "duration_ms": parsed.summary.duration_ms
                },
                "parse_format": format!("{:?}", config.parse_format)
            }));

            info!(
                "Repository test complete: passed={}, total={}, failed={}",
                passed, parsed.summary.total, parsed.summary.failed
            );

            result
        }
        Err(e) => TestExecutionResult::error(
            test_def.id.clone(),
            test_def.name.clone(),
            format!("Failed to execute test command: {}", e),
        ),
    }
}

/// Parsed test results
struct ParsedTestResults {
    summary: TestSuiteSummary,
    tests: Vec<IndividualTestResult>,
    coverage: Option<CoverageReport>,
}

/// Substitute variables like ${PROJECT_ROOT} in strings
fn substitute_variables(input: &str) -> String {
    let mut result = input.to_string();

    // ${PROJECT_ROOT} - current directory or env var
    if result.contains("${PROJECT_ROOT}") {
        let project_root = std::env::var("PROJECT_ROOT").unwrap_or_else(|_| {
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .to_string()
        });
        result = result.replace("${PROJECT_ROOT}", &project_root);
    }

    // ${HOME} - user home directory
    if result.contains("${HOME}") {
        if let Some(home) = dirs::home_dir() {
            result = result.replace("${HOME}", &home.to_string_lossy());
        }
    }

    result
}

/// Parse pytest JSON output (pytest-json-report)
fn parse_pytest_json(stdout: &str, stderr: &str, working_dir: &str) -> ParsedTestResults {
    let mut summary = TestSuiteSummary::default();
    let mut tests = Vec::new();

    // Try to find and parse pytest-json-report output
    let report_path = std::path::Path::new(working_dir).join("report.json");
    let json_str = if report_path.exists() {
        fs::read_to_string(&report_path).ok()
    } else {
        // Try to extract JSON from stdout
        stdout
            .lines()
            .find(|line| line.starts_with('{') && line.contains("\"tests\""))
            .map(|s| s.to_string())
    };

    if let Some(json_str) = json_str {
        if let Ok(report) = serde_json::from_str::<serde_json::Value>(&json_str) {
            // Parse summary
            if let Some(summary_obj) = report.get("summary") {
                summary.passed = summary_obj
                    .get("passed")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                summary.failed = summary_obj
                    .get("failed")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                summary.skipped = summary_obj
                    .get("skipped")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                summary.total = summary.passed + summary.failed + summary.skipped;
                summary.duration_ms = report
                    .get("duration")
                    .and_then(|v| v.as_f64())
                    .map(|d| (d * 1000.0) as u64)
                    .unwrap_or(0);
            }

            // Parse individual tests
            if let Some(test_array) = report.get("tests").and_then(|t| t.as_array()) {
                for test in test_array {
                    let nodeid = test.get("nodeid").and_then(|n| n.as_str()).unwrap_or("");
                    let outcome = test
                        .get("outcome")
                        .and_then(|o| o.as_str())
                        .unwrap_or("unknown");
                    let duration = test.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0);

                    let mut error = None;
                    if outcome == "failed" {
                        if let Some(call) = test.get("call") {
                            if let Some(longrepr) = call.get("longrepr").and_then(|l| l.as_str()) {
                                error = Some(TestError {
                                    error_type: "AssertionError".to_string(),
                                    message: extract_error_message(longrepr),
                                    traceback: Some(longrepr.to_string()),
                                    relevant_code: extract_relevant_code(longrepr),
                                    diff: extract_diff(longrepr),
                                });
                            }
                        }
                    }

                    // Extract file path and line number from nodeid
                    let (file_path, line_number) = parse_pytest_nodeid(nodeid);

                    tests.push(IndividualTestResult {
                        name: nodeid.to_string(),
                        file_path,
                        line_number,
                        status: outcome.to_string(),
                        duration_ms: Some((duration * 1000.0) as u64),
                        error,
                        stdout: test
                            .get("stdout")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string()),
                        stderr: test
                            .get("stderr")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string()),
                        metadata: Default::default(),
                    });
                }
            }
        }
    }

    // Fallback: parse from stdout/stderr if no JSON
    if tests.is_empty() {
        let parsed = parse_pytest_text(stdout, stderr);
        summary = parsed.summary;
        tests = parsed.tests;
    }

    ParsedTestResults {
        summary,
        tests,
        coverage: None,
    }
}

/// Parse pytest nodeid to extract file path and approximate line number
fn parse_pytest_nodeid(nodeid: &str) -> (Option<String>, Option<u32>) {
    // nodeids look like: tests/test_foo.py::TestClass::test_method
    let parts: Vec<&str> = nodeid.split("::").collect();
    if let Some(file_part) = parts.first() {
        return (Some(file_part.to_string()), None);
    }
    (None, None)
}

/// Parse pytest text output (fallback)
fn parse_pytest_text(stdout: &str, _stderr: &str) -> ParsedTestResults {
    let mut summary = TestSuiteSummary::default();
    let mut tests = Vec::new();

    // Look for summary line like "5 passed, 2 failed in 1.23s"
    let re_passed = regex::Regex::new(r"(\d+) passed").ok();
    let re_failed = regex::Regex::new(r"(\d+) failed").ok();
    let re_skipped = regex::Regex::new(r"(\d+) skipped").ok();

    for line in stdout.lines() {
        if line.contains("passed") || line.contains("failed") {
            // Extract numbers
            if let Some(ref re) = re_passed {
                if let Some(cap) = re.captures(line) {
                    summary.passed = cap[1].parse().unwrap_or(0);
                }
            }
            if let Some(ref re) = re_failed {
                if let Some(cap) = re.captures(line) {
                    summary.failed = cap[1].parse().unwrap_or(0);
                }
            }
            if let Some(ref re) = re_skipped {
                if let Some(cap) = re.captures(line) {
                    summary.skipped = cap[1].parse().unwrap_or(0);
                }
            }
            summary.total = summary.passed + summary.failed + summary.skipped;
            break;
        }
    }

    // Try to extract individual test results from verbose output
    for line in stdout.lines() {
        if line.contains("PASSED") || line.contains("FAILED") || line.contains("SKIPPED") {
            let status = if line.contains("PASSED") {
                "passed"
            } else if line.contains("FAILED") {
                "failed"
            } else {
                "skipped"
            };

            // Extract test name (format: "test_name PASSED" or "tests/test_file.py::test_name PASSED")
            let name = line
                .split_whitespace()
                .next()
                .unwrap_or("unknown")
                .to_string();

            tests.push(IndividualTestResult {
                name,
                file_path: None,
                line_number: None,
                status: status.to_string(),
                duration_ms: None,
                error: None,
                stdout: None,
                stderr: None,
                metadata: Default::default(),
            });
        }
    }

    ParsedTestResults {
        summary,
        tests,
        coverage: None,
    }
}

/// Parse Jest JSON output
fn parse_jest_json(stdout: &str, _stderr: &str, _working_dir: &str) -> ParsedTestResults {
    let mut summary = TestSuiteSummary::default();
    let mut tests = Vec::new();

    // Try to find jest-json-reporter output
    if let Ok(report) = serde_json::from_str::<serde_json::Value>(stdout) {
        // Parse from Jest JSON format
        if let Some(num_passed) = report.get("numPassedTests").and_then(|v| v.as_u64()) {
            summary.passed = num_passed as u32;
        }
        if let Some(num_failed) = report.get("numFailedTests").and_then(|v| v.as_u64()) {
            summary.failed = num_failed as u32;
        }
        if let Some(num_pending) = report.get("numPendingTests").and_then(|v| v.as_u64()) {
            summary.skipped = num_pending as u32;
        }
        summary.total = summary.passed + summary.failed + summary.skipped;

        // Parse test results
        if let Some(results) = report.get("testResults").and_then(|r| r.as_array()) {
            for file_result in results {
                if let Some(assertion_results) = file_result
                    .get("assertionResults")
                    .and_then(|a| a.as_array())
                {
                    let file_path = file_result
                        .get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string());

                    for assertion in assertion_results {
                        let name = assertion
                            .get("fullName")
                            .or_else(|| assertion.get("title"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown")
                            .to_string();

                        let status = assertion
                            .get("status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("unknown")
                            .to_string();

                        let duration_ms = assertion.get("duration").and_then(|d| d.as_u64());

                        let error = if status == "failed" {
                            assertion.get("failureMessages").and_then(|msgs| {
                                msgs.as_array().and_then(|arr| {
                                    arr.first()
                                        .and_then(|msg| msg.as_str())
                                        .map(|msg| TestError {
                                            error_type: "AssertionError".to_string(),
                                            message: extract_error_message(msg),
                                            traceback: Some(msg.to_string()),
                                            relevant_code: None,
                                            diff: extract_diff(msg),
                                        })
                                })
                            })
                        } else {
                            None
                        };

                        tests.push(IndividualTestResult {
                            name,
                            file_path: file_path.clone(),
                            line_number: None,
                            status,
                            duration_ms,
                            error,
                            stdout: None,
                            stderr: None,
                            metadata: Default::default(),
                        });
                    }
                }
            }
        }
    }

    ParsedTestResults {
        summary,
        tests,
        coverage: None,
    }
}

/// Parse Vitest JSON output (similar to Jest)
fn parse_vitest_json(stdout: &str, stderr: &str, working_dir: &str) -> ParsedTestResults {
    // Vitest JSON format is very similar to Jest
    parse_jest_json(stdout, stderr, working_dir)
}

/// Parse cargo test output
fn parse_cargo_test(stdout: &str, stderr: &str) -> ParsedTestResults {
    let mut summary = TestSuiteSummary::default();
    let mut tests = Vec::new();

    // Parse test results from cargo test output
    // Format: "test path::to::test ... ok" or "test path::to::test ... FAILED"
    for line in stdout.lines().chain(stderr.lines()) {
        if line.starts_with("test ")
            && (line.contains(" ok") || line.contains(" FAILED") || line.contains(" ignored"))
        {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                let name = parts[1].to_string();
                let status = if line.contains(" FAILED") {
                    "failed"
                } else if line.contains(" ignored") {
                    "skipped"
                } else {
                    "passed"
                };

                tests.push(IndividualTestResult {
                    name,
                    file_path: None,
                    line_number: None,
                    status: status.to_string(),
                    duration_ms: None,
                    error: None,
                    stdout: None,
                    stderr: None,
                    metadata: Default::default(),
                });

                match status {
                    "passed" => summary.passed += 1,
                    "failed" => summary.failed += 1,
                    "skipped" => summary.skipped += 1,
                    _ => {}
                }
            }
        }
    }

    summary.total = summary.passed + summary.failed + summary.skipped;

    // Extract error details for failed tests
    let combined = format!("{}\n{}", stdout, stderr);
    let mut current_failure: Option<(String, String)> = None;
    let mut in_failure_block = false;
    let _failure_content = String::new();

    for line in combined.lines() {
        if line.starts_with("---- ") && line.ends_with(" ----") {
            // Start of failure block
            let test_name = line
                .trim_start_matches("---- ")
                .trim_end_matches(" ----")
                .trim_end_matches(" stdout");
            if let Some((prev_name, prev_content)) = current_failure.take() {
                // Store previous failure
                if let Some(test) = tests.iter_mut().find(|t| t.name == prev_name) {
                    test.error = Some(TestError {
                        error_type: "panic".to_string(),
                        message: extract_error_message(&prev_content),
                        traceback: Some(prev_content),
                        relevant_code: None,
                        diff: None,
                    });
                }
            }
            current_failure = Some((test_name.to_string(), String::new()));
            in_failure_block = true;
        } else if in_failure_block {
            if let Some((_, ref mut content)) = current_failure {
                content.push_str(line);
                content.push('\n');
            }
        }
    }

    // Handle last failure
    if let Some((name, content)) = current_failure {
        if let Some(test) = tests.iter_mut().find(|t| t.name == name) {
            test.error = Some(TestError {
                error_type: "panic".to_string(),
                message: extract_error_message(&content),
                traceback: Some(content),
                relevant_code: None,
                diff: None,
            });
        }
    }

    ParsedTestResults {
        summary,
        tests,
        coverage: None,
    }
}

/// Parse go test JSON output
fn parse_go_test(stdout: &str, _stderr: &str) -> ParsedTestResults {
    let mut summary = TestSuiteSummary::default();
    let mut tests = Vec::new();

    // Go test JSON output has one JSON object per line
    for line in stdout.lines() {
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
            let action = event.get("Action").and_then(|a| a.as_str()).unwrap_or("");
            let test_name = event.get("Test").and_then(|t| t.as_str());

            if let Some(name) = test_name {
                match action {
                    "pass" => {
                        let elapsed = event.get("Elapsed").and_then(|e| e.as_f64());
                        tests.push(IndividualTestResult {
                            name: name.to_string(),
                            file_path: None,
                            line_number: None,
                            status: "passed".to_string(),
                            duration_ms: elapsed.map(|e| (e * 1000.0) as u64),
                            error: None,
                            stdout: None,
                            stderr: None,
                            metadata: Default::default(),
                        });
                        summary.passed += 1;
                    }
                    "fail" => {
                        let elapsed = event.get("Elapsed").and_then(|e| e.as_f64());
                        tests.push(IndividualTestResult {
                            name: name.to_string(),
                            file_path: None,
                            line_number: None,
                            status: "failed".to_string(),
                            duration_ms: elapsed.map(|e| (e * 1000.0) as u64),
                            error: None,
                            stdout: None,
                            stderr: None,
                            metadata: Default::default(),
                        });
                        summary.failed += 1;
                    }
                    "skip" => {
                        tests.push(IndividualTestResult {
                            name: name.to_string(),
                            file_path: None,
                            line_number: None,
                            status: "skipped".to_string(),
                            duration_ms: None,
                            error: None,
                            stdout: None,
                            stderr: None,
                            metadata: Default::default(),
                        });
                        summary.skipped += 1;
                    }
                    _ => {}
                }
            }
        }
    }

    summary.total = summary.passed + summary.failed + summary.skipped;

    ParsedTestResults {
        summary,
        tests,
        coverage: None,
    }
}

/// Parse generic test output (fallback)
fn parse_generic(stdout: &str, stderr: &str, exit_code: i32) -> ParsedTestResults {
    let mut summary = TestSuiteSummary::default();
    let tests = Vec::new();

    // Simple heuristic: count lines with PASS/FAIL patterns
    let combined = format!("{}\n{}", stdout, stderr);

    for line in combined.lines() {
        let line_upper = line.to_uppercase();
        if line_upper.contains("PASS") || line_upper.contains("OK") || line_upper.contains("✓") {
            summary.passed += 1;
        } else if line_upper.contains("FAIL")
            || line_upper.contains("ERROR")
            || line_upper.contains("✗")
        {
            summary.failed += 1;
        } else if line_upper.contains("SKIP") || line_upper.contains("PENDING") {
            summary.skipped += 1;
        }
    }

    // If we didn't find any patterns, base on exit code
    if summary.total == 0 {
        summary.total = 1;
        if exit_code == 0 {
            summary.passed = 1;
        } else {
            summary.failed = 1;
        }
    } else {
        summary.total = summary.passed + summary.failed + summary.skipped;
    }

    ParsedTestResults {
        summary,
        tests,
        coverage: None,
    }
}

/// Extract a concise error message from a longer error string
fn extract_error_message(full_error: &str) -> String {
    // Take first non-empty line that looks like an error message
    for line in full_error.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && !trimmed.starts_with("at ")
            && !trimmed.starts_with("File ")
            && !trimmed.contains("Traceback")
        {
            // Truncate long messages
            if trimmed.len() > 200 {
                return format!("{}...", &trimmed[..197]);
            }
            return trimmed.to_string();
        }
    }
    "Test failed".to_string()
}

/// Extract relevant code snippet from error
fn extract_relevant_code(full_error: &str) -> Option<String> {
    // Look for assertion lines
    for line in full_error.lines() {
        if line.contains("assert") || line.contains("expect") {
            return Some(line.trim().to_string());
        }
    }
    None
}

/// Extract diff from error message (expected vs actual)
fn extract_diff(full_error: &str) -> Option<String> {
    let mut in_diff = false;
    let mut diff_lines = Vec::new();

    for line in full_error.lines() {
        if line.contains("Expected:")
            || line.contains("Received:")
            || line.contains("---")
            || line.contains("+++")
        {
            in_diff = true;
        }
        if in_diff {
            diff_lines.push(line);
            if diff_lines.len() > 20 {
                break;
            }
        }
    }

    if diff_lines.is_empty() {
        None
    } else {
        Some(diff_lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_substitute_variables() {
        let result = substitute_variables("${PROJECT_ROOT}/src");
        assert!(!result.contains("${PROJECT_ROOT}"));
    }

    #[test]
    fn test_parse_pytest_text() {
        let stdout = "tests/test_foo.py::test_one PASSED\ntests/test_foo.py::test_two FAILED\n2 passed, 1 failed in 0.5s";
        let result = parse_pytest_text(stdout, "");
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 1);
    }

    #[test]
    fn test_extract_error_message() {
        let error = "AssertionError: Expected 5, got 3\n  at test.py:42";
        let msg = extract_error_message(error);
        assert!(msg.contains("Expected"));
    }
}
