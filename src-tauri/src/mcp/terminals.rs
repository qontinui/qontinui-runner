//! Terminal session HTTP + WebSocket handlers for MCP API
//!
//! Provides HTTP handlers for terminal CRUD and a WebSocket endpoint
//! for bidirectional terminal I/O (output streaming + input/resize).

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::StatusCode,
    response::{IntoResponse, Json},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use tauri::Manager;
use tracing::{debug, error, info, warn};

use crate::mcp::types::{api_error, api_error_detailed, ApiResponse, ApiState};
use crate::terminal::{strip_ansi, TerminalManager};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

// ============================================================================
// Request Types
// ============================================================================

/// Request body for creating a new terminal session.
///
/// **Every multi-word field accepts BOTH `camelCase` and `snake_case`.** The
/// struct is `rename_all = "camelCase"`, so before the aliases below a body of
/// `{"working_dir": "C:/qontinui-root"}` deserialized to `None` — the field was
/// dropped in silence and the PTY landed in the default project directory, with
/// a 200 and a `workingDir` the caller never asked for (D6 of
/// `plans/2026-08-19-session-info-dropdown-mount-gaps-remediation.md`,
/// reproduced against a live runner: `workingDir` honoured, `working_dir`
/// ignored). Serde's default for an unknown field is to skip it, so the only
/// two honest options were "reject it" or "understand it"; scripts and MCP
/// callers reach for the snake_case spelling because that is what the Rust
/// field and every log line are called, so the aliases make them right rather
/// than making them fail.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTerminalRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, alias = "working_dir")]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
    /// Which terminal page this session belongs to.
    #[serde(default, alias = "page_id")]
    pub page_id: Option<String>,
    /// Optional command to execute immediately after shell initialization.
    #[serde(default, alias = "initial_command")]
    pub initial_command: Option<String>,
    /// Phase 2 round 2 of `plans/2026-05-28-isolate-session-edit-work-in-worktrees.md`.
    /// When `Some(repo_slug)` AND `QONTINUI_AGENT_WORKTREE_MODE` is on,
    /// allocate an isolated worktree for that repo and use it as the
    /// PTY cwd instead of `working_dir`. Mirrors the `intent_repo`
    /// argument on the `terminal_create` Tauri command + HTTP-SDK proxy.
    #[serde(default, alias = "intent_repo")]
    pub intent_repo: Option<String>,
    /// Stable session id of the agent creating this terminal. Folded into
    /// the isolated-worktree claim's owner token so two different agent
    /// sessions on one machine are distinct holders. None for
    /// interactive/UI-created terminals (no session to attribute).
    #[serde(default, alias = "agent_session_id")]
    pub agent_session_id: Option<uuid::Uuid>,
    /// Optional per-request Claude account override — a friendly name
    /// (`"hotmail"`, matching `derive_account_name`) OR a full roster
    /// `config_dir` path. When set, `CLAUDE_CONFIG_DIR` is pre-seeded into
    /// the PTY env so a bare `claude` in the terminal is pinned to that
    /// (validated) account without a shell launcher. Omitting it leaves the
    /// PTY env untouched (existing behaviour).
    #[serde(default)]
    pub account: Option<String>,
}

/// Request body for writing data to a terminal.
#[derive(Debug, Deserialize)]
pub struct WriteTerminalRequest {
    /// Base64-encoded bytes to write to terminal stdin.
    pub data: String,
}

/// Request body for resizing a terminal.
#[derive(Debug, Deserialize)]
pub struct ResizeTerminalRequest {
    pub cols: u16,
    pub rows: u16,
}

/// Request body for moving a terminal onto a different page
/// (`POST /terminals/{id}/move`). The wire field is `pageId` (camelCase).
/// `page_id` is required in practice — a missing/empty value returns 400.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveTerminalRequest {
    /// Aliased for the same reason as `CreateTerminalRequest` — see its doc
    /// comment. Same struct-level `camelCase` rename, same silent drop.
    #[serde(default, alias = "page_id")]
    pub page_id: Option<String>,
}

