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
//! Phases 3-7 extend it by adding entries to
//! [`COMMAND_DISPATCH_TIMEOUT_S`] / `handlers/dispatch.py`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, error, info, warn};

/// Per-command synchronous timeout (seconds). The web-side
/// `dispatch_and_wait` default is 30s; we cap subprocess wall-clock at
/// the next-higher value so the runner side returns an error
/// envelope rather than letting the subprocess linger past the web's
/// timeout window.
const COMMAND_DISPATCH_TIMEOUT_S: u64 = 35;

/// Commands recognised by this dispatcher. Each entry MUST have a
/// corresponding handler in
/// `qontinui-runner/python-bridge/handlers/dispatch.py`.
pub fn is_supported_command(command_name: &str) -> bool {
    matches!(command_name, "state_machine.discover_ui_bridge")
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
        let mut c = Command::new("poetry");
        c.current_dir(&bridge_dir);
        c.arg("run").arg("python").arg("-u");
        c.arg("-m").arg("handlers.dispatch").arg(command_name);
        c
    } else {
        debug!("ws_bridge_dispatch: falling back to system python");
        let mut c = Command::new("python");
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

    let output_fut = child.wait_with_output();
    let output =
        match tokio::time::timeout(Duration::from_secs(COMMAND_DISPATCH_TIMEOUT_S), output_fut)
            .await
        {
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
                    COMMAND_DISPATCH_TIMEOUT_S, command_name
                );
                return error_envelope(
                    command_name,
                    payload,
                    "internal_error",
                    &format!("python dispatcher exceeded {COMMAND_DISPATCH_TIMEOUT_S}s wall-clock"),
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
