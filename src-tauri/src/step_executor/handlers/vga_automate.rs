//! Handler for the `vga_automate` step type.
//!
//! Delegates execution to the Python `qontinui.vga.worker` module, which loads
//! the referenced VGA state machine, focuses the target process, and executes
//! each action in the `action_sequence` by grounding the element prompt against
//! a fresh screenshot and dispatching the HAL click/type/wait primitive.
//!
//! The worker emits line-delimited JSON events on stdout:
//!   - `{"kind": "vga.step", ...}` — per action progress
//!   - `{"kind": "vga.done", "status": "success|failed", ...}` — terminal
//!
//! This handler spawns the worker, relays each `vga.step` event to the runner
//! event bus (so it surfaces in the Actions page / frontend live log), applies
//! the overall `timeout_ms`, and returns a structured result carrying the
//! `run_id` and terminal status.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command as TokioCommand;
use tracing::{debug, info, warn};

use super::{ExecutionStepConfig, HandlerContext, StepHandler, StepHandlerResult};

/// Default overall step timeout when `timeout_ms` is not specified.
const DEFAULT_TIMEOUT_MS: u64 = 300_000; // 5 minutes

/// Minimum and maximum bounds on the overall step timeout (from schema).
const MIN_TIMEOUT_MS: u64 = 1_000;
const MAX_TIMEOUT_MS: u64 = 3_600_000;

pub struct VgaAutomateHandler;

