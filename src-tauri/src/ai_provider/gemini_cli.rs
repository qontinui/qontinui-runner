#![allow(dead_code)]

use super::process::spawn_and_wait_with_doctor;
use super::types::AiResponse;
use crate::doctor::DoctorHandle;
use crate::settings::{self, CliExecutionMode};
use tracing::{debug, error, info};

/// Run a prompt via Gemini CLI.
///
/// The process runs until completion — health monitoring is handled by the Doctor service.
pub(super) fn run_gemini_cli(
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
                crate::process_helpers::cmd_no_window().args([
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
            crate::process_helpers::no_window("wsl").args([
                gemini_program,
                "--model",
                model,
                "-p",
                prompt,
            ]),
            "Gemini CLI response (WSL)",
            doctor_handle,
        ),
        CliExecutionMode::Native => spawn_and_wait_with_doctor(
            crate::process_helpers::no_window(gemini_program)
                .args(["--model", model, "-p", prompt]),
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
