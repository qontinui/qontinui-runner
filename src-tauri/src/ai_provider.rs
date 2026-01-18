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

use crate::ai_router::{TaskContext, TaskRouter};
use crate::settings::{self, AiProvider, CliExecutionMode};
use std::process::Command;
use tracing::{debug, error, info, warn};

/// Result of running an AI prompt
#[derive(Debug)]
pub struct AiResponse {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

/// Run an AI prompt synchronously and return the response.
///
/// This function selects the appropriate provider based on settings and
/// executes the prompt, waiting for the response.
///
/// # Arguments
/// * `prompt` - The prompt to send to the AI
/// * `timeout_seconds` - Maximum time to wait for response (0 = use default from settings)
///
/// # Returns
/// `AiResponse` with success status, output, and any error message
pub fn run_prompt_sync(prompt: &str, timeout_seconds: u64) -> AiResponse {
    let ai_settings = settings::get_ai_settings();
    let timeout = if timeout_seconds > 0 {
        timeout_seconds
    } else {
        ai_settings.claude_cli.timeout_seconds
    };

    info!(
        "Running AI prompt via {:?} (timeout: {}s, prompt length: {} chars)",
        ai_settings.provider,
        timeout,
        prompt.len()
    );

    match ai_settings.provider {
        AiProvider::ClaudeCli => run_claude_cli(prompt, &ai_settings.claude_cli, timeout, None),
        AiProvider::ClaudeApi => run_claude_api(prompt, &ai_settings.claude_api, timeout, None),
        AiProvider::GeminiCli => run_gemini_cli(prompt, &ai_settings.gemini_cli, timeout, None),
        AiProvider::GeminiApi => run_gemini_api(prompt, &ai_settings.gemini_api, timeout, None),
    }
}

/// Run an AI prompt with intelligent routing based on task complexity.
///
/// This function assesses the complexity of the task based on the provided context
/// and routes it to an appropriate model (simple tasks → cheaper models, complex → powerful models).
///
/// # Arguments
/// * `prompt` - The prompt to send to the AI
/// * `context` - Task context for complexity assessment (files, verification criteria, etc.)
/// * `timeout_seconds` - Maximum time to wait for response (0 = use default from settings)
///
/// # Returns
/// `AiResponse` with success status, output, and any error message
pub fn run_prompt_with_routing(
    prompt: &str,
    context: &TaskContext,
    timeout_seconds: u64,
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

    let timeout = if timeout_seconds > 0 {
        timeout_seconds
    } else {
        ai_settings.claude_cli.timeout_seconds
    };

    info!(
        "Running AI prompt via {:?} (timeout: {}s, prompt length: {} chars, routed: {})",
        ai_settings.provider,
        timeout,
        prompt.len(),
        model_override.is_some()
    );

    match ai_settings.provider {
        AiProvider::ClaudeCli => {
            run_claude_cli(prompt, &ai_settings.claude_cli, timeout, model_override.as_deref())
        }
        AiProvider::ClaudeApi => {
            run_claude_api(prompt, &ai_settings.claude_api, timeout, model_override.as_deref())
        }
        AiProvider::GeminiCli => {
            // Gemini routing only works within Gemini models, warn if trying to route to Claude model
            if let Some(ref model) = model_override {
                if model.starts_with("claude") {
                    warn!("Cannot route to Claude model when using Gemini provider, using default Gemini model");
                    run_gemini_cli(prompt, &ai_settings.gemini_cli, timeout, None)
                } else {
                    run_gemini_cli(prompt, &ai_settings.gemini_cli, timeout, model_override.as_deref())
                }
            } else {
                run_gemini_cli(prompt, &ai_settings.gemini_cli, timeout, None)
            }
        }
        AiProvider::GeminiApi => {
            if let Some(ref model) = model_override {
                if model.starts_with("claude") {
                    warn!("Cannot route to Claude model when using Gemini provider, using default Gemini model");
                    run_gemini_api(prompt, &ai_settings.gemini_api, timeout, None)
                } else {
                    run_gemini_api(prompt, &ai_settings.gemini_api, timeout, model_override.as_deref())
                }
            } else {
                run_gemini_api(prompt, &ai_settings.gemini_api, timeout, None)
            }
        }
    }
}

/// Run a prompt via Claude CLI
fn run_claude_cli(
    prompt: &str,
    settings: &settings::ClaudeCliSettings,
    _timeout_seconds: u64,
    model_override: Option<&str>,
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

    // For long prompts, use stdin piping instead of command-line argument
    // This avoids Windows' command line length limitations (8191 chars)
    let use_stdin = prompt.len() > 8000;

    if use_stdin {
        // Use file-based approach for long prompts (avoids Windows cmd length limits)
        run_claude_cli_with_file(prompt, claude_program, effective_mode, config_dir, model_override)
    } else {
        // Use command-line argument for short prompts
        run_claude_cli_with_arg(prompt, claude_program, effective_mode, config_dir, model_override)
    }
}

/// Run Claude CLI with prompt in a temp file (for long prompts)
///
/// Writes the prompt to a temp file and uses PowerShell to read it and pipe to Claude.
/// This avoids Windows command line length limitations.
fn run_claude_cli_with_file(
    prompt: &str,
    claude_program: &str,
    effective_mode: CliExecutionMode,
    config_dir: Option<&str>,
    model_override: Option<&str>,
) -> AiResponse {
    // Write prompt to a temp file
    let temp_dir = std::env::temp_dir();
    let prompt_file = temp_dir.join(format!("ai-prompt-{}.txt", uuid::Uuid::new_v4()));

    if let Err(e) = std::fs::write(&prompt_file, prompt) {
        return AiResponse {
            success: false,
            output: String::new(),
            error: Some(format!("Failed to write prompt to temp file: {}", e)),
        };
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
            Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    &ps_command,
                ])
                .output()
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
            Command::new("wsl")
                .args(["bash", "-c", &wsl_prompt])
                .output()
        }
        CliExecutionMode::Native => {
            // On Unix, use cat to read and pipe
            let native_cmd = if let Some(dir) = config_dir {
                format!(
                    "export CLAUDE_CONFIG_DIR='{}'; cat '{}' | {} --print{}",
                    dir, prompt_path, claude_program, model_flag
                )
            } else {
                format!("cat '{}' | {} --print{}", prompt_path, claude_program, model_flag)
            };
            Command::new("sh").args(["-c", &native_cmd]).output()
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
            AiResponse {
                success: false,
                output: String::new(),
                error: Some(error_msg),
            }
        }
    }
}

