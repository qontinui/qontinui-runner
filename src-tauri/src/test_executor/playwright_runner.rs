//! Playwright CDP Test Runner
//!
//! Executes Playwright tests that connect to an existing browser via CDP.
//! This allows verification tests to run against the same browser that
//! qontinui GUI automation is controlling.

use super::types::{AssertionResult, TestDefinition, TestExecutionResult, TestType};
use std::fs;
use std::process::Command;
use tracing::{info, warn};
use uuid::Uuid;

/// Generate a wrapper script that connects to an existing Chrome browser via CDP
fn generate_cdp_wrapper_script(user_code: &str, cdp_port: u16) -> String {
    format!(
        r#"import {{ chromium, test as baseTest, expect }} from '@playwright/test';

// CDP connection wrapper - connects to existing Chrome browser
// Chrome must be started with: chrome.exe --remote-debugging-port={cdp_port}

const test = baseTest.extend<{{ cdpPage: import('@playwright/test').Page }}>({{
  cdpPage: async ({{}}, use) => {{
    // Connect to Chrome running with remote debugging
    const browser = await chromium.connectOverCDP('http://127.0.0.1:{cdp_port}');

    // Get the first context (usually the main browser window)
    const contexts = browser.contexts();
    if (contexts.length === 0) {{
      throw new Error('No browser contexts found. Make sure Chrome has at least one window open.');
    }}
    const context = contexts[0];

    // Get the first page or create one
    const pages = context.pages();
    const page = pages.length > 0 ? pages[0] : await context.newPage();

    await use(page);

    // Don't close the browser - we want to keep the user's session
  }},
}});

// Re-export expect for use in tests
export {{ expect }};

// User's verification test
test('Verification Test', async ({{ cdpPage: page }}) => {{
  // User's test code runs on the existing browser page
{user_code}
}});
"#,
        cdp_port = cdp_port,
        user_code = indent_code(user_code, 2)
    )
}

