#![allow(dead_code)]

use super::config::get_effective_config_dir;
use super::process::spawn_and_wait_with_doctor;
use super::types::AiResponse;
use crate::doctor::DoctorHandle;
use crate::settings::{self, CliExecutionMode};
use crate::str_utils::truncate_str;
use tracing::{debug, error, info, warn};

/// Run a prompt via Claude CLI.
///
/// The process runs until completion — health monitoring is handled by the Doctor service.
pub(super) fn run_claude_cli(
    prompt: &str,
    settings: &settings::ClaudeCliSettings,
    model_override: Option<&str>,
    doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    let system = std::env::consts::OS;

    // Determine effective execution mode
    let effective_mode = match settings.execution_mode {
        CliExecutionMode::Auto => {
            if system == "windows" {
                CliExecutionMode::WindowsNative
            } else {
                CliExecutionMode::Native
            }
        }
        mode => mode,
    };

    let claude_program = settings.custom_path.as_deref().unwrap_or("claude");
    let effective_dir = get_effective_config_dir(settings);
    let config_dir = effective_dir.as_deref();

    info!(
        "Running Claude CLI (mode: {:?}, program: {}, config_dir: {:?}, model_override: {:?}, prompt_len: {})",
        effective_mode, claude_program, config_dir, model_override, prompt.len()
    );

    // On Windows, always use stdin piping (file-based approach) because
    // cmd.exe /c interprets special characters (", %, ^, &, |, >, <) in
    // command-line arguments, which corrupts JSON content in prompts.
    // On other platforms, only use stdin for long prompts.
    let use_stdin =
        matches!(effective_mode, CliExecutionMode::WindowsNative) || prompt.len() > 8000;

    if use_stdin {
        // Use file-based approach for long prompts (avoids Windows cmd length limits)
        run_claude_cli_with_file(
            prompt,
            claude_program,
            effective_mode,
            config_dir,
            model_override,
            doctor_handle,
        )
    } else {
        // Use command-line argument for short prompts
        run_claude_cli_with_arg(
            prompt,
            claude_program,
            effective_mode,
            config_dir,
            model_override,
            doctor_handle,
        )
    }
}

/// Run Claude CLI with prompt in a temp file (for long prompts).
///
/// Writes the prompt to a temp file and uses PowerShell to read it and pipe to Claude.
/// This avoids Windows command line length limitations.
/// The process runs until completion — health monitoring is handled by the Doctor service.
fn run_claude_cli_with_file(
    prompt: &str,
    claude_program: &str,
    effective_mode: CliExecutionMode,
    config_dir: Option<&str>,
    model_override: Option<&str>,
    doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    // Write prompt to a temp file
    let temp_dir = std::env::temp_dir();
    let prompt_file = temp_dir.join(format!("ai-prompt-{}.txt", uuid::Uuid::new_v4()));

    if let Err(e) = std::fs::write(&prompt_file, prompt) {
        return AiResponse::error(format!("Failed to write prompt to temp file: {}", e));
    }

    // Build a PowerShell command that reads the file and pipes to Claude
    let prompt_path = prompt_file.to_string_lossy();

    // Build model flag if override is provided
    let model_flag = model_override
        .map(|m| format!(" --model {}", m))
        .unwrap_or_default();

    let output_result = match effective_mode {
        CliExecutionMode::WindowsNative | CliExecutionMode::Auto => {
            // Use PowerShell to read file and pipe to Claude
            // This properly handles the stdin piping that cmd.exe struggles with
            // If config_dir is set, we need to set the env var in PowerShell
            let ps_command = if let Some(dir) = config_dir {
                format!(
                    "$env:CLAUDE_CONFIG_DIR = '{}'; Get-Content -Path '{}' -Raw -Encoding UTF8 | {} --print{}",
                    dir, prompt_path, claude_program, model_flag
                )
            } else {
                format!(
                    "Get-Content -Path '{}' -Raw -Encoding UTF8 | {} --print{}",
                    prompt_path, claude_program, model_flag
                )
            };
            spawn_and_wait_with_doctor(
                crate::process_helpers::no_window("powershell.exe").args([
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    &ps_command,
                ]),
                "Claude CLI response (file)",
                doctor_handle,
            )
        }
        CliExecutionMode::Wsl => {
            // For WSL, use cat to read file and pipe
            let wsl_path = prompt_file.to_string_lossy().replace("\\", "/");
            // Convert Windows path to WSL path
            let wsl_prompt = if let Some(dir) = config_dir {
                format!(
                    "export CLAUDE_CONFIG_DIR='{}'; cat '{}' | {} --print{}",
                    dir,
                    wsl_path.replace("C:", "/mnt/c"),
                    claude_program,
                    model_flag
                )
            } else {
                format!(
                    "cat '{}' | {} --print{}",
                    wsl_path.replace("C:", "/mnt/c"),
                    claude_program,
                    model_flag
                )
            };
            spawn_and_wait_with_doctor(
                crate::process_helpers::no_window("wsl").args(["bash", "-c", &wsl_prompt]),
                "Claude CLI response (WSL file)",
                doctor_handle,
            )
        }
        CliExecutionMode::Native => {
            // On Unix, use cat to read and pipe
            let native_cmd = if let Some(dir) = config_dir {
                format!(
                    "export CLAUDE_CONFIG_DIR='{}'; cat '{}' | {} --print{}",
                    dir, prompt_path, claude_program, model_flag
                )
            } else {
                format!(
                    "cat '{}' | {} --print{}",
                    prompt_path, claude_program, model_flag
                )
            };
            spawn_and_wait_with_doctor(
                crate::process_helpers::no_window("sh").args(["-c", &native_cmd]),
                "Claude CLI response (native file)",
                doctor_handle,
            )
        }
    };

    // Clean up temp file
    let _ = std::fs::remove_file(&prompt_file);

    match output_result {
        Ok(output) => process_cli_output(output),
        Err(e) => {
            let error_msg = format!(
                "Failed to execute Claude CLI: {}. Is Claude Code installed and in PATH?",
                e
            );
            error!("{}", error_msg);
            AiResponse::error(error_msg)
        }
    }
}

