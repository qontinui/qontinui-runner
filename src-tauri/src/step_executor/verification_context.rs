//! Verification context formatting for AI consumption.
//!
//! Contains functions and impl blocks that format verification results,
//! failure contexts, and execution summaries into markdown text that
//! gets passed to the AI during agentic phases.

use crate::str_utils::truncate_str;

use super::executor_types::{ExecutionResult, VerificationPhaseResult};

/// Extract Unix-style env var prefixes (KEY=VALUE) from a command string.
/// cmd.exe doesn't support "KEY=VALUE command" syntax, so we parse out
/// env vars to pass them via Command::env() instead.
/// Example: "SKIP_WEB_SERVER=1 npx test" -> ([("SKIP_WEB_SERVER", "1")], "npx test")
fn extract_env_prefix_for_cmd(command: &str) -> (Vec<(String, String)>, String) {
    let mut envs = Vec::new();
    let mut remaining = command.trim();

    while let Some(eq_pos) = remaining.find('=') {
        let prefix = &remaining[..eq_pos];
        if prefix.is_empty()
            || prefix.contains(' ')
            || !prefix.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            break;
        }
        let after_eq = &remaining[eq_pos + 1..];
        let value_end = after_eq.find(' ').unwrap_or(after_eq.len());
        let value = &after_eq[..value_end];
        envs.push((prefix.to_string(), value.to_string()));
        remaining = after_eq[value_end..].trim_start();
    }

    (envs, remaining.to_string())
}

/// Handlers store their output in different shapes inside `output_data`.
/// This function tries common patterns to extract human-readable text:
/// 1. `output_data.output` — combined stdout+stderr (used by check handler)
/// 2. `output_data.summary` — text summary (used by check_group handler)
/// 3. `output_data` as a top-level string
/// 4. Fallback: pretty-printed JSON (truncated)
pub fn extract_text_from_output_data(output_data: &Option<serde_json::Value>) -> Option<String> {
    let data = output_data.as_ref()?;

    // 1. Direct string field "output" (check handler puts combined stdout+stderr here)
    if let Some(output) = data.get("output").and_then(|v| v.as_str()) {
        if !output.is_empty() {
            return Some(output.to_string());
        }
    }

    // 2. "summary" field (check_group handler, etc.)
    if let Some(summary) = data.get("summary").and_then(|v| v.as_str()) {
        if !summary.is_empty() {
            return Some(summary.to_string());
        }
    }

    // 3. Top-level string value
    if let Some(s) = data.as_str() {
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }

    // 4. Render as pretty JSON for any other structured output
    // Skip trivial values that wouldn't be useful to the AI
    if data.is_null() {
        return None;
    }
    if let Some(obj) = data.as_object() {
        if obj.is_empty() {
            return None;
        }
        // Skip if only contains "skipped": true (disabled steps)
        if obj.len() == 2 && obj.contains_key("skipped") {
            return None;
        }
    }

    let json_str = serde_json::to_string_pretty(data).ok()?;
    if json_str.len() > 4000 {
        Some(format!(
            "{}...\n[truncated, {} more chars]",
            truncate_str(&json_str, 4000),
            json_str.len() - 4000
        ))
    } else {
        Some(json_str)
    }
}

/// Categorize a verification step failure based on output patterns.
///
/// Returns a category string that helps the AI understand the nature of the failure:
/// - "infrastructure" — connectivity, timeout, or service availability issues
/// - "setup_issue" — missing files, modules, or configuration
/// - "test_failure" — assertion failures or test expectation mismatches
/// - "unknown" — no recognized pattern
pub fn categorize_failure(output: &str) -> &'static str {
    let lower = output.to_lowercase();

    // Infrastructure issues: connectivity, timeouts, service unavailability
    if lower.contains("connection refused")
        || lower.contains("econnrefused")
        || lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("econnreset")
        || lower.contains("enotfound")
        || lower.contains("ehostunreach")
        || lower.contains("network error")
        || lower.contains("socket hang up")
    {
        return "infrastructure";
    }

    // Setup issues: missing files, modules, configuration
    if lower.contains("no such file")
        || lower.contains("not found")
        || lower.contains("missing module")
        || lower.contains("module not found")
        || lower.contains("cannot find module")
        || lower.contains("modulenotfounderror")
        || lower.contains("importerror")
        || lower.contains("command not found")
        || lower.contains("is not recognized")
        || lower.contains("no such command")
    {
        return "setup_issue";
    }

    // Test failures: assertions, expectations, test framework errors
    if lower.contains("assertionerror")
        || lower.contains("assert_eq")
        || lower.contains("assert_ne")
        || lower.contains("expected")
        || lower.contains("assertion failed")
        || lower.contains("test failed")
        || lower.contains("expect(")
        || lower.contains("tobetruthy")
        || lower.contains("toequal")
        || lower.contains("tomatch")
    {
        return "test_failure";
    }

    "unknown"
}

