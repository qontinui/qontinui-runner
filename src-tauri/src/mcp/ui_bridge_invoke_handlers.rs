//! HTTP handlers for the UI Bridge invoke proxy — Phase 3I.1 + 3I.2.
//!
//! Exposes two routes:
//!
//! - `GET  /ui-bridge/commands` — returns the static allowlist so callers
//!   can discover what's proxyable, along with the wire-level `args_schema`
//!   and `response_schema` for each command.
//! - `POST /ui-bridge/invoke/{command_name}` — dispatches the named Tauri
//!   command over Tauri IPC (via a round-trip through the React frontend's
//!   `invoke()` helper) and returns the result synchronously.
//!
//! See `src-tauri/src/ui_bridge_invoke.rs` for the store + allowlist.
//! See `src-tauri/src/mcp_api.rs` for the Tauri event listener that feeds
//! responses back from the frontend.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::Emitter;
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::ui_bridge_invoke::{is_allowlisted, ProxyableCommand, UI_BRIDGE_COMMANDS};

/// Wire descriptor returned by `GET /ui-bridge/commands`.
///
/// A thin owned clone of [`ProxyableCommand`] — the allowlist stores
/// `&'static str`s, but the JSON response expects owned strings so the
/// standard `ApiResponse` envelope serializes cleanly.
#[derive(Debug, Clone, Serialize)]
pub struct CommandDescriptor {
    pub name: String,
    pub description: String,
    pub args_schema: String,
    pub response_schema: String,
}

impl From<&ProxyableCommand> for CommandDescriptor {
    fn from(cmd: &ProxyableCommand) -> Self {
        Self {
            name: cmd.name.to_string(),
            description: cmd.description.to_string(),
            args_schema: cmd.args_schema.to_string(),
            response_schema: cmd.response_schema.to_string(),
        }
    }
}

/// Request body for `POST /ui-bridge/invoke/{command_name}`.
///
/// `args` is a free-form JSON object forwarded verbatim to the frontend's
/// `invoke(command, args)` call. Tauri's IPC maps top-level camelCase
/// keys to the Rust command's snake_case parameters.
#[derive(Debug, Deserialize)]
pub struct InvokeRequestBody {
    #[serde(default = "default_args")]
    pub args: Value,
}

fn default_args() -> Value {
    // Default to an empty object so commands that take no args work with
    // `POST .../invoke/<cmd>` and an empty body.
    serde_json::json!({})
}

/// Query string for `POST /ui-bridge/invoke/...?timeoutMs=N`.
///
/// Opt-in override for the default 30s timeout. Caps at whatever the
/// underlying tokio::time::timeout accepts (u64 ms); we don't enforce an
/// upper bound here because unusual deployments (slow networks, sleeping
/// runners) may legitimately need longer waits.
#[derive(Debug, Deserialize)]
pub struct InvokeTimeoutQuery {
    #[serde(default, rename = "timeoutMs")]
    pub timeout_ms: Option<u64>,
}

pub(crate) const DEFAULT_INVOKE_TIMEOUT_MS: u64 = 30_000;

/// `GET /ui-bridge/commands`
///
/// Return the static allowlist as JSON so external callers can discover
/// what commands this runner will proxy without reading the source.
pub async fn ui_bridge_commands_handler() -> Json<ApiResponse<Vec<CommandDescriptor>>> {
    let commands: Vec<CommandDescriptor> = UI_BRIDGE_COMMANDS.iter().map(Into::into).collect();
    Json(ApiResponse::success(commands))
}

/// `POST /ui-bridge/invoke/{command_name}`
///
/// Dispatch an allowlisted Tauri command over Tauri IPC and block (with
/// timeout) for the response from the React frontend.
///
/// Returns:
/// - 200 + `ApiResponse::success(value)` — command resolved; `value` is
///   the command's return value serialized as JSON (possibly `null` for
///   `()` returns).
/// - 400 — command not in the allowlist.
/// - 500 — frontend invoked the command but it threw; response body
///   includes the error string.
/// - 504 — frontend didn't respond before the timeout. The pending entry
///   is cancelled so a late response doesn't leak memory.
pub async fn ui_bridge_invoke_handler(
    State(state): State<Arc<ApiState>>,
    Path(command): Path<String>,
    Query(q): Query<InvokeTimeoutQuery>,
    Json(body): Json<InvokeRequestBody>,
) -> Result<Json<ApiResponse<Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Allowlist gate. Done before any resource allocation so the rejection
    // path is cheap. This is the INVOKE tier — unchanged by the observe-tier
    // work (P2). Observe has its own orthogonal gate; see the observe handler
    // in `mcp/ui_bridge/gated_flow.rs`.
    if !is_allowlisted(&command) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "command '{}' not in UI Bridge allowlist — call GET /ui-bridge/commands to list reachable commands",
                command
            ))),
        ));
    }

    let timeout_ms = q.timeout_ms.unwrap_or(DEFAULT_INVOKE_TIMEOUT_MS);
    let value = perform_invoke_round_trip(&state, &command, &body.args, timeout_ms).await?;
    Ok(Json(ApiResponse::success(value)))
}