#[async_trait]
impl StepHandler for VgaAutomateHandler {
    fn step_type(&self) -> &'static str {
        "vga_automate"
    }

    fn display_name(&self) -> &'static str {
        "VGA Automate"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // 1. Parse and validate step config
        let state_machine_id = match step.vga_state_machine_id.clone() {
            Some(id) if !id.is_empty() => id,
            _ => {
                return StepHandlerResult::failure(
                    "vga_automate step missing required field state_machine_id",
                );
            }
        };
        let target_process = match step.vga_target_process.clone() {
            Some(p) if !p.is_empty() => p,
            _ => {
                return StepHandlerResult::failure(
                    "vga_automate step missing required field target_process",
                );
            }
        };
        let action_sequence = step
            .vga_action_sequence
            .clone()
            .unwrap_or_else(|| Value::Array(vec![]));
        if !action_sequence.is_array() {
            return StepHandlerResult::failure(
                "vga_automate: action_sequence must be a JSON array",
            );
        }
        let step_count = action_sequence.as_array().map(|a| a.len()).unwrap_or(0);

        let timeout_ms = step
            .vga_timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS);

        if step.vga_async.unwrap_or(false) {
            return StepHandlerResult::failure(
                "vga_automate: async mode is not yet supported (set async=false)",
            );
        }

        // 2. Emit action_started
        let action_id = format!("vga-automate-{}", state_machine_id);
        let step_name = step
            .name
            .clone()
            .unwrap_or_else(|| format!("VGA Automate [{}]", target_process));
        let action_name = format!("VGA: {}", step_name);
        let metadata = json!({
            "step_type": "vga_automate",
            "state_machine_id": state_machine_id,
            "target_process": target_process,
            "action_count": step_count,
            "timeout_ms": timeout_ms,
        });
        let (start_ts, sequence) = context
            .event_emitter
            .emit_action_started(&action_id, &action_name, metadata)
            .await;

        // 3. Write action_sequence to a temp JSON file (shared with Python worker)
        let temp_dir = std::env::temp_dir();
        let action_file =
            temp_dir.join(format!("qontinui-vga-actions-{}.json", uuid::Uuid::new_v4()));
        let action_json = match serde_json::to_string(&action_sequence) {
            Ok(s) => s,
            Err(e) => {
                let err = format!("Failed to serialize action_sequence: {}", e);
                context
                    .event_emitter
                    .emit_action_failed(&action_id, &action_name, start_ts, sequence, &err, None)
                    .await;
                return StepHandlerResult::failure(err);
            }
        };
        if let Err(e) = tokio::fs::write(&action_file, &action_json).await {
            let err = format!("Failed to write action_sequence temp file: {}", e);
            context
                .event_emitter
                .emit_action_failed(&action_id, &action_name, start_ts, sequence, &err, None)
                .await;
            return StepHandlerResult::failure(err);
        }

        // 4. Build the Python worker command
        let pg_url = std::env::var("RUNNER_DATABASE_URL").unwrap_or_else(|_| {
            "host=localhost port=5432 user=qontinui_user \
             password=qontinui_dev_password dbname=qontinui_db"
                .to_string()
        });

        let mut cmd = match build_vga_worker_command(&state_machine_id, &action_file, &target_process, &pg_url) {
            Ok(c) => c,
            Err(e) => {
                let _ = tokio::fs::remove_file(&action_file).await;
                context
                    .event_emitter
                    .emit_action_failed(&action_id, &action_name, start_ts, sequence, &e, None)
                    .await;
                return StepHandlerResult::failure(e);
            }
        };
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PYTHONUNBUFFERED", "1");

        info!(
            "Spawning VGA worker: state_machine={} target={} actions={} timeout_ms={}",
            state_machine_id, target_process, step_count, timeout_ms
        );

        // 5. Spawn and drive the worker under an overall timeout
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tokio::fs::remove_file(&action_file).await;
                let err = format!("Failed to spawn VGA worker: {}", e);
                context
                    .event_emitter
                    .emit_action_failed(&action_id, &action_name, start_ts, sequence, &err, None)
                    .await;
                return StepHandlerResult::failure(err);
            }
        };

        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                let _ = child.kill().await;
                let _ = tokio::fs::remove_file(&action_file).await;
                let err = "VGA worker stdout was not piped".to_string();
                context
                    .event_emitter
                    .emit_action_failed(&action_id, &action_name, start_ts, sequence, &err, None)
                    .await;
                return StepHandlerResult::failure(err);
            }
        };

        // Drain stderr in the background so the child doesn't block on a full pipe.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    debug!("[vga.worker stderr] {}", line);
                }
            });
        }

        let process_fut = async {
            let mut reader = BufReader::new(stdout).lines();
            let mut terminal_status: Option<String> = None;
            let mut terminal_error: Option<String> = None;
            let mut run_id: Option<String> = None;
            let mut step_events: u64 = 0;

            loop {
                match reader.next_line().await {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let value: Value = match serde_json::from_str(trimmed) {
                            Ok(v) => v,
                            Err(e) => {
                                debug!(
                                    "[vga.worker stdout non-JSON] {} (parse err: {})",
                                    trimmed, e
                                );
                                continue;
                            }
                        };
                        let kind = value.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                        match kind {
                            "vga.step" => {
                                step_events += 1;
                                info!(
                                    "vga.step: action={} status={} iou={:?}",
                                    value.get("action").and_then(|v| v.as_str()).unwrap_or("?"),
                                    value
                                        .get("status")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("?"),
                                    value.get("iou").and_then(|v| v.as_f64()),
                                );
                                // Surface progress as an intermediate tree event so
                                // it appears in the Actions page / frontend live log.
                                // TreeEventEmitter only exposes started/completed/failed
                                // today, so we emit a completed-shaped "sub-action" for
                                // each VGA step under a derived action_id.
                                let sub_id = format!("{}-step-{}", action_id, step_events);
                                let (sub_ts, sub_seq) = context
                                    .event_emitter
                                    .emit_action_started(
                                        &sub_id,
                                        &format!("VGA step: {}", sub_id),
                                        value.clone(),
                                    )
                                    .await;
                                context
                                    .event_emitter
                                    .emit_action_completed(
                                        &sub_id,
                                        &format!("VGA step: {}", sub_id),
                                        sub_ts,
                                        sub_seq,
                                        value.clone(),
                                    )
                                    .await;
                            }
                            "vga.done" => {
                                terminal_status = value
                                    .get("status")
                                    .and_then(|s| s.as_str())
                                    .map(|s| s.to_string());
                                terminal_error = value
                                    .get("error")
                                    .and_then(|e| e.as_str())
                                    .map(|s| s.to_string());
                                run_id = value
                                    .get("run_id")
                                    .and_then(|r| r.as_str())
                                    .map(|s| s.to_string());
                                info!(
                                    "vga.done: status={:?} run_id={:?}",
                                    terminal_status, run_id
                                );
                                break;
                            }
                            other => {
                                debug!("[vga.worker stdout unknown kind] kind={} line={}", other, trimmed);
                            }
                        }
                    }
                    Ok(None) => {
                        // EOF — worker closed stdout without emitting vga.done.
                        break;
                    }
                    Err(e) => {
                        warn!("Error reading vga worker stdout: {}", e);
                        break;
                    }
                }
            }

            // Wait for the process to exit to collect its status.
            let exit_status = child.wait().await;
            (
                exit_status,
                terminal_status,
                terminal_error,
                run_id,
                step_events,
            )
        };

        let timeout_fut = tokio::time::timeout(Duration::from_millis(timeout_ms), process_fut);
        let result = timeout_fut.await;

        // Clean up the temp action file regardless of outcome.
        let _ = tokio::fs::remove_file(&action_file).await;

        match result {
            Ok((exit_status, terminal_status, terminal_error, run_id, step_events)) => {
                let exit_ok = matches!(&exit_status, Ok(s) if s.success());
                let status_ok = terminal_status.as_deref() == Some("success");
                let run_id_val = run_id.clone().unwrap_or_default();

                if status_ok && exit_ok {
                    let data = json!({
                        "run_id": run_id_val,
                        "status": "success",
                        "step_count": step_count,
                        "step_events_emitted": step_events,
                    });
                    context
                        .event_emitter
                        .emit_action_completed(
                            &action_id,
                            &action_name,
                            start_ts,
                            sequence,
                            data.clone(),
                        )
                        .await;
                    StepHandlerResult::success_with_data(data)
                } else {
                    let err_msg = terminal_error.clone().unwrap_or_else(|| {
                        format!(
                            "VGA worker failed (terminal_status={:?}, exit_status={:?})",
                            terminal_status, exit_status
                        )
                    });
                    let data = json!({
                        "run_id": run_id_val,
                        "status": terminal_status.clone().unwrap_or_else(|| "failed".into()),
                        "error": err_msg,
                        "step_count": step_count,
                        "step_events_emitted": step_events,
                    });
                    context
                        .event_emitter
                        .emit_action_failed(
                            &action_id,
                            &action_name,
                            start_ts,
                            sequence,
                            &err_msg,
                            Some(data.clone()),
                        )
                        .await;
                    StepHandlerResult::failure_with_data(err_msg, data)
                }
            }
            Err(_elapsed) => {
                // Timeout: the child is still owned by the process_fut, which has
                // been dropped. tokio::process children do NOT kill on drop by
                // default, and we can't recover the handle here. Instead we rely
                // on `kill_on_drop(true)` being set on the Command (below) — but
                // Command::kill_on_drop is the correct fix, so ensure it was set.
                warn!(
                    "VGA worker timed out after {}ms; child kill delegated to kill_on_drop",
                    timeout_ms
                );
                let err_msg = format!("VGA worker exceeded timeout of {}ms", timeout_ms);
                let data = json!({
                    "status": "timeout",
                    "error": err_msg,
                    "timeout_ms": timeout_ms,
                });
                // Emit a structured timeout event for observability.
                let timeout_id = format!("{}-timeout", action_id);
                let (ts_ts, ts_seq) = context
                    .event_emitter
                    .emit_action_started(
                        &timeout_id,
                        "VGA timeout",
                        json!({ "timeout_ms": timeout_ms }),
                    )
                    .await;
                context
                    .event_emitter
                    .emit_action_failed(
                        &timeout_id,
                        "VGA timeout",
                        ts_ts,
                        ts_seq,
                        &err_msg,
                        Some(data.clone()),
                    )
                    .await;
                context
                    .event_emitter
                    .emit_action_failed(
                        &action_id,
                        &action_name,
                        start_ts,
                        sequence,
                        &err_msg,
                        Some(data.clone()),
                    )
                    .await;
                StepHandlerResult::failure_with_data(err_msg, data)
            }
        }
    }
}

