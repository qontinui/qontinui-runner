//! Shell Command Step Handler
//!
//! Handles shell command execution steps with variable expansion,
//! PowerShell detection, and timeout support.

use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};
use tracing::{info, warn};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::orchestrator::context_propagation::ExpressionEvaluator;
use crate::step_executor::events::TreeEventEmitter;
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for shell command execution steps.
pub struct ShellCommandHandler;

#[async_trait]
impl StepHandler for ShellCommandHandler {
    fn step_type(&self) -> &'static str {
        "shell_command"
    }

    fn display_name(&self) -> &'static str {
        "Shell Command"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // Get the command template
        let template_command = match &step.shell_command {
            Some(cmd) => cmd.clone(),
            None => {
                if step.shell_command_id.is_some() {
                    return StepHandlerResult::failure(
                        "Shell command lookup by ID not yet implemented in step executor",
                    );
                }
                return StepHandlerResult::failure("No shell command specified");
            }
        };

        // Expand variables in the command using runtime context
        let evaluator = ExpressionEvaluator::new();
        let has_variables = evaluator.has_expressions(&template_command);
        let command = evaluator.evaluate(&template_command, context.runtime_context());

        // Track which variables were resolved (for logging)
        let resolved_variables: Option<HashMap<String, String>> = if has_variables {
            let expressions = evaluator.find_expressions(&template_command);
            let mut vars = HashMap::new();
            for expr in expressions {
                let resolved =
                    evaluator.evaluate(&format!("{{{{{}}}}}", expr), context.runtime_context());
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

        let step_name = step.name.as_deref().unwrap_or("Shell Command");
        let working_directory = step.shell_command_working_directory.clone();
        let fail_on_error = step.shell_command_fail_on_error.unwrap_or(true);
        let timeout_secs = step.timeout_seconds;

        // Detect if command uses PowerShell syntax
        let is_powershell = Self::is_powershell_command(&command);
        let shell_type = Self::get_shell_type(is_powershell);

        let timeout_str = timeout_secs
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "disabled".to_string());
        info!(
            "Executing shell command '{}': {} (shell: {}, timeout: {}, working_dir: {:?})",
            step_name, command, shell_type, timeout_str, working_directory
        );

        // Generate action ID and emit start event
        let sequence = TreeEventEmitter::next_sequence();
        let action_id = TreeEventEmitter::generate_action_id("shell-command", sequence);

        // Truncate command for display
        let command_display = if command.len() > 50 {
            format!("{}...", &command[..50])
        } else {
            command.clone()
        };

        let metadata = json!({
            "command": &command_display,
            "shell_type": shell_type,
            "working_directory": working_directory.as_deref().unwrap_or(""),
            "timeout_seconds": timeout_secs,
        });

        let (start_timestamp, sequence) = context
            .event_emitter
            .emit_action_started(&action_id, &format!("SHELL: {}", step_name), metadata)
            .await;

        // Execute the command
        let start = std::time::Instant::now();
        let (success, exit_code, stdout, stderr) =
            Self::run_command(&command, is_powershell, working_directory.as_deref(), timeout_secs)
                .await;
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

        // Truncate stdout/stderr for display
        let stdout_display = Self::truncate_for_display(&stdout, 200);
        let stderr_display = Self::truncate_for_display(&stderr, 200);

        // Determine overall success based on fail_on_error setting
        let (final_success, error_msg, output_data) = if success {
            let output_data = if stdout.is_empty() {
                None
            } else {
                Some(json!(stdout))
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
            info!(
                "Shell command '{}' failed but fail_on_error=false, continuing",
                step_name
            );
            let error_msg = if !stderr.is_empty() {
                format!("(ignored) Command failed: {}", stderr.trim())
            } else {
                format!("(ignored) Command failed with exit code {:?}", exit_code)
            };
            (true, Some(error_msg), Some(json!(stdout)))
        };

        // Build result metadata
        let result_metadata = json!({
            "command": &command_display,
            "shell_type": shell_type,
            "working_directory": working_directory.as_deref().unwrap_or(""),
            "exit_code": exit_code,
            "stdout": &stdout_display,
            "stderr": &stderr_display,
            "duration_ms": duration_ms,
        });

        // Emit completion event
        if final_success {
            context
                .event_emitter
                .emit_action_completed(
                    &action_id,
                    &format!("SHELL: {}", step_name),
                    start_timestamp,
                    sequence,
                    result_metadata,
                )
                .await;

            let mut result = StepHandlerResult::success();
            if let Some(data) = output_data {
                result = result.with_data(data);
            }
            result
        } else {
            context
                .event_emitter
                .emit_action_failed(
                    &action_id,
                    &format!("SHELL: {}", step_name),
                    start_timestamp,
                    sequence,
                    error_msg.as_deref().unwrap_or("Unknown error"),
                    Some(result_metadata),
                )
                .await;

            StepHandlerResult::failure(error_msg.unwrap_or_else(|| "Command failed".to_string()))
        }
    }
}

impl ShellCommandHandler {
    /// Check if a command uses PowerShell syntax.
    fn is_powershell_command(command: &str) -> bool {
        command.contains("Get-")
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
            || command.contains("| ?")
    }

    /// Get the shell type string for logging.
    fn get_shell_type(is_powershell: bool) -> &'static str {
        if cfg!(target_os = "windows") && is_powershell {
            "powershell"
        } else if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "sh"
        }
    }

    /// Truncate a string for display purposes.
    fn truncate_for_display(s: &str, max_len: usize) -> String {
        if s.len() > max_len {
            format!("{}...", &s[..max_len])
        } else {
            s.to_string()
        }
    }

    /// Run a shell command with optional timeout.
    async fn run_command(
        command: &str,
        is_powershell: bool,
        working_directory: Option<&str>,
        timeout_secs: Option<u64>,
    ) -> (bool, Option<i32>, String, String) {
        // Build the command
        let mut cmd = if cfg!(target_os = "windows") {
            if is_powershell {
                let mut c = Command::new("powershell");
                c.args(["-NoProfile", "-NonInteractive", "-Command", command]);
                c
            } else {
                let mut c = Command::new("cmd");
                c.args(["/C", command]);
                c
            }
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", command]);
            c
        };

        // Set working directory if specified
        if let Some(wd) = working_directory {
            cmd.current_dir(wd);
        }

        // Capture stdout and stderr
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Execute with or without timeout
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
                    format!("Failed to execute command: {}", e),
                ),
                Err(_) => (
                    false,
                    None,
                    String::new(),
                    format!("Command timed out after {} seconds", timeout_secs_val),
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
                    format!("Failed to execute command: {}", e),
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_command_handler_step_type() {
        let handler = ShellCommandHandler;
        assert_eq!(handler.step_type(), "shell_command");
        assert_eq!(handler.display_name(), "Shell Command");
    }

    #[test]
    fn test_is_powershell_command() {
        assert!(ShellCommandHandler::is_powershell_command("Get-Process"));
        assert!(ShellCommandHandler::is_powershell_command("Set-Location"));
        assert!(ShellCommandHandler::is_powershell_command("$env:PATH"));
        assert!(ShellCommandHandler::is_powershell_command("gci | Where-Object { $_.Name }"));
        assert!(!ShellCommandHandler::is_powershell_command("echo hello"));
        assert!(!ShellCommandHandler::is_powershell_command("ls -la"));
    }

    #[test]
    fn test_truncate_for_display() {
        assert_eq!(
            ShellCommandHandler::truncate_for_display("short", 10),
            "short"
        );
        assert_eq!(
            ShellCommandHandler::truncate_for_display("this is a long string", 10),
            "this is a ..."
        );
    }
}
