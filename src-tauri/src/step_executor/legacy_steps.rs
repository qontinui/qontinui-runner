//! Legacy inline step execution methods.
//!
//! These implementations predate the handler system (`handlers/` module) and are
//! retained for backward compatibility with call sites that still invoke them
//! directly on `StepExecutor`. New step types should be added as handlers instead.

#![allow(dead_code)]

use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tracing::{info, warn};

use crate::display::RawEvent;
use crate::executor::file_logger::FileLogger;
use crate::orchestrator::context_propagation::ExpressionEvaluator;
use crate::str_utils::truncate_str;

use super::executor::StepExecutor;
use super::executor_types::*;
use super::log_watch::{
    get_default_log_source_names, LogError, CONTEXT_LINES, DEFAULT_ERROR_PATTERNS,
};

/// Extract Unix-style env var prefixes (KEY=VALUE) from a command string.
/// cmd.exe doesn't support "KEY=VALUE command" syntax, so we parse out
/// env vars to pass them via Command::env() instead.
/// Example: "SKIP_WEB_SERVER=1 npx test" -> ([("SKIP_WEB_SERVER", "1")], "npx test")
pub fn extract_env_prefix_for_cmd(command: &str) -> (Vec<(String, String)>, String) {
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

impl StepExecutor {
    /// Run a Playwright test script via HTTP API
    #[tracing::instrument(
        name = "playwright.test.script",
        skip(self),
        fields(
            test_name = %script_id
        )
    )]
    async fn run_playwright_script(
        &self,
        script_id: &str,
    ) -> (bool, Option<String>, Option<String>) {
        let client = reqwest::Client::new();
        let base_url = crate::mcp::types::get_self_base_url_from_env();
        let url = format!("{}/playwright/tests/{}/run", base_url, script_id);

        match client
            .post(&url)
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
        {
            Ok(response) => {
                if let Ok(json) = response.json::<serde_json::Value>().await {
                    let success = json
                        .get("success")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let error = if !success {
                        json.get("error")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    } else {
                        None
                    };
                    (success, error, None)
                } else {
                    (
                        false,
                        Some("Failed to parse Playwright response".to_string()),
                        None,
                    )
                }
            }
            Err(e) => (
                false,
                Some(format!("Playwright request error: {}", e)),
                None,
            ),
        }
    }

    /// Run inline Playwright script content (for combined scripts)
    ///
    /// This runs script content directly without needing a script ID.
    /// Used for combined setup+verification scripts.
    #[tracing::instrument(
        name = "playwright.test.inline",
        skip(self, content),
        fields(
            test_name = %script_name,
            content_length = %content.len(),
            target_url = ?target_url
        )
    )]
    async fn run_playwright_inline(
        &self,
        content: &str,
        target_url: Option<&str>,
        script_name: &str,
    ) -> (bool, Option<String>, Option<String>) {
        info!(
            "Running inline Playwright script: {} ({} chars)",
            script_name,
            content.len()
        );

        // Run the inline script using the playwright executor
        match crate::playwright::run_script_inline(content, target_url, script_name) {
            Ok(result) => {
                let error = if !result.passed {
                    result.error.clone()
                } else {
                    None
                };
                (result.passed, error, None)
            }
            Err(e) => (false, Some(format!("Inline Playwright error: {}", e)), None),
        }
    }

    /// Execute a verification test by ID and return simplified (success, error) tuple
    ///
    /// This is the legacy interface used by execute_single_step.
    async fn execute_verification_test(
        &self,
        test_id: &str,
        is_critical: bool,
    ) -> Result<(bool, Option<String>), String> {
        use crate::test_executor::TestStatus;

        let result = self.execute_verification_test_with_details(test_id).await?;

        // Log the result
        if result.status == TestStatus::Passed {
            info!(
                "Test '{}' passed in {}ms ({}/{} assertions)",
                result.test_name,
                result.duration_ms,
                result.assertions_passed,
                result.assertions_passed + result.assertions_failed
            );
            Ok((true, None))
        } else {
            let error_msg = format!(
                "Test '{}' {}: {} ({}/{} assertions passed)",
                result.test_name,
                match result.status {
                    TestStatus::Failed => "failed",
                    TestStatus::Error => "errored",
                    TestStatus::Timeout => "timed out",
                    _ => "did not pass",
                },
                result.error.as_deref().unwrap_or("Unknown error"),
                result.assertions_passed,
                result.assertions_passed + result.assertions_failed
            );

            warn!("{}", error_msg);

            // If critical, report as step failure; otherwise, log but succeed
            if is_critical {
                Ok((false, Some(error_msg)))
            } else {
                info!("Non-critical test failure - step continues");
                Ok((true, Some(format!("(Non-critical) {}", error_msg))))
            }
        }
    }

    /// Execute a verification test by ID and return the full TestExecutionResult
    ///
    /// This provides rich details for verification phase context building.
    async fn execute_verification_test_with_details(
        &self,
        test_id: &str,
    ) -> Result<crate::test_executor::TestExecutionResult, String> {
        use crate::database::TestType as DbTestType;
        use crate::test_executor::{self, TestCategory, TestDefinition, TestType, VisionConfig};

        info!("Executing verification test with details: {}", test_id);

        // Get the test from database
        let verification_test = self
            .app_state
            .checkpoint_db
            .get_verification_test(test_id)?
            .ok_or_else(|| format!("Verification test not found: {}", test_id))?;

        // Convert database TestType to test_executor TestType
        let test_type = match verification_test.test_type {
            DbTestType::PlaywrightCdp => TestType::PlaywrightCdp,
            DbTestType::QontinuiVision => TestType::QontinuiVision,
            DbTestType::PythonScript => TestType::PythonScript,
            DbTestType::RepositoryTest => TestType::RepositoryTest,
        };

        // Parse vision config if present
        let vision_config: Option<VisionConfig> = verification_test
            .vision_config
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        // Parse repo test config if present
        let repo_test_config = verification_test
            .repo_test_config
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        // Convert to TestDefinition
        let test_def = TestDefinition {
            id: verification_test.id.clone(),
            name: verification_test.name.clone(),
            test_type,
            category: TestCategory::Custom, // Default to Custom
            playwright_code: verification_test.playwright_code.clone(),
            vision_config,
            python_code: verification_test.python_code.clone(),
            repo_test_config,
            timeout_seconds: verification_test.timeout_seconds.unwrap_or(60),
            is_critical: verification_test.is_critical,
            config: verification_test.config.clone(),
        };

        // Execute the test (synchronous)
        let result = test_executor::execute_test(&test_def);

        Ok(result)
    }

    // =========================================================================
    // Log Watch Step Execution
    // =========================================================================

    /// Execute a log_watch step: scan .dev-logs/ for error patterns.
    ///
    /// Reads configured log sources and scans the tail of each file for
    /// error patterns (ERROR, Exception, Traceback, etc.). Returns success
    /// with any detected errors in the output string. The log_watch step is
    /// typically non-critical (required=false), so errors are informational.
    async fn execute_log_watch_step(
        &self,
        _step: &ExecutionStepConfig,
    ) -> (bool, Option<String>, Option<String>) {
        use std::io::{BufRead, BufReader};

        let dev_logs = Self::get_dev_logs_dir();
        let source_names = get_default_log_source_names();

        let mut all_errors: Vec<LogError> = Vec::new();
        let mut scanned_sources = 0;

        for source_name in &source_names {
            let log_path = dev_logs.join(source_name);
            if !log_path.exists() {
                continue;
            }

            let file = match std::fs::File::open(&log_path) {
                Ok(f) => f,
                Err(e) => {
                    warn!("log_watch: Could not open {}: {}", source_name, e);
                    continue;
                }
            };

            scanned_sources += 1;

            // Keep only the last 500 + CONTEXT_LINES lines in a ring buffer
            // to avoid reading entire large log files into memory.
            let reader = BufReader::new(file);
            let window_size = 500 + CONTEXT_LINES;
            let mut ring: VecDeque<String> = VecDeque::with_capacity(window_size + 1);
            let mut total_lines: usize = 0;

            for line in reader.lines().map_while(Result::ok) {
                if ring.len() >= window_size {
                    ring.pop_front();
                }
                ring.push_back(line);
                total_lines += 1;
            }

            // The ring now holds the last `window_size` lines of the file.
            // We scan only the last 500 of those (skipping the CONTEXT_LINES prefix
            // which exist solely to provide context_before for the first matches).
            let ring_len = ring.len();
            let scan_start = ring_len.saturating_sub(500);
            // Offset to convert ring index to original file line number
            let ring_offset = total_lines.saturating_sub(ring_len);

            for ring_idx in scan_start..ring_len {
                let line = &ring[ring_idx];
                for pattern in DEFAULT_ERROR_PATTERNS {
                    if line.contains(pattern) {
                        // Collect context lines from the ring buffer
                        let ctx_start = ring_idx.saturating_sub(CONTEXT_LINES);
                        let ctx_end = (ring_idx + CONTEXT_LINES + 1).min(ring_len);

                        let context_before: Vec<String> =
                            ring.range(ctx_start..ring_idx).cloned().collect();
                        let context_after: Vec<String> = if ring_idx + 1 < ctx_end {
                            ring.range(ring_idx + 1..ctx_end).cloned().collect()
                        } else {
                            Vec::new()
                        };

                        all_errors.push(LogError {
                            source: source_name.clone(),
                            line_number: ring_offset + ring_idx + 1,
                            timestamp: None,
                            message: line.clone(),
                            context_before,
                            context_after,
                            error_type: pattern.to_string(),
                        });
                        break; // Only match first pattern per line
                    }
                }
            }
        }

        let output = if all_errors.is_empty() {
            format!(
                "Log watch: scanned {} source(s), no errors detected.",
                scanned_sources
            )
        } else {
            // Deduplicate and limit output
            let error_count = all_errors.len();
            let display_limit = 10;
            let mut summary = format!(
                "Log watch: scanned {} source(s), {} error(s) detected.\n",
                scanned_sources, error_count
            );
            for (i, err) in all_errors.iter().take(display_limit).enumerate() {
                summary.push_str(&format!(
                    "\n[{}] {}:{} — {}\n  {}\n",
                    i + 1,
                    err.source,
                    err.line_number,
                    err.error_type,
                    // Truncate long lines
                    if err.message.len() > 200 {
                        format!("{}...", &err.message[..200])
                    } else {
                        err.message.clone()
                    }
                ));
            }
            if error_count > display_limit {
                summary.push_str(&format!(
                    "\n... and {} more error(s)\n",
                    error_count - display_limit
                ));
            }
            summary
        };

        info!(
            "log_watch: {}",
            if all_errors.is_empty() {
                "clean"
            } else {
                "errors found"
            }
        );

        // log_watch always returns success — it's informational.
        // The step is typically marked required=false so it won't fail the workflow.
        (true, None, Some(output))
    }

    // =========================================================================
    // Shell Command Step Execution
    // =========================================================================

    /// Execute a shell command step
    ///
    /// Check if a command uses bash/Unix syntax that cmd.exe cannot handle.
    /// Delegates to `ShellCommandHandler::is_bash_command()`.
    fn is_bash_command(command: &str) -> bool {
        super::handlers::shell_command::ShellCommandHandler::is_bash_command(command)
    }

    /// Supports variable expansion using `{{variable_name}}` syntax in the command.
    /// Variables are resolved from the runtime context.
    /// timeout_secs: None = no timeout (disabled by default), Some(n) = timeout after n seconds
    async fn execute_shell_command_step(
        &self,
        step: &ExecutionStepConfig,
        timeout_secs: Option<u64>,
    ) -> (bool, Option<String>, Option<String>) {
        use std::process::Stdio;
        use tokio::time::{timeout, Duration};

        // Get the command template - either directly or by looking up shell_command_id from database
        let template_command = match &step.shell_command {
            Some(cmd) => cmd.clone(),
            None => {
                // If no direct command, check for shell_command_id
                if let Some(id) = &step.shell_command_id {
                    match self.app_state.checkpoint_db.get_shell_command(id) {
                        Ok(Some(cmd)) => cmd.command,
                        Ok(None) => {
                            return (
                                false,
                                Some(format!("Shell command not found: {}", id)),
                                None,
                            );
                        }
                        Err(e) => {
                            return (
                                false,
                                Some(format!("Failed to look up shell command {}: {}", id, e)),
                                None,
                            );
                        }
                    }
                } else {
                    return (false, Some("No shell command specified".to_string()), None);
                }
            }
        };

        // Expand variables in the command using runtime context
        let evaluator = ExpressionEvaluator::new();
        let has_variables = evaluator.has_expressions(&template_command);
        let command = evaluator.evaluate(&template_command, &self.runtime_context);

        // Track which variables were resolved (for UI display)
        let resolved_variables: Option<HashMap<String, String>> = if has_variables {
            let expressions = evaluator.find_expressions(&template_command);
            let mut vars = HashMap::new();
            for expr in expressions {
                // Try to resolve the expression to get the value
                let resolved =
                    evaluator.evaluate(&format!("{{{{{}}}}}", expr), &self.runtime_context);
                // Only include if it was actually resolved (doesn't still contain braces)
                if !resolved.contains("{{") {
                    vars.insert(expr, resolved);
                }
            }
            if vars.is_empty() {
                None
            } else {
                Some(vars)
            }
        } else {
            None
        };

        // Log variable expansion if applicable
        if has_variables {
            info!(
                "Shell command variables expanded: template='{}' -> resolved='{}'",
                template_command, command
            );
            if let Some(ref vars) = resolved_variables {
                info!("Resolved variables: {:?}", vars);
            }
        }

        // Runtime sanitization: replace jq with python since jq is unavailable on Windows MSYS
        let command = if command.contains("| jq ") {
            let sanitized =
                super::handlers::shell_command::ShellCommandHandler::replace_jq_with_python_static(
                    &command,
                );
            if sanitized != command {
                info!(
                    "Legacy executor: jq→python replacement applied: {}",
                    &sanitized[..sanitized.len().min(100)]
                );
            }
            sanitized
        } else {
            command
        };

        // Runtime sanitization: on Windows, Python outputs \r\n line endings which
        // corrupt URLs when piped through xargs (curl rejects \r in URL paths).
        // Insert `tr -d '\r'` before xargs to strip carriage returns.
        let command = if cfg!(target_os = "windows") && command.contains("| xargs") {
            let sanitized = command.replace("| xargs", "| tr -d '\\r' | xargs");
            if sanitized != command {
                info!("Windows CR sanitization: inserted tr -d '\\r' before xargs");
            }
            sanitized
        } else {
            command
        };

        let step_name = step.name.as_deref().unwrap_or("Shell Command");
        // Resolve relative paths to absolute so child processes get the correct CWD
        let working_directory = step.shell_command_working_directory.clone().map(|wd| {
            let p = std::path::Path::new(&wd);
            if p.is_relative() {
                match std::env::current_dir() {
                    Ok(cwd) => {
                        let resolved = cwd.join(p);
                        match resolved.canonicalize() {
                            Ok(abs) => abs.to_string_lossy().to_string(),
                            Err(_) => resolved.to_string_lossy().to_string(),
                        }
                    }
                    Err(_) => wd,
                }
            } else {
                wd
            }
        });
        let fail_on_error = step.shell_command_fail_on_error.unwrap_or(true);

        // Detect if command uses PowerShell syntax
        let is_powershell = command.contains("Get-")
            || command.contains("Set-")
            || command.contains("New-")
            || command.contains("Remove-")
            || command.contains("Invoke-")
            || command.contains("ForEach-Object")
            || command.contains("Where-Object")
            || command.contains("Select-Object")
            || command.contains("$_")
            || command.contains("$env:")
            || command.contains("-ErrorAction")
            || command.contains("| %")
            || command.contains("| ?");

        // Detect bash/Unix commands that cmd.exe cannot handle
        let is_bash = !is_powershell && Self::is_bash_command(&command);

        let shell_type = if cfg!(target_os = "windows") && is_powershell {
            "powershell"
        } else if cfg!(target_os = "windows") && is_bash {
            "bash"
        } else if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "sh"
        };

        let timeout_str = timeout_secs
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "disabled".to_string());
        info!(
            "Executing shell command '{}': {} (shell: {}, timeout: {}, working_dir: {:?})",
            step_name, command, shell_type, timeout_str, working_directory
        );

        // Generate sequence number and timestamp for tree events
        use std::sync::atomic::{AtomicU32, Ordering};
        static SHELL_COMMAND_SEQUENCE: AtomicU32 = AtomicU32::new(1);
        let sequence = SHELL_COMMAND_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let action_id = format!("shell-command-{}", sequence);

        // Truncate command for display (first 50 chars)
        let command_display = if command.len() > 50 {
            format!("{}...", truncate_str(&command, 50))
        } else {
            command.clone()
        };

        // Build action node for tree events
        let action_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("SHELL: {}", step_name),
            "timestamp": timestamp,
            "status": "pending",
            "metadata": {
                "command": &command_display,
                "shell_type": shell_type,
                "working_directory": working_directory.as_deref().unwrap_or(""),
                "timeout_seconds": timeout_secs,
            }
        });

        // Emit action_started tree event to file log
        FileLogger::log_tree_event("action_started", &action_node, &[], timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: "action_started".to_string(),
                timestamp,
                data: json!({ "node": action_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event("action_started", &action_node, timestamp, sequence);

        // NOTE: Database event logging is handled by the unified workflow executor
        // (execute_steps_with_log_sources -> log_step_event) to avoid duplicates.
        // Tree events above are still emitted for the Session/Actions page.

        // Build the command - use PowerShell for PowerShell syntax, bash for
        // Unix-style commands, and cmd.exe as default on Windows
        let mut cmd = if cfg!(target_os = "windows") {
            if is_powershell {
                let mut c = crate::process_helpers::tokio_no_window("powershell");
                c.args(["-NoProfile", "-NonInteractive", "-Command", &command]);
                c
            } else if is_bash {
                // Use Git Bash for Unix-style commands on Windows.
                // Resolve the full path to avoid accidentally picking up WSL's
                // bash.exe which can fail when no WSL distro is installed.
                let bash_path =
                    super::handlers::shell_command::ShellCommandHandler::find_git_bash()
                        .unwrap_or_else(|| "bash".to_string());
                let mut c = crate::process_helpers::tokio_no_window(&bash_path);
                // Ensure MSYS2 /usr/bin is on PATH so tools like cat, grep, sed
                // are available even when bash is invoked non-interactively.
                if let Some(usr_bin) = std::path::Path::new(&bash_path).parent() {
                    let usr_bin_str = usr_bin.to_string_lossy();
                    let current_path = std::env::var("PATH").unwrap_or_default();
                    if !current_path.contains(&*usr_bin_str) {
                        c.env("PATH", format!("{};{}", usr_bin_str, current_path));
                    }
                }
                c.args(["-c", &command]);
                c
            } else {
                // cmd.exe doesn't understand single quotes — strip them.
                // Also extract Unix-style KEY=VALUE env var prefixes since
                // cmd.exe doesn't support "KEY=VALUE command" syntax.
                let stripped = command.replace('\'', "");
                let (extra_envs, actual_cmd) = extract_env_prefix_for_cmd(&stripped);
                let mut c = crate::process_helpers::tokio_cmd_no_window();
                c.args(["/C", &actual_cmd]);
                for (key, value) in extra_envs {
                    c.env(key, value);
                }
                c
            }
        } else {
            let mut c = crate::process_helpers::tokio_no_window("sh");
            c.args(["-c", &command]);
            c
        };

        // Set working directory if specified
        if let Some(ref wd) = working_directory {
            cmd.current_dir(wd);
        }

        // Capture stdout and stderr
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Execute with optional timeout
        let start = std::time::Instant::now();

        // Process the result - execute with or without timeout depending on setting
        let (success, exit_code, stdout, stderr) = if let Some(timeout_secs_val) = timeout_secs {
            // Execute with timeout
            let timeout_duration = Duration::from_secs(timeout_secs_val);
            let output_result = timeout(timeout_duration, cmd.output()).await;

            match output_result {
                Ok(Ok(output)) => {
                    let exit_code = output.status.code();
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let success = output.status.success();
                    (success, exit_code, stdout, stderr)
                }
                Ok(Err(e)) => {
                    warn!("Failed to execute shell command '{}': {}", step_name, e);
                    (
                        false,
                        None,
                        String::new(),
                        format!("Failed to execute command: {}", e),
                    )
                }
                Err(_) => {
                    warn!(
                        "Shell command '{}' timed out after {}s",
                        step_name, timeout_secs_val
                    );
                    (
                        false,
                        None,
                        String::new(),
                        format!("Command timed out after {} seconds", timeout_secs_val),
                    )
                }
            }
        } else {
            // No timeout - execute without timeout wrapper
            match cmd.output().await {
                Ok(output) => {
                    let exit_code = output.status.code();
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let success = output.status.success();
                    (success, exit_code, stdout, stderr)
                }
                Err(e) => {
                    warn!("Failed to execute shell command '{}': {}", step_name, e);
                    (
                        false,
                        None,
                        String::new(),
                        format!("Failed to execute command: {}", e),
                    )
                }
            }
        };
        let duration_ms = start.elapsed().as_millis() as u64;

        info!(
            "Shell command '{}' completed: success={}, exit_code={:?}, duration={}ms",
            step_name, success, exit_code, duration_ms
        );

        // Log output if present
        if !stdout.is_empty() {
            info!("Shell command stdout:\n{}", stdout.trim());
        }
        if !stderr.is_empty() {
            if success {
                info!("Shell command stderr:\n{}", stderr.trim());
            } else {
                warn!("Shell command stderr:\n{}", stderr.trim());
            }
        }

        // Determine overall success based on fail_on_error setting
        let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let duration = end_timestamp - timestamp;

        // Truncate stdout/stderr for display
        let stdout_display = if stdout.len() > 200 {
            format!("{}...", truncate_str(&stdout, 200))
        } else {
            stdout.clone()
        };
        let stderr_display = if stderr.len() > 200 {
            format!("{}...", truncate_str(&stderr, 200))
        } else {
            stderr.clone()
        };

        let (final_success, error_msg, output_data) = if success {
            // Return stdout in the screenshot_path field (repurposed for output data)
            let output_data = if stdout.is_empty() {
                None
            } else {
                Some(stdout.clone())
            };
            (true, None, output_data)
        } else if fail_on_error {
            let error_msg = if !stderr.is_empty() {
                format!(
                    "Command failed (exit code {:?}): {}",
                    exit_code,
                    stderr.trim()
                )
            } else {
                format!("Command failed with exit code {:?}", exit_code)
            };
            (false, Some(error_msg), None)
        } else {
            // Return success but include the error message
            info!(
                "Shell command '{}' failed but fail_on_error=false, continuing",
                step_name
            );
            let error_msg = if !stderr.is_empty() {
                format!("(ignored) Command failed: {}", stderr.trim())
            } else {
                format!("(ignored) Command failed with exit code {:?}", exit_code)
            };
            (true, Some(error_msg), Some(stdout.clone()))
        };

        // Build completed action node
        let completed_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("SHELL: {}", step_name),
            "timestamp": end_timestamp,
            "status": if final_success { "success" } else { "failed" },
            "duration": duration,
            "metadata": {
                "command": &command_display,
                "shell_type": shell_type,
                "working_directory": working_directory.as_deref().unwrap_or(""),
                "exit_code": exit_code,
                "stdout": &stdout_display,
                "stderr": &stderr_display,
                "duration_ms": duration_ms,
            }
        });

        let event_type = if final_success {
            "action_completed"
        } else {
            "action_failed"
        };

        // Emit completion tree event to file log
        FileLogger::log_tree_event(event_type, &completed_node, &[], end_timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: event_type.to_string(),
                timestamp: end_timestamp,
                data: json!({ "node": completed_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event(event_type, &completed_node, end_timestamp, sequence);

        // NOTE: Database event logging is handled by the unified workflow executor
        // (execute_steps_with_log_sources -> log_step_event) to avoid duplicates.
        // Tree events above are still emitted for the Session/Actions page.

        (final_success, error_msg, output_data)
    }

    // =========================================================================
    // Check Step Execution
    // =========================================================================

    /// Execute a code quality check step
    /// timeout_secs: None = no timeout (disabled by default), Some(n) = timeout after n seconds
    async fn execute_check_step(
        &self,
        step: &ExecutionStepConfig,
        timeout_secs: Option<u64>,
    ) -> (bool, Option<String>, Option<String>) {
        use std::process::Stdio;
        use tokio::time::{timeout, Duration};

        // Debug logging to trace check_type values
        info!(
            "execute_check_step: step_name={:?}, check_type={:?}, check_command={:?}, working_dir={:?}",
            step.name, step.check_type, step.check_command, step.check_working_directory
        );

        let check_type = step.check_type.as_deref().unwrap_or("custom_command");
        let step_name = step.name.as_deref().unwrap_or("Check");
        // Note: Due to serde alias conflict, "working_directory" goes to shell_command_working_directory
        // So we check both fields for backwards compatibility.
        // Resolve relative paths to absolute so child processes get the correct CWD
        // regardless of the runner process's own working directory.
        let working_directory = step
            .check_working_directory
            .clone()
            .or_else(|| step.shell_command_working_directory.clone())
            .map(|wd| {
                let p = std::path::Path::new(&wd);
                if p.is_relative() {
                    match std::env::current_dir() {
                        Ok(cwd) => {
                            let resolved = cwd.join(p);
                            match resolved.canonicalize() {
                                Ok(abs) => abs.to_string_lossy().to_string(),
                                Err(_) => resolved.to_string_lossy().to_string(),
                            }
                        }
                        Err(_) => wd,
                    }
                } else {
                    wd
                }
            });

        // Handle http_status check type separately (doesn't need language detection)
        if check_type == "http_status" {
            return self
                .execute_http_status_check(step, step_name, timeout_secs)
                .await;
        }

        // Detect project type from working directory to auto-select appropriate tools
        let detected_language = {
            let work_dir = working_directory.as_deref().unwrap_or(".");
            let path = std::path::Path::new(work_dir);

            if path.join("Cargo.toml").exists() {
                "rust"
            } else if path.join("pyproject.toml").exists()
                || path.join("setup.py").exists()
                || path.join("requirements.txt").exists()
            {
                "python"
            } else if path.join("go.mod").exists() {
                "go"
            } else if path.join("tsconfig.json").exists() {
                "typescript"
            } else if path.join("package.json").exists() {
                "javascript"
            } else if path.join("CMakeLists.txt").exists()
                || path.join("Makefile").exists()
                || path.join("configure.ac").exists()
            {
                "c_cpp"
            } else if path.join("build.gradle").exists()
                || path.join("build.gradle.kts").exists()
                || path.join("pom.xml").exists()
            {
                "java"
            } else if path.join("mix.exs").exists() {
                "elixir"
            } else if path.join("Gemfile").exists() {
                "ruby"
            } else if path.join("composer.json").exists() {
                "php"
            } else if path.join("Package.swift").exists() {
                "swift"
            } else if path.join("*.csproj").exists() || path.join("*.sln").exists() {
                // Note: glob patterns don't work with exists(), but we'll check for common .NET files
                "dotnet"
            } else {
                "unknown"
            }
        };

        // Additional check for .NET projects (need to actually scan directory)
        let detected_language = if detected_language == "unknown" {
            let work_dir = working_directory.as_deref().unwrap_or(".");
            let path = std::path::Path::new(work_dir);
            if let Ok(entries) = std::fs::read_dir(path) {
                let has_dotnet = entries.filter_map(|e| e.ok()).any(|entry| {
                    let name = entry.file_name().to_string_lossy().to_string();
                    name.ends_with(".csproj") || name.ends_with(".sln") || name.ends_with(".fsproj")
                });
                if has_dotnet {
                    "dotnet"
                } else {
                    "unknown"
                }
            } else {
                "unknown"
            }
        } else {
            detected_language
        };

        info!(
            "Check step '{}': detected language = {}",
            step_name, detected_language
        );

        // Get the command to run - auto-detect based on language if not specified
        // Note: Due to serde alias conflict, "command" in JSON goes to shell_command, not check_command
        // So we check both fields for backwards compatibility with frontend using "command" field
        let explicit_command = step
            .check_command
            .as_ref()
            .filter(|s| !s.is_empty())
            .or_else(|| step.shell_command.as_ref().filter(|s| !s.is_empty()));

        let command = match explicit_command {
            Some(cmd) => Some(cmd.clone()),
            None => {
                // Auto-select commands based on detected language and check type
                match (check_type, detected_language) {
                    // Python checks
                    ("lint", "python") => Some("ruff check .".to_string()),
                    ("format", "python") => Some("black --check .".to_string()),
                    ("typecheck", "python") => Some("mypy .".to_string()),
                    ("analyze", "python") => Some("ruff check . --statistics".to_string()),
                    ("security", "python") => Some("pip-audit".to_string()),

                    // Rust checks
                    ("lint", "rust") => Some("cargo clippy -- -D warnings".to_string()),
                    ("format", "rust") => Some("cargo fmt --check".to_string()),
                    ("typecheck", "rust") => Some("cargo check".to_string()),
                    ("analyze", "rust") => Some("cargo clippy --all-targets --all-features".to_string()),
                    ("security", "rust") => Some("cargo audit".to_string()),

                    // Go checks
                    ("lint", "go") => Some("golangci-lint run".to_string()),
                    ("format", "go") => Some("gofmt -l .".to_string()),
                    ("typecheck", "go") => Some("go vet ./...".to_string()),
                    ("analyze", "go") => Some("go vet ./... && staticcheck ./...".to_string()),
                    ("security", "go") => Some("gosec ./...".to_string()),

                    // TypeScript checks
                    ("lint", "typescript") => Some("npx eslint . --ext .ts,.tsx".to_string()),
                    ("format", "typescript") => Some("npx prettier --check .".to_string()),
                    ("typecheck", "typescript") => Some("npx tsc --noEmit".to_string()),
                    ("analyze", "typescript") => Some("npx eslint . --ext .ts,.tsx --format json".to_string()),
                    ("security", "typescript") => Some("npm audit".to_string()),

                    // JavaScript checks
                    ("lint", "javascript") => Some("npx eslint .".to_string()),
                    ("format", "javascript") => Some("npx prettier --check .".to_string()),
                    ("typecheck", "javascript") => None, // No typecheck for plain JS
                    ("analyze", "javascript") => Some("npx eslint . --format json".to_string()),
                    ("security", "javascript") => Some("npm audit".to_string()),

                    // C/C++ checks (using common tools)
                    ("lint", "c_cpp") => Some("cppcheck --enable=all .".to_string()),
                    ("format", "c_cpp") => Some("clang-format --dry-run -Werror **/*.cpp **/*.c **/*.h".to_string()),
                    ("typecheck", "c_cpp") => Some("make -n".to_string()), // Dry-run make
                    ("analyze", "c_cpp") => Some("cppcheck --enable=all --xml .".to_string()),
                    ("security", "c_cpp") => Some("flawfinder .".to_string()),

                    // Java checks
                    ("lint", "java") => Some("./gradlew checkstyleMain || mvn checkstyle:check".to_string()),
                    ("format", "java") => Some("./gradlew spotlessCheck || mvn spotless:check".to_string()),
                    ("typecheck", "java") => Some("./gradlew compileJava || mvn compile".to_string()),
                    ("analyze", "java") => Some("./gradlew pmd || mvn pmd:check".to_string()),
                    ("security", "java") => Some("./gradlew dependencyCheckAnalyze || mvn org.owasp:dependency-check-maven:check".to_string()),

                    // Ruby checks
                    ("lint", "ruby") => Some("bundle exec rubocop".to_string()),
                    ("format", "ruby") => Some("bundle exec rubocop --format offenses".to_string()),
                    ("typecheck", "ruby") => Some("bundle exec srb tc".to_string()), // Sorbet
                    ("analyze", "ruby") => Some("bundle exec rubocop --format json".to_string()),
                    ("security", "ruby") => Some("bundle exec bundler-audit check".to_string()),

                    // PHP checks
                    ("lint", "php") => Some("./vendor/bin/phpcs".to_string()),
                    ("format", "php") => Some("./vendor/bin/php-cs-fixer fix --dry-run --diff".to_string()),
                    ("typecheck", "php") => Some("./vendor/bin/phpstan analyse".to_string()),
                    ("analyze", "php") => Some("./vendor/bin/phpmd . text cleancode,codesize,controversial".to_string()),
                    ("security", "php") => Some("composer audit".to_string()),

                    // Elixir checks
                    ("lint", "elixir") => Some("mix credo".to_string()),
                    ("format", "elixir") => Some("mix format --check-formatted".to_string()),
                    ("typecheck", "elixir") => Some("mix dialyzer".to_string()),
                    ("analyze", "elixir") => Some("mix credo --format json".to_string()),
                    ("security", "elixir") => Some("mix deps.audit".to_string()),

                    // Swift checks
                    ("lint", "swift") => Some("swiftlint".to_string()),
                    ("format", "swift") => Some("swiftformat --lint .".to_string()),
                    ("typecheck", "swift") => Some("swift build".to_string()),
                    ("analyze", "swift") => Some("swiftlint --reporter json".to_string()),
                    ("security", "swift") => None, // No standard security tool

                    // .NET checks
                    ("lint", "dotnet") => Some("dotnet format --verify-no-changes".to_string()),
                    ("format", "dotnet") => Some("dotnet format --verify-no-changes".to_string()),
                    ("typecheck", "dotnet") => Some("dotnet build --no-restore".to_string()),
                    ("analyze", "dotnet") => Some("dotnet build /p:TreatWarningsAsErrors=true".to_string()),
                    ("security", "dotnet") => Some("dotnet list package --vulnerable".to_string()),

                    // Unknown language - skip gracefully
                    (check_type_val, "unknown") => {
                        warn!(
                            "Check step '{}': No language detected, skipping {} check. \
                            Specify a command explicitly or ensure project has recognizable marker files.",
                            step_name, check_type_val
                        );
                        None
                    }

                    // Catch-all for unrecognized check types on known languages
                    _ => {
                        warn!(
                            "Check step '{}': Unsupported check type '{}' for language '{}', skipping.",
                            step_name, check_type, detected_language
                        );
                        None
                    }
                }
            }
        };

        // Handle the case where no command could be determined (skip gracefully)
        let command = match command {
            Some(cmd) => cmd,
            None => {
                info!(
                    "Check step '{}' skipped: no applicable check for type '{}' and language '{}'",
                    step_name, check_type, detected_language
                );
                // Return success with a warning message
                return (
                    true,
                    Some(format!(
                        "Skipped: No {} check available for {} projects. Specify a command explicitly if needed.",
                        check_type, detected_language
                    )),
                    None,
                );
            }
        };
        let auto_fix = step.check_auto_fix.unwrap_or(false);

        // Modify command for auto-fix if enabled (language-aware)
        let final_command = if auto_fix {
            match (check_type, detected_language) {
                // Python auto-fix
                ("lint", "python") => command.replace("ruff check", "ruff check --fix"),
                ("format", "python") => command.replace("--check", ""),

                // Rust auto-fix
                ("lint", "rust") => command.replace("cargo clippy", "cargo clippy --fix"),
                ("format", "rust") => command.replace("--check", ""),

                // Go auto-fix
                ("lint", "go") => command.replace("golangci-lint run", "golangci-lint run --fix"),
                ("format", "go") => command.replace("gofmt -l", "gofmt -w"),

                // TypeScript/JavaScript auto-fix
                ("lint", "typescript") | ("lint", "javascript") => {
                    if command.contains("eslint") {
                        format!("{} --fix", command)
                    } else {
                        command.replace("lint", "lint:fix")
                    }
                }
                ("format", "typescript") | ("format", "javascript") => {
                    if command.contains("prettier") {
                        command.replace("--check", "--write")
                    } else {
                        command
                            .replace("format:check", "format")
                            .replace("--check", "")
                    }
                }

                // C/C++ auto-fix
                ("format", "c_cpp") => command.replace("--dry-run -Werror", "-i"),

                // Ruby auto-fix
                ("lint", "ruby") | ("format", "ruby") => format!("{} --autocorrect", command),

                // PHP auto-fix
                ("lint", "php") => command.replace("phpcs", "phpcbf"),
                ("format", "php") => command.replace("--dry-run --diff", ""),

                // Elixir auto-fix
                ("format", "elixir") => command.replace("--check-formatted", ""),

                // Swift auto-fix
                ("lint", "swift") => format!("{} --fix", command),
                ("format", "swift") => command.replace("--lint", ""),

                // .NET auto-fix
                ("lint", "dotnet") | ("format", "dotnet") => {
                    command.replace("--verify-no-changes", "")
                }

                // For languages without auto-fix, just return the command as-is
                _ => command,
            }
        } else {
            command
        };

        let timeout_str = timeout_secs
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "disabled".to_string());
        info!(
            "Executing check '{}' ({}): {} (timeout: {}, working_dir: {:?})",
            step_name, check_type, final_command, timeout_str, working_directory
        );

        // Generate sequence number and timestamp for tree events
        use std::sync::atomic::{AtomicU32, Ordering};
        static CHECK_SEQUENCE: AtomicU32 = AtomicU32::new(1);
        let sequence = CHECK_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let action_id = format!("check-{}", sequence);

        // Truncate command for display
        let command_display = if final_command.len() > 50 {
            format!("{}...", truncate_str(&final_command, 50))
        } else {
            final_command.clone()
        };

        // Build action node for tree events
        let action_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("CHECK: {}", step_name),
            "timestamp": timestamp,
            "status": "pending",
            "metadata": {
                "check_type": check_type,
                "command": &command_display,
                "working_directory": working_directory.as_deref().unwrap_or(""),
                "auto_fix": auto_fix,
                "timeout_seconds": timeout_secs,
            }
        });

        // Emit action_started tree event to file log
        FileLogger::log_tree_event("action_started", &action_node, &[], timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: "action_started".to_string(),
                timestamp,
                data: json!({ "node": action_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event("action_started", &action_node, timestamp, sequence);

        // Detect if command uses PowerShell syntax (same logic as shell_command_step)
        let is_powershell = final_command.contains("Get-")
            || final_command.contains("Set-")
            || final_command.contains("New-")
            || final_command.contains("Remove-")
            || final_command.contains("Invoke-")
            || final_command.contains("ForEach-Object")
            || final_command.contains("Where-Object")
            || final_command.contains("Select-Object")
            || final_command.contains("$_")
            || final_command.contains("$env:")
            || final_command.contains("-ErrorAction")
            || final_command.contains("| %")
            || final_command.contains("| ?");

        // Detect bash/Unix commands that cmd.exe cannot handle
        let is_bash = !is_powershell && Self::is_bash_command(&final_command);

        // Build the command
        let mut cmd = if cfg!(target_os = "windows") {
            if is_powershell {
                let mut c = crate::process_helpers::tokio_no_window("powershell");
                c.args(["-NoProfile", "-NonInteractive", "-Command", &final_command]);
                c
            } else if is_bash {
                // Use Git Bash for Unix-style commands on Windows.
                let bash_path =
                    super::handlers::shell_command::ShellCommandHandler::find_git_bash()
                        .unwrap_or_else(|| "bash".to_string());
                let mut c = crate::process_helpers::tokio_no_window(&bash_path);
                // Ensure MSYS2 /usr/bin is on PATH for cat, grep, etc.
                if let Some(usr_bin) = std::path::Path::new(&bash_path).parent() {
                    let usr_bin_str = usr_bin.to_string_lossy();
                    let current_path = std::env::var("PATH").unwrap_or_default();
                    if !current_path.contains(&*usr_bin_str) {
                        c.env("PATH", format!("{};{}", usr_bin_str, current_path));
                    }
                }
                c.args(["-c", &final_command]);
                c
            } else {
                // cmd.exe doesn't understand single quotes — strip them.
                // Also extract Unix-style KEY=VALUE env var prefixes since
                // cmd.exe doesn't support "KEY=VALUE command" syntax.
                let stripped = final_command.replace('\'', "");
                let (extra_envs, actual_cmd) = extract_env_prefix_for_cmd(&stripped);
                let mut c = crate::process_helpers::tokio_cmd_no_window();
                c.args(["/C", &actual_cmd]);
                for (key, value) in extra_envs {
                    c.env(key, value);
                }
                c
            }
        } else {
            let mut c = crate::process_helpers::tokio_no_window("sh");
            c.args(["-c", &final_command]);
            c
        };

        // Set working directory if specified
        if let Some(ref wd) = working_directory {
            cmd.current_dir(wd);
        }

        // Capture stdout and stderr
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Execute with optional timeout
        let start = std::time::Instant::now();

        // Helper to process command output
        let process_output = |output: std::process::Output,
                              duration_ms: u64|
         -> (bool, Option<String>, Option<String>) {
            let exit_code = output.status.code();
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let success = output.status.success();

            info!(
                "Check '{}' completed: success={}, exit_code={:?}, duration={}ms",
                step_name, success, exit_code, duration_ms
            );

            if success {
                let output_data = if stdout.is_empty() {
                    None
                } else {
                    Some(stdout)
                };
                (true, None, output_data)
            } else {
                // IMPORTANT: Capture BOTH stdout and stderr for failed checks
                // so the AI can see the full error context for fixing
                let mut combined_output = String::new();
                if !stdout.is_empty() {
                    combined_output.push_str("=== STDOUT ===\n");
                    combined_output.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !combined_output.is_empty() {
                        combined_output.push_str("\n\n");
                    }
                    combined_output.push_str("=== STDERR ===\n");
                    combined_output.push_str(&stderr);
                }
                let error_summary = if !stderr.is_empty() {
                    stderr.lines().take(5).collect::<Vec<_>>().join("\n")
                } else {
                    stdout.lines().take(5).collect::<Vec<_>>().join("\n")
                };
                (
                    false,
                    Some(format!(
                        "Check failed (exit code {:?}): {}",
                        exit_code,
                        error_summary.trim()
                    )),
                    Some(combined_output), // Return full output for AI context
                )
            }
        };

        // Process the result - execute with or without timeout depending on setting
        let (final_success, error_msg, output_data) = if let Some(timeout_secs_val) = timeout_secs {
            // Execute with timeout
            let timeout_duration = Duration::from_secs(timeout_secs_val);
            let output_result = timeout(timeout_duration, cmd.output()).await;
            let duration_ms = start.elapsed().as_millis() as u64;

            match output_result {
                Ok(Ok(output)) => process_output(output, duration_ms),
                Ok(Err(e)) => {
                    warn!("Failed to execute check '{}': {}", step_name, e);
                    (false, Some(format!("Failed to execute check: {}", e)), None)
                }
                Err(_) => {
                    warn!(
                        "Check '{}' timed out after {}s",
                        step_name, timeout_secs_val
                    );
                    (
                        false,
                        Some(format!(
                            "Check timed out after {} seconds",
                            timeout_secs_val
                        )),
                        None,
                    )
                }
            }
        } else {
            // No timeout - execute without timeout wrapper
            let duration_ms = start.elapsed().as_millis() as u64;
            match cmd.output().await {
                Ok(output) => process_output(output, duration_ms),
                Err(e) => {
                    warn!("Failed to execute check '{}': {}", step_name, e);
                    (false, Some(format!("Failed to execute check: {}", e)), None)
                }
            }
        };

        // Emit completion event
        let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let duration = end_timestamp - timestamp;
        let total_duration_ms = start.elapsed().as_millis() as u64;

        let completed_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("CHECK: {}", step_name),
            "timestamp": end_timestamp,
            "status": if final_success { "success" } else { "failed" },
            "duration": duration,
            "metadata": {
                "check_type": check_type,
                "command": &command_display,
                "working_directory": working_directory.as_deref().unwrap_or(""),
                "duration_ms": total_duration_ms,
                "error": error_msg.as_deref().unwrap_or(""),
            }
        });

        let event_type = if final_success {
            "action_completed"
        } else {
            "action_failed"
        };

        // Emit completion tree event to file log
        FileLogger::log_tree_event(event_type, &completed_node, &[], end_timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: event_type.to_string(),
                timestamp: end_timestamp,
                data: json!({ "node": completed_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event(event_type, &completed_node, end_timestamp, sequence);

        (final_success, error_msg, output_data)
    }

    // =========================================================================
    // HTTP Status Check Execution
    // =========================================================================

    /// Execute an HTTP status check
    ///
    /// Makes an HTTP GET request to the specified URL and verifies the status code
    /// matches the expected value. Useful for health checks before running tests.
    /// timeout_secs: None = no timeout (disabled by default), Some(n) = timeout after n seconds
    async fn execute_http_status_check(
        &self,
        step: &ExecutionStepConfig,
        step_name: &str,
        timeout_secs: Option<u64>,
    ) -> (bool, Option<String>, Option<String>) {
        use std::time::Duration;

        // Get the URL to check
        let url = match &step.check_url {
            Some(u) => u.clone(),
            None => {
                return (
                    false,
                    Some("check_url is required for http_status check".to_string()),
                    None,
                );
            }
        };

        let expected_status = step.expected_status.unwrap_or(200);
        // Cap at 5 minutes if specified, otherwise use a large default for the HTTP client
        let timeout = timeout_secs
            .map(|t| Duration::from_secs(t.min(300)))
            .unwrap_or(Duration::from_secs(300)); // 5 min default for HTTP checks
        let timeout_str = timeout_secs
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "disabled".to_string());

        info!(
            "Executing HTTP status check '{}': url={}, expected_status={}, timeout={}",
            step_name, url, expected_status, timeout_str
        );

        // Generate sequence number and timestamp for tree events
        use std::sync::atomic::{AtomicU32, Ordering};
        static HTTP_CHECK_SEQUENCE: AtomicU32 = AtomicU32::new(1);
        let sequence = HTTP_CHECK_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let action_id = format!("http-check-{}", sequence);

        // Build action node for tree events
        let action_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("HTTP CHECK: {}", step_name),
            "timestamp": timestamp,
            "status": "pending",
            "metadata": {
                "check_type": "http_status",
                "url": &url,
                "expected_status": expected_status,
                "timeout_seconds": timeout.as_secs(),
            }
        });

        // Emit action_started tree event to file log
        FileLogger::log_tree_event("action_started", &action_node, &[], timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: "action_started".to_string(),
                timestamp,
                data: json!({ "node": action_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event("action_started", &action_node, timestamp, sequence);

        // Make the HTTP request
        let start = std::time::Instant::now();
        let client = match reqwest::Client::builder().timeout(timeout).build() {
            Ok(c) => c,
            Err(e) => {
                let error_msg = format!("Failed to create HTTP client: {}", e);
                warn!("{}", error_msg);
                return (false, Some(error_msg), None);
            }
        };

        let result = client.get(&url).send().await;
        let duration_ms = start.elapsed().as_millis() as u64;

        // Process the result
        let (final_success, error_msg, output_data) = match result {
            Ok(response) => {
                let actual_status = response.status().as_u16();
                info!(
                    "HTTP check '{}' completed: actual_status={}, expected={}, duration={}ms",
                    step_name, actual_status, expected_status, duration_ms
                );

                if actual_status == expected_status {
                    (
                        true,
                        None,
                        Some(
                            json!({
                                "status": actual_status,
                                "url": url,
                                "duration_ms": duration_ms
                            })
                            .to_string(),
                        ),
                    )
                } else {
                    (
                        false,
                        Some(format!(
                            "Expected status {} but got {} from {}",
                            expected_status, actual_status, url
                        )),
                        Some(
                            json!({
                                "status": actual_status,
                                "expected": expected_status,
                                "url": url,
                                "duration_ms": duration_ms
                            })
                            .to_string(),
                        ),
                    )
                }
            }
            Err(e) => {
                // Categorize error for better AI understanding
                let error_msg = if e.is_connect() {
                    format!(
                        "Server not running at {} - Connection refused. Make sure the service is started.",
                        url
                    )
                } else if e.is_timeout() {
                    format!(
                        "Server at {} not responding - Request timed out after {}s. The service may be overloaded or not running.",
                        url, timeout.as_secs()
                    )
                } else if e.is_request() {
                    format!("Invalid request to {}: {}", url, e)
                } else {
                    format!("Failed to reach {}: {}", url, e)
                };

                warn!("HTTP check '{}' failed: {}", step_name, error_msg);
                (
                    false,
                    Some(error_msg.clone()),
                    Some(
                        json!({
                            "error": error_msg,
                            "url": url,
                            "duration_ms": duration_ms
                        })
                        .to_string(),
                    ),
                )
            }
        };

        // Emit completion event
        let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let duration = end_timestamp - timestamp;

        let completed_node = json!({
            "id": &action_id,
            "node_type": "action",
            "name": format!("HTTP CHECK: {}", step_name),
            "timestamp": end_timestamp,
            "status": if final_success { "success" } else { "failed" },
            "duration": duration,
            "metadata": {
                "check_type": "http_status",
                "url": &url,
                "expected_status": expected_status,
                "duration_ms": duration_ms,
                "error": error_msg.as_deref().unwrap_or(""),
            }
        });

        let event_type = if final_success {
            "action_completed"
        } else {
            "action_failed"
        };

        // Emit completion tree event to file log
        FileLogger::log_tree_event(event_type, &completed_node, &[], end_timestamp, sequence);

        // Also add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: event_type.to_string(),
                timestamp: end_timestamp,
                data: json!({ "node": completed_node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // Emit to Tauri frontend for action log refresh
        self.emit_tree_event(event_type, &completed_node, end_timestamp, sequence);

        (final_success, error_msg, output_data)
    }

    // =========================================================================
    // Check Group Step Execution
    // =========================================================================

    /// Execute all checks in a check group
    /// Returns: (success, error_message, summary_text, individual_check_results)
    /// timeout_secs: None = no timeout (disabled by default), Some(n) = timeout after n seconds
    async fn execute_check_group_step(
        &self,
        step: &ExecutionStepConfig,
        _timeout_secs: Option<u64>,
    ) -> (
        bool,
        Option<String>,
        Option<String>,
        Option<Vec<IndividualCheckResult>>,
    ) {
        let step_name = step.name.as_deref().unwrap_or("Check Group");
        let group_id = match &step.check_group_id {
            Some(id) => id.clone(),
            None => {
                return (
                    false,
                    Some("No check group ID specified for check_group step".to_string()),
                    None,
                    None,
                );
            }
        };

        info!(
            "execute_check_group_step: step_name={:?}, group_id={:?}",
            step_name, group_id
        );

        // Get the checkpoint_db from app_state
        let db = &self.app_state.checkpoint_db;

        // Get the group
        let group = match db.get_check_group(&group_id) {
            Ok(Some(g)) => g,
            Ok(None) => {
                return (
                    false,
                    Some(format!("Check group not found: {}", group_id)),
                    None,
                    None,
                );
            }
            Err(e) => {
                return (
                    false,
                    Some(format!("Failed to get check group: {}", e)),
                    None,
                    None,
                );
            }
        };

        if !group.enabled {
            info!("Check group '{}' is disabled, skipping", group.name);
            return (
                true,
                None,
                Some(format!("Check group '{}' is disabled", group.name)),
                None,
            );
        }

        // Get checks in the group
        let checks = match db.get_checks_in_group(&group_id) {
            Ok(c) => c,
            Err(e) => {
                return (
                    false,
                    Some(format!("Failed to get checks in group: {}", e)),
                    None,
                    None,
                );
            }
        };

        if checks.is_empty() {
            return (
                true,
                None,
                Some(format!("No checks in group '{}'", group.name)),
                None,
            );
        }

        info!(
            "Executing check group '{}' with {} checks (stop_on_failure: {})",
            group.name,
            checks.len(),
            group.stop_on_failure
        );

        // Execute each check
        use crate::check_executor::{execute_check, CheckDefinition, CheckTool, CheckType};
        use std::time::Instant;

        let start_time = Instant::now();
        let mut passed = 0;
        let mut failed = 0;
        let mut skipped = 0;
        let mut results_output = Vec::new();
        let mut check_results: Vec<IndividualCheckResult> = Vec::new();

        for check in &checks {
            if !check.enabled {
                results_output.push(format!("  [SKIPPED] {} (disabled)", check.name));
                check_results.push(IndividualCheckResult {
                    name: check.name.clone(),
                    status: "skipped".to_string(),
                    duration_ms: 0,
                    issues_found: 0,
                    issues_fixed: 0,
                    files_checked: 0,
                    error_message: Some("Check is disabled".to_string()),
                    output: None,
                    issues: Vec::new(),
                });
                skipped += 1;
                continue;
            }

            let check_def = CheckDefinition {
                id: check.id.clone(),
                name: check.name.clone(),
                check_type: serde_json::from_str(&format!("\"{}\"", check.check_type))
                    .unwrap_or(CheckType::Lint),
                tool: serde_json::from_str(&format!("\"{}\"", check.tool))
                    .unwrap_or(CheckTool::Custom),
                command: check.command.clone(),
                working_directory: check.working_directory.clone(),
                config_path: check.config_path.clone(),
                auto_fix: check.auto_fix,
                fail_on_warning: check.fail_on_warning,
                timeout_seconds: check.timeout_seconds,
                is_critical: check.is_critical,
            };

            let result = execute_check(&check_def);
            let is_success = result.is_success();

            // Extract issues from structured output (limit to 50 to avoid huge payloads)
            let issues: Vec<CheckIssueDetail> = result
                .structured_output
                .as_ref()
                .map(|so| {
                    so.issues
                        .iter()
                        .take(50) // Limit to 50 issues per check
                        .map(|issue| CheckIssueDetail {
                            file: issue.file.clone(),
                            line: issue.line,
                            column: issue.column,
                            code: issue.code.clone(),
                            message: issue.message.clone(),
                            severity: format!("{:?}", issue.severity).to_lowercase(),
                            fixable: issue.fixable,
                        })
                        .collect()
                })
                .unwrap_or_default();

            // Build individual check result
            let check_result = IndividualCheckResult {
                name: check.name.clone(),
                status: if is_success { "passed" } else { "failed" }.to_string(),
                duration_ms: result.duration_ms,
                issues_found: result.issues_found,
                issues_fixed: result.issues_fixed,
                files_checked: result.files_checked,
                error_message: result.error.clone(),
                output: if result.output.len() > 2000 {
                    Some(format!("{}... (truncated)", &result.output[..2000]))
                } else if !result.output.is_empty() {
                    Some(result.output.clone())
                } else {
                    None
                },
                issues,
            };
            check_results.push(check_result);

            if is_success {
                passed += 1;
                results_output.push(format!(
                    "  [PASSED] {} ({}ms, {} issues found, {} fixed)",
                    check.name, result.duration_ms, result.issues_found, result.issues_fixed
                ));
            } else {
                failed += 1;
                results_output.push(format!(
                    "  [FAILED] {} ({}ms): {}",
                    check.name,
                    result.duration_ms,
                    result.error.as_deref().unwrap_or(&result.output)
                ));

                if group.stop_on_failure {
                    results_output.push("  Stopping due to stop_on_failure setting".to_string());
                    break;
                }
            }
        }

        let duration_ms = start_time.elapsed().as_millis();
        let total = passed + failed;
        let success = failed == 0;

        let summary = format!(
            "Check group '{}': {}/{} passed ({}ms total)\n{}",
            group.name,
            passed,
            total,
            duration_ms,
            results_output.join("\n")
        );

        info!(
            "Check group '{}' completed: {}/{} passed, {} skipped ({}ms)",
            group.name, passed, total, skipped, duration_ms
        );

        if success {
            (true, None, Some(summary), Some(check_results))
        } else {
            (
                false,
                Some(format!(
                    "Check group '{}' failed: {}/{} passed",
                    group.name, passed, total
                )),
                Some(summary),
                Some(check_results),
            )
        }
    }
}