/// Build the `tokio::process::Command` that runs `python -m qontinui.vga.worker …`.
///
/// Python resolution priority:
///   1. `QONTINUI_PYTHON_PATH` env var (advanced users)
///   2. Poetry in the `qontinui` source tree (when runnable as sibling repo)
///   3. System `python` / `python3` on PATH
fn build_vga_worker_command(
    state_machine_id: &str,
    action_file: &std::path::Path,
    target_process: &str,
    pg_url: &str,
) -> Result<TokioCommand, String> {
    // Priority 1: explicit override
    if let Ok(custom_python) = std::env::var("QONTINUI_PYTHON_PATH") {
        let custom_path = PathBuf::from(&custom_python);
        if custom_path.exists() {
            info!("VGA worker using QONTINUI_PYTHON_PATH: {:?}", custom_path);
            let mut cmd = crate::process_helpers::tokio_no_window(&custom_path);
            cmd.arg("-u")
                .arg("-m")
                .arg("qontinui.vga.worker")
                .arg("--state-machine-id")
                .arg(state_machine_id)
                .arg("--action-sequence-json")
                .arg(action_file)
                .arg("--target-process")
                .arg(target_process)
                .arg("--pg-url")
                .arg(pg_url);
            cmd.kill_on_drop(true);
            return Ok(cmd);
        } else {
            warn!(
                "QONTINUI_PYTHON_PATH set but path does not exist: {:?}; falling back",
                custom_path
            );
        }
    }

    // Priority 2: locate the qontinui python source tree (sibling of qontinui-runner)
    // and run through poetry if pyproject.toml exists.
    if let Some(qontinui_dir) = find_qontinui_dir() {
        let pyproject = qontinui_dir.join("pyproject.toml");
        if pyproject.exists() {
            info!("VGA worker using Poetry from: {:?}", qontinui_dir);
            let mut cmd = crate::process_helpers::tokio_no_window("poetry");
            cmd.current_dir(&qontinui_dir);
            cmd.arg("run")
                .arg("python")
                .arg("-u")
                .arg("-m")
                .arg("qontinui.vga.worker")
                .arg("--state-machine-id")
                .arg(state_machine_id)
                .arg("--action-sequence-json")
                .arg(action_file)
                .arg("--target-process")
                .arg(target_process)
                .arg("--pg-url")
                .arg(pg_url);
            cmd.kill_on_drop(true);
            return Ok(cmd);
        }
    }

    // Priority 3: system python fallback
    warn!("VGA worker: no poetry/venv found — falling back to system python");
    let python_bin = if cfg!(target_os = "windows") {
        "python"
    } else {
        "python3"
    };
    let mut cmd = crate::process_helpers::tokio_no_window(python_bin);
    cmd.arg("-u")
        .arg("-m")
        .arg("qontinui.vga.worker")
        .arg("--state-machine-id")
        .arg(state_machine_id)
        .arg("--action-sequence-json")
        .arg(action_file)
        .arg("--target-process")
        .arg(target_process)
        .arg("--pg-url")
        .arg(pg_url);
    cmd.kill_on_drop(true);
    Ok(cmd)
}