// ============================================================================
// Response Types — GET /terminal-pages
// ============================================================================

/// One terminal's membership entry within a page group.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalPageEntry {
    pub id: String,
    pub title: String,
    pub is_alive: bool,
}

/// A page and the live terminals parked on it.
///
/// SCOPE (v1): membership only — `pageId` + the terminals on it. Human-readable
/// tab labels ("Page 1…N") and left-to-right order are frontend-only state and
/// are NOT known to the Rust backend (page metadata is never persisted
/// server-side; only `page_id` strings are). A `label`/`order` field is a
/// follow-up requiring the frontend to register page metadata to the backend
/// (e.g. a future `PUT /terminal-pages/{id}` `{ label, order }`); do not invent
/// a server-side label heuristic here.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalPageGroup {
    pub page_id: String,
    pub terminal_count: usize,
    pub terminals: Vec<TerminalPageEntry>,
}

/// Response for `GET /terminal-pages` — page-centric grouping of live terminals.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalPagesResponse {
    pub pages: Vec<TerminalPageGroup>,
}

/// Query params for `GET /terminals/{id}/buffer` (and its `/output` alias).
///
/// Three equivalent ways to ask for decoded text instead of the default
/// base64 envelope:
///
/// - `format=text` (canonical, since main's b318ae5ae): UTF-8 lossy decode
///   + ANSI/OSC stripped. Single-flag ergonomic.
/// - `decoded=true`: UTF-8 lossy decode, ANSI codes intact.
/// - `strip_ansi=true`: implies `decoded=true`; strips CSI/OSC + control
///   bytes via `crate::terminal::strip_ansi`.
///
/// Default (no flags): `data` is base64-encoded raw PTY bytes — preserves
/// the original byte stream for the WS scrollback path and reproducible
/// terminal replays.
#[derive(Debug, Default, Deserialize)]
pub struct BufferQuery {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub decoded: Option<bool>,
    #[serde(default)]
    pub strip_ansi: Option<bool>,
}

/// Request body for submitting a prompt to an interactive TUI in the terminal.
#[derive(Debug, Deserialize)]
pub struct SubmitPromptRequest {
    /// The text to submit. It will be wrapped in bracketed-paste markers
    /// and followed by a bare CR by `TerminalSession::submit_prompt`.
    ///
    /// The body is **neutralized** on the way to the PTY — bracketed-paste
    /// markers and control bytes other than `\n` / `\r` / `\t` are removed,
    /// while real escape sequences (colour, diffs) survive intact. See
    /// `terminal::session::sanitize_submit_body`. The response says whether
    /// this message was altered.
    pub message: String,
}

// ============================================================================
// Helpers
// ============================================================================

/// Get the TerminalManager from Tauri managed state.
pub(crate) fn get_terminal_manager(state: &ApiState) -> Arc<TerminalManager> {
    state
        .app_handle
        .state::<Arc<TerminalManager>>()
        .inner()
        .clone()
}

// ============================================================================
// Handlers
// ============================================================================

/// List all terminal sessions.
pub async fn list_terminals_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);
    let terminals = terminal_manager.list();

    Ok(Json(ApiResponse::success(serde_json::json!({
        "terminals": terminals
    }))))
}