/// Shared HTTP→Tauri invoke round-trip used by both the invoke proxy and the
/// observe tier (`mcp/ui_bridge/gated_flow.rs`).
///
/// Registers a oneshot keyed by a fresh `request_id`, emits
/// `ui-bridge:invoke-request` to the MAIN window, and awaits the frontend's
/// `InvokeResponse` with `timeout_ms`. Returns the raw command result `Value`
/// on success (possibly `Value::Null` for `()` returns), or the same
/// `(StatusCode, ApiResponse<()>)` error envelope the invoke handler has always
/// returned:
/// - 500 — emit failed, frontend threw, or the response channel closed.
/// - 504 — no frontend response before the timeout (pending entry cancelled so
///   a late response can't leak).
///
/// The caller is responsible for its OWN allow/observe gate BEFORE calling
/// this — this helper does not consult any allowlist. The observe tier applies
/// its projection to the returned `Value` so the raw payload never leaves the
/// process.
pub(crate) async fn perform_invoke_round_trip(
    state: &Arc<ApiState>,
    command: &str,
    args: &Value,
    timeout_ms: u64,
) -> Result<Value, (StatusCode, Json<ApiResponse<()>>)> {
    let request_id = uuid::Uuid::new_v4().to_string();

    // Reserve the pending slot before emitting the event so the React side can
    // never deliver faster than we register. (Under heavy load Tauri's event
    // delivery is reliably slower than our mutex lock, but we structure the
    // code to preserve the invariant.)
    let (sender, receiver) = oneshot::channel();
    state
        .ui_bridge_invoke_store
        .register(request_id.clone(), sender)
        .await;

    // Emit the event. If the emit itself fails (no webview, app shutting
    // down), release the pending slot and bail with 500.
    let payload = serde_json::json!({
        "request_id": request_id,
        "command": command,
        "args": args,
    });
    info!(
        command = %command,
        request_id = %request_id,
        timeout_ms,
        "ui_bridge_invoke: emitting ui-bridge:invoke-request"
    );
    // Target the canonical MAIN window only — see the matching emit in
    // mcp/ui_bridge/page.rs. A global broadcast reaches every webview window
    // (main + pop-out `term-N` terminals); each runs `invoke(command, args)`
    // and emits a duplicate `ui-bridge:invoke-response` for the same
    // request_id, fanning out side-effecting invokes and racing the oneshot.
    if let Err(e) = state.app_handle.emit_to(
        qontinui_runner_lib::get_main_window_label(),
        "ui-bridge:invoke-request",
        &payload,
    ) {
        state.ui_bridge_invoke_store.cancel(&request_id).await;
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "invoke proxy: failed to emit ui-bridge:invoke-request: {}",
                e
            ))),
        ));
    }

    // Block on the response with the configured timeout.
    let wait = tokio::time::timeout(Duration::from_millis(timeout_ms), receiver).await;

    match wait {
        Ok(Ok(resp)) => {
            // Frontend delivered a response.
            if resp.ok {
                Ok(resp.result.unwrap_or(Value::Null))
            } else {
                let err = resp.error.unwrap_or_else(|| {
                    "frontend reported invoke failure without an error message".to_string()
                });
                warn!(
                    command = %command,
                    request_id = %request_id,
                    error = %err,
                    "ui_bridge_invoke: frontend invoke returned error"
                );
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!(
                        "invoke proxy: frontend invoke of '{}' failed: {}",
                        command, err
                    ))),
                ))
            }
        }
        Ok(Err(_recv_err)) => {
            // Sender was dropped without sending — either the frontend
            // listener crashed, or the listener in `mcp_api.rs` parsed a
            // malformed payload and discarded it. Treat as a 500 so the
            // caller knows this isn't a timeout.
            state.ui_bridge_invoke_store.cancel(&request_id).await;
            warn!(
                command = %command,
                request_id = %request_id,
                "ui_bridge_invoke: oneshot sender dropped (frontend response lost)"
            );
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "invoke proxy: frontend response channel closed before a response was delivered (command={}, request_id={})",
                    command, request_id
                ))),
            ))
        }
        Err(_elapsed) => {
            // Remove the pending entry so a late response can't leak.
            state.ui_bridge_invoke_store.cancel(&request_id).await;
            warn!(
                command = %command,
                request_id = %request_id,
                timeout_ms,
                "ui_bridge_invoke: timed out waiting for frontend response"
            );
            Err((
                StatusCode::GATEWAY_TIMEOUT,
                Json(api_error(format!(
                    "invoke proxy: timed out waiting for frontend response (command={}, timeout={}ms)",
                    command, timeout_ms
                ))),
            ))
        }
    }
}
