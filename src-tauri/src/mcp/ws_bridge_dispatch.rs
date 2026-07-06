//! Web -> runner WebSocket bridge: command dispatch to Python handlers.
//!
//! When `qontinui-web` HTTP endpoints need work that lives in the
//! `qontinui` Python library, they publish a typed command onto
//! `runner:commands:{runner_id}`. The
//! [`backend_relay`](crate::mcp::backend_relay) forwards the message
//! over the runner's WS connection; the runner-side route handlers in
//! that module dispatch into this module via
//! [`dispatch_command`].
//!
//! The dispatcher spawns a one-shot Python subprocess running
//! `handlers.dispatch <command_name>` from the
//! `qontinui-runner/python-bridge/` tree, pipes the JSON payload to its
//! stdin, and parses the JSON response from its stdout. The response
//! is then sent back over the WS as a `command_response` message; the
//! `runners_ws.py` handler on the web side broadcasts it on
//! `runner:responses:{runner_id}`, where the originating HTTP handler
//! (`dispatch_and_wait`) is subscribed and filtering for matching
//! `request_id`.
//!
//! Phase 2 of plan
//! `plans/2026-05-17-web-runner-ws-bridge-plan-b.md` introduces this
//! module + the single `state_machine.discover_ui_bridge` command.
//! Phase 3 adds `state_machine.ui_bridge.discover` and
//! `state_machine.ui_bridge.pathfind`. Phases 4-7 extend it further by
//! adding entries to [`is_supported_command`] / `handlers/dispatch.py`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, error, info, warn};

/// Default per-command synchronous timeout (seconds). The web-side
/// `dispatch_and_wait` default is 30s; we cap subprocess wall-clock at
/// the next-higher value so the runner side returns an error
/// envelope rather than letting the subprocess linger past the web's
/// timeout window.
const COMMAND_DISPATCH_TIMEOUT_S: u64 = 35;

/// Long-running command timeout (seconds). Phase 4's
/// `recording_pipeline.*` commands run the state-discovery +
/// transition-detection pipeline over thousands of UI events; non-trivial
/// recordings take minutes. The web-side handler dispatches these on a
/// background task with a matching long `timeout_s` and persists the
/// result asynchronously, so the longer subprocess wall-clock here is
/// intentional. Half-hour ceiling matches the web-side boot-time TTL on
/// `recording_pipeline_runs` rows; pipelines hitting this limit should
/// be investigated as legitimate runaway, not a config tweak.
const LONG_COMMAND_DISPATCH_TIMEOUT_S: u64 = 1800;

/// Resolve the per-command subprocess wall-clock timeout (seconds).
fn dispatch_timeout_for(command_name: &str) -> u64 {
    match command_name {
        "recording_pipeline.process"
        | "recording_pipeline.process_with_playbook"
        | "recording_pipeline.merge" => LONG_COMMAND_DISPATCH_TIMEOUT_S,
        _ => COMMAND_DISPATCH_TIMEOUT_S,
    }
}

/// Commands recognised by this dispatcher. Each entry MUST have a
/// corresponding handler in
/// `qontinui-runner/python-bridge/handlers/dispatch.py`.
pub fn is_supported_command(command_name: &str) -> bool {
    matches!(
        command_name,
        "state_machine.discover_ui_bridge"
            | "state_machine.ui_bridge.discover"
            | "state_machine.ui_bridge.pathfind"
            | "recording_pipeline.process"
            | "recording_pipeline.process_with_playbook"
            | "recording_pipeline.merge"
            | "vision.compute_perceptual_hash"
            | "vision.compare_screenshots"
            | "discovery.background_removal"
            | "click_analysis.tune_profile"
    )
}