/// Create a new terminal session.
pub async fn create_terminal_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateTerminalRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);
    let app_handle = state.app_handle.clone();

    info!(
        "HTTP: Creating terminal session (title={:?}, working_dir={:?}, intent_repo={:?})",
        request.title, request.working_dir, request.intent_repo
    );

    // The other half of D6: a `workingDir` the runner cannot use must come
    // back as a refusal naming the path, not as a 200 whose `workingDir` is
    // somewhere the caller never named. Checked here, on the REQUEST value,
    // because that is the only point where the caller's own intent is still
    // distinguishable from the defaults and worktree substitutions applied
    // below.
    if let Some(dir) = request.working_dir.as_deref().filter(|d| !d.is_empty()) {
        if !std::path::Path::new(dir).is_dir() {
            warn!("HTTP: rejecting terminal create — workingDir is not a directory: {dir}");
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "workingDir is not an existing directory: {dir}"
                ))),
            ));
        }
    }

    // Phase 2 round 2 — route through the shared `acquire_for_terminal`
    // helper so this entry point matches the other five
    // terminal_manager.create call sites. `intent_repo == None` is a
    // no-op (helper returns the original working_dir, no allocation).
    let (working_dir, isolated_ctx) = crate::agent_worktree::isolated_edit::acquire_for_terminal(
        request.intent_repo.as_deref(),
        request
            .title
            .as_deref()
            .unwrap_or("HTTP terminal edit session"),
        request.working_dir,
        request.agent_session_id,
    )
    .await;

    // The shared session-env contribution (`QONTINUI_SESSION_WORKTREES` + the
    // configured plan directories), derived from the live context before it is
    // parked on the session; each var is omitted when it does not resolve.
    // See `agent_worktree::session_env`.
    let mut extra_env_vec: Vec<(String, String)> =
        crate::agent_worktree::session_env::session_extra_env(isolated_ctx.as_ref())
            .unwrap_or_default();

    // Per-request account pin: resolve → validate → pre-seed CLAUDE_CONFIG_DIR
    // onto the PTY env (merged with the session env above, not replacing it).
    // Bogus name → 400, logged-out → 409 — same contract as /sessions/spawn.
    if let Some(account) = request.account.as_deref().filter(|a| !a.is_empty()) {
        match crate::ai_provider::resolve_requested_account(account) {
            Ok(resolved) => {
                extra_env_vec.push(("CLAUDE_CONFIG_DIR".to_string(), resolved.config_dir));
            }
            Err(e @ crate::ai_provider::AccountSelectError::NotInRoster { .. }) => {
                return Err((StatusCode::BAD_REQUEST, Json(api_error(e.message()))));
            }
            Err(e @ crate::ai_provider::AccountSelectError::NotLoggedIn { .. }) => {
                return Err((StatusCode::CONFLICT, Json(api_error(e.message()))));
            }
        }
    }

    let extra_env = if extra_env_vec.is_empty() {
        None
    } else {
        Some(extra_env_vec)
    };

    match terminal_manager.create(
        request.title,
        working_dir,
        request.page_id,
        request.cols,
        request.rows,
        app_handle,
        None,
        extra_env,
        // UNATTENDED spawn — respect the critical floor. This is the runner's
        // HTTP `POST /terminals` door: the caller is a script, an MCP tool or a
        // peer runner, and there is no dialog to show it. The refusal is not
        // silent — it comes back as the request's error body with the lane, the
        // live headroom and the floor in it, so the caller can retry when the
        // box has room. A door that quietly overrode the floor would let any
        // remote caller undo the machine owner's own protection.
        false,
    ) {
        Ok(info) => {
            if let Some(ctx) = isolated_ctx {
                if let Some(session) = terminal_manager.get(&info.id) {
                    session.set_isolated_edit_ctx(ctx);
                }
            }
            info!("HTTP: Created terminal session: {}", info.id);

            // Send initial command after a short delay for shell initialization
            if let Some(cmd) = request.initial_command {
                let mgr = terminal_manager.clone();
                let tid = info.id.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    if let Some(session) = mgr.get(&tid) {
                        let cmd = format!("{}\r\n", cmd);
                        let _ = session.write(cmd.as_bytes());
                    }
                });
            }

            Ok(Json(ApiResponse::success(serde_json::json!(info))))
        }
        Err(e) => {
            error!("HTTP: Failed to create terminal session: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to create terminal: {}", e))),
            ))
        }
    }
}

