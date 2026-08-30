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
/// - 503 + `code: SERVER_MODE_NO_WEBVIEW` — this runner is headless
///   (`QONTINUI_SERVER_MODE`), so there is no frontend that could ever
///   answer. Returned immediately, without waiting out `timeout_ms`.
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

/// Machine-readable `code` on the headless-runner rejection.
///
/// Distinct from a genuine 504 on purpose: a caller that sees this knows no
/// amount of waiting, retrying, or raising `timeoutMs` will help — this
/// process has no webview and never will.
pub(crate) const SERVER_MODE_NO_WEBVIEW_CODE: &str = "SERVER_MODE_NO_WEBVIEW";

/// Reject an invoke that can never be answered, BEFORE any waiting happens.
///
/// A runner launched with `QONTINUI_SERVER_MODE` never creates a main window
/// (`main.rs`'s `if server_mode { ... }` arm skips `build_main_window`), so
/// there is no frontend to run `invoke(command, args)` and emit
/// `ui-bridge:invoke-response`. Before this gate existed, every UI-Bridge
/// invoke against a headless runner sat out the full
/// [`DEFAULT_INVOKE_TIMEOUT_MS`] (30s) and then reported a 504 — which reads
/// as "the frontend is slow/wedged" when the truth is "there is no frontend".
///
/// `server_mode` is passed in rather than read here so the mapping is unit
/// testable; production callers pass
/// [`crate::webview_recovery::is_server_mode`].
///
/// Returns `None` on a windowed runner — the round-trip proceeds unchanged,
/// including its 504 timeout arm, which must keep meaning "a webview exists
/// but did not answer".
pub(crate) fn no_webview_rejection(
    command: &str,
    server_mode: bool,
) -> Option<(StatusCode, Json<ApiResponse<()>>)> {
    if !server_mode {
        return None;
    }
    Some((
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiResponse::<()>::error_with_code_and_suggestions(
            format!(
                "invoke proxy: this runner was launched in server mode (QONTINUI_SERVER_MODE) \
                 and has no webview, so no frontend can answer the invoke of '{}'. This is not \
                 a timeout and will not succeed on retry.",
                command
            ),
            SERVER_MODE_NO_WEBVIEW_CODE,
            vec![
                "Pair this runner over the CLI door instead of the UI: \
                 `qontinui_profile device pair --pair-code <CODE>`"
                    .to_string(),
                "Confirm the diagnosis with `GET /health` — a server-mode runner reports \
                 frontendState \"window_missing\" and never grows a window."
                    .to_string(),
                "If you need a webview-backed invoke, run a windowed runner (launch without \
                 QONTINUI_SERVER_MODE)."
                    .to_string(),
            ],
        )),
    ))
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
/// - 503 — this runner is in server mode and has no webview at all; see
///   [`no_webview_rejection`]. Checked FIRST, before the oneshot is
///   registered and before anything is emitted, so a headless caller fails
///   in microseconds instead of waiting out `timeout_ms`.
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
    // Headless fast-fail, BEFORE the oneshot is registered and before anything
    // is emitted. On a server-mode runner there is no webview to answer, so
    // every path below can only end in the 30s timeout. See
    // `no_webview_rejection`.
    if let Some(rejection) =
        no_webview_rejection(command, crate::webview_recovery::is_server_mode())
    {
        warn!(
            command = %command,
            "ui_bridge_invoke: rejecting invoke — runner is in server mode and has no webview"
        );
        return Err(rejection);
    }

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

    // Emit the event. If the emit itself fails, release the pending slot and
    // bail with 500.
    //
    // CORRECTION (2026-08-29): this comment used to claim the 500 arm covers
    // "no webview". It does not. `emit_to` a window label that does not exist
    // succeeds SILENTLY — Tauri resolves the label against the live window
    // map and simply delivers to nobody, returning `Ok(())`. So on a headless
    // runner the emit never errored; the call fell straight through to the
    // `receiver` await and reported a 504 thirty seconds later. That is why the
    // server-mode gate at the top of this function exists: it is the only thing
    // standing between a webview-less runner and a 30-second lie. In practice
    // this 500 arm fires for shutdown/IPC-serialization failures, not for a
    // missing window.
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

#[cfg(test)]
mod server_mode_gate_tests {
    use super::*;