/// Dispatch a WS bridge command to its Python handler.
///
/// Returns `Some(response_value)` on success or a structured error
/// envelope (also a `Value`) on failure. The caller wraps the result
/// in a `command_response` WS frame.
pub async fn dispatch_command(command_name: &str, payload: &Value) -> Value {
    info!(
        "ws_bridge_dispatch: starting command={} request_id={:?}",
        command_name,
        payload.get("request_id"),
    );

    if !is_supported_command(command_name) {
        warn!(
            "ws_bridge_dispatch: unknown command {} — returning error envelope",
            command_name
        );
        return error_envelope(
            command_name,
            payload,
            "unknown_command",
            &format!(
                "command {:?} is not registered on this runner",
                command_name
            ),
        );
    }

    let bridge_dir = match find_python_bridge_dir() {
        Some(p) => p,
        None => {
            error!("ws_bridge_dispatch: python-bridge dir not found");
            return error_envelope(
                command_name,
                payload,
                "internal_error",
                "could not locate qontinui-runner/python-bridge/ on disk",
            );
        }
    };

    let pyproject = bridge_dir.join("pyproject.toml");
    let mut cmd = if pyproject.exists() {
        debug!("ws_bridge_dispatch: using poetry from {:?}", bridge_dir);
        let mut c = crate::process_helpers::tokio_no_window("poetry");
        c.current_dir(&bridge_dir);
        c.arg("run").arg("python").arg("-u");
        c.arg("-m").arg("handlers.dispatch").arg(command_name);
        c
    } else {
        debug!("ws_bridge_dispatch: falling back to system python");
        let mut c = crate::process_helpers::tokio_no_window("python");
        c.current_dir(&bridge_dir);
        c.arg("-u")
            .arg("-m")
            .arg("handlers.dispatch")
            .arg(command_name);
        c
    };

    cmd.env("PYTHONUNBUFFERED", "1");
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            error!("ws_bridge_dispatch: spawn failed: {}", e);
            return error_envelope(
                command_name,
                payload,
                "internal_error",
                &format!("failed to spawn python dispatcher: {e}"),
            );
        }
    };

    // Write the payload to stdin and close.
    let payload_bytes = match serde_json::to_vec(payload) {
        Ok(b) => b,
        Err(e) => {
            return error_envelope(
                command_name,
                payload,
                "internal_error",
                &format!("payload serialize failed: {e}"),
            );
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(&payload_bytes).await {
            warn!("ws_bridge_dispatch: stdin write failed: {}", e);
            // Continue — child may still produce output / non-zero exit.
        }
        let _ = stdin.shutdown().await;
    }

    let timeout_s = dispatch_timeout_for(command_name);
    let output_fut = child.wait_with_output();
    let output = match tokio::time::timeout(Duration::from_secs(timeout_s), output_fut).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            error!("ws_bridge_dispatch: wait_with_output failed: {}", e);
            return error_envelope(
                command_name,
                payload,
                "internal_error",
                &format!("python process wait failed: {e}"),
            );
        }
        Err(_) => {
            error!(
                "ws_bridge_dispatch: timed out after {}s — command={}",
                timeout_s, command_name
            );
            return error_envelope(
                command_name,
                payload,
                "internal_error",
                &format!("python dispatcher exceeded {timeout_s}s wall-clock"),
            );
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            "ws_bridge_dispatch: dispatcher exit code={:?} stderr={}",
            output.status.code(),
            stderr,
        );
        // Try to parse stdout as the dispatcher's structured-error
        // envelope first (dispatcher.py emits one even on non-zero exit
        // for unknown_command / invalid_payload).
        if let Ok(parsed) = serde_json::from_slice::<Value>(&output.stdout) {
            return merge_with_correlation(command_name, payload, parsed);
        }
        return error_envelope(
            command_name,
            payload,
            "internal_error",
            &format!(
                "python dispatcher exited with code {:?}; stderr={}",
                output.status.code(),
                stderr,
            ),
        );
    }

    match serde_json::from_slice::<Value>(&output.stdout) {
        Ok(response) => {
            debug!(
                "ws_bridge_dispatch: command={} succeeded request_id={:?}",
                command_name,
                response.get("request_id"),
            );
            merge_with_correlation(command_name, payload, response)
        }
        Err(e) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            error!(
                "ws_bridge_dispatch: stdout JSON parse failed: {} stdout={}",
                e, stdout,
            );
            error_envelope(
                command_name,
                payload,
                "internal_error",
                &format!("python dispatcher stdout JSON parse failed: {e}"),
            )
        }
    }
}

/// Build a structured error envelope shaped like the
/// `qontinui_schemas.commands.state_machine.DiscoverUIBridgeError`
/// payload. Generic across all WS bridge commands.
fn error_envelope(command_name: &str, payload: &Value, error: &str, message: &str) -> Value {
    json!({
        "type": "command_response",
        "command": command_name,
        "request_id": payload.get("request_id"),
        "error": error,
        "message": message,
    })
}

/// Ensure the dispatcher response carries the wire-framing fields the
/// web-side `dispatch_and_wait` filter expects: `type`, `command`, and
/// the originating `request_id`. The Python handler already sets these,
/// but defending here keeps the contract local to this module.
fn merge_with_correlation(command_name: &str, payload: &Value, mut response: Value) -> Value {
    if let Some(obj) = response.as_object_mut() {
        obj.entry("type")
            .or_insert_with(|| Value::String("command_response".to_string()));
        obj.entry("command")
            .or_insert_with(|| Value::String(command_name.to_string()));
        if !obj.contains_key("request_id") {
            if let Some(rid) = payload.get("request_id") {
                obj.insert("request_id".to_string(), rid.clone());
            }
        }
    }
    response
}