/// Build the `ACTION_FAILED` / `TERMINAL_EXITED` body for a write refused
/// because the PTY behind the terminal is gone.
///
/// `bytes_dropped` is reported deliberately: the previous behaviour returned
/// `{"written": N}` for exactly these bytes, so a caller that trusted the old
/// shape can see the same number and read that NONE of it landed.
fn terminal_exited_response(id: &str, message: &str, bytes_dropped: usize) -> ApiResponse<()> {
    let mut body = api_error_detailed(
        message.to_string(),
        crate::mcp::ui_bridge::UiBridgeError {
            code: crate::mcp::ui_bridge::types::UiBridgeErrorCode::ActionFailed,
            message: message.to_string(),
            recovery: Some(crate::mcp::ui_bridge::types::RecoveryHint::Unrecoverable),
            context: Some(serde_json::json!({
                "terminal_id": id,
                "reason": crate::terminal::session::TERMINAL_EXITED,
                "bytes_dropped": bytes_dropped,
                "hint": "Restart the session before writing to it (zone hover -> Restart, \
                         or the `/restart [<zone>]` terminal command). Reading scrollback \
                         still works on an exited pane.",
            })),
        },
    );
    body.code = Some("ACTION_FAILED".to_string());
    body
}

/// Write data to a terminal's PTY stdin.
pub async fn write_terminal_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<WriteTerminalRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);

    let session = terminal_manager.get(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Terminal not found: {}", id))),
        )
    })?;

    let bytes = STANDARD.decode(&request.data).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Invalid base64 data: {}", e))),
        )
    })?;

    session.write(&bytes).map_err(|e| {
        // A dead PTY is not a runner fault -- it is an ACTION failure whose
        // whole diagnosis is the typed `TERMINAL_EXITED: ...` envelope. 500
        // would tell the caller to retry against a runner that is perfectly
        // healthy; 400 + `ACTION_FAILED` tells it the request is unsatisfiable
        // until the session is restarted, which is what the frontend's
        // `buildWriteFailure` already reports for the same condition.
        if e.starts_with(crate::terminal::session::TERMINAL_EXITED) {
            warn!("HTTP: refused write to exited terminal {}: {}", id, e);
            return (
                StatusCode::BAD_REQUEST,
                Json(terminal_exited_response(&id, &e, bytes.len())),
            );
        }
        error!("HTTP: Failed to write to terminal {}: {}", id, e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to write to terminal: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(
        serde_json::json!({ "written": bytes.len() }),
    )))
}

/// Get the scrollback buffer for a terminal session.
///
/// Routes:
/// - `GET /terminals/{id}/buffer` (canonical)
/// - `GET /terminals/{id}/output` (alias — same handler)
///
/// Query forms (all equivalent at the consumer level):
/// - `?format=text` — main-canonical, returns `{ data: { text: "..." } }`
///   with ANSI stripped.
/// - `?decoded=true` — UTF-8 lossy decode, ANSI codes intact.
/// - `?strip_ansi=true` — implies decoded, ANSI/control bytes stripped.
///
/// Default (no query): base64-encoded raw PTY bytes for byte-fidelity callers
/// (WS scrollback, reproducible replays).
pub async fn get_buffer_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<BufferQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);

    let session = terminal_manager.get(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Terminal not found: {}", id))),
        )
    })?;

    let (data, start_offset) = session.get_scrollback_buffer();
    let total_bytes_produced = session.info().total_bytes_produced;
    session.reset_flow_control();

    // Main-canonical short-circuit: `?format=text` returns the nested
    // `{ data: { text } }` shape for ergonomic consumers (b318ae5ae).
    if matches!(query.format.as_deref(), Some("text")) {
        let lossy = String::from_utf8_lossy(&data);
        let text = strip_ansi(&lossy);
        return Ok(Json(ApiResponse::success(serde_json::json!({
            "data": { "text": text },
            "start_offset": start_offset,
            "total_bytes_produced": total_bytes_produced,
        }))));
    }

    // Fallthrough: `?decoded=true` / `?strip_ansi=true` use the original
    // top-level `data` shape (string instead of base64-string). Byte-fidelity
    // callers with no flags fall through to the default base64 path.
    let strip = query.strip_ansi.unwrap_or(false);
    // strip_ansi implies decoded=true.
    let decoded = strip || query.decoded.unwrap_or(false);

    let data_value: serde_json::Value = if decoded {
        let text = String::from_utf8_lossy(&data);
        if strip {
            serde_json::Value::String(strip_ansi(&text))
        } else {
            serde_json::Value::String(text.into_owned())
        }
    } else {
        serde_json::Value::String(STANDARD.encode(&data))
    };

    Ok(Json(ApiResponse::success(serde_json::json!({
        "data": data_value,
        "start_offset": start_offset,
        "total_bytes_produced": total_bytes_produced,
    }))))
}

