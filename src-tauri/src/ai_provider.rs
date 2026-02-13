//! AI Provider Module
//!
//! Provides a unified interface for running AI prompts across different providers:
//! - Claude CLI (subscription-based, recommended)
//! - Claude API (per-token billing)
//! - Gemini CLI (OAuth or API key auth)
//! - Gemini API (direct HTTP calls)
//!
//! This module should be the ONLY place that spawns AI processes/calls.
//! Other modules should use this module to interact with AI providers.

#![allow(dead_code)]

use crate::ai_router::{TaskContext, TaskRouter};
use crate::config_facade::ai_keychain;
use crate::doctor::{DoctorHandle, ProcessRegistration, ProcessType};
use crate::settings::{self, AiProvider, CliExecutionMode};
use std::process::{Command, Stdio};
use tracing::{debug, error, info, warn};

/// Result of running an AI prompt
#[derive(Debug, Clone)]
pub struct AiResponse {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    /// Input tokens consumed (available for API providers only)
    pub input_tokens: Option<u64>,
    /// Output tokens generated (available for API providers only)
    pub output_tokens: Option<u64>,
}

impl AiResponse {
    /// Create a successful response with token information
    pub fn success_with_tokens(output: String, input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            success: true,
            output,
            error: None,
            input_tokens: Some(input_tokens),
            output_tokens: Some(output_tokens),
        }
    }

    /// Create a successful response without token information (for CLI providers)
    pub fn success(output: String) -> Self {
        Self {
            success: true,
            output,
            error: None,
            input_tokens: None,
            output_tokens: None,
        }
    }

    /// Create an error response
    pub fn error(message: String) -> Self {
        Self {
            success: false,
            output: String::new(),
            error: Some(message),
            input_tokens: None,
            output_tokens: None,
        }
    }

    /// Create an error response with partial output
    pub fn error_with_output(output: String, message: String) -> Self {
        Self {
            success: false,
            output,
            error: Some(message),
            input_tokens: None,
            output_tokens: None,
        }
    }

    /// Get total tokens (input + output)
    pub fn total_tokens(&self) -> Option<u64> {
        match (self.input_tokens, self.output_tokens) {
            (Some(input), Some(output)) => Some(input + output),
            _ => None,
        }
    }
}

/// Spawn a command and register it with the Doctor health monitor.
///
/// If a `DoctorHandle` is provided, the process is registered before waiting
/// for output and unregistered afterwards. If no handle is provided, falls
/// back to the standard `.output()` call.
fn spawn_and_wait_with_doctor(
    cmd: &mut Command,
    label: &str,
    doctor_handle: Option<&DoctorHandle>,
) -> std::io::Result<std::process::Output> {
    match doctor_handle {
        Some(handle) => {
            // Must pipe stdout/stderr so wait_with_output() can capture them.
            // Without this, spawn() inherits parent's stdio and wait_with_output()
            // returns empty buffers.
            let child = cmd
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            let pid = child.id();

            // Register with Doctor
            let reg = ProcessRegistration {
                pid,
                process_type: ProcessType::ResponseOneShot,
                label: label.to_string(),
                last_activity: None,
            };
            if let Err(e) = handle.register_blocking(reg) {
                warn!("Failed to register process with Doctor: {}", e);
            }

            let output = child.wait_with_output()?;

            // Unregister (Doctor will auto-unregister dead processes, but explicit is cleaner)
            if let Err(e) = handle.unregister_blocking(pid) {
                debug!("Failed to unregister process with Doctor: {}", e);
            }

            Ok(output)
        }
        None => cmd.output(),
    }
}

/// Run an AI prompt synchronously and return the response.
///
/// This function selects the appropriate provider based on settings and
/// executes the prompt, waiting for the response. The process runs until
/// completion — health monitoring is handled by the Doctor service.
///
/// # Arguments
/// * `prompt` - The prompt to send to the AI
/// * `doctor_handle` - Optional Doctor health monitor handle for process registration
///
/// # Returns
/// `AiResponse` with success status, output, and any error message
pub fn run_prompt_sync(prompt: &str, doctor_handle: Option<&DoctorHandle>) -> AiResponse {
    let ai_settings = settings::get_ai_settings();

    info!(
        "Running AI prompt via {:?} (prompt length: {} chars)",
        ai_settings.provider,
        prompt.len()
    );

    match ai_settings.provider {
        AiProvider::ClaudeCli => {
            run_claude_cli(prompt, &ai_settings.claude_cli, None, doctor_handle)
        }
        AiProvider::ClaudeApi => {
            run_claude_api(prompt, &ai_settings.claude_api, None, doctor_handle)
        }
        AiProvider::GeminiCli => {
            run_gemini_cli(prompt, &ai_settings.gemini_cli, None, doctor_handle)
        }
        AiProvider::GeminiApi => {
            run_gemini_api(prompt, &ai_settings.gemini_api, None, doctor_handle)
        }
    }
}

