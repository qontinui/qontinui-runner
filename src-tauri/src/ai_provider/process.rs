#![allow(dead_code)]

use crate::doctor::{DoctorHandle, ProcessRegistration, ProcessType};
use std::process::{Command, Stdio};
use tracing::{debug, warn};

/// Apply the env preamble every AI-CLI one-shot spawned through this module
/// gets, ending with the credential-value scrub.
///
/// This is the shared choke point for BOTH the `claude --print` and the
/// `gemini -p` one-shots (`ai_provider::claude_cli::run_claude_cli`,
/// `ai_provider::gemini_cli::run_gemini_cli`, each in their WSL and native
/// arms), so covering it here covers all of them.
///
/// The scrub is deliberately the LAST env mutation: the caller owns the
/// `&mut Command` and has already finished setting `CLAUDE_CONFIG_DIR` and the
/// argv by the time it reaches [`spawn_and_wait_with_doctor`], and nothing
/// after this point touches the env. See
/// `crate::terminal::CREDENTIAL_VALUE_ENV_VARS` for why the runner is the
/// chokepoint and why the habitual `JWT|KEY|TOKEN|SECRET` redaction filter
/// misses these names.
///
/// Extracted from [`spawn_and_wait_with_doctor`] so the scrub call site is
/// unit-testable without spawning a real subprocess.
pub(crate) fn prepare_ai_child_env(cmd: &mut Command) {
    // Remove CLAUDECODE env var so nested Claude CLI sessions don't refuse to start.
    // The runner legitimately needs to spawn Claude CLI as a subprocess, not as a nested session.
    cmd.env_remove("CLAUDECODE");
    // Same rule, sibling marker — see `session::transport::claude_cli` docs.
    cmd.env_remove(qontinui_runner_lib::claude_env::CLAUDE_CHILD_SESSION_ENV);

    // Inject trace ID for cross-process correlation
    cmd.env("QONTINUI_TRACE_ID", uuid::Uuid::new_v4().to_string());

    // Full non-interactive git credential posture — this seam spawns a
    // `claude` that can run `git push`, so without it a credential prompt
    // is an infinite silent hang. One shared list, eight seams; see
    // `credential_helper::non_interactive_git_env`.
    crate::credential_helper::apply_non_interactive_git_env_std(cmd);
    crate::terminal::scrub_credential_env_std(cmd);
}

/// Spawn a command and register it with the Doctor health monitor.
///
/// If a `DoctorHandle` is provided, the process is registered before waiting
/// for output and unregistered afterwards. If no handle is provided, falls
/// back to the standard `.output()` call.
pub(super) fn spawn_and_wait_with_doctor(
    cmd: &mut Command,
    label: &str,
    doctor_handle: Option<&DoctorHandle>,
) -> std::io::Result<std::process::Output> {
    prepare_ai_child_env(cmd);

    match doctor_handle {
        Some(handle) => {
            // Must pipe stdout/stderr so wait_with_output() can capture them.
            // Without this, spawn() inherits parent's stdio and wait_with_output()
            // returns empty buffers.
            let child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
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

#[cfg(test)]
mod tests {
    use super::prepare_ai_child_env;
    use std::process::Command;

    // =======================================================================
    // AI one-shot choke point — production call-site coverage for the
    // credential scrub (plan
    // 2026-08-07-runner-context-visibility-and-session-env-secret-hygiene).
    //
    // `prepare_ai_child_env` IS the env preamble `spawn_and_wait_with_doctor`
    // applies, so deleting the `scrub_credential_env_std` call from it reddens
    // this test. It covers the `claude --print` AND `gemini -p` one-shots at
    // once — every arm of both providers routes through here.
    // =======================================================================

    #[test]
    fn ai_child_env_preamble_scrubs_credential_values() {
        let mut cmd = Command::new("dummy");
        // As the caller / inherited process env would have supplied them.
        for name in crate::terminal::CREDENTIAL_VALUE_ENV_VARS {
            cmd.env(name, "hunter2");
        }
        // A legitimate caller-set var from `run_claude_cli` / `run_gemini_cli`.
        cmd.env("CLAUDE_CONFIG_DIR", "/tmp/claude-config");

        prepare_ai_child_env(&mut cmd);

        crate::terminal::assert_credentials_scrubbed_std(&cmd, "prepare_ai_child_env");

        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().to_string(),
                    v.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect();
        assert!(
            envs.iter().any(
                |(k, v)| k == "CLAUDE_CONFIG_DIR" && v.as_deref() == Some("/tmp/claude-config")
            ),
            "the caller's account pin must survive the preamble"
        );
        // The nested-session markers this preamble also owns.
        for marker in ["CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION"] {
            assert!(
                envs.iter().any(|(k, v)| k == marker && v.is_none()),
                "{marker} must still be stripped"
            );
        }
    }

    /// The non-interactive git credential posture, asserted from the ONE shared
    /// list so this seam cannot drift from the other seven. Removing the
    /// `apply_non_interactive_git_env_*` call from the production function
    /// reddens this test.
    #[test]
    fn ai_child_env_preamble_applies_non_interactive_git_posture() {
        let mut cmd = Command::new("dummy");
        cmd.env("GIT_ASKPASS", "/some/gui/askpass");

        prepare_ai_child_env(&mut cmd);

        crate::credential_helper::assert_non_interactive_git_posture_std(
            &cmd,
            "prepare_ai_child_env",
        );
    }
}