    /// Unwrap the rejection into `(status, body)` so assertions read cleanly.
    fn reject(command: &str, server_mode: bool) -> (StatusCode, ApiResponse<()>) {
        let (status, Json(body)) = no_webview_rejection(command, server_mode)
            .unwrap_or_else(|| panic!("expected a rejection for server_mode={server_mode}"));
        (status, body)
    }

    #[test]
    fn server_mode_invoke_is_rejected_with_503() {
        let (status, body) = reject("dismiss_recent_crash", true);
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "a webview-less runner must say 'I cannot serve this', not 'the frontend was slow'"
        );
        assert!(!body.success);
        assert!(body.error.is_some());
    }

    #[test]
    fn server_mode_rejection_carries_a_distinct_machine_readable_code() {
        let (_, body) = reject("dismiss_recent_crash", true);
        assert_eq!(body.code.as_deref(), Some(SERVER_MODE_NO_WEBVIEW_CODE));
        assert_eq!(SERVER_MODE_NO_WEBVIEW_CODE, "SERVER_MODE_NO_WEBVIEW");

        // The 504 timeout arm is built with `api_error`, which sets no `code`.
        // That asymmetry is the whole point: a caller can tell "no webview
        // exists" apart from "the webview did not answer in time".
        let timeout_body = api_error("invoke proxy: timed out waiting for frontend response");
        assert!(
            timeout_body.code.is_none(),
            "if the timeout arm ever grows a code it must NOT be {SERVER_MODE_NO_WEBVIEW_CODE}"
        );
    }

    #[test]
    fn server_mode_rejection_names_the_cause_and_the_cli_pairing_door() {
        let (_, body) = reject("redeem_pair_code", true);
        let error = body.error.expect("error message present");
        assert!(
            error.contains("server mode") && error.contains("QONTINUI_SERVER_MODE"),
            "the body must name the cause, not just fail: {error}"
        );
        assert!(
            error.contains("no webview"),
            "the body must say there is no webview: {error}"
        );
        assert!(
            error.contains("redeem_pair_code"),
            "the body must name the command that was refused: {error}"
        );

        let suggestions = body.suggestions.expect("recovery suggestions present");
        assert!(
            suggestions
                .iter()
                .any(|s| s.contains("qontinui_profile device pair --pair-code <CODE>")),
            "the caller must be pointed at the CLI pairing door: {suggestions:?}"
        );
    }

    #[test]
    fn a_windowed_runner_is_not_rejected() {
        // The whole point of gating on `server_mode` rather than on "did the
        // frontend answer": a windowed runner whose frontend is wedged must
        // still go the full round-trip and read as a 504, not a 503.
        assert!(
            no_webview_rejection("dismiss_recent_crash", false).is_none(),
            "a windowed runner must fall through to the unchanged round-trip"
        );
    }

    #[test]
    fn server_mode_rejection_does_not_wait_out_the_invoke_timeout() {
        // The defect this closes was a 30.018s 504. The gate is a synchronous
        // branch taken before the oneshot is registered and before any emit,
        // so it cannot consult a clock at all; assert that empirically with a
        // bound orders of magnitude under the timeout it replaces.
        let started = std::time::Instant::now();
        let (status, _) = reject("dismiss_recent_crash", true);
        let elapsed = started.elapsed();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            elapsed < Duration::from_millis(DEFAULT_INVOKE_TIMEOUT_MS / 100),
            "rejection took {elapsed:?}; it must be immediate, not a fraction of the {DEFAULT_INVOKE_TIMEOUT_MS}ms timeout"
        );
    }

    #[test]
    fn the_production_gate_reads_the_process_wide_server_mode_flag() {
        // Under `cargo test` nothing calls `set_server_mode`, so the OnceLock
        // is unset and reads as `false` ("has a webview") — the same default
        // asserted by `webview_recovery::server_mode_flag_defaults_to_false_when_unset`.
        // That default is exactly why the ordering guard in
        // `tests/server_mode_flag_precedes_http_listener.rs` exists: if the
        // HTTP listener could ever bind before `set_server_mode`, this gate
        // would silently read `false` on a headless runner and the 30-second
        // hang would come straight back.
        assert!(
            !crate::webview_recovery::is_server_mode(),
            "a test process is not a server-mode runner"
        );
        assert!(no_webview_rejection(
            "dismiss_recent_crash",
            crate::webview_recovery::is_server_mode()
        )
        .is_none());
    }
}