/// Run an AI prompt with intelligent routing based on task complexity.
///
/// This function assesses the complexity of the task based on the provided context
/// and routes it to an appropriate model (simple tasks -> cheaper models, complex -> powerful models).
/// The process runs until completion — health monitoring is handled by the Doctor service.
///
/// # Arguments
/// * `prompt` - The prompt to send to the AI
/// * `context` - Task context for complexity assessment (files, verification criteria, etc.)
/// * `doctor_handle` - Optional Doctor health monitor handle for process registration
///
/// # Returns
/// `AiResponse` with success status, output, and any error message
pub fn run_prompt_with_routing(
    prompt: &str,
    context: &TaskContext,
    doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    let ai_settings = settings::get_ai_settings();
    let routing_config = &ai_settings.routing;

    // Determine the model to use based on routing
    let model_override = if routing_config.enabled {
        let router = TaskRouter::new(routing_config.clone());
        let assessment = router.assess_and_route(context);
        let model = router.route_task(context);

        info!(
            "Task routing: complexity={:?}, confidence={:.2}, model={}",
            assessment.complexity, assessment.confidence, model
        );
        for factor in &assessment.factors {
            debug!("Routing factor: {}", factor);
        }

        Some(model)
    } else {
        debug!("Task routing disabled, using default model");
        None
    };

    info!(
        "Running AI prompt via {:?} (prompt length: {} chars, routed: {})",
        ai_settings.provider,
        prompt.len(),
        model_override.is_some()
    );

    match ai_settings.provider {
        AiProvider::ClaudeCli => run_claude_cli(
            prompt,
            &ai_settings.claude_cli,
            model_override.as_deref(),
            doctor_handle,
        ),
        AiProvider::ClaudeApi => run_claude_api(
            prompt,
            &ai_settings.claude_api,
            model_override.as_deref(),
            doctor_handle,
        ),
        AiProvider::GeminiCli => {
            // Gemini routing only works within Gemini models, warn if trying to route to Claude model
            if let Some(ref model) = model_override {
                if model.starts_with("claude") {
                    warn!("Cannot route to Claude model when using Gemini provider, using default Gemini model");
                    run_gemini_cli(prompt, &ai_settings.gemini_cli, None, doctor_handle)
                } else {
                    run_gemini_cli(
                        prompt,
                        &ai_settings.gemini_cli,
                        model_override.as_deref(),
                        doctor_handle,
                    )
                }
            } else {
                run_gemini_cli(prompt, &ai_settings.gemini_cli, None, doctor_handle)
            }
        }
        AiProvider::GeminiApi => {
            if let Some(ref model) = model_override {
                if model.starts_with("claude") {
                    warn!("Cannot route to Claude model when using Gemini provider, using default Gemini model");
                    run_gemini_api(prompt, &ai_settings.gemini_api, None, doctor_handle)
                } else {
                    run_gemini_api(
                        prompt,
                        &ai_settings.gemini_api,
                        model_override.as_deref(),
                        doctor_handle,
                    )
                }
            } else {
                run_gemini_api(prompt, &ai_settings.gemini_api, None, doctor_handle)
            }
        }
    }
}

/// Run a prompt via Claude CLI.
///
/// The process runs until completion — health monitoring is handled by the Doctor service.
fn run_claude_cli(
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
    let config_dir = settings.config_dir.as_deref();

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
                Command::new("powershell.exe").args([
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
                Command::new("wsl").args(["bash", "-c", &wsl_prompt]),
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
                Command::new("sh").args(["-c", &native_cmd]),
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
            let mut cmd = Command::new("cmd.exe");
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
            let mut cmd = Command::new("wsl");
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
            let mut cmd = Command::new(claude_program);
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
                if stderr.len() > 1000 { &stderr[..1000] } else { &stderr }
            );
            // Return as error since empty output is not useful
            AiResponse::error(format!(
                "Claude CLI produced no output. stderr: {}",
                if stderr.len() > 500 { &stderr[..500] } else { &stderr }
            ))
        } else {
            AiResponse::success(stdout)
        }
    } else {
        error!("Claude CLI failed (exit code {:?}): {}", output.status.code(), stderr);
        AiResponse::error_with_output(stdout, format!("Claude CLI failed: {}", stderr))
    }
}

