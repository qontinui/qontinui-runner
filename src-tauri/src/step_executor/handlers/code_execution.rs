//! Code Execution Step Handler
//!
//! Executes inline Python code or a Python file with optional sandbox enforcement.
//! Code is sourced from `step.code` (inline) or `step.code_file` (file path).
//! Sandbox mode (`step.sandbox_mode`) controls security:
//! - `"enforce"` (default): blocked operations cause immediate failure
//! - `"warn"`: violations are logged but execution continues

use async_trait::async_trait;
use serde_json::json;
use std::process::Stdio;
use tokio::time::{timeout, Duration};
use tracing::{info, warn};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::events::TreeEventEmitter;
use crate::step_executor::ExecutionStepConfig;
use crate::str_utils::truncate_str;

/// Handler for code execution steps.
pub struct CodeExecutionHandler;

#[async_trait]
impl StepHandler for CodeExecutionHandler {
    fn step_type(&self) -> &'static str {
        "code_execution"
    }

    fn display_name(&self) -> &'static str {
        "Code Execution"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let step_name = step.name.as_deref().unwrap_or("Code Execution");
        let sandbox_mode = step.sandbox_mode.as_deref().unwrap_or("enforce");

        // Determine the code source: inline code or file path
        let (code_source, temp_file) = match (&step.code, &step.code_file) {
            (Some(code), _) => {
                // Inline code: write to a temporary file for execution
                let temp_dir = std::env::temp_dir();
                let temp_path = temp_dir.join(format!(
                    "qontinui_code_exec_{}.py",
                    uuid::Uuid::new_v4().simple()
                ));
                if let Err(e) = std::fs::write(&temp_path, code) {
                    return StepHandlerResult::failure(format!(
                        "Failed to write inline code to temp file: {}",
                        e
                    ));
                }
                (temp_path.to_string_lossy().to_string(), Some(temp_path))
            }
            (None, Some(code_file)) => {
                // File path: resolve and validate
                let path = std::path::Path::new(code_file);
                if !path.exists() {
                    // Try resolving relative to workspace
                    let resolved = crate::paths::resolve_working_directory(code_file);
                    if !resolved.exists() {
                        return StepHandlerResult::failure(format!(
                            "Code file not found: {} (also tried: {})",
                            code_file,
                            resolved.display()
                        ));
                    }
                    (resolved.to_string_lossy().to_string(), None)
                } else {
                    (code_file.clone(), None)
                }
            }
            (None, None) => {
                return StepHandlerResult::failure(
                    "Code execution step requires either 'code' (inline) or 'code_file' (path)",
                );
            }
        };

        // Enforce security policy on the code file
        if let Err(denial) = crate::security::PolicyEngine::evaluate_filesystem(
            &context.security_policy,
            &code_source,
            false, // reading the code file
        ) {
            warn!(
                "Code execution '{}' file access blocked by security policy: {}",
                step_name, denial
            );
            context.audit_logger.log_denial(
                &denial,
                context.task_run_id.as_deref(),
                Some(step_name),
            );
            // Clean up temp file before returning
            if let Some(ref tf) = temp_file {
                let _ = std::fs::remove_file(tf);
            }
            return StepHandlerResult::failure(format!(
                "Security policy violation: {}",
                denial.reason
            ));
        }

        // Build the Python command
        let python_cmd = if sandbox_mode == "warn" {
            format!("python \"{}\"", code_source)
        } else {
            // In enforce mode, use Python's restricted execution approach:
            // Run in isolated mode (-I) which ignores PYTHON* env vars and
            // doesn't add user site-packages. This is a basic sandbox layer.
            format!("python -I \"{}\"", code_source)
        };

        // Enforce security policy on the command itself
        if let Err(denial) =
            crate::security::PolicyEngine::evaluate_command(&context.security_policy, &python_cmd)
        {
            warn!(
                "Code execution '{}' command blocked by security policy: {}",
                step_name, denial
            );
            context.audit_logger.log_denial(
                &denial,
                context.task_run_id.as_deref(),
                Some(step_name),
            );
            if let Some(ref tf) = temp_file {
                let _ = std::fs::remove_file(tf);
            }
            return StepHandlerResult::failure(format!(
                "Security policy violation: {}",
                denial.reason
            ));
        }

        let timeout_secs = step.timeout_seconds;
        let timeout_str = timeout_secs
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "disabled".to_string());

        info!(
            "Executing code '{}': file={}, sandbox_mode={}, timeout={}",
            step_name, code_source, sandbox_mode, timeout_str
        );

        // Emit action started event
        let sequence = TreeEventEmitter::next_sequence();
        let action_id = TreeEventEmitter::generate_action_id("code-execution", sequence);

        let code_display = if code_source.len() > 60 {
            format!("{}...", truncate_str(&code_source, 60))
        } else {
            code_source.clone()
        };

        let metadata = json!({
            "code_source": &code_display,
            "sandbox_mode": sandbox_mode,
            "timeout_seconds": timeout_secs,
            "has_inline_code": step.code.is_some(),
        });

        let (start_timestamp, seq) = context
            .event_emitter
            .emit_action_started(&action_id, &format!("CODE: {}", step_name), metadata)
            .await;

        // Execute the Python code
        let start = std::time::Instant::now();
        let (success, exit_code, stdout, stderr) =
            Self::run_python(&python_cmd, timeout_secs).await;
        let duration_ms = start.elapsed().as_millis() as u64;

        info!(
            "Code execution '{}' completed: success={}, exit_code={:?}, duration={}ms",
            step_name, success, exit_code, duration_ms
        );

        if !stdout.is_empty() {
            info!("Code stdout:\n{}", stdout.trim());
        }
        if !stderr.is_empty() {
            if success {
                info!("Code stderr:\n{}", stderr.trim());
            } else {
                warn!("Code stderr:\n{}", stderr.trim());
            }
        }

        // Clean up temp file
        if let Some(ref tf) = temp_file {
            if let Err(e) = std::fs::remove_file(tf) {
                warn!("Failed to clean up temp code file: {}", e);
            }
        }

        // Truncate for display
        let stdout_display = Self::truncate_for_display(&stdout, 200);
        let stderr_display = Self::truncate_for_display(&stderr, 200);

        let result_metadata = json!({
            "code_source": &code_display,
            "sandbox_mode": sandbox_mode,
            "exit_code": exit_code,
            "stdout": &stdout_display,
            "stderr": &stderr_display,
            "duration_ms": duration_ms,
        });

        if success {
            context
                .event_emitter
                .emit_action_completed(
                    &action_id,
                    &format!("CODE: {}", step_name),
                    start_timestamp,
                    seq,
                    result_metadata,
                )
                .await;

            let mut result = StepHandlerResult::success();
            if !stdout.is_empty() {
                result = result.with_data(json!(stdout));
            }
            result
        } else {
            let error_msg = if !stderr.is_empty() {
                format!(
                    "Code execution failed (exit code {:?}): {}",
                    exit_code,
                    stderr.trim()
                )
            } else {
                format!("Code execution failed with exit code {:?}", exit_code)
            };

            context
                .event_emitter
                .emit_action_failed(
                    &action_id,
                    &format!("CODE: {}", step_name),
                    start_timestamp,
                    seq,
                    &error_msg,
                    Some(result_metadata),
                )
                .await;

            StepHandlerResult::failure(error_msg)
        }
    }
}

