//! In-app devenv enrollment commands (Phase 2 — no terminal).
//!
//! Wrap the shared `env_agent::enroll` core so the runner's Settings UI can
//! enroll this machine into a web devenv environment without a terminal: the
//! operator pastes the one-time code and clicks Enroll. `spawn_env_capture()`
//! (`main.rs`) picks up the mid-session enroll on its next tick — its enrollment
//! gate is checked INSIDE the loop — so the drift view populates without a
//! runner restart.

use serde::Serialize;
use tracing::info;

use qontinui_runner_lib::env_agent::enroll::{self, EnrollParams};

use super::CommandResponse;

/// Current enrollment status for the Settings panel (mirrors `env show`).
#[derive(Serialize)]
struct EnrollStatus {
    enrolled: bool,
    machine_id: Option<String>,
    environment_id: Option<String>,
    backend_url: Option<String>,
    enrolled_at: Option<String>,
    /// Whether a machine key is present in secure storage (capture can push).
    has_key: bool,
}

/// Enroll this machine into a devenv environment via a one-time code.
///
/// `backend` is optional — when omitted it resolves from the active profile's
/// `coord_url` (same order as the CLI). The enroll POST + key storage + config
/// write run on a blocking pool via the shared core; nothing is persisted on
/// failure.
#[tauri::command]
pub async fn devenv_enroll(
    code: String,
    backend: Option<String>,
) -> Result<CommandResponse, String> {
    let outcome = tokio::task::spawn_blocking(move || {
        let backend = enroll::resolve_backend_base(backend.as_deref())?;
        let (machine_id, hostname) = enroll::local_machine_identity();
        enroll::run_enroll(EnrollParams {
            code,
            backend,
            machine_id,
            hostname,
            environment_override: None,
        })
    })
    .await
    .map_err(|e| format!("enroll task panicked: {e}"))??;

    info!(
        "devenv_enroll: enrolled machine_id={} environment_id={} backend={}",
        outcome.machine_id, outcome.environment_id, outcome.backend_url
    );

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Enrolled into environment {} — config capture will start shortly.",
            outcome.environment_id
        )),
        data: Some(
            serde_json::to_value(outcome).map_err(|e| format!("serialize enroll outcome: {e}"))?,
        ),
    })
}

/// Read the current enrollment status (mirrors `qontinui_profile env show`).
#[tauri::command]
pub fn devenv_enroll_status() -> Result<CommandResponse, String> {
    use qontinui_runner_lib::env_agent::config::EnvAgentConfig;

    let cfg = EnvAgentConfig::load();
    let has_key = qontinui_runner_lib::secure_storage::SecureStorage::new()
        .ok()
        .and_then(|s| s.get_agent_machine_key().ok().flatten())
        .map(|k| !k.is_empty())
        .unwrap_or(false);

    let status = match cfg {
        Some(c) => EnrollStatus {
            enrolled: c.is_enrolled(),
            machine_id: Some(c.machine_id),
            environment_id: Some(c.environment_id),
            backend_url: Some(c.backend_url),
            enrolled_at: c.enrolled_at,
            has_key,
        },
        None => EnrollStatus {
            enrolled: false,
            machine_id: None,
            environment_id: None,
            backend_url: None,
            enrolled_at: None,
            has_key,
        },
    };

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(
            serde_json::to_value(status).map_err(|e| format!("serialize enroll status: {e}"))?,
        ),
    })
}