/// Run a prompt via Claude API (direct HTTP calls)
///
/// Claude API responses include usage information:
/// ```json
/// {
///   "usage": {
///     "input_tokens": 123,
///     "output_tokens": 456
///   }
/// }
/// ```
fn run_claude_api(
    prompt: &str,
    settings: &settings::ClaudeApiSettings,
    model_override: Option<&str>,
    _doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    let model = model_override.unwrap_or(&settings.model);
    info!(
        "Running Claude API (model: {}, override: {})",
        model,
        model_override.is_some()
    );

    // Get API key from keychain using KeychainHelper
    let api_key = match ai_keychain().get("claude_api") {
        Ok(Some(key)) => key,
        Ok(None) => {
            return AiResponse::error(
                "No Claude API key configured. Please set your API key in Settings.".to_string(),
            )
        }
        Err(e) => return AiResponse::error(format!("Failed to retrieve API key: {}", e)),
    };

    // Use blocking reqwest client for synchronous HTTP request.
    // No timeout — the Doctor service monitors process health externally.
    let client = match reqwest::blocking::Client::builder().build() {
        Ok(c) => c,
        Err(e) => return AiResponse::error(format!("Failed to create HTTP client: {}", e)),
    };

    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "model": model,
            "max_tokens": settings.max_tokens,
            "messages": [{"role": "user", "content": prompt}]
        }))
        .send();

    match response {
        Ok(resp) => {
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().unwrap_or_default();
                return AiResponse::error(format!("Claude API error ({}): {}", status, body));
            }

            match resp.json::<serde_json::Value>() {
                Ok(json) => {
                    // Extract text content from response
                    let content = json["content"]
                        .as_array()
                        .and_then(|arr| arr.first())
                        .and_then(|c| c["text"].as_str())
                        .unwrap_or("")
                        .to_string();

                    // Extract token usage from response
                    // Claude API format: {"usage": {"input_tokens": N, "output_tokens": N}}
                    let input_tokens = json["usage"]["input_tokens"].as_u64();
                    let output_tokens = json["usage"]["output_tokens"].as_u64();

                    if let (Some(input), Some(output)) = (input_tokens, output_tokens) {
                        debug!(
                            "Claude API tokens - input: {}, output: {}, total: {}",
                            input,
                            output,
                            input + output
                        );
                        AiResponse::success_with_tokens(content, input, output)
                    } else {
                        debug!(
                            "Claude API response missing token counts: usage={:?}",
                            json["usage"]
                        );
                        AiResponse::success(content)
                    }
                }
                Err(e) => AiResponse::error(format!("Failed to parse API response: {}", e)),
            }
        }
        Err(e) => AiResponse::error(format!("Claude API request failed: {}", e)),
    }
}

/// Run a prompt via Gemini CLI.
///
/// The process runs until completion — health monitoring is handled by the Doctor service.
fn run_gemini_cli(
    prompt: &str,
    settings: &settings::GeminiCliSettings,
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

    let gemini_program = settings.custom_path.as_deref().unwrap_or("gemini");
    let model = model_override.unwrap_or(&settings.model);

    info!(
        "Running Gemini CLI (mode: {:?}, program: {}, model: {}, override: {})",
        effective_mode,
        gemini_program,
        model,
        model_override.is_some()
    );

    let output_result = match effective_mode {
        CliExecutionMode::WindowsNative | CliExecutionMode::Auto => {
            // On Windows, use cmd.exe /c to handle .cmd files from npm install
            spawn_and_wait_with_doctor(
                Command::new("cmd.exe").args([
                    "/c",
                    gemini_program,
                    "--model",
                    model,
                    "-p",
                    prompt,
                ]),
                "Gemini CLI response",
                doctor_handle,
            )
        }
        CliExecutionMode::Wsl => spawn_and_wait_with_doctor(
            Command::new("wsl").args([gemini_program, "--model", model, "-p", prompt]),
            "Gemini CLI response (WSL)",
            doctor_handle,
        ),
        CliExecutionMode::Native => spawn_and_wait_with_doctor(
            Command::new(gemini_program).args(["--model", model, "-p", prompt]),
            "Gemini CLI response (native)",
            doctor_handle,
        ),
    };

    match output_result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if output.status.success() {
                debug!("Gemini CLI response length: {} chars", stdout.len());
                // Gemini CLI doesn't expose token counts in stdout
                AiResponse::success(stdout)
            } else {
                error!("Gemini CLI failed: {}", stderr);
                AiResponse::error_with_output(stdout, format!("Gemini CLI failed: {}", stderr))
            }
        }
        Err(e) => {
            let error_msg = format!(
                "Failed to execute Gemini CLI: {}. Is Gemini CLI installed and in PATH?",
                e
            );
            error!("{}", error_msg);
            AiResponse::error(error_msg)
        }
    }
}