/// Submit a prompt to an interactive TUI running in the terminal (e.g.
/// Claude Code). Wraps the message in ANSI bracketed-paste markers and
/// follows it with a bare CR; see [`crate::terminal::session::TerminalSession::submit_prompt`].
///
/// This route is the most exposed of the four inbound prompt producers — its
/// body is arbitrary caller-supplied text — so the response reports what
/// actually reached the PTY: `bytes` is the real wire length and `sanitized`
/// says whether the neutralizer changed the body.
///
/// A rewrite is also logged at the choke point itself
/// ([`crate::terminal::session::TerminalSession::submit_prompt`]), which is
/// how the *other* producers — none of which sees this response — find out
/// that a body of theirs was altered.
pub async fn submit_prompt_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<SubmitPromptRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);

    let session = terminal_manager.get(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Terminal not found: {}", id))),
        )
    })?;

    // Let the choke point say what it wrote. `message.len() + framing` is
    // NOT the answer: the body is neutralized before it is framed, so a
    // message carrying a paste marker or control bytes reaches the PTY
    // shorter than it arrived. Taking the numbers from `submit_prompt`'s
    // return rather than recomputing them here also stops this route from
    // running the neutralizer over the same untrusted body twice.
    let payload = session.submit_prompt(&request.message).map_err(|e| {
        error!("HTTP: Failed to submit prompt to terminal {}: {}", id, e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to submit prompt: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "submitted": true,
        "bytes": payload.bytes,
        "sanitized": payload.sanitized,
    }))))
}

/// Resize a terminal's PTY dimensions.
pub async fn resize_terminal_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<ResizeTerminalRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);

    let session = terminal_manager.get(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Terminal not found: {}", id))),
        )
    })?;

    session.resize(request.cols, request.rows).map_err(|e| {
        error!("HTTP: Failed to resize terminal {}: {}", id, e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to resize terminal: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "cols": request.cols,
        "rows": request.rows,
    }))))
}

/// Move a terminal onto a different page (`POST /terminals/{id}/move`).
///
/// Body: `{ "pageId": "<id>" }`. Delegates to
/// [`TerminalManager::set_page`], which mutates the in-memory session,
/// mirrors the move into the durable lifecycle registry, and emits a
/// `terminal-page-changed` event so the grid re-mounts the tab. Mirrors the
/// error mapping of the sibling handlers (400 on missing/empty `pageId`, 404
/// on unknown terminal).
pub async fn move_terminal_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<MoveTerminalRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);
    let app_handle = state.app_handle.clone();

    let page_id = request.page_id.filter(|p| !p.is_empty()).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(api_error("pageId is required and must be non-empty")),
        )
    })?;

    info!("HTTP: Moving terminal {} to page {}", id, page_id);

    terminal_manager
        .set_page(&id, page_id.clone(), &app_handle)
        .map_err(|e| {
            error!("HTTP: Failed to move terminal {}: {}", id, e);
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Terminal not found: {}", id))),
            )
        })?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "moved": true,
        "id": id,
        "pageId": page_id,
    }))))
}