/// Run Claude CLI with prompt as command-line argument (for short prompts).
///
/// The process runs until completion — health monitoring is handled by the Doctor service.
fn run_claude_cli_with_arg(
    prompt: &str,
    claude_program: &str,
    effective_mode: CliExecutionMode,
    config_dir: Option<&str>,
    model_override: Option<&str>,
    doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    let output_result = match effective_mode {
        CliExecutionMode::WindowsNative | CliExecutionMode::Auto => {
            let mut cmd = crate::process_helpers::cmd_no_window();
            let mut args = vec!["/c", claude_program, "--print", "-p", prompt];
            if let Some(model) = model_override {
                args.push("--model");
                args.push(model);
            }
            cmd.args(&args);
            if let Some(dir) = config_dir {
                cmd.env("CLAUDE_CONFIG_DIR", dir);
            }
            spawn_and_wait_with_doctor(&mut cmd, "Claude CLI response (arg)", doctor_handle)
        }
        CliExecutionMode::Wsl => {
            let mut cmd = crate::process_helpers::no_window("wsl");
            let mut args = vec![claude_program, "--print", "-p", prompt];
            if let Some(model) = model_override {
                args.push("--model");
                args.push(model);
            }
            cmd.args(&args);
            if let Some(dir) = config_dir {
                cmd.env("CLAUDE_CONFIG_DIR", dir);
            }
            spawn_and_wait_with_doctor(&mut cmd, "Claude CLI response (WSL arg)", doctor_handle)
        }
        CliExecutionMode::Native => {
            let mut cmd = crate::process_helpers::no_window(claude_program);
            let mut args = vec!["--print", "-p", prompt];
            if let Some(model) = model_override {
                args.push("--model");
                args.push(model);
            }
            cmd.args(&args);
            if let Some(dir) = config_dir {
                cmd.env("CLAUDE_CONFIG_DIR", dir);
            }
            spawn_and_wait_with_doctor(&mut cmd, "Claude CLI response (native arg)", doctor_handle)
        }
    };

    match output_result {
        Ok(output) => process_cli_output(output),
        Err(e) => {
            let error_msg = format!(
                "Failed to execute Claude CLI: {}. Is Claude Code installed and in PATH?",
                e
            );
            error!("{}", error_msg);
            AiResponse::error(error_msg)
        }
    }
}

/// Process CLI output into AiResponse
///
/// Note: CLI providers don't expose token counts in their output,
/// so input_tokens and output_tokens will be None.
fn process_cli_output(output: std::process::Output) -> AiResponse {
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        debug!("Claude CLI response length: {} chars", stdout.len());
        if stdout.trim().is_empty() && !stderr.trim().is_empty() {
            // CLI exited 0 but produced no stdout — stderr likely has the real error
            warn!(
                "Claude CLI exited successfully but stdout is empty. stderr ({} chars): {}",
                stderr.len(),
                if stderr.len() > 1000 {
                    truncate_str(&stderr, 1000)
                } else {
                    &stderr
                }
            );
            // Return as error since empty output is not useful
            AiResponse::error(format!(
                "Claude CLI produced no output. stderr: {}",
                if stderr.len() > 500 {
                    truncate_str(&stderr, 500)
                } else {
                    &stderr
                }
            ))
        } else {
            AiResponse::success(stdout)
        }
    } else {
        let exit_code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());

        // Include stdout in error when stderr is empty (common on Windows where
        // PowerShell piping may redirect child stderr to its own error stream)
        let diagnostic = if stderr.trim().is_empty() && !stdout.trim().is_empty() {
            format!(
                "Claude CLI failed (exit {}): stdout: {}",
                exit_code,
                if stdout.len() > 500 {
                    truncate_str(&stdout, 500)
                } else {
                    &stdout
                }
            )
        } else if stderr.trim().is_empty() {
            format!(
                "Claude CLI failed (exit {}) with no output. Check that 'claude' is in PATH and authenticated.",
                exit_code
            )
        } else {
            format!("Claude CLI failed (exit {}): {}", exit_code, stderr)
        };

        error!("{}", diagnostic);
        AiResponse::error_with_output(stdout, diagnostic)
    }
}
