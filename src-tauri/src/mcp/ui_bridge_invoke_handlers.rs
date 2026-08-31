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
use crate::ui_bridge_invoke::{
    dispatch_for, is_allowlisted, Dispatch, ProxyableCommand, UI_BRIDGE_COMMANDS,
};

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
///   Never returned for a [`Dispatch::InProcess`] command, which needs no
///   frontend in the first place.
/// - 504 — frontend didn't respond before the timeout. The pending entry
///   is cancelled so a late response doesn't leak memory. Also unreachable
///   for a [`Dispatch::InProcess`] command — nothing is awaited.
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

/// Build the in-process dispatch table and the list of names it covers from a
/// SINGLE source.
///
/// The Design-decision section of
/// `plans/2026-08-29-headless-pairing-and-authorization.md` names the risk this
/// closes: an in-process table beside Tauri's `generate_handler!` is a *second
/// registration surface that can drift*. The macro removes the inner half of
/// that drift — [`IN_PROCESS_DISPATCH_ARMS`] and the `match` in
/// [`run_in_process`] are generated from the same tokens, so an arm cannot
/// exist without appearing in the list or vice versa. The remaining half (this
/// set vs. the [`Dispatch::InProcess`] entries in the allowlist) is pinned in
/// BOTH directions by `in_process_dispatch_tests`.
///
/// `$state` / `$args` are passed in as call-site identifiers so the arm
/// expressions can name them under `macro_rules!` hygiene.
macro_rules! in_process_dispatch_table {
    (
        ($state:ident, $args:ident) {
            $($name:literal => $call:expr,)+
        }
    ) => {
        /// Command names that have an in-process dispatch arm. Generated
        /// alongside the `match` in [`run_in_process`] — see
        /// [`in_process_dispatch_table`].
        pub(crate) const IN_PROCESS_DISPATCH_ARMS: &[&str] = &[$($name),+];

        /// Serve a [`Dispatch::InProcess`] command by calling its Rust fn
        /// directly. Emits nothing, registers no oneshot, and awaits no
        /// frontend, so it is unaffected by whether a webview exists.
        async fn run_in_process(
            $state: &Arc<ApiState>,
            command: &str,
            $args: &Value,
        ) -> Result<Value, (StatusCode, Json<ApiResponse<()>>)> {
            match command {
                $($name => $call.await,)+
                // Unreachable while `in_process_dispatch_tests` is green: the
                // drift test fails the build before a mismarked allowlist
                // entry can reach a caller. Kept as a loud 500 rather than a
                // panic so a hand-edited binary degrades instead of aborting.
                _ => Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!(
                        "invoke proxy: '{}' is marked Dispatch::InProcess in the UI Bridge \
                         allowlist but has no in-process dispatch arm — this is a \
                         registration bug in the runner, not a caller error",
                        command
                    ))),
                )),
            }
        }
    };
}

in_process_dispatch_table! {
    (state, args) {
        "redeem_pair_code" => in_process_redeem_pair_code(args),
        "dismiss_recent_crash" => in_process_dismiss_recent_crash(state),
    }
}

/// 400 for args that do not match an in-process command's `args_schema`.
///
/// The frontend arm gets this validation from Tauri's IPC deserializer; an
/// in-process arm has to do it itself, and must not silently coerce.
fn in_process_bad_args(command: &str, detail: &str) -> (StatusCode, Json<ApiResponse<()>>) {
    (
        StatusCode::BAD_REQUEST,
        Json(api_error(format!(
            "invoke proxy: invalid args for in-process command '{}': {}",
            command, detail
        ))),
    )
}

/// 500 for a command that ran in-process and returned its own `Err(String)`.
///
/// Deliberately worded "in-process invoke" rather than "frontend invoke" (the
/// wording of the round-trip's 500 arm) so a caller reading the body can tell
/// which transport actually executed the command.
fn in_process_command_failed(command: &str, err: String) -> (StatusCode, Json<ApiResponse<()>>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(api_error(format!(
            "invoke proxy: in-process invoke of '{}' failed: {}",
            command, err
        ))),
    )
}