/// Best-effort search for the `python-bridge/` directory containing the
/// handler module. Tries the executable-relative path used by
/// `executor::process::ProcessManager::find_bridge_script` first, then
/// `current_dir`-relative fallbacks.
fn find_python_bridge_dir() -> Option<PathBuf> {
    let candidates = vec![
        std::env::current_exe().ok().and_then(|p| {
            // Walk up from `<runner>/src-tauri/target/{debug,release}/qontinui-runner.exe`
            // to `<runner>/python-bridge/`.
            p.parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
                .map(|p| p.join("python-bridge"))
        }),
        std::env::current_dir().ok().and_then(|p| {
            if p.ends_with("debug") || p.ends_with("release") {
                p.parent()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.parent())
                    .map(|p| p.join("python-bridge"))
            } else if p.ends_with("src-tauri") {
                p.parent().map(|p| p.join("python-bridge"))
            } else {
                Some(p.join("python-bridge"))
            }
        }),
    ];

    candidates
        .into_iter()
        .flatten()
        .find(|p| p.join("handlers").join("dispatch.py").exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_supported_command_recognises_phase_2_and_3() {
        assert!(is_supported_command("state_machine.discover_ui_bridge"));
        assert!(is_supported_command("state_machine.ui_bridge.discover"));
        assert!(is_supported_command("state_machine.ui_bridge.pathfind"));
    }

    #[test]
    fn is_supported_command_recognises_phase_4_recording_pipeline() {
        assert!(is_supported_command("recording_pipeline.process"));
        assert!(is_supported_command(
            "recording_pipeline.process_with_playbook"
        ));
        assert!(is_supported_command("recording_pipeline.merge"));
    }

    #[test]
    fn is_supported_command_recognises_phase_5_click_analysis() {
        assert!(is_supported_command("click_analysis.tune_profile"));
    }

    #[test]
    fn is_supported_command_recognises_phase_6_discovery() {
        assert!(is_supported_command("discovery.background_removal"));
    }

    #[test]
    fn is_supported_command_rejects_unknown() {
        assert!(!is_supported_command("state_machine.unknown"));
        assert!(!is_supported_command("recording_pipeline.unknown"));
        assert!(!is_supported_command("discovery.unknown"));
        assert!(!is_supported_command(""));
    }

    #[test]
    fn dispatch_timeout_recording_pipeline_is_long() {
        // Phase 4 recording_pipeline.* commands run minute-scale; the
        // default 35s timeout would short-circuit them. Verify the
        // long-timeout branch fires for each Phase 4 command name.
        assert_eq!(
            dispatch_timeout_for("recording_pipeline.process"),
            LONG_COMMAND_DISPATCH_TIMEOUT_S
        );
        assert_eq!(
            dispatch_timeout_for("recording_pipeline.process_with_playbook"),
            LONG_COMMAND_DISPATCH_TIMEOUT_S
        );
        assert_eq!(
            dispatch_timeout_for("recording_pipeline.merge"),
            LONG_COMMAND_DISPATCH_TIMEOUT_S
        );
        // Sanity: the long timeout must exceed the default; otherwise
        // dispatch_timeout_for is a no-op for the Phase 4 commands.
        // `const_assert` would be preferable but is not in the std lib;
        // a runtime check at the bottom of an existing test is the next
        // simplest expression of the invariant.
        const _: () = assert!(LONG_COMMAND_DISPATCH_TIMEOUT_S > COMMAND_DISPATCH_TIMEOUT_S);
    }

    #[test]
    fn dispatch_timeout_phase_2_3_keeps_default() {
        // Phase 2/3 commands are sub-second to sub-30s; keep the default
        // dispatcher timeout so a hung subprocess surfaces quickly.
        assert_eq!(
            dispatch_timeout_for("state_machine.discover_ui_bridge"),
            COMMAND_DISPATCH_TIMEOUT_S
        );
        assert_eq!(
            dispatch_timeout_for("state_machine.ui_bridge.discover"),
            COMMAND_DISPATCH_TIMEOUT_S
        );
        assert_eq!(
            dispatch_timeout_for("state_machine.ui_bridge.pathfind"),
            COMMAND_DISPATCH_TIMEOUT_S
        );
        // Phase 6 discovery.background_removal is sub-5-second per the
        // plan; default timeout suffices.
        assert_eq!(
            dispatch_timeout_for("discovery.background_removal"),
            COMMAND_DISPATCH_TIMEOUT_S
        );
        // Unknown commands also fall through to the default; the
        // is_supported_command guard rejects them before the dispatch
        // path is reached.
        assert_eq!(
            dispatch_timeout_for("unknown_command"),
            COMMAND_DISPATCH_TIMEOUT_S
        );
    }
}