impl VerificationPhaseResult {
    /// Build a failure context string for the agentic phase
    ///
    /// This summarizes what failed so the AI knows what to work on.
    /// Includes detailed per-step output, command info, and failure categorization.
    pub fn build_failure_context(&self) -> String {
        if self.all_passed {
            return String::new();
        }

        let mut context = String::new();
        context.push_str("## Verification Results\n\n");
        context.push_str(&format!(
            "**Status:** {} of {} verification steps passed\n\n",
            self.passed_steps, self.total_steps
        ));

        // App health status from UI Bridge (if available)
        if let Some(ref health) = self.app_health {
            let status = health
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            // Only include health info when the app is unhealthy — don't add noise for healthy apps
            if status == "degraded" || status == "broken" {
                let score = health.get("score").and_then(|v| v.as_u64()).unwrap_or(0);
                context.push_str(&format!(
                    "**App Health:** {} (score: {}/100)\n",
                    status.to_uppercase(),
                    score
                ));
                if let Some(breakdown) = health.get("breakdown") {
                    let crashes = breakdown
                        .get("crashes")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let errors = breakdown
                        .get("errors")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let warnings = breakdown
                        .get("warnings")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    context.push_str(&format!(
                        "  Crashes: {}, Errors: {}, Warnings: {}\n",
                        crashes, errors, warnings
                    ));
                }
                if let Some(top_issue) = health.get("topIssue") {
                    if let Some(msg) = top_issue.get("message").and_then(|v| v.as_str()) {
                        let severity = top_issue
                            .get("severity")
                            .and_then(|v| v.as_str())
                            .unwrap_or("error");
                        context.push_str(&format!("  Top issue: [{}] {}\n", severity, msg));
                    }
                }
                context.push('\n');
            }
        }

        // List failed steps with details including command and failure category
        context.push_str("### Failed Steps\n\n");
        for result in &self.step_results {
            if !result.success {
                // Include failure category prefix if available
                let category_prefix = if let Some(ref cat) = result.failure_category {
                    if cat != "unknown" {
                        format!("[{}] ", cat)
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };

                // Header: step name, type, and check subtype if present
                if let Some(ref check_type) = result.config.check_type {
                    context.push_str(&format!(
                        "#### {}{} ({}, {})\n",
                        category_prefix, result.step_name, result.step_type, check_type
                    ));
                } else {
                    context.push_str(&format!(
                        "#### {}{} ({})\n",
                        category_prefix, result.step_name, result.step_type
                    ));
                }

                // Include the command that was run (if available)
                if let Some(ref cmd) = result.config.command {
                    context.push_str(&format!("**Command:** `{}`\n", cmd));
                }

                // Include the working directory where the command ran (if set)
                if let Some(ref wd) = result.config.working_directory {
                    context.push_str(&format!("**Working Directory:** `{}`\n", wd));
                }

                if let Some(error) = &result.error {
                    context.push_str(&format!("**Error:** {}\n", error));
                }

                if let Some(details) = &result.verification_details {
                    if let Some(stdout) = &details.stdout {
                        if !stdout.is_empty() {
                            // Truncate long output
                            let truncated = if stdout.len() > 2000 {
                                format!(
                                    "{}...\n[truncated, {} more chars]",
                                    truncate_str(stdout, 2000),
                                    stdout.len() - 2000
                                )
                            } else {
                                stdout.clone()
                            };
                            context.push_str(&format!("**Output:**\n```\n{}\n```\n", truncated));
                        }
                    }
                    if let Some(stderr) = &details.stderr {
                        if !stderr.is_empty() {
                            let truncated = if stderr.len() > 1000 {
                                format!("{}...\n[truncated]", truncate_str(stderr, 1000))
                            } else {
                                stderr.clone()
                            };
                            context.push_str(&format!("**Stderr:**\n```\n{}\n```\n", truncated));
                        }
                    }
                    if let Some(passed) = details.assertions_passed {
                        if let Some(total) = details.assertions_total {
                            context.push_str(&format!(
                                "**Assertions:** {}/{} passed\n",
                                passed, total
                            ));
                        }
                    }
                    if let Some(ref console_errors) = details.console_errors {
                        if !console_errors.is_empty() {
                            context.push_str(&format!(
                                "**Console Errors ({}):**\n",
                                console_errors.len()
                            ));
                            for err in console_errors.iter().take(10) {
                                let msg = err
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("Unknown error");
                                let err_type =
                                    err.get("type").and_then(|v| v.as_str()).unwrap_or("error");
                                context.push_str(&format!("- [{}] {}\n", err_type, msg));
                            }
                            if console_errors.len() > 10 {
                                context.push_str(&format!(
                                    "  ... and {} more console errors\n",
                                    console_errors.len() - 10
                                ));
                            }
                        }
                    }
                }

                // === Generic structured data extraction ===
                // These render structured details based on data presence, not step type.
                // Any step that produces check_results or assertion results gets them rendered.

                // Individual check results (e.g., from check_group steps)
                if let Some(ref details) = result.verification_details {
                    if let Some(ref checks) = details.check_results {
                        for check in checks {
                            if check.status == "failed" {
                                context.push_str(&format!("**Check: {} [FAILED]**\n", check.name));
                                if let Some(ref err) = check.error_message {
                                    context.push_str(&format!("Error: {}\n", err));
                                }
                                if let Some(ref output) = check.output {
                                    if !output.is_empty() {
                                        let truncated = if output.len() > 3000 {
                                            format!(
                                                "{}...\n[truncated, {} more chars]",
                                                truncate_str(output, 3000),
                                                output.len() - 3000
                                            )
                                        } else {
                                            output.clone()
                                        };
                                        context.push_str(&format!("```\n{}\n```\n", truncated));
                                    }
                                }
                                if !check.issues.is_empty() {
                                    context.push_str("Issues:\n");
                                    for issue in check.issues.iter().take(30) {
                                        context.push_str(&format!("- {}", issue.file));
                                        if let Some(line) = issue.line {
                                            context.push_str(&format!(":{}", line));
                                            if let Some(col) = issue.column {
                                                context.push_str(&format!(":{}", col));
                                            }
                                        }
                                        if let Some(ref code) = issue.code {
                                            context.push_str(&format!(" [{}]", code));
                                        }
                                        context.push_str(&format!(" {}\n", issue.message));
                                    }
                                    if check.issues.len() > 30 {
                                        context.push_str(&format!(
                                            "  ... and {} more issues\n",
                                            check.issues.len() - 30
                                        ));
                                    }
                                }
                                context.push('\n');
                            }
                        }
                    }
                }

                // Spec assertion results (from output_data, any step producing spec_result)
                if let Some(ref output_data) = result.output_data {
                    if let Some(assertion_results) = output_data
                        .get("spec_result")
                        .and_then(|sr| sr.get("assertionResults"))
                        .and_then(|ar| ar.as_array())
                    {
                        context.push_str("**Assertion Details:**\n");
                        for ar in assertion_results {
                            let passed =
                                ar.get("passed").and_then(|v| v.as_bool()).unwrap_or(false);
                            let status = if passed { "PASSED" } else { "FAILED" };
                            let target = ar.get("target").and_then(|v| v.as_str()).unwrap_or("?");
                            let target_desc = ar
                                .get("targetDescription")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");

                            context.push_str(&format!("- [{}] {}", status, target));
                            if !target_desc.is_empty() && target_desc != target {
                                context.push_str(&format!(" ({})", target_desc));
                            }
                            context.push('\n');

                            if let Some(search) = ar.get("searchDetails") {
                                let confidence = search
                                    .get("confidence")
                                    .and_then(|v| v.as_f64())
                                    .unwrap_or(0.0);
                                let reasons = search
                                    .get("matchReasons")
                                    .and_then(|v| v.as_array())
                                    .map(|arr| {
                                        arr.iter()
                                            .filter_map(|r| r.as_str())
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    })
                                    .unwrap_or_default();
                                let candidates = search
                                    .get("candidateCount")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                context.push_str(&format!(
                                    "  Element found: yes (confidence: {:.2}, match: \"{}\", candidates: {})\n",
                                    confidence, reasons, candidates
                                ));
                            } else if !passed
                                && ar.get("failureReason").and_then(|v| v.as_str())
                                    == Some("Element could not be found")
                            {
                                context.push_str("  Element found: no\n");
                            }

                            if !passed {
                                let expected = ar
                                    .get("expected")
                                    .map(|v| format!("{}", v))
                                    .unwrap_or_default();
                                let actual = ar
                                    .get("actual")
                                    .map(|v| format!("{}", v))
                                    .unwrap_or_default();
                                if !expected.is_empty() || !actual.is_empty() {
                                    context.push_str(&format!(
                                        "  Expected: {}, Actual: {}\n",
                                        expected, actual
                                    ));
                                }
                                if let Some(reason) =
                                    ar.get("failureReason").and_then(|v| v.as_str())
                                {
                                    context.push_str(&format!("  Reason: {}\n", reason));
                                }
                                if let Some(suggestion) =
                                    ar.get("suggestion").and_then(|v| v.as_str())
                                {
                                    context.push_str(&format!("  Suggestion: {}\n", suggestion));
                                }
                            }
                        }
                    }
                }

                context.push('\n');
            }
        }

        // List passed steps briefly
        let passed: Vec<_> = self.step_results.iter().filter(|r| r.success).collect();
        if !passed.is_empty() {
            context.push_str("### Passed Steps\n\n");
            for result in passed {
                context.push_str(&format!(
                    "- ✓ {} ({}ms)\n",
                    result.step_name, result.duration_ms
                ));
            }
        }

        // Phase-level console errors (captured between steps, not during any specific step)
        if let Some(ref console_errors) = self.console_errors {
            if !console_errors.is_empty() {
                context.push_str("\n### Console Errors During Verification\n\n");
                context.push_str(&format!(
                    "{} console error(s) captured during the verification phase:\n\n",
                    console_errors.len()
                ));
                for err in console_errors.iter().take(15) {
                    let msg = err
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown error");
                    let err_level = err.get("level").and_then(|v| v.as_str()).unwrap_or("error");
                    context.push_str(&format!("- [{}] {}\n", err_level, msg));
                }
                if console_errors.len() > 15 {
                    context.push_str(&format!("  ... and {} more\n", console_errors.len() - 15));
                }
                context.push('\n');
            }
        }

        // Browser events — richer than console errors, includes HMR failures,
        // React error boundaries, resource load errors, network errors
        if let Some(ref events) = self.browser_events {
            if !events.is_empty() {
                context.push_str("### Browser Events During Verification\n\n");
                for event in events.iter().take(15) {
                    // FingerprintedEvent shape: { fingerprint, event: AnyCapturedEvent, count, firstSeen, lastSeen }
                    // AnyCapturedEvent has: type, level, message, stack, timestamp, url
                    let inner = event.get("event");
                    let msg = inner
                        .and_then(|e| e.get("message"))
                        .and_then(|v| v.as_str())
                        // Fallback: raw (non-fingerprinted) event with top-level message
                        .or_else(|| event.get("message").and_then(|v| v.as_str()));

                    if let Some(msg) = msg {
                        let severity = inner
                            .and_then(|e| e.get("level"))
                            .and_then(|v| v.as_str())
                            .or_else(|| inner.and_then(|e| e.get("type")).and_then(|v| v.as_str()))
                            .unwrap_or("error");
                        let count = event.get("count").and_then(|v| v.as_u64()).unwrap_or(1);
                        if count > 1 {
                            context.push_str(&format!("- [{}] {} (x{})\n", severity, msg, count));
                        } else {
                            context.push_str(&format!("- [{}] {}\n", severity, msg));
                        }
                    }
                }
                if events.len() > 15 {
                    context.push_str(&format!("  ... and {} more events\n", events.len() - 15));
                }
                context.push('\n');
            }
        }

        // Network failures — failed HTTP requests from the SDK app
        if let Some(ref failures) = self.network_failures {
            if !failures.is_empty() {
                context.push_str("### Failed Network Requests\n\n");
                for failure in failures.iter().take(10) {
                    let request = failure.get("request");
                    let response = failure.get("response");
                    let error = failure.get("error").and_then(|v| v.as_str());

                    let method = request
                        .and_then(|r| r.get("method"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let url = request
                        .and_then(|r| r.get("url"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let status = response
                        .and_then(|r| r.get("statusCode"))
                        .and_then(|v| v.as_u64());

                    if let Some(status_code) = status {
                        let status_text = response
                            .and_then(|r| r.get("statusText"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        context.push_str(&format!(
                            "- {} {} → {} {}\n",
                            method, url, status_code, status_text
                        ));
                    } else if let Some(err_msg) = error {
                        context.push_str(&format!("- {} {} → {}\n", method, url, err_msg));
                    } else {
                        context.push_str(&format!("- {} {} → failed\n", method, url));
                    }
                }
                if failures.len() > 10 {
                    context.push_str(&format!(
                        "  ... and {} more failed requests\n",
                        failures.len() - 10
                    ));
                }
                context.push('\n');
            }
        }

        context
    }

    /// Build a brief summary for logging
    pub fn summary(&self) -> String {
        if self.all_passed {
            format!(
                "Verification PASSED: {}/{} steps in {}ms",
                self.passed_steps, self.total_steps, self.total_duration_ms
            )
        } else {
            format!(
                "Verification FAILED: {}/{} steps passed, {} failed in {}ms{}",
                self.passed_steps,
                self.total_steps,
                self.failed_steps,
                self.total_duration_ms,
                if self.critical_failure {
                    " (CRITICAL)"
                } else {
                    ""
                }
            )
        }
    }
}

impl ExecutionResult {
    /// Generate a markdown summary of the execution results
    pub fn to_markdown_summary(&self) -> String {
        if self.steps.is_empty() {
            return String::new();
        }

        let mut summary = String::new();
        summary.push_str("\n## Pre-Execution Results\n\n");
        summary.push_str("The following steps were executed deterministically by the runner:\n\n");

        for result in &self.steps {
            summary.push_str(&format!(
                "{}. **{}** ({}): {} in {}ms\n",
                result.step_index + 1,
                result.step_name,
                result.step_type,
                if result.success {
                    "Success".to_string()
                } else {
                    format!(
                        "Failed - {}",
                        result.error.as_deref().unwrap_or("unknown error")
                    )
                },
                result.duration_ms
            ));

            if let Some(ref path) = result.screenshot_path {
                summary.push_str(&format!("   Screenshot: `{}`\n", path));
            }
        }

        summary.push_str(&format!(
            "\n**Summary:** {} of {} steps completed successfully.\n",
            self.successful_steps, self.total_steps
        ));

        if self.failed_steps > 0 {
            summary.push_str("\n**Note:** Some steps failed. Please analyze the errors above.\n");
        }

        // Include captured logs if any
        if let Some(ref logs) = self.captured_logs {
            if !logs.sources.is_empty() {
                summary.push_str("\n## Application Logs (Captured During Automation)\n\n");

                for (name, content) in &logs.sources {
                    if !content.trim().is_empty() {
                        summary.push_str(&format!("### {} Logs\n\n```\n", name));
                        // Limit to last 100 lines to avoid overwhelming the AI
                        let lines: Vec<&str> = content.lines().collect();
                        let start = if lines.len() > 100 {
                            lines.len() - 100
                        } else {
                            0
                        };
                        for line in &lines[start..] {
                            summary.push_str(line);
                            summary.push('\n');
                        }
                        summary.push_str("```\n\n");
                    }
                }
            }
        }

        summary
    }
}
