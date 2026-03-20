#![allow(dead_code)]

use crate::doctor::{DoctorHandle, ProcessRegistration, ProcessType};
use std::process::{Command, Stdio};
use tracing::{debug, warn};

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
    // Remove CLAUDECODE env var so nested Claude CLI sessions don't refuse to start.
    // The runner legitimately needs to spawn Claude CLI as a subprocess, not as a nested session.
    cmd.env_remove("CLAUDECODE");

    // Inject trace ID for cross-process correlation
    cmd.env("QONTINUI_TRACE_ID", uuid::Uuid::new_v4().to_string());

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