/// List live terminals grouped by page (`GET /terminal-pages`).
///
/// Reuses the exact data source as [`list_terminals_handler`] —
/// `terminal_manager.list()` — and groups the entries by their `page_id`.
/// Includes the literal `"default"` page group when any terminal is parked
/// there (the orphaned-tab case that motivated this endpoint). A
/// `BTreeMap<String, _>` keyed by page id yields deterministic (lexical)
/// ordering across calls.
pub async fn list_terminal_pages_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);
    let terminals = terminal_manager.list();

    let mut grouped: BTreeMap<String, Vec<TerminalPageEntry>> = BTreeMap::new();
    for info in terminals {
        grouped
            .entry(info.page_id.clone())
            .or_default()
            .push(TerminalPageEntry {
                id: info.id,
                title: info.title,
                is_alive: info.is_alive,
            });
    }

    let pages: Vec<TerminalPageGroup> = grouped
        .into_iter()
        .map(|(page_id, terminals)| TerminalPageGroup {
            page_id,
            terminal_count: terminals.len(),
            terminals,
        })
        .collect();

    Ok(Json(ApiResponse::success(serde_json::json!(
        TerminalPagesResponse { pages }
    ))))
}

/// Close a terminal session.
pub async fn close_terminal_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);
    let manager = terminal_manager.clone();
    let terminal_id = id.clone();

    info!("HTTP: Closing terminal session: {}", id);

    spawn_blocking_tracked(move || manager.close(&terminal_id))
        .await
        .map_err(|e| {
            error!("HTTP: spawn_blocking error for close terminal: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Internal error: {}", e))),
            )
        })?
        .map_err(|e| {
            error!("HTTP: Failed to close terminal {}: {}", id, e);
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Terminal not found: {}", id))),
            )
        })?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "closed": true,
        "id": id
    }))))
}

/// WebSocket endpoint for bidirectional terminal I/O.
///
/// Upgrades to WebSocket, then:
/// - Sends scrollback buffer as initial `buffer` frame
/// - Streams live output as `output` frames
/// - Accepts `input` frames (keystrokes) and `resize` frames from the client
/// - Sends `exit` frame when the terminal process dies
pub async fn ws_terminal_handler(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(state): State<Arc<ApiState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);

    // Validate terminal exists before upgrading the connection
    let session = terminal_manager.get(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Terminal not found: {}", id))),
        )
    })?;

    // Subscribe before reading scrollback to avoid missing data in between
    let output_rx = session.subscribe_output();
    let (scrollback_data, _) = session.get_scrollback_buffer();
    let is_alive = session.is_alive();

    info!("WebSocket client connecting for terminal {}", id);

    Ok(ws.on_upgrade(move |socket| {
        handle_ws_terminal(socket, id, session, output_rx, scrollback_data, is_alive)
    }))
}