/// Run a prompt via Gemini API (direct HTTP calls)
///
/// Gemini API responses include usage metadata:
/// ```json
/// {
///   "usageMetadata": {
///     "promptTokenCount": 123,
///     "candidatesTokenCount": 456,
///     "totalTokenCount": 579
///   }
/// }
/// ```
fn run_gemini_api(
    prompt: &str,
    settings: &settings::GeminiApiSettings,
    model_override: Option<&str>,
    _doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    let model = model_override.unwrap_or(&settings.model);
    info!(
        "Running Gemini API (model: {}, temp: {}, override: {})",
        model,
        settings.temperature,
        model_override.is_some()
    );

    // Get API key from keychain using KeychainHelper
    let api_key = match ai_keychain().get("gemini_api") {
        Ok(Some(key)) => key,
        Ok(None) => {
            return AiResponse::error(
                "No Gemini API key configured. Please set your API key in Settings.".to_string(),
            )
        }
        Err(e) => return AiResponse::error(format!("Failed to retrieve API key: {}", e)),
    };

    // Use blocking reqwest client for synchronous HTTP request.
    // No timeout — the Doctor service monitors process health externally.
    let client = match reqwest::blocking::Client::builder().build() {
        Ok(c) => c,
        Err(e) => return AiResponse::error(format!("Failed to create HTTP client: {}", e)),
    };

    // Gemini API endpoint
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        model, api_key
    );

    let response = client
        .post(&url)
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "contents": [{"parts": [{"text": prompt}]}],
            "generationConfig": {
                "temperature": settings.temperature,
                "maxOutputTokens": settings.max_output_tokens
            }
        }))
        .send();

    match response {
        Ok(resp) => {
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().unwrap_or_default();
                return AiResponse::error(format!("Gemini API error ({}): {}", status, body));
            }

            match resp.json::<serde_json::Value>() {
                Ok(json) => {
                    // Extract text content from response
                    let content = json["candidates"]
                        .as_array()
                        .and_then(|arr| arr.first())
                        .and_then(|c| c["content"]["parts"].as_array())
                        .and_then(|parts| parts.first())
                        .and_then(|p| p["text"].as_str())
                        .unwrap_or("")
                        .to_string();

                    // Extract token usage from response
                    // Gemini API format: {"usageMetadata": {"promptTokenCount": N, "candidatesTokenCount": N}}
                    let input_tokens = json["usageMetadata"]["promptTokenCount"].as_u64();
                    let output_tokens = json["usageMetadata"]["candidatesTokenCount"].as_u64();

                    if let (Some(input), Some(output)) = (input_tokens, output_tokens) {
                        debug!(
                            "Gemini API tokens - input: {}, output: {}, total: {}",
                            input,
                            output,
                            input + output
                        );
                        AiResponse::success_with_tokens(content, input, output)
                    } else {
                        debug!(
                            "Gemini API response missing token counts: usageMetadata={:?}",
                            json["usageMetadata"]
                        );
                        AiResponse::success(content)
                    }
                }
                Err(e) => AiResponse::error(format!("Failed to parse API response: {}", e)),
            }
        }
        Err(e) => AiResponse::error(format!("Gemini API request failed: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ai_response_success() {
        let response = AiResponse::success("Hello!".to_string());
        assert!(response.success);
        assert_eq!(response.output, "Hello!");
        assert!(response.error.is_none());
        assert!(response.input_tokens.is_none());
        assert!(response.output_tokens.is_none());
        assert!(response.total_tokens().is_none());
    }

    #[test]
    fn test_ai_response_success_with_tokens() {
        let response = AiResponse::success_with_tokens("Hello!".to_string(), 100, 50);
        assert!(response.success);
        assert_eq!(response.output, "Hello!");
        assert!(response.error.is_none());
        assert_eq!(response.input_tokens, Some(100));
        assert_eq!(response.output_tokens, Some(50));
        assert_eq!(response.total_tokens(), Some(150));
    }

    #[test]
    fn test_ai_response_failure() {
        let response = AiResponse::error("Connection failed".to_string());
        assert!(!response.success);
        assert!(response.error.is_some());
        assert_eq!(response.error, Some("Connection failed".to_string()));
        assert!(response.input_tokens.is_none());
        assert!(response.output_tokens.is_none());
    }

    #[test]
    fn test_ai_response_failure_with_output() {
        let response = AiResponse::error_with_output(
            "partial output".to_string(),
            "Error occurred".to_string(),
        );
        assert!(!response.success);
        assert_eq!(response.output, "partial output");
        assert_eq!(response.error, Some("Error occurred".to_string()));
    }
}
