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

    // Silently refresh OAuth credentials if expired before spawning the subprocess.
    super::oauth_refresh::try_ensure_valid_credentials(config_dir);

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
                let escaped_dir = dir.replace('\'', "''");
                format!(
                    "$env:CLAUDE_CONFIG_DIR = '{}'; Get-Content -Path '{}' -Raw -Encoding UTF8 | {} --print{}",
                    escaped_dir, prompt_path, claude_program, model_flag
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
                let escaped_dir = dir.replace('\'', "'\\''");
                format!(
                    "export CLAUDE_CONFIG_DIR='{}'; cat '{}' | {} --print{}",
                    escaped_dir,
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
                let escaped_dir = dir.replace('\'', "'\\''");
                format!(
                    "export CLAUDE_CONFIG_DIR='{}'; cat '{}' | {} --print{}",
                    escaped_dir, prompt_path, claude_program, model_flag
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

// ── One-shot option scorer (terminal auto-response judge-rework Phase 2) ──────
//
// The terminal auto-response engine's `resolve_by_scoring` rules need each
// policy option scored on each dimension *before* coord composes a winner.
// Rather than coord running an LLM, the runner scores via its OWN authenticated
// Claude CLI — same binary resolution + config-dir/auth/env discipline as
// `run_claude_cli` above, but as a non-interactive `claude -p <prompt>` print
// call with a bounded timeout that returns the raw stdout (the scores JSON) and
// fails closed (`None`) on a nonzero exit / timeout / missing binary. The
// caller (`terminal::auto_response`) builds the prompt, parses the JSON, and is
// the one that fails-safe-injects-nothing on a `None`.

use crate::settings::ClaudeCliSettings;
use std::process::Stdio;
use std::time::Duration;

/// Path to the `claude` binary for the one-shot scorer. Honors
/// `QONTINUI_CLAUDE_BIN` (the same override the agent runtime + mock fixture
/// use) first, then the user's configured `custom_path`, then PATH `claude` —
/// so the scorer runs the SAME CLI the runner already uses.
fn scorer_claude_program(settings: &ClaudeCliSettings) -> String {
    std::env::var("QONTINUI_CLAUDE_BIN")
        .ok()
        .or_else(|| settings.custom_path.clone())
        .unwrap_or_else(|| "claude".to_string())
}

/// Run `claude -p <prompt>` (non-interactive print mode) once and return its
/// trimmed stdout, or `None` on any failure (spawn error / missing binary /
/// nonzero exit / timeout). Fail-closed by design — the auto-response caller
/// injects NOTHING on `None`.
///
/// Auth/config reuse: resolves the effective config dir the same way the rest
/// of the CLI provider does ([`get_effective_config_dir`]) and exports it as
/// `CLAUDE_CONFIG_DIR`, refreshing OAuth credentials first — so the one-shot
/// authenticates as the SAME account the runner's sessions use. `CLAUDECODE` is
/// removed so a runner already running inside Claude Code can still spawn the
/// nested print call (mirrors [`process::spawn_and_wait_with_doctor`]).
///
/// Bounded by `timeout`: the child is killed and `None` returned if it does not
/// exit in time, so a wedged CLI never strands the scheduler task.
pub(crate) fn score_options_via_cli(
    prompt: &str,
    settings: &ClaudeCliSettings,
    timeout: Duration,
) -> Option<String> {
    let program = scorer_claude_program(settings);
    let config_dir = get_effective_config_dir(settings);

    // Refresh OAuth before spawning, same as run_claude_cli.
    super::oauth_refresh::try_ensure_valid_credentials(config_dir.as_deref());

    // Build the command. On Windows route through cmd.exe /c (so a `.cmd`/`.ps1`
    // shim on PATH resolves), elsewhere invoke the program directly. The prompt
    // is piped over STDIN (not passed as a `-p` argv) — the scoring prompt is
    // long + multi-line + JSON-heavy, and cmd.exe corrupts special chars
    // (", %, ^, &, |, <, >) in arguments (the exact reason `run_claude_cli`
    // always pipes on Windows). `claude --print` reads the prompt from stdin
    // when no positional prompt is given.
    let mut cmd = if std::env::consts::OS == "windows" {
        let mut c = crate::process_helpers::cmd_no_window();
        c.args(["/c", &program, "--print"]);
        c
    } else {
        let mut c = crate::process_helpers::no_window(&program);
        c.arg("--print");
        c
    };
    if let Some(dir) = config_dir.as_deref() {
        cmd.env("CLAUDE_CONFIG_DIR", dir);
    }
    cmd.env_remove("CLAUDECODE");
    cmd.env("QONTINUI_TRACE_ID", uuid::Uuid::new_v4().to_string());
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "auto_response scorer: failed to spawn claude CLI — no scores");
            return None;
        }
    };

    // Write the prompt to the child's stdin and close it so the CLI sees EOF
    // and starts producing output. Failure to write → fail closed. The scoring
    // prompt is a few KB (terminal context + a handful of options/dimensions),
    // well under the OS pipe buffer, so a single blocking write-before-read
    // can't deadlock against the child filling its stdout pipe.
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        if let Err(e) = stdin.write_all(prompt.as_bytes()) {
            warn!(error = %e, "auto_response scorer: writing prompt to stdin failed — no scores");
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        // Drop closes stdin (EOF).
    }

    // Bounded wait: poll for exit up to `timeout`, then kill + fail closed.
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    warn!(
                        timeout_secs = timeout.as_secs(),
                        "auto_response scorer: claude CLI timed out — killing, no scores"
                    );
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                warn!(error = %e, "auto_response scorer: wait failed — no scores");
                let _ = child.kill();
                return None;
            }
        }
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            warn!(error = %e, "auto_response scorer: collecting output failed — no scores");
            return None;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            exit = ?output.status.code(),
            stderr = %truncate_str(&stderr, 300),
            "auto_response scorer: claude CLI nonzero exit — no scores"
        );
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        debug!("auto_response scorer: claude CLI produced empty stdout — no scores");
        return None;
    }
    Some(stdout)
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