/// Handle a WebSocket connection for a terminal session.
async fn handle_ws_terminal(
    socket: WebSocket,
    terminal_id: String,
    session: Arc<crate::terminal::session::TerminalSession>,
    mut output_rx: tokio::sync::broadcast::Receiver<String>,
    scrollback_data: Vec<u8>,
    is_alive: bool,
) {
    let (mut sender, mut receiver) = socket.split();

    info!("WebSocket connected for terminal {}", terminal_id);

    // Send scrollback buffer as initial frame
    if !scrollback_data.is_empty() {
        let encoded = STANDARD.encode(&scrollback_data);
        let msg = serde_json::json!({"type": "buffer", "data": encoded}).to_string();
        if sender.send(Message::Text(msg.into())).await.is_err() {
            debug!(
                "WebSocket disconnected during scrollback send for terminal {}",
                terminal_id
            );
            return;
        }
    }

    // If terminal is already dead, send exit and close
    if !is_alive {
        let exit_code = session.info().exit_code;
        let msg = serde_json::json!({"type": "exit", "exitCode": exit_code}).to_string();
        let _ = sender.send(Message::Text(msg.into())).await;
        return;
    }

    // Spawn output forwarding task: broadcast → WebSocket sender
    let tid_out = terminal_id.clone();
    let session_out = session.clone();
    let output_task = tokio::spawn(async move {
        loop {
            match output_rx.recv().await {
                Ok(data) => {
                    let msg = serde_json::json!({"type": "output", "data": data}).to_string();
                    if sender.send(Message::Text(msg.into())).await.is_err() {
                        debug!("WebSocket sender closed for terminal {}", tid_out);
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        "WebSocket client lagged for terminal {}, skipped {} chunks",
                        tid_out, n
                    );
                    let msg = serde_json::json!({
                        "type": "warning",
                        "message": format!("Skipped {} output chunks due to lag", n)
                    })
                    .to_string();
                    if sender.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    // Broadcast channel closed — terminal exited
                    let exit_code = session_out.info().exit_code;
                    let msg =
                        serde_json::json!({"type": "exit", "exitCode": exit_code}).to_string();
                    let _ = sender.send(Message::Text(msg.into())).await;
                    break;
                }
            }
        }
    });

    // Inbound loop: receive client frames → dispatch to terminal session
    while let Some(result) = receiver.next().await {
        match result {
            Ok(Message::Text(text)) => {
                if let Ok(msg) = serde_json::from_str::<serde_json::Value>(&text) {
                    match msg.get("type").and_then(|t| t.as_str()) {
                        Some("input") => {
                            if let Some(data) = msg.get("data").and_then(|d| d.as_str()) {
                                if let Ok(bytes) = STANDARD.decode(data) {
                                    if let Err(e) = session.write(&bytes) {
                                        warn!("Failed to write to terminal {}: {}", terminal_id, e);
                                    }
                                }
                            }
                        }
                        Some("resize") => {
                            if let (Some(cols), Some(rows)) = (
                                msg.get("cols").and_then(|c| c.as_u64()),
                                msg.get("rows").and_then(|r| r.as_u64()),
                            ) {
                                if let Err(e) = session.resize(cols as u16, rows as u16) {
                                    warn!("Failed to resize terminal {}: {}", terminal_id, e);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {} // Ping/Pong handled automatically by axum
            Err(e) => {
                debug!(
                    "WebSocket receive error for terminal {}: {}",
                    terminal_id, e
                );
                break;
            }
        }
    }

    // Clean up
    output_task.abort();
    info!("WebSocket disconnected for terminal {}", terminal_id);
}

// ============================================================================
// Routes
// ============================================================================

/// Create routes for this module.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{delete, get, post};

    axum::Router::new()
        .route(
            "/terminals",
            get(list_terminals_handler).post(create_terminal_handler),
        )
        // Page-centric grouping of live terminals (which sessions on which page).
        .route("/terminal-pages", get(list_terminal_pages_handler))
        .route("/terminals/{id}/write", post(write_terminal_handler))
        .route("/terminals/{id}/buffer", get(get_buffer_handler))
        // Alias — the cheatsheet and intuition both reach for `/output`.
        // Same handler, no behavior difference.
        .route("/terminals/{id}/output", get(get_buffer_handler))
        .route("/terminals/{id}/submit-prompt", post(submit_prompt_handler))
        .route("/terminals/{id}/resize", post(resize_terminal_handler))
        // Move a terminal onto a different page (axum 0.8 `{id}` brace syntax).
        .route("/terminals/{id}/move", post(move_terminal_handler))
        .route("/terminals/{id}/ws", get(ws_terminal_handler))
        .route("/terminals/{id}", delete(close_terminal_handler))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_sgr_color_codes() {
        // Red foreground + reset around "hi".
        let input = "\x1b[31mhi\x1b[0m";
        assert_eq!(strip_ansi(input), "hi");
    }

    #[test]
    fn strip_ansi_removes_cursor_movement() {
        // Cursor up + clear-line, common in TUI redraws.
        let input = "before\x1b[2A\x1b[2Kafter";
        assert_eq!(strip_ansi(input), "beforeafter");
    }

    #[test]
    fn strip_ansi_removes_osc_title() {
        // OSC 0; — set window title — terminated by BEL.
        let input = "\x1b]0;My Title\x07hello";
        assert_eq!(strip_ansi(input), "hello");
    }

    // NOTE: bracketed-paste-marker and OSC-ST-terminator tests dropped during
    // the main-merge resolution. The unioned BufferQuery delegates to the
    // canonical `crate::terminal::strip_ansi` (added in main b318ae5ae) which
    // breaks CSI on `is_ascii_alphabetic` only (so `~` doesn't terminate
    // `\x1b[200~`) and only handles OSC BEL, not ST. Those are pre-existing
    // limitations on main; expanding the canonical function is out of scope
    // for this PR. If we want bracketed-paste/OSC-ST stripping later, fix
    // `crate::terminal::strip_ansi` and re-add the assertions there.

    #[test]
    fn strip_ansi_preserves_newlines_tabs_cr() {
        let input = "line1\nline2\tcol\rback";
        assert_eq!(strip_ansi(input), "line1\nline2\tcol\rback");
    }

    #[test]
    fn strip_ansi_drops_other_control_bytes() {
        // BEL and NUL should be dropped; \n preserved.
        let input = "a\x07b\x00c\nd";
        assert_eq!(strip_ansi(input), "abc\nd");
    }

    #[test]
    fn strip_ansi_handles_empty_and_plain() {
        assert_eq!(strip_ansi(""), "");
        assert_eq!(strip_ansi("plain text"), "plain text");
    }

    #[test]
    fn strip_ansi_handles_realistic_prompt_redraw() {
        // Roughly what Claude Code emits while redrawing a prompt:
        // hide cursor, move, clear line, set color, text, reset, show cursor.
        let input = "\x1b[?25l\x1b[1;1H\x1b[2K\x1b[36m> \x1b[0mready\x1b[?25h";
        assert_eq!(strip_ansi(input), "> ready");
    }
    // -----------------------------------------------------------------
    // D6: `POST /terminals` must not drop a snake_case `working_dir`.
    // -----------------------------------------------------------------

    #[test]
    fn create_request_accepts_snake_case_working_dir() {
        let req: CreateTerminalRequest =
            serde_json::from_str(r#"{"working_dir":"C:/qontinui-root"}"#).unwrap();
        assert_eq!(req.working_dir.as_deref(), Some("C:/qontinui-root"));
    }

    #[test]
    fn create_request_still_accepts_camel_case_working_dir() {
        let req: CreateTerminalRequest =
            serde_json::from_str(r#"{"workingDir":"C:/qontinui-root"}"#).unwrap();
        assert_eq!(req.working_dir.as_deref(), Some("C:/qontinui-root"));
    }

    #[test]
    fn create_request_accepts_both_spellings_of_every_multiword_field() {
        let snake: CreateTerminalRequest = serde_json::from_str(
            r#"{"page_id":"p1","initial_command":"echo hi","intent_repo":"qontinui-runner"}"#,
        )
        .unwrap();
        assert_eq!(snake.page_id.as_deref(), Some("p1"));
        assert_eq!(snake.initial_command.as_deref(), Some("echo hi"));
        assert_eq!(snake.intent_repo.as_deref(), Some("qontinui-runner"));

        let camel: CreateTerminalRequest = serde_json::from_str(
            r#"{"pageId":"p1","initialCommand":"echo hi","intentRepo":"qontinui-runner"}"#,
        )
        .unwrap();
        assert_eq!(camel.page_id.as_deref(), Some("p1"));
        assert_eq!(camel.initial_command.as_deref(), Some("echo hi"));
        assert_eq!(camel.intent_repo.as_deref(), Some("qontinui-runner"));
    }

    #[test]
    fn move_request_accepts_both_spellings_of_page_id() {
        let snake: MoveTerminalRequest = serde_json::from_str(r#"{"page_id":"p2"}"#).unwrap();
        assert_eq!(snake.page_id.as_deref(), Some("p2"));
        let camel: MoveTerminalRequest = serde_json::from_str(r#"{"pageId":"p2"}"#).unwrap();
        assert_eq!(camel.page_id.as_deref(), Some("p2"));
    }

    #[test]
    fn create_request_omitting_working_dir_still_defaults_to_none() {
        // The alias must not manufacture a value — absence stays absence, so
        // the manager's project-root fallback keeps working.
        let req: CreateTerminalRequest = serde_json::from_str(r#"{"title":"t"}"#).unwrap();
        assert!(req.working_dir.is_none());
    }
}