/// Locate the `qontinui` python source tree (sibling of `qontinui-runner`).
///
/// Searches upward from the current exe and the current working directory.
fn find_qontinui_dir() -> Option<PathBuf> {
    let candidates = [
        // From the runner's exe: target/debug/qontinui-runner.exe → qontinui-root/qontinui
        std::env::current_exe().ok().and_then(|exe| {
            exe.parent() // target/debug
                .and_then(|p| p.parent()) // target
                .and_then(|p| p.parent()) // src-tauri
                .and_then(|p| p.parent()) // qontinui-runner
                .and_then(|p| p.parent()) // qontinui-root
                .map(|root| root.join("qontinui"))
        }),
        // From cwd: …/qontinui-root/<anywhere>
        std::env::current_dir().ok().and_then(|cwd| {
            let mut p: &std::path::Path = &cwd;
            loop {
                let candidate = p.join("qontinui").join("pyproject.toml");
                if candidate.exists() {
                    return Some(p.join("qontinui"));
                }
                match p.parent() {
                    Some(parent) => p = parent,
                    None => return None,
                }
            }
        }),
    ];

    for candidate in candidates.iter().flatten() {
        if candidate.join("pyproject.toml").exists() {
            debug!("Found qontinui python dir at: {:?}", candidate);
            return Some(candidate.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_type_is_vga_automate() {
        assert_eq!(VgaAutomateHandler.step_type(), "vga_automate");
    }

    #[test]
    fn display_name_is_stable() {
        assert_eq!(VgaAutomateHandler.display_name(), "VGA Automate");
    }
}