/// Indent code by a number of spaces
fn indent_code(code: &str, spaces: usize) -> String {
    let indent = " ".repeat(spaces);
    code.lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("{}{}", indent, line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse Playwright JSON output to extract test results
fn parse_playwright_output(stdout: &str) -> (u32, u32, u32, Option<String>) {
    let mut tests_passed = 0u32;
    let mut tests_failed = 0u32;
    let mut tests_skipped = 0u32;
    let mut error_message: Option<String> = None;

    // Try to parse JSON output
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(stdout) {
        if let Some(suites) = json.get("suites").and_then(|s| s.as_array()) {
            for suite in suites {
                if let Some(specs) = suite.get("specs").and_then(|s| s.as_array()) {
                    for spec in specs {
                        if let Some(tests) = spec.get("tests").and_then(|t| t.as_array()) {
                            for test in tests {
                                if let Some(results) =
                                    test.get("results").and_then(|r| r.as_array())
                                {
                                    for result in results {
                                        if let Some(status) =
                                            result.get("status").and_then(|s| s.as_str())
                                        {
                                            match status {
                                                "passed" => tests_passed += 1,
                                                "failed" | "timedOut" => {
                                                    tests_failed += 1;
                                                    // Extract error message
                                                    if error_message.is_none() {
                                                        if let Some(err) = result.get("error") {
                                                            if let Some(msg) = err
                                                                .get("message")
                                                                .and_then(|m| m.as_str())
                                                            {
                                                                error_message =
                                                                    Some(msg.to_string());
                                                            }
                                                        }
                                                    }
                                                }
                                                "skipped" => tests_skipped += 1,
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    (tests_passed, tests_failed, tests_skipped, error_message)
}

/// Execute a Playwright CDP verification test
pub fn execute_playwright_test(test_def: &TestDefinition) -> TestExecutionResult {
    let start_time = std::time::Instant::now();

    // Validate test type
    if test_def.test_type != TestType::PlaywrightCdp {
        return TestExecutionResult::error(
            test_def.id.clone(),
            test_def.name.clone(),
            "Invalid test type: expected PlaywrightCdp".to_string(),
        );
    }

    // Get the Playwright code
    let playwright_code = match &test_def.playwright_code {
        Some(code) if !code.trim().is_empty() => code.clone(),
        _ => {
            return TestExecutionResult::error(
                test_def.id.clone(),
                test_def.name.clone(),
                "No Playwright code provided".to_string(),
            );
        }
    };

    // Get CDP port from config (default 9222)
    let cdp_port = test_def
        .config
        .get("cdp_port")
        .and_then(|p| p.as_u64())
        .unwrap_or(9222) as u16;

    // Find the qontinui-runner directory
    let exe_path = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            return TestExecutionResult::error(
                test_def.id.clone(),
                test_def.name.clone(),
                format!("Failed to get executable path: {}", e),
            );
        }
    };

    // Walk up to find node_modules/@playwright directory
    let mut runner_dir = exe_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();

    for _ in 0..10 {
        if runner_dir.join("node_modules").join("@playwright").exists() {
            break;
        }
        if let Some(parent) = runner_dir.parent() {
            runner_dir = parent.to_path_buf();
        } else {
            break;
        }
    }

    // Verify we found @playwright/test
    if !runner_dir.join("node_modules").join("@playwright").exists() {
        return TestExecutionResult::error(
            test_def.id.clone(),
            test_def.name.clone(),
            "Could not find @playwright/test. Make sure it's installed in qontinui-runner."
                .to_string(),
        );
    }

    // Create a directory for verification test scripts
    let test_scripts_dir = runner_dir.join("verification-test-scripts");
    if let Err(e) = fs::create_dir_all(&test_scripts_dir) {
        return TestExecutionResult::error(
            test_def.id.clone(),
            test_def.name.clone(),
            format!("Failed to create test scripts directory: {}", e),
        );
    }

    // Generate the wrapped script
    let wrapped_script = generate_cdp_wrapper_script(&playwright_code, cdp_port);

    // Write the script to a temp file
    let script_filename = format!("verify-{}.spec.ts", Uuid::new_v4());
    let script_path = test_scripts_dir.join(&script_filename);
    if let Err(e) = fs::write(&script_path, &wrapped_script) {
        return TestExecutionResult::error(
            test_def.id.clone(),
            test_def.name.clone(),
            format!("Failed to write test script: {}", e),
        );
    }

    // Prepare output directory
    let output_dir = test_scripts_dir
        .join("results")
        .join(Uuid::new_v4().to_string());
    if let Err(e) = fs::create_dir_all(&output_dir) {
        let _ = fs::remove_file(&script_path);
        return TestExecutionResult::error(
            test_def.id.clone(),
            test_def.name.clone(),
            format!("Failed to create output directory: {}", e),
        );
    }

    info!(
        "Executing Playwright verification test: {} (CDP port: {})",
        test_def.name, cdp_port
    );

    // Build command args
    let timeout_ms = test_def.timeout_seconds * 1000;
    let npx_args = format!(
        "npx playwright test {} --reporter=json --output={} --timeout={} --headed",
        script_filename,
        output_dir.to_string_lossy(),
        timeout_ms
    );

    // Execute the test
    let output = if cfg!(target_os = "windows") {
        Command::new("cmd.exe")
            .args(["/c", &npx_args])
            .current_dir(&test_scripts_dir)
            .output()
    } else {
        Command::new("npx")
            .args([
                "playwright",
                "test",
                &script_filename,
                "--reporter=json",
                &format!("--output={}", output_dir.to_string_lossy()),
                &format!("--timeout={}", timeout_ms),
                "--headed",
            ])
            .current_dir(&test_scripts_dir)
            .output()
    };

    // Clean up the script file
    let _ = fs::remove_file(&script_path);

    let duration_ms = start_time.elapsed().as_millis() as u64;

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            if !stderr.is_empty() {
                warn!("Playwright stderr: {}", stderr);
            }

            let (tests_passed, tests_failed, tests_skipped, error_msg) =
                parse_playwright_output(&stdout);

            let passed = tests_failed == 0 && output.status.success();

            // Collect screenshots from output directory
            let screenshots = collect_screenshots(&output_dir);

            // Build assertion results
            let assertions = if passed {
                vec![AssertionResult {
                    name: "playwright_test".to_string(),
                    passed: true,
                    message: Some(format!("{} tests passed", tests_passed)),
                    expected: None,
                    actual: None,
                    duration_ms: Some(duration_ms),
                    metadata: Default::default(),
                }]
            } else {
                vec![AssertionResult {
                    name: "playwright_test".to_string(),
                    passed: false,
                    message: error_msg.clone(),
                    expected: None,
                    actual: None,
                    duration_ms: Some(duration_ms),
                    metadata: Default::default(),
                }]
            };

            let mut result = if passed {
                TestExecutionResult::passed(
                    test_def.id.clone(),
                    test_def.name.clone(),
                    duration_ms,
                    format!("{}\n{}", stdout, stderr),
                )
            } else {
                TestExecutionResult::failed(
                    test_def.id.clone(),
                    test_def.name.clone(),
                    duration_ms,
                    format!("{}\n{}", stdout, stderr),
                    error_msg.unwrap_or_else(|| "Test failed".to_string()),
                )
            };

            result.screenshots = screenshots;
            result.assertions = assertions;
            result.assertions_passed = tests_passed;
            result.assertions_failed = tests_failed;

            info!(
                "Playwright test complete: passed={}, tests_passed={}, tests_failed={}",
                passed, tests_passed, tests_failed
            );

            result
        }
        Err(e) => TestExecutionResult::error(
            test_def.id.clone(),
            test_def.name.clone(),
            format!("Failed to execute Playwright: {}", e),
        ),
    }
}

/// Collect screenshots from the output directory
fn collect_screenshots(output_dir: &std::path::Path) -> Vec<String> {
    let mut screenshots = Vec::new();

    if let Ok(entries) = fs::read_dir(output_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(ext) = path.extension() {
                if ext == "png" || ext == "jpg" || ext == "jpeg" {
                    screenshots.push(path.to_string_lossy().to_string());
                }
            }
        }
    }

    screenshots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_indent_code() {
        let code = "line1\nline2";
        let indented = indent_code(code, 2);
        assert!(indented.starts_with("  line1"));
        assert!(indented.contains("\n  line2"));
    }

    #[test]
    fn test_generate_cdp_wrapper() {
        let code = "await expect(page.locator('h1')).toBeVisible();";
        let wrapper = generate_cdp_wrapper_script(code, 9222);
        assert!(wrapper.contains("chromium.connectOverCDP"));
        assert!(wrapper.contains("http://127.0.0.1:9222"));
        assert!(wrapper.contains("toBeVisible"));
    }
}