/// Read an optional nullable string arg, rejecting a wrong-typed value rather
/// than treating it as absent.
///
/// `Err` carries only the detail line, not the whole HTTP envelope, so this
/// helper stays small and the caller keeps ownership of the status code.
fn optional_string_arg(args: &Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{}` must be a string or null", key)),
    }
}

/// In-process arm for `redeem_pair_code`
/// (`crate::commands::web_integration::redeem_pair_code`).
///
/// Needs no Tauri context at all — the command is a plain
/// `async fn(String, Option<String>) -> Result<RedeemPairCodeResponse, String>`
/// that resolves the device id and web base from disk and the active profile.
/// That is why this arm takes only `args`.
async fn in_process_redeem_pair_code(
    args: &Value,
) -> Result<Value, (StatusCode, Json<ApiResponse<()>>)> {
    const COMMAND: &str = "redeem_pair_code";

    let code = match args.get("code") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => {
            return Err(in_process_bad_args(COMMAND, "`code` (string) is required"))
        }
        Some(_) => return Err(in_process_bad_args(COMMAND, "`code` must be a string")),
    };
    let backend_url = optional_string_arg(args, "backendUrl")
        .map_err(|detail| in_process_bad_args(COMMAND, &detail))?;

    let response = crate::commands::web_integration::redeem_pair_code(code, backend_url)
        .await
        .map_err(|e| in_process_command_failed(COMMAND, e))?;

    serde_json::to_value(response).map_err(|e| {
        in_process_command_failed(COMMAND, format!("could not serialize response: {}", e))
    })
}

/// In-process arm for `dismiss_recent_crash` (`crate::crash_dumps`).
///
/// The command takes `tauri::State<'_, Arc<AppState>>`, which is resolved from
/// the `AppHandle` [`ApiState`] already carries — `main.rs` `.manage()`s the
/// same `Arc<AppState>` the HTTP server was handed. Resolved with `try_state`
/// rather than `state`, which panics when the type is unmanaged: a 500 naming
/// the cause beats taking the runner down.
async fn in_process_dismiss_recent_crash(
    state: &Arc<ApiState>,
) -> Result<Value, (StatusCode, Json<ApiResponse<()>>)> {
    use tauri::Manager;
    const COMMAND: &str = "dismiss_recent_crash";

    let app_state = state
        .app_handle
        .try_state::<Arc<crate::commands::AppState>>()
        .ok_or_else(|| {
            in_process_command_failed(
                COMMAND,
                "Arc<AppState> is not managed by this Tauri app, so the crash-dump state \
                 cannot be reached in-process"
                    .to_string(),
            )
        })?;

    crate::crash_dumps::dismiss_recent_crash(app_state)
        .await
        .map(|()| Value::Null)
        .map_err(|e| in_process_command_failed(COMMAND, e))
}

/// What [`perform_invoke_round_trip`] does with a request, decided before any
/// resource is allocated. See [`decide_invoke`] — the ordering lives there and
/// nowhere else.
pub(crate) enum InvokeDecision {
    /// Call the command's Rust fn directly; no webview involved.
    InProcess,
    /// Refuse: server mode, no webview, and no in-process arm.
    NoWebview((StatusCode, Json<ApiResponse<()>>)),
    /// Do the historical round-trip through the frontend.
    Frontend,
}

/// Decide how to serve `command`. **The order of the two checks below is the
/// contract**, and it is the reason this is a function rather than two inline
/// `if`s: an in-process command must be served on a headless runner, so the
/// in-process test outranks [`no_webview_rejection`]. Swapping them would 503
/// exactly the commands the headless door exists to provide — pinned by
/// `in_process_dispatch_tests::in_process_is_served_under_server_mode_where_frontend_503s`.
pub(crate) fn decide_invoke(command: &str, server_mode: bool) -> InvokeDecision {
    if dispatch_for(command) == Some(Dispatch::InProcess) {
        return InvokeDecision::InProcess;
    }
    match no_webview_rejection(command, server_mode) {
        Some(rejection) => InvokeDecision::NoWebview(rejection),
        None => InvokeDecision::Frontend,
    }
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
///   [`no_webview_rejection`]. Checked before the oneshot is registered and
///   before anything is emitted, so a headless caller fails in microseconds
///   instead of waiting out `timeout_ms`.
/// - 504 — no frontend response before the timeout (pending entry cancelled so
///   a late response can't leak).
///
/// A [`Dispatch::InProcess`] command never reaches any of the above: it is
/// routed out of this function by [`decide_invoke`] into [`run_in_process`],
/// which calls the command's Rust fn directly. That happens on windowed
/// runners too, not only headless ones, so there is exactly one code path to
/// keep working.
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
    // Route BEFORE the oneshot is registered and before anything is emitted.
    // `decide_invoke` owns the ordering: in-process dispatch is tested FIRST,
    // because a webview-independent command has nothing to be rejected for,
    // and the headless fast-fail second, because on a server-mode runner every
    // path below can only end in the 30s timeout.
    match decide_invoke(command, crate::webview_recovery::is_server_mode()) {
        InvokeDecision::InProcess => {
            info!(
                command = %command,
                "ui_bridge_invoke: dispatching in-process (command needs no webview)"
            );
            return run_in_process(state, command, args).await;
        }
        InvokeDecision::NoWebview(rejection) => {
            warn!(
                command = %command,
                "ui_bridge_invoke: rejecting invoke — runner is in server mode and has no webview"
            );
            return Err(rejection);
        }
        InvokeDecision::Frontend => {}
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

#[cfg(test)]
mod in_process_dispatch_tests {
    use super::*;
    use crate::ui_bridge_invoke::UI_BRIDGE_OBSERVE_COMMANDS;

    /// Every allowlist entry across both tiers, invoke + observe-only.
    fn all_entries() -> impl Iterator<Item = &'static ProxyableCommand> {
        UI_BRIDGE_COMMANDS
            .iter()
            .chain(UI_BRIDGE_OBSERVE_COMMANDS.iter())
    }

    // -- drift: allowlist ⇄ dispatch arms, both directions -----------------

    #[test]
    fn every_in_process_allowlist_entry_has_a_dispatch_arm() {
        for cmd in all_entries() {
            if cmd.dispatch == Dispatch::InProcess {
                assert!(
                    IN_PROCESS_DISPATCH_ARMS.contains(&cmd.name),
                    "'{}' is marked Dispatch::InProcess in the allowlist but has no arm in \
                     `in_process_dispatch_table!`. Over HTTP it would return a 500 registration \
                     error instead of running. Add the arm, or set it back to Dispatch::Frontend.",
                    cmd.name
                );
            }
        }
    }

    #[test]
    fn every_dispatch_arm_is_an_in_process_allowlist_entry() {
        for name in IN_PROCESS_DISPATCH_ARMS {
            let entry = all_entries().find(|c| c.name == *name).unwrap_or_else(|| {
                panic!(
                    "`in_process_dispatch_table!` has an arm for '{name}' which is in neither \
                     UI_BRIDGE_COMMANDS nor UI_BRIDGE_OBSERVE_COMMANDS — dead code that can \
                     never be reached, because `is_allowlisted` rejects the name first."
                )
            });
            assert_eq!(
                entry.dispatch,
                Dispatch::InProcess,
                "'{name}' has an in-process dispatch arm but its allowlist entry still says \
                 Dispatch::Frontend, so the arm is never taken and the command still needs a \
                 webview."
            );
        }
    }

    #[test]
    fn the_in_process_set_is_exactly_the_two_webview_independent_commands() {
        // A `Dispatch::InProcess` entry is served by a runner that has no
        // window and no signed-in operator in front of it, so widening this
        // set is an authorization-surface change. Pinning it by name means a
        // third command cannot be added without a reviewer editing this test.
        let mut in_process: Vec<&str> = all_entries()
            .filter(|c| c.dispatch == Dispatch::InProcess)
            .map(|c| c.name)
            .collect();
        in_process.sort_unstable();
        assert_eq!(in_process, vec!["dismiss_recent_crash", "redeem_pair_code"]);
    }

    #[test]
    fn every_other_allowlist_entry_still_dispatches_to_the_frontend() {
        for cmd in all_entries() {
            if IN_PROCESS_DISPATCH_ARMS.contains(&cmd.name) {
                continue;
            }
            assert_eq!(
                cmd.dispatch,
                Dispatch::Frontend,
                "adding the `dispatch` field must leave every pre-existing entry on the \
                 frontend round-trip; '{}' changed",
                cmd.name
            );
        }
    }

    // -- ordering vs. the Phase 1 no-webview rejection ---------------------

    #[test]
    fn in_process_is_served_under_server_mode_where_frontend_503s() {
        // This is the whole point of the phase. Same runner, same
        // `server_mode = true`, two commands, two outcomes.
        let InvokeDecision::NoWebview((status, Json(body))) =
            decide_invoke("get_web_integration_status", true)
        else {
            panic!("a Dispatch::Frontend command must still be refused on a webview-less runner");
        };
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body.code.as_deref(), Some(SERVER_MODE_NO_WEBVIEW_CODE));

        for name in IN_PROCESS_DISPATCH_ARMS {
            assert!(
                matches!(decide_invoke(name, true), InvokeDecision::InProcess),
                "'{name}' has no webview dependency, so a server-mode runner must serve it \
                 rather than return {SERVER_MODE_NO_WEBVIEW_CODE}"
            );
        }
    }

    #[test]
    fn no_webview_rejection_alone_would_have_refused_the_in_process_commands() {
        // Proves the previous test is load-bearing rather than tautological:
        // the Phase 1 gate, consulted on its own, rejects these two names. It
        // is only the ORDER inside `decide_invoke` that saves them.
        for name in IN_PROCESS_DISPATCH_ARMS {
            assert!(
                no_webview_rejection(name, true).is_some(),
                "if `no_webview_rejection` ever stops covering '{name}', the ordering guarded \
                 by `decide_invoke` no longer guards anything"
            );
        }
    }

    #[test]
    fn in_process_dispatch_is_not_conditional_on_server_mode() {
        // Deliberate design point: routing in-process ALWAYS — not only when
        // headless — means windowed and headless runners exercise one path,
        // so the headless arm cannot rot untested.
        for name in IN_PROCESS_DISPATCH_ARMS {
            for server_mode in [false, true] {
                assert!(
                    matches!(decide_invoke(name, server_mode), InvokeDecision::InProcess),
                    "'{name}' must dispatch in-process with server_mode={server_mode}"
                );
            }
        }
    }

    #[test]
    fn a_frontend_command_on_a_windowed_runner_still_takes_the_round_trip() {
        assert!(matches!(
            decide_invoke("get_web_integration_status", false),
            InvokeDecision::Frontend
        ));
    }

    // -- the arms actually run ---------------------------------------------

    #[tokio::test]
    async fn in_process_redeem_pair_code_returns_the_commands_own_validation_error() {
        // A blank code is rejected by `redeem_pair_code` before it reads the
        // device id or touches the network, so this exercises the real command
        // hermetically. The observable win over the old behaviour: a caller
        // gets a real validation error instead of a 30s 504 (or, since Phase 1,
        // a 503 saying the command is unreachable).
        let (status, Json(body)) =
            in_process_redeem_pair_code(&serde_json::json!({ "code": "  " }))
                .await
                .expect_err("a blank pair code must fail");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let error = body.error.expect("error message present");
        assert!(
            error.contains("pair code is empty"),
            "the caller must see the command's own error, not a transport error: {error}"
        );
        assert!(
            error.contains("in-process invoke"),
            "the body must say which transport ran the command: {error}"
        );
        assert_ne!(
            body.code.as_deref(),
            Some(SERVER_MODE_NO_WEBVIEW_CODE),
            "an in-process command must never carry the no-webview code"
        );
    }

    #[tokio::test]
    async fn in_process_redeem_pair_code_rejects_bad_args_with_400() {
        for args in [
            serde_json::json!({}),
            serde_json::json!({ "code": null }),
            serde_json::json!({ "code": 123 }),
        ] {
            let (status, _) = in_process_redeem_pair_code(&args)
                .await
                .expect_err("missing or wrong-typed `code` must be refused");
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "args {args} must be an arg error, not a command failure"
            );
        }

        let (status, Json(body)) =
            in_process_redeem_pair_code(&serde_json::json!({ "code": "ABC123", "backendUrl": 7 }))
                .await
                .expect_err("a wrong-typed `backendUrl` must be refused, not silently dropped");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body
            .error
            .expect("error message present")
            .contains("backendUrl"));
    }

    #[test]
    fn an_arm_missing_from_the_table_would_be_a_named_registration_error() {
        // `run_in_process`'s fallback arm is unreachable while the drift tests
        // above are green; assert its wording anyway so the failure mode a
        // hand-edited binary would hit is a diagnosable 500 rather than a
        // panic or a silent success.
        let (status, Json(body)) = in_process_command_failed("x", "y".to_string());
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.error.unwrap().contains("in-process invoke of 'x'"));
    }
}