/// Run Claude CLI with prompt as command-line argument (for short prompts)
fn run_claude_cli_with_arg(
    prompt: &str,
    claude_program: &str,
    effective_mode: CliExecutionMode,
    config_dir: Option<&str>,
    model_override: Option<&str>,
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
            cmd.output()
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
            cmd.output()
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
            cmd.output()
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
            AiResponse {
                success: false,
                output: String::new(),
                error: Some(error_msg),
            }
        }
    }
}

/// Process CLI output into AiResponse
fn process_cli_output(output: std::process::Output) -> AiResponse {
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        debug!("Claude CLI response length: {} chars", stdout.len());
        AiResponse {
            success: true,
            output: stdout,
            error: None,
        }
    } else {
        error!("Claude CLI failed: {}", stderr);
        AiResponse {
            success: false,
            output: stdout,
            error: Some(format!("Claude CLI failed: {}", stderr)),
        }
    }
}

/// Run a prompt via Claude API (direct HTTP calls)
fn run_claude_api(
    prompt: &str,
    settings: &settings::ClaudeApiSettings,
    _timeout_seconds: u64,
    model_override: Option<&str>,
) -> AiResponse {
    use keyring::Entry;

    let model = model_override.unwrap_or(&settings.model);
    info!("Running Claude API (model: {}, override: {})", model, model_override.is_some());

    // Get API key from keychain
    let api_key = match Entry::new("com.qontinui.runner.ai", "claude_api") {
        Ok(entry) => match entry.get_password() {
            Ok(key) => key,
            Err(keyring::Error::NoEntry) => {
                return AiResponse {
                    success: false,
                    output: String::new(),
                    error: Some(
                        "No Claude API key configured. Please set your API key in Settings."
                            .to_string(),
                    ),
                }
            }
            Err(e) => {
                return AiResponse {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to retrieve API key: {}", e)),
                }
            }
        },
        Err(e) => {
            return AiResponse {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to access keychain: {}", e)),
            }
        }
    };

    // Use blocking reqwest client for synchronous HTTP request
    let client = match reqwest::blocking::Client::builder().build() {
        Ok(c) => c,
        Err(e) => {
            return AiResponse {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to create HTTP client: {}", e)),
            }
        }
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
                return AiResponse {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Claude API error ({}): {}", status, body)),
                };
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

                    AiResponse {
                        success: true,
                        output: content,
                        error: None,
                    }
                }
                Err(e) => AiResponse {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to parse API response: {}", e)),
                },
            }
        }
        Err(e) => AiResponse {
            success: false,
            output: String::new(),
            error: Some(format!("Claude API request failed: {}", e)),
        },
    }
}