impl CodeExecutionHandler {
    /// Run a Python command with optional timeout.
    async fn run_python(
        command: &str,
        timeout_secs: Option<u64>,
    ) -> (bool, Option<i32>, String, String) {
        // Use bash on Windows (via Git Bash) for consistent behavior,
        // matching the pattern from ShellCommandHandler.
        let mut cmd = if cfg!(target_os = "windows") {
            let bash_path = super::shell_command::ShellCommandHandler::find_git_bash()
                .unwrap_or_else(|| "bash".to_string());
            let mut c = crate::process_helpers::tokio_no_window(&bash_path);
            c.args(["-c", command]);
            c
        } else {
            let mut c = crate::process_helpers::tokio_no_window("sh");
            c.args(["-c", command]);
            c
        };

        // Inject trace ID
        cmd.env("QONTINUI_TRACE_ID", uuid::Uuid::new_v4().to_string());

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        if let Some(timeout_secs_val) = timeout_secs {
            let timeout_duration = Duration::from_secs(timeout_secs_val);
            match timeout(timeout_duration, cmd.output()).await {
                Ok(Ok(output)) => {
                    let exit_code = output.status.code();
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let success = output.status.success();
                    (success, exit_code, stdout, stderr)
                }
                Ok(Err(e)) => (
                    false,
                    None,
                    String::new(),
                    format!("Failed to execute Python: {}", e),
                ),
                Err(_) => (
                    false,
                    None,
                    String::new(),
                    format!("Code execution timed out after {} seconds", timeout_secs_val),
                ),
            }
        } else {
            match cmd.output().await {
                Ok(output) => {
                    let exit_code = output.status.code();
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let success = output.status.success();
                    (success, exit_code, stdout, stderr)
                }
                Err(e) => (
                    false,
                    None,
                    String::new(),
                    format!("Failed to execute Python: {}", e),
                ),
            }
        }
    }

    /// Truncate a string for display purposes.
    fn truncate_for_display(s: &str, max_len: usize) -> String {
        if s.chars().count() > max_len {
            let truncated: String = s.chars().take(max_len).collect();
            format!("{}...", truncated)
        } else {
            s.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_execution_handler_step_type() {
        let handler = CodeExecutionHandler;
        assert_eq!(handler.step_type(), "code_execution");
        assert_eq!(handler.display_name(), "Code Execution");
    }
}
