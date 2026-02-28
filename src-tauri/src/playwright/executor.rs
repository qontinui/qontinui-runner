//! Playwright script execution
//!
//! Contains the run_script function and CDP wrapper generation.

use super::parser::{
    collect_error_context, collect_screenshots, find_trace_file, find_video_files,
    parse_playwright_json,
};
use super::results::{PlaywrightResult, StructuredTestOutput, WorkflowStatus};
use super::script_storage::{
    get_results_dir, get_script, load_script_library, save_script_library,
};
use super::types::DisplayMode;
use crate::executor::file_logger::{FileLogger, PlaywrightLogParams};
use crate::settings;
use std::fs;
use tracing::{info, warn};
use uuid::Uuid;

/// Generate a wrapper script that connects to an existing Chrome browser via CDP
/// instead of launching a new browser instance.
fn generate_cdp_wrapper_script(original_script: &str, _target_url: &str) -> String {
    // We need to transform the script to:
    // 1. Use chromium.connectOverCDP instead of launching a new browser
    // 2. Use the existing page from the connected browser
    // 3. Execute the test logic on that page
    // Note: We intentionally don't navigate - the test runs on whatever page is open

    format!(
        r#"import {{ chromium, test as baseTest, expect }} from '@playwright/test';

// CDP connection wrapper - connects to existing Chrome browser
// Chrome must be started with: chrome.exe --remote-debugging-port=9222

const test = baseTest.extend<{{ cdpPage: import('@playwright/test').Page }}>( {{
  cdpPage: async ({{}}, use) => {{
    // Connect to Chrome running with remote debugging
    const browser = await chromium.connectOverCDP('http://127.0.0.1:9222');

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

// Original test adapted to use CDP connection
test('CDP: Test on existing browser', async ({{ cdpPage: page }}) => {{
  // Using the page as-is - no navigation, test runs on whatever page is open
  // This is the whole point of "Open Browser" mode!

  // ========================================
  // Original test logic extracted below:
  // ========================================

{extracted_test_body}
}});
"#,
        extracted_test_body = extract_test_body(original_script)
    )
}

/// Extract the test body from a Playwright script, removing the import and test wrapper
fn extract_test_body(script: &str) -> String {
    // Find the test body between the async ({ page }) => { and the closing });
    // This is a simplified extraction - it looks for common patterns

    let script = script.trim();

    // Try to find the test function body
    if let Some(start) = script.find("async ({ page })") {
        if let Some(arrow_pos) = script[start..].find("=>") {
            let body_start = start + arrow_pos + 2;
            // Find the opening brace
            if let Some(brace_start) = script[body_start..].find('{') {
                let actual_start = body_start + brace_start + 1;
                // Find the matching closing brace (simplified - just find the last });)
                if let Some(end) = script.rfind("});") {
                    let body = &script[actual_start..end];
                    // Indent the body content
                    return body
                        .lines()
                        .map(|line| format!("  {}", line))
                        .collect::<Vec<_>>()
                        .join("\n");
                }
            }
        }
    }

    // Also try with destructured page in different formats
    if let Some(start) = script.find("async ({") {
        if let Some(arrow_pos) = script[start..].find("=>") {
            let body_start = start + arrow_pos + 2;
            if let Some(brace_start) = script[body_start..].find('{') {
                let actual_start = body_start + brace_start + 1;
                if let Some(end) = script.rfind("});") {
                    let body = &script[actual_start..end];
                    return body
                        .lines()
                        .map(|line| format!("  {}", line))
                        .collect::<Vec<_>>()
                        .join("\n");
                }
            }
        }
    }

    // Fallback: return original script as a comment with a placeholder
    format!(
        "  // Could not extract test body automatically.\n  // Original script:\n{}",
        script
            .lines()
            .map(|line| format!("  // {}", line))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Execute a Playwright script and return the result
pub fn run_script(
    id: &str,
    target_url_override: Option<String>,
) -> Result<PlaywrightResult, String> {
    let script = get_script(id).ok_or_else(|| format!("Script not found: {}", id))?;

    // Find the qontinui-runner directory (where Playwright is installed in node_modules)
    // Test files MUST be placed in this directory for Node.js to find @playwright/test
    let exe_path =
        std::env::current_exe().map_err(|e| format!("Failed to get executable path: {}", e))?;

    info!("Executable path: {}", exe_path.display());

    // Walk up to find node_modules/@playwright directory
    let mut runner_dir = exe_path
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "Failed to get executable directory".to_string())?;

    for _ in 0..10 {
        if runner_dir.join("node_modules").join("@playwright").exists() {
            info!("Found node_modules at: {}", runner_dir.display());
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
        return Err(format!(
            "Could not find @playwright/test in node_modules. \
             Make sure @playwright/test is installed in qontinui-runner. \
             Searched from: {}",
            exe_path.display()
        ));
    }

    // Create a user-scripts subdirectory in the runner directory for test files
    // This ensures Node.js can find @playwright/test from the test file's location
    let user_scripts_dir = runner_dir.join("playwright-user-scripts");
    fs::create_dir_all(&user_scripts_dir)
        .map_err(|e| format!("Failed to create user scripts directory: {}", e))?;

    // Write the script file
    let script_file = user_scripts_dir.join(format!("{}.spec.ts", id));

    // Prepare script content with potential URL override
    let mut content = script.script_content.clone();
    if let Some(url) = target_url_override {
        if content.contains("baseURL:") {
            let re = regex::Regex::new(r#"baseURL:\s*['"`][^'"`]*['"`]"#)
                .map_err(|e| format!("Regex error: {}", e))?;
            content = re
                .replace(&content, format!(r#"baseURL: '{}'"#, url))
                .to_string();
        }
    }

    // For CDP connection mode, wrap the script to connect to existing browser
    if script.display_mode == DisplayMode::ConnectExisting {
        content = generate_cdp_wrapper_script(&content, &script.target_url);
    }

    fs::write(&script_file, &content).map_err(|e| format!("Failed to write script file: {}", e))?;

    // Prepare output directory for this run
    let results_dir = get_results_dir()?;
    let run_id = Uuid::new_v4().to_string();
    let output_dir = results_dir.join(&run_id);
    fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    info!(
        "Executing playwright script: {} (browser: {}, display_mode: {:?})",
        script.name, script.browser, script.display_mode
    );

    let start_time = std::time::Instant::now();
    let system = std::env::consts::OS;

    // Build args - just use the filename since playwright.config.ts sets testDir
    let script_filename = format!("{}.spec.ts", id);
    let mut args = vec![
        "playwright".to_string(),
        "test".to_string(),
        script_filename,
        "--reporter=json".to_string(),
        format!("--output={}", output_dir.to_string_lossy()),
        format!("--timeout={}", script.timeout_seconds * 1000),
    ];

    // Add headed flag based on display mode
    match script.display_mode {
        DisplayMode::Headless => {
            // Default behavior, no extra args needed
        }
        DisplayMode::Headed => {
            args.push("--headed".to_string());
        }
        DisplayMode::ConnectExisting => {
            // For CDP connection, we need to use headed mode and connect to existing browser
            // The script content should use connectOverCDP - see generate_cdp_wrapper_script
            args.push("--headed".to_string());
        }
    }

    // Load Playwright settings to pass as environment variables
    let playwright_settings = settings::get_playwright_settings();
    info!("Playwright settings loaded: skip_web_server={}, has_username={}, has_password={}, has_base_url={}",
        playwright_settings.skip_web_server,
        playwright_settings.test_username.is_some(),
        playwright_settings.test_password.is_some(),
        playwright_settings.base_url.is_some()
    );

    let output = if system == "windows" {
        // On Windows, use cmd.exe to ensure npx is found via PATH
        let npx_args = std::iter::once("npx".to_string())
            .chain(args)
            .collect::<Vec<_>>()
            .join(" ");

        info!(
            "Running Playwright via cmd.exe in dir {}: {}",
            runner_dir.display(),
            npx_args
        );

        let mut cmd = crate::process_helpers::cmd_no_window();
        cmd.args(["/c", &npx_args]).current_dir(&runner_dir);

        // Set Playwright environment variables from settings
        if let Some(username) = &playwright_settings.test_username {
            cmd.env("PLAYWRIGHT_TEST_USERNAME", username);
        }
        if let Some(password) = &playwright_settings.test_password {
            cmd.env("PLAYWRIGHT_TEST_PASSWORD", password);
        }
        if let Some(base_url) = &playwright_settings.base_url {
            cmd.env("PLAYWRIGHT_BASE_URL", base_url);
        }
        if playwright_settings.skip_web_server {
            cmd.env("SKIP_WEB_SERVER", "1");
        }

        cmd.output()
            .map_err(|e| format!("Failed to execute playwright via cmd.exe: {}", e))?
    } else {
        info!(
            "Running Playwright in dir {}: npx {}",
            runner_dir.display(),
            args.join(" ")
        );

        let mut cmd = crate::process_helpers::no_window("npx");
        cmd.args(&args).current_dir(&runner_dir);

        // Set Playwright environment variables from settings
        if let Some(username) = &playwright_settings.test_username {
            cmd.env("PLAYWRIGHT_TEST_USERNAME", username);
        }
        if let Some(password) = &playwright_settings.test_password {
            cmd.env("PLAYWRIGHT_TEST_PASSWORD", password);
        }
        if let Some(base_url) = &playwright_settings.base_url {
            cmd.env("PLAYWRIGHT_BASE_URL", base_url);
        }
        if playwright_settings.skip_web_server {
            cmd.env("SKIP_WEB_SERVER", "1");
        }

        cmd.output()
            .map_err(|e| format!("Failed to execute playwright: {}", e))?
    };

    // Clean up the script file after execution
    let _ = fs::remove_file(&script_file);

    let duration_ms = start_time.elapsed().as_millis() as u64;
    let executed_at = chrono::Utc::now().to_rfc3339();

    // Parse JSON output
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    info!("Playwright stdout length: {}", stdout.len());
    if !stderr.is_empty() {
        warn!("Playwright stderr: {}", stderr);
    }

    // Try to parse JSON reporter output
    let (tests_passed, tests_failed, tests_skipped, specs, error) = parse_playwright_json(&stdout);

    let passed = tests_failed == 0 && output.status.success();

    // Collect screenshots from output directory
    let screenshots = collect_screenshots(&output_dir);

    // Build structured output for AI analysis
    // Include both stdout (after JSON) and stderr for complete picture
    let mut console_lines: Vec<String> = Vec::new();

    // Add stderr (contains line reporter output showing step-by-step progress)
    for line in stderr.lines() {
        console_lines.push(line.to_string());
    }

    // Add any non-JSON stdout lines (might have useful info)
    for line in stdout.lines() {
        // Skip JSON lines (they start with { or are part of JSON structure)
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && !trimmed.starts_with('{')
            && !trimmed.starts_with('}')
            && !trimmed.starts_with('[')
            && !trimmed.starts_with(']')
            && !trimmed.starts_with('"')
        {
            console_lines.push(line.to_string());
        }
    }

    // Collect page snapshot from error-context.md files
    let page_snapshot = collect_error_context(&output_dir);

    let structured_output = StructuredTestOutput {
        specs,
        console_output: console_lines,
        network_requests: None,
        page_snapshot,
    };

    // Populate workflow_status if this is workflow automation
    let workflow_status = if script.is_workflow_automation || script.workflow_objective.is_some() {
        Some(WorkflowStatus {
            objective: script.workflow_objective.clone(),
            script_passed: passed,
            script_passed_note: Some(
                "Script execution succeeded. This is EXPECTED for workflow automation. \
                 Script success does NOT mean workflow success."
                    .to_string(),
            ),
            objective_verified: None, // Pending verification
            verification_method: Some("pending".to_string()),
            verification_notes: None,
            success_criteria: script.success_criteria.clone(),
            criteria_results: vec![],
            verification_hints: vec![
                "Check the final screenshot to verify the objective was achieved".to_string(),
                "Look at the page snapshot YAML for expected elements".to_string(),
                "Verify there are no error messages or toasts".to_string(),
            ],
        })
    } else {
        None
    };

    let result = PlaywrightResult {
        passed,
        tests_passed,
        tests_failed,
        tests_skipped,
        duration_ms,
        error,
        report_path: output_dir.to_str().map(|s| s.to_string()),
        screenshots,
        video_paths: find_video_files(&output_dir),
        trace_path: find_trace_file(&output_dir),
        executed_at,
        structured_output: Some(structured_output),
        workflow_status,
    };

    // Save result to script's last_result
    let mut library = load_script_library();
    if let Some(s) = library.scripts.iter_mut().find(|s| s.id == id) {
        s.last_result = Some(result.clone());
        let _ = save_script_library(&library);
    }

    // Clean up temp file
    let _ = fs::remove_file(&script_file);

    info!(
        "Playwright execution complete: passed={}, tests_passed={}, tests_failed={}",
        result.passed, result.tests_passed, result.tests_failed
    );

    // Log to .dev-logs for AI Developer workflows
    let display_mode_str = match script.display_mode {
        DisplayMode::Headless => "headless",
        DisplayMode::Headed => "headed",
        DisplayMode::ConnectExisting => "connect_existing",
    };

    // Convert specs to tuple format for logging
    let spec_tuples: Vec<(String, String, u64, Option<String>)> = result
        .structured_output
        .as_ref()
        .map(|so| {
            so.specs
                .iter()
                .map(|s| {
                    (
                        s.title.clone(),
                        s.status.clone(),
                        s.duration_ms,
                        s.error.clone(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    let console_output: Vec<String> = result
        .structured_output
        .as_ref()
        .map(|so| so.console_output.clone())
        .unwrap_or_default();

    let page_snapshot = result
        .structured_output
        .as_ref()
        .and_then(|so| so.page_snapshot.as_deref());

    let log_params = PlaywrightLogParams {
        script_id: &script.id,
        script_name: &script.name,
        target_url: Some(&script.target_url),
        display_mode: Some(display_mode_str),
        browser: Some(&script.browser),
        passed: result.passed,
        tests_passed: result.tests_passed,
        tests_failed: result.tests_failed,
        tests_skipped: result.tests_skipped,
        duration_ms: result.duration_ms,
        error: result.error.as_deref(),
        original_screenshots: &result.screenshots,
        console_output: &console_output,
        specs: &spec_tuples,
        page_snapshot,
        workflow_status: result.workflow_status.clone(),
    };
    FileLogger::log_playwright_execution(&log_params);

    Ok(result)
}

/// Execute inline Playwright script content (for combined scripts)
///
/// This is similar to run_script but takes the script content directly
/// instead of fetching it from the library by ID.
pub fn run_script_inline(
    content: &str,
    target_url: Option<&str>,
    script_name: &str,
) -> Result<PlaywrightResult, String> {
    // Find the qontinui-runner directory (where Playwright is installed in node_modules)
    let exe_path =
        std::env::current_exe().map_err(|e| format!("Failed to get executable path: {}", e))?;

    info!("Running inline Playwright script: {}", script_name);

    // Walk up to find node_modules/@playwright directory
    let mut runner_dir = exe_path
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "Failed to get executable directory".to_string())?;

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
        return Err(format!(
            "Could not find @playwright/test in node_modules. \
             Make sure @playwright/test is installed in qontinui-runner. \
             Searched from: {}",
            exe_path.display()
        ));
    }

    // Create a user-scripts subdirectory for test files
    let user_scripts_dir = runner_dir.join("playwright-user-scripts");
    fs::create_dir_all(&user_scripts_dir)
        .map_err(|e| format!("Failed to create user scripts directory: {}", e))?;

    // Generate a unique ID for this inline script
    let inline_id = format!("inline-{}", Uuid::new_v4());
    let script_file = user_scripts_dir.join(format!("{}.spec.ts", inline_id));

    // Prepare script content with potential URL override
    let mut final_content = content.to_string();
    if let Some(url) = target_url {
        if final_content.contains("baseURL:") {
            let re = regex::Regex::new(r#"baseURL:\s*['"`][^'"`]*['"`]"#)
                .map_err(|e| format!("Regex error: {}", e))?;
            final_content = re
                .replace(&final_content, format!(r#"baseURL: '{}'"#, url))
                .to_string();
        }
    }

    fs::write(&script_file, &final_content)
        .map_err(|e| format!("Failed to write script file: {}", e))?;

    // Prepare output directory for this run
    let results_dir = get_results_dir()?;
    let run_id = Uuid::new_v4().to_string();
    let output_dir = results_dir.join(&run_id);
    fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    info!("Executing inline playwright script: {}", script_name);

    let start_time = std::time::Instant::now();
    let system = std::env::consts::OS;

    // Build args
    let script_filename = format!("{}.spec.ts", inline_id);
    let args = vec![
        "playwright".to_string(),
        "test".to_string(),
        script_filename,
        "--reporter=json".to_string(),
        format!("--output={}", output_dir.to_string_lossy()),
        "--timeout=0".to_string(), // No timeout - run until completion
        "--headed".to_string(),    // Use headed mode for visibility
    ];

    // Load Playwright settings
    let playwright_settings = settings::get_playwright_settings();

    let output = if system == "windows" {
        let npx_args = std::iter::once("npx".to_string())
            .chain(args)
            .collect::<Vec<_>>()
            .join(" ");

        info!(
            "Running inline Playwright via cmd.exe in dir {}: {}",
            runner_dir.display(),
            npx_args
        );

        let mut cmd = crate::process_helpers::cmd_no_window();
        cmd.args(["/c", &npx_args]).current_dir(&runner_dir);

        // Set Playwright environment variables from settings
        if let Some(username) = &playwright_settings.test_username {
            cmd.env("PLAYWRIGHT_TEST_USERNAME", username);
        }
        if let Some(password) = &playwright_settings.test_password {
            cmd.env("PLAYWRIGHT_TEST_PASSWORD", password);
        }
        if let Some(base_url) = &playwright_settings.base_url {
            cmd.env("PLAYWRIGHT_BASE_URL", base_url);
        }
        if playwright_settings.skip_web_server {
            cmd.env("SKIP_WEB_SERVER", "1");
        }

        cmd.output()
            .map_err(|e| format!("Failed to execute playwright via cmd.exe: {}", e))?
    } else {
        info!(
            "Running inline Playwright in dir {}: npx {}",
            runner_dir.display(),
            args.join(" ")
        );

        let mut cmd = crate::process_helpers::no_window("npx");
        cmd.args(&args).current_dir(&runner_dir);

        if let Some(username) = &playwright_settings.test_username {
            cmd.env("PLAYWRIGHT_TEST_USERNAME", username);
        }
        if let Some(password) = &playwright_settings.test_password {
            cmd.env("PLAYWRIGHT_TEST_PASSWORD", password);
        }
        if let Some(base_url) = &playwright_settings.base_url {
            cmd.env("PLAYWRIGHT_BASE_URL", base_url);
        }
        if playwright_settings.skip_web_server {
            cmd.env("SKIP_WEB_SERVER", "1");
        }

        cmd.output()
            .map_err(|e| format!("Failed to execute playwright: {}", e))?
    };

    // Clean up the script file after execution
    let _ = fs::remove_file(&script_file);

    let duration_ms = start_time.elapsed().as_millis() as u64;
    let executed_at = chrono::Utc::now().to_rfc3339();

    // Parse JSON output
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    info!("Inline Playwright stdout length: {}", stdout.len());
    if !stderr.is_empty() {
        warn!("Inline Playwright stderr: {}", stderr);
    }

    // Parse results
    let (tests_passed, tests_failed, tests_skipped, specs, error) = parse_playwright_json(&stdout);
    let passed = tests_failed == 0 && output.status.success();

    // Collect screenshots
    let screenshots = collect_screenshots(&output_dir);

    // Build console output
    let mut console_lines: Vec<String> = Vec::new();
    for line in stderr.lines() {
        console_lines.push(line.to_string());
    }

    let page_snapshot = collect_error_context(&output_dir);

    let structured_output = StructuredTestOutput {
        specs,
        console_output: console_lines,
        network_requests: None,
        page_snapshot,
    };

    let result = PlaywrightResult {
        passed,
        tests_passed,
        tests_failed,
        tests_skipped,
        duration_ms,
        error,
        report_path: output_dir.to_str().map(|s| s.to_string()),
        screenshots,
        video_paths: find_video_files(&output_dir),
        trace_path: find_trace_file(&output_dir),
        executed_at,
        structured_output: Some(structured_output),
        workflow_status: None,
    };

    info!(
        "Inline Playwright execution complete: passed={}, tests_passed={}, tests_failed={}",
        result.passed, result.tests_passed, result.tests_failed
    );

    // Log to .dev-logs
    let log_params = PlaywrightLogParams {
        script_id: &inline_id,
        script_name,
        target_url,
        display_mode: Some("headed"),
        browser: Some("chromium"),
        passed: result.passed,
        tests_passed: result.tests_passed,
        tests_failed: result.tests_failed,
        tests_skipped: result.tests_skipped,
        duration_ms: result.duration_ms,
        error: result.error.as_deref(),
        original_screenshots: &result.screenshots,
        console_output: &result
            .structured_output
            .as_ref()
            .map(|s| s.console_output.clone())
            .unwrap_or_default(),
        specs: &result
            .structured_output
            .as_ref()
            .map(|so| {
                so.specs
                    .iter()
                    .map(|s| {
                        (
                            s.title.clone(),
                            s.status.clone(),
                            s.duration_ms,
                            s.error.clone(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        page_snapshot: result
            .structured_output
            .as_ref()
            .and_then(|so| so.page_snapshot.as_deref()),
        workflow_status: None,
    };
    FileLogger::log_playwright_execution(&log_params);

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_test_body_simple() {
        let script = r#"
import { test, expect } from '@playwright/test';

test('example', async ({ page }) => {
  await page.goto('/');
  await expect(page).toHaveTitle('Home');
});
"#;
        let body = extract_test_body(script);
        assert!(body.contains("await page.goto"));
        assert!(body.contains("await expect"));
    }

    #[test]
    fn test_extract_test_body_fallback() {
        let script = "not a valid test script";
        let body = extract_test_body(script);
        assert!(body.contains("Could not extract test body"));
    }

    #[test]
    fn test_generate_cdp_wrapper() {
        let script = r#"
import { test, expect } from '@playwright/test';

test('login', async ({ page }) => {
  await page.fill('#username', 'user');
});
"#;
        let wrapper = generate_cdp_wrapper_script(script, "http://localhost:3000");
        assert!(wrapper.contains("chromium.connectOverCDP"));
        assert!(wrapper.contains("CDP connection wrapper"));
    }
}