/// Run a prompt via Gemini CLI
fn run_gemini_cli(
    prompt: &str,
    settings: &settings::GeminiCliSettings,
    _timeout_seconds: u64,
    model_override: Option<&str>,
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
        effective_mode, gemini_program, model, model_override.is_some()
    );

    let output_result = match effective_mode {
        CliExecutionMode::WindowsNative | CliExecutionMode::Auto => {
            // On Windows, use cmd.exe /c to handle .cmd files from npm install
            Command::new("cmd.exe")
                .args([
                    "/c",
                    gemini_program,
                    "--model",
                    model,
                    "-p",
                    prompt,
                ])
                .output()
        }
        CliExecutionMode::Wsl => Command::new("wsl")
            .args([gemini_program, "--model", model, "-p", prompt])
            .output(),
        CliExecutionMode::Native => Command::new(gemini_program)
            .args(["--model", model, "-p", prompt])
            .output(),
    };

    match output_result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if output.status.success() {
                debug!("Gemini CLI response length: {} chars", stdout.len());
                AiResponse {
                    success: true,
                    output: stdout,
                    error: None,
                }
            } else {
                error!("Gemini CLI failed: {}", stderr);
                AiResponse {
                    success: false,
                    output: stdout,
                    error: Some(format!("Gemini CLI failed: {}", stderr)),
                }
            }
        }
        Err(e) => {
            let error_msg = format!(
                "Failed to execute Gemini CLI: {}. Is Gemini CLI installed and in PATH?",
                e
            );
            error!("{}", error_msg);
            AiResponse {
                success: false,
                output: String::new(),
                error: Some(error_msg),
            }
        }
    }
}

/// Run a prompt via Gemini API (direct HTTP calls)
fn run_gemini_api(
    prompt: &str,
    settings: &settings::GeminiApiSettings,
    _timeout_seconds: u64,
    model_override: Option<&str>,
) -> AiResponse {
    use keyring::Entry;

    let model = model_override.unwrap_or(&settings.model);
    info!(
        "Running Gemini API (model: {}, temp: {}, override: {})",
        model, settings.temperature, model_override.is_some()
    );

    // Get API key from keychain
    let api_key = match Entry::new("com.qontinui.runner.ai", "gemini_api") {
        Ok(entry) => match entry.get_password() {
            Ok(key) => key,
            Err(keyring::Error::NoEntry) => {
                return AiResponse {
                    success: false,
                    output: String::new(),
                    error: Some(
                        "No Gemini API key configured. Please set your API key in Settings."
                            .to_string(),
                    ),
                }
            }
            Err(e) => {
                return AiResponse {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to retrieve API key: {}", e)),
                }
            }
        },
        Err(e) => {
            return AiResponse {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to access keychain: {}", e)),
            }
        }
    };

    // Use blocking reqwest client for synchronous HTTP request
    let client = match reqwest::blocking::Client::builder().build() {
        Ok(c) => c,
        Err(e) => {
            return AiResponse {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to create HTTP client: {}", e)),
            }
        }
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
                return AiResponse {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Gemini API error ({}): {}", status, body)),
                };
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

                    AiResponse {
                        success: true,
                        output: content,
                        error: None,
                    }
                }
                Err(e) => AiResponse {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to parse API response: {}", e)),
                },
            }
        }
        Err(e) => AiResponse {
            success: false,
            output: String::new(),
            error: Some(format!("Gemini API request failed: {}", e)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ai_response_success() {
        let response = AiResponse {
            success: true,
            output: "Hello!".to_string(),
            error: None,
        };
        assert!(response.success);
        assert_eq!(response.output, "Hello!");
        assert!(response.error.is_none());
    }

    #[test]
    fn test_ai_response_failure() {
        let response = AiResponse {
            success: false,
            output: String::new(),
            error: Some("Connection failed".to_string()),
        };
        assert!(!response.success);
        assert!(response.error.is_some());
    }
}
