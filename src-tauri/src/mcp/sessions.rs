//! HTTP surface for session lifecycle from agents (Coordinator,
//! `/auto-review`, `/summarize-session`).
//!
//! Productivity-stack §4 specifies four endpoints under
//! `mcp::sessions::routes()`:
//!
//! - `POST /sessions/spawn` — like `mcp::task_runs::create_ai_session` but
//!   accepts an optional `role` field. Recognised roles dispatch the
//!   matching slash-command body as the new session's first user message,
//!   so the Coordinator can spawn an `auto-review` worker, a fresh
//!   `coordinate` instance, etc., without a parallel HTTP route per role.
//!   It also accepts a mutually-exclusive free-form `prompt` for work no role
//!   covers — without it, an agent that had just written a task brief could
//!   not hand it to a session, because the only alternatives were the five
//!   fixed roles or a generic session that did not know what it was for.
//!   (`POST /task-runs` takes a prompt but only inserts a row; it spawns
//!   nothing, so it is not a substitute.)
//! - `POST /sessions/<id>/message` — HTTP wrapper around the
//!   `send_user_message` Tauri command at
//!   `commands::ai_session::send_user_message`.
//! - `GET /sessions/<id>/touched-files` — wraps
//!   `pg::session_touched_files::get_files_touched`.
//! - `GET /sessions/<id>/transcript` — returns the session's stored
//!   transcript JSON. Reuses the workspace-scanning logic from
//!   `commands::transcript`.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::Manager;
use tracing::{info, warn};

use crate::database::CreateTaskRunInput;
use crate::mcp::shared::AiSessionContext;
use crate::mcp::types::ApiState;
use crate::terminal::transcript;

// =============================================================================
// /sessions/spawn
// =============================================================================

/// Slash-command bodies the runner can dispatch automatically. Naming
/// matches the `.claude/commands/<role>.md` filenames so `/coordinate`
/// and friends can route by role string without a translation map.
fn role_slash_command(role: &str) -> Option<&'static str> {
    match role {
        "auto-review" => Some("/auto-review"),
        "coordinate" => Some("/coordinate"),
        "decompose-plan" => Some("/decompose-plan"),
        "summarize-session" => Some("/summarize-session"),
        "implement-plan" => Some("/implement-plan"),
        _ => None,
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpawnSessionRequest {
    /// Display name for the new session's tab.
    #[serde(default = "default_session_name")]
    pub task_name: String,
    /// Optional role discriminator. When `Some(role)` and the role is in
    /// the allowlist, the session's first user message is the matching
    /// `/<role>` slash command followed by `args` (if any).
    #[serde(default)]
    pub role: Option<String>,
    /// Optional argument string appended to the slash command. Only used
    /// when `role` is set.
    #[serde(default)]
    pub args: Option<String>,
    /// Free-form first user message, for work no `role` covers.
    ///
    /// The role allow-list is deliberately closed — a role maps to a
    /// `.claude/commands/<role>.md` body, so an unknown role is a typo, not an
    /// instruction. But that left a real gap: an agent that had just WRITTEN a
    /// task (an analysis brief, a prompt file, a hand-off summary) had no way
    /// to hand it to a new session. Every spawn was either one of five fixed
    /// roles or a generic "respond helpfully" session that did not know what it
    /// was for, so the work had to be started by a human retyping it.
    ///
    /// This is also the carrier for the context-exhaustion watcher's
    /// handoff-summary prompt (session-autonomy-fabric Phase 7).
    ///
    /// Ignored when `role` is set: a role's slash command IS its prompt, and
    /// silently concatenating the two would produce a session running a
    /// half-command. Supply `args` to parameterise a role instead — the 400
    /// below makes that explicit rather than letting the field vanish.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Optional per-request Claude account override — a friendly name
    /// (`"hotmail"`, matching `derive_account_name`) OR a full roster
    /// `config_dir` path. When set, the spawned session's `CLAUDE_CONFIG_DIR`
    /// is pinned to that (validated) account instead of the global resolution.
    /// Omitting it reproduces today's behaviour exactly.
    #[serde(default)]
    pub account: Option<String>,
}

fn default_session_name() -> String {
    "Coordinator-spawned session".to_string()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnSessionResponse {
    pub task_run_id: String,
    pub task_name: String,
    pub state: String,
    pub role: Option<String>,
    /// True when the role was recognised and the slash command was
    /// dispatched as the initial prompt; false for plain ad-hoc sessions.
    pub dispatched_slash_command: bool,
    /// Set when the agent-registry decision was `warn_proceed`: the spawn went
    /// ahead, but the caller is told which disposition fired. `None` on a plain
    /// allow. (Denials and degrades never reach a response body — they are a
    /// 403 with the reason.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spawn_authorization_warning: Option<String>,
    /// The friendly account name pinned for this spawn, echoed back.
    /// `None` when no `account` was requested (global resolution used).
    pub account: Option<String>,
    /// The config dir pinned as `CLAUDE_CONFIG_DIR`. `None` when no `account`
    /// was requested.
    pub config_dir: Option<String>,
    /// A human-readable warning when the pinned account is currently
    /// rate-limited (spawned anyway per the caller's explicit request).
    pub cooldown_warning: Option<String>,
}

/// The new session's first user message.
///
/// Precedence, and why: a `role` wins because its slash command IS the prompt
/// (the handler rejects role+prompt outright, so this arm is only reached for a
/// role-only spawn). A free-form `prompt` is used verbatim so a spawning agent
/// can hand over the actual task. The generic greeting is the last resort —
/// reached only when the caller supplied neither, i.e. the pre-existing ad-hoc
/// session. Whitespace-only inputs are treated as absent, so a caller that
/// builds a prompt by string-joining and ends up with `"  "` gets the honest
/// fallback instead of a session whose first message is blank.
///
/// Pure — no state, no I/O — so the precedence is testable without a running
/// app, which is the whole reason it is not inline in the handler.
fn initial_prompt_for(
    slash_command: Option<&str>,
    args: Option<&str>,
    prompt: Option<&str>,
) -> String {
    match (slash_command, args) {
        (Some(cmd), Some(args)) if !args.trim().is_empty() => format!("{} {}", cmd, args.trim()),
        (Some(cmd), _) => cmd.to_string(),
        (None, _) => match prompt.map(str::trim) {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => "You are an AI assistant in a session initiated from the Coordinator. \
                  Respond helpfully and conversationally."
                .to_string(),
        },
    }
}

async fn spawn_session(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SpawnSessionRequest>,
) -> Result<Json<SpawnSessionResponse>, (StatusCode, String)> {
    // Validate role early so a typo doesn't silently fall back to a plain
    // session — the agent likely meant a specific role.
    let slash_command = match req.role.as_deref() {
        Some(role) if !role.is_empty() => match role_slash_command(role) {
            Some(cmd) => Some(cmd),
            None => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!(
                        "Unknown role '{}'. Allowed: auto-review, coordinate, \
                         decompose-plan, summarize-session, implement-plan",
                        role
                    ),
                ));
            }
        },
        _ => None,
    };

    // `role` and `prompt` are mutually exclusive. Rejecting the combination is
    // the honest reading: a role's slash command IS the prompt, so honouring
    // both would mean picking one and silently dropping the other — and the
    // dropped one would be the caller's actual intent about half the time.
    if slash_command.is_some() && req.prompt.as_deref().is_some_and(|p| !p.trim().is_empty()) {
        return Err((
            StatusCode::BAD_REQUEST,
            "`prompt` cannot be combined with `role`: a role's slash command IS its first \
             message. Use `args` to parameterise the role, or drop `role` to send a free-form \
             prompt."
                .to_string(),
        ));
    }

    // Agent-registry spawn authorization (plan
    // `2026-07-28-migrate-claude-md-into-qontinui.md` Phase 4c, served clause
    // `agent-spawn-authorization`). `/sessions/spawn` mints a session that
    // OUTLIVES the request that asked for it, so it is a
    // `standing_continuation`: a standing per-path opt-in, default OFF for a
    // fresh user. The role (`auto-review`, `coordinate`, …) is the registry
    // key when one was given; a plain spawn resolves against the per-path row.
    let decision = crate::agent_authorization::authorize_spawn(
        req.role.as_deref().filter(|r| !r.is_empty()),
        crate::agent_authorization::SpawnPath::StandingContinuation,
    )
    .await;
    if let Some(refusal) = decision.refusal() {
        return Err((StatusCode::FORBIDDEN, refusal));
    }
    let authz_warning = match &decision {
        crate::agent_authorization::SpawnDecision::Warn { reason } => Some(reason.clone()),
        _ => None,
    };

    // Resolve an explicit per-request account override, if any. Fail with a
    // clear 4xx BEFORE creating any state — bogus name → 400, logged-out → 409.
    let resolved_account = match req.account.as_deref() {
        Some(account) if !account.is_empty() => {
            match crate::ai_provider::resolve_requested_account(account) {
                Ok(resolved) => Some(resolved),
                Err(e @ crate::ai_provider::AccountSelectError::NotInRoster { .. }) => {
                    return Err((StatusCode::BAD_REQUEST, e.message()));
                }
                Err(e @ crate::ai_provider::AccountSelectError::NotLoggedIn { .. }) => {
                    return Err((StatusCode::CONFLICT, e.message()));
                }
            }
        }
        _ => None,
    };

    // Echo fields + cooldown warning derived from the resolved account.
    let resp_account = resolved_account.as_ref().map(|r| r.account_name.clone());
    let resp_config_dir = resolved_account.as_ref().map(|r| r.config_dir.clone());
    let resp_cooldown_warning = resolved_account.as_ref().and_then(|r| {
        r.cooldown_remaining_secs.map(|secs| {
            format!(
                "account '{}' is rate-limited for another {}s; spawning anyway per explicit request",
                r.account_name, secs
            )
        })
    });

    let task_run_id = uuid::Uuid::new_v4().to_string();

    let input = CreateTaskRunInput::new(&task_run_id, &req.task_name)
        .with_prompt("Coordinator-spawned session")
        .with_workflow_type("chat");
    state
        .app_state
        .pg_db
        .create_task_run(&input)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let session_manager: Arc<crate::claude_session::SessionManager> = state
        .app_handle
        .state::<Arc<crate::claude_session::SessionManager>>()
        .inner()
        .clone();

    let working_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    // Build the initial prompt. For role-driven spawns the prompt IS the
    // slash command line — Claude Code resolves `/coordinate` against the
    // .claude/commands/coordinate.md body at session start. For plain
    // spawns we fall back to a generic system prompt to match the existing
    // ad-hoc create_ai_session behaviour.
    let initial_prompt =
        initial_prompt_for(slash_command, req.args.as_deref(), req.prompt.as_deref());

    let mut session_ctx = AiSessionContext::setup(&task_run_id, &req.task_name);
    if let Some(ref resolved) = resolved_account {
        session_ctx.pinned_config_dir = Some(resolved.config_dir.clone());
    }

    let dispatched = slash_command.is_some();
    let role_for_response = req.role.clone();

    // Wrap the spawn+register+initial-prompt sequence in spawn_blocking so the
    // CLI init handshake can't race SessionManager::register — otherwise output
    // events emitted during the handshake land before the session is reachable
    // by id and the chunk writer drops them.
    let sm = session_manager.clone();
    let handle = state.app_handle.clone();
    let trid = task_run_id.clone();
    let working_dir_for_closure = working_dir.clone();
    let initial_prompt_for_closure = initial_prompt.clone();
    let spawn_result = tokio::task::spawn_blocking(move || {
        let session = match crate::claude_session::ClaudeSession::spawn(
            &working_dir_for_closure,
            &trid,
            &handle,
            Some(session_ctx),
            None,
            None,
            None,
            None,
            None,
            None, // tool_policy
            None, // cli_session_ctx
            None, // agent_log_emitter — mcp session path, no coord agent_logs
        ) {
            Ok(s) => Arc::new(s),
            Err(e) => return Err(format!("spawn failed: {}", e)),
        };

        if let Err(e) = sm.register(&trid, session.clone()) {
            return Err(format!("register failed: {}", e));
        }

        crate::commands::ai_session::emit_session_state(&handle, &trid, &trid, session.state());

        if let Err(e) = session.send_initial_prompt(&initial_prompt_for_closure) {
            return Err(format!("initial prompt failed: {}", e));
        }

        crate::commands::ai_session::emit_session_state(&handle, &trid, &trid, session.state());

        Ok(())
    })
    .await;

    match spawn_result {
        Ok(Ok(())) => {
            // `prompt_chars` rather than the prompt itself: a free-form spawn
            // prompt can be long and may carry task detail that does not belong
            // in the runner log, but "was a prompt actually dispatched, and was
            // it non-trivial" is exactly what you want when a spawned session
            // turns out to be sitting idle.
            info!(
                "Spawned session task_run_id={} role={:?} dispatched={} prompt_chars={}",
                task_run_id,
                req.role,
                dispatched,
                req.prompt.as_deref().map(str::len).unwrap_or(0)
            );
            Ok(Json(SpawnSessionResponse {
                task_run_id,
                task_name: req.task_name,
                state: "ready".to_string(),
                role: role_for_response,
                dispatched_slash_command: dispatched,
                spawn_authorization_warning: authz_warning,
                account: resp_account,
                config_dir: resp_config_dir,
                cooldown_warning: resp_cooldown_warning,
            }))
        }
        Ok(Err(e)) => {
            warn!(
                "Failed to spawn role={:?} session {}: {}",
                req.role, task_run_id, e
            );
            Ok(Json(SpawnSessionResponse {
                task_run_id,
                task_name: req.task_name,
                state: "error".to_string(),
                role: role_for_response,
                dispatched_slash_command: false,
                spawn_authorization_warning: authz_warning,
                account: resp_account,
                config_dir: resp_config_dir,
                cooldown_warning: resp_cooldown_warning,
            }))
        }
        Err(join_err) => {
            warn!(
                "spawn_blocking join error for session {}: {}",
                task_run_id, join_err
            );
            Ok(Json(SpawnSessionResponse {
                task_run_id,
                task_name: req.task_name,
                state: "error".to_string(),
                role: role_for_response,
                dispatched_slash_command: false,
                spawn_authorization_warning: authz_warning,
                account: resp_account,
                config_dir: resp_config_dir,
                cooldown_warning: resp_cooldown_warning,
            }))
        }
    }
}

// =============================================================================
// /sessions/<id>/message
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct SendMessageRequest {
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageResponse {
    pub task_run_id: String,
    pub queued: bool,
}

async fn send_message(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, (StatusCode, String)> {
    if req.message.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "message must not be empty".to_string(),
        ));
    }

    let session_manager: Arc<crate::claude_session::SessionManager> = state
        .app_handle
        .state::<Arc<crate::claude_session::SessionManager>>()
        .inner()
        .clone();

    let session = session_manager.get(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("No active session found for task_run_id: {}", id),
        )
    })?;

    // ClaudeSession::send_user_message returns true when the message went
    // out immediately, false when the worker was Processing and the
    // message was queued. Surface that as `queued` so callers can match
    // the Tauri command's behaviour.
    let sent_immediately = session.send_user_message(&req.message).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("send_user_message failed: {}", e),
        )
    })?;

    Ok(Json(SendMessageResponse {
        task_run_id: id,
        queued: !sent_immediately,
    }))
}

// =============================================================================
// /sessions/<id>/touched-files
// =============================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TouchedFilesResponse {
    pub task_run_id: String,
    pub files: Vec<String>,
}

async fn get_touched_files(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<TouchedFilesResponse>, (StatusCode, String)> {
    let files = state
        .app_state
        .pg_db
        .get_files_touched(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(TouchedFilesResponse {
        task_run_id: id,
        files,
    }))
}

// =============================================================================
// /sessions/<id>/transcript
// =============================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptResponse {
    pub task_run_id: String,
    pub messages: Vec<serde_json::Value>,
}

async fn get_transcript(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<TranscriptResponse>, (StatusCode, String)> {
    // The runner's task_run_id is not necessarily the Claude
    // session_id used by the on-disk JSONL transcripts. The DB stores
    // the worker's persisted output_log by task_run_id, which is the
    // canonical "what the agent did" record an agentic reviewer needs.
    let output = state
        .app_state
        .pg_db
        .get_task_output(&id)
        .await
        .unwrap_or_default();

    if !output.trim().is_empty() {
        // Wrap the output_log in a single-message envelope so the response
        // shape is uniform across the DB-backed and JSONL-backed paths.
        let messages = vec![serde_json::json!({
            "role": "transcript",
            "content": output,
        })];
        return Ok(Json(TranscriptResponse {
            task_run_id: id,
            messages,
        }));
    }

    // Fall back to scanning the on-disk Claude Code config dirs by
    // session_id. This is the same code path
    // `commands::transcript::transcript_read_session` uses; we re-run it
    // for HTTP callers.
    let project = crate::mcp::shared::get_workspace_paths_internal()
        .map(|(root, _, _)| root.to_string_lossy().to_string())
        .unwrap_or_default();

    let config_dirs = transcript::find_claude_config_dirs();
    for dir in &config_dirs {
        if let Ok(messages) = transcript::read_session(dir, &project, &id) {
            let json_messages: Vec<serde_json::Value> = messages
                .into_iter()
                .filter_map(|m| serde_json::to_value(&m).ok())
                .collect();
            return Ok(Json(TranscriptResponse {
                task_run_id: id,
                messages: json_messages,
            }));
        }
    }

    Ok(Json(TranscriptResponse {
        task_run_id: id,
        messages: Vec::new(),
    }))
}

// =============================================================================
// /sessions/history
// =============================================================================

/// Query params for `GET /sessions/history` — snake_case keys (`since_ms`,
/// `page_id`, `account`, `include_shells`, `limit`), mapped onto
/// [`crate::session::past_sessions::PastSessionsOpts`].
#[derive(Debug, Default, Deserialize)]
pub struct HistoryQuery {
    pub since_ms: Option<i64>,
    pub page_id: Option<String>,
    pub account: Option<String>,
    pub include_shells: Option<bool>,
    pub limit: Option<usize>,
}

/// `GET /sessions/history` — the DISPLAY-only "previous sessions" listing: the
/// full registry (open + closed) merged with the append-only snapshot HISTORY
/// (ids older than the 24 h registry retention), each row carrying its real
/// `--resume` name, account, resume command, and a re-probed
/// `transcriptExists` / `restorable`. Returns the same `Vec<PastSession>` JSON
/// as the `terminal_session_list_history` Tauri command, under `{ sessions }`.
async fn list_history(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = state
        .app_handle
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        .ok_or((
            StatusCode::SERVICE_UNAVAILABLE,
            "session lifecycle store unavailable".to_string(),
        ))?;
    // Port-scoped snapshot file, matching main.rs's write-side derivation.
    let port = crate::mcp::types::get_mcp_api_port();
    let snapshot_path = crate::session::snapshot_history::snapshot_path_for_port(port);
    let opts = crate::session::past_sessions::PastSessionsOpts {
        since_ms: q.since_ms,
        page_id: q.page_id,
        account: q.account,
        include_shells: q.include_shells.unwrap_or(false),
        limit: q.limit,
    };
    let sessions =
        crate::session::past_sessions::build_past_sessions(store.inner(), &snapshot_path, &opts);
    Ok(Json(serde_json::json!({ "sessions": sessions })))
}

// =============================================================================
// /sessions/<id>/continuation-verdict
// =============================================================================

/// Resolve the best coord-facing session key for a continuation-verdict call
/// — LOCAL state first (the phase's "consult local state" goal):
///
/// 1. A runner-registered AI session: the path id is a `task_run_id` the
///    Tauri-managed [`AiCoordRegistrar`] maps to its coord session UUID.
/// 2. The Claude session id from the Stop-hook payload (`session_id`) — the
///    key coord's session-identity resolver understands for PTY/terminal
///    sessions (where the path id is the runner TERMINAL id, which coord
///    does not key on).
/// 3. The raw path id (already a session UUID on some spawn paths).
///
/// [`AiCoordRegistrar`]: crate::claude_session::coord_register::AiCoordRegistrar
fn resolve_session_key(
    state: &Arc<ApiState>,
    path_id: &str,
    hook_input: &serde_json::Value,
) -> String {
    if let Some(registrar) = state
        .app_handle
        .try_state::<Arc<crate::claude_session::coord_register::AiCoordRegistrar>>()
    {
        if let Some(coord_id) = registrar.session_id_for(path_id) {
            return coord_id.to_string();
        }
    }
    if let Some(sid) = crate::mcp::continuation_verdict::session_id_from(hook_input) {
        return sid;
    }
    path_id.to_string()
}

/// `POST /sessions/{id}/continuation-verdict` — the Stop-hook decision
/// endpoint (plan `2026-07-17-session-autonomy-fabric.md` Phase 1, D4). Body
/// = the raw Claude Stop-hook payload (parsed LENIENTLY — an empty or
/// non-JSON body reads as `{}` so a curl probe works). Always 200 with
/// `{decision, prompt?, …}` — every error path inside the verdict fail-opens
/// to `allow`, because a broken verdict endpoint must never trap a session
/// at turn-end.
async fn continuation_verdict(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Json<crate::mcp::continuation_verdict::VerdictResponse> {
    let hook_input: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    let key = resolve_session_key(&state, &id, &hook_input);
    Json(crate::mcp::continuation_verdict::continuation_verdict(&key, &hook_input).await)
}

// =============================================================================
// /sessions/<id>/context-low
// =============================================================================

/// `POST /sessions/{id}/context-low` — the PreCompact hook's landing pad
/// (plan `2026-07-17-session-autonomy-fabric.md` Phase 7). `{id}` is the
/// runner terminal id the hook script sends (`QONTINUI_TERMINAL_ID`, falling
/// back to the Claude session id) — the same key space as the grid-scan
/// watcher, so BOTH signals share one once-per-session debounce. Body = the
/// raw Claude PreCompact payload (parsed LENIENTLY — empty/non-JSON reads as
/// `{}` so a curl probe works). Always 200: the endpoint is fail-open by
/// design (a broken watcher must never break a hook), and all policy lives in
/// `terminal::context_watcher::on_precompact_signal` (flag-gated
/// `QONTINUI_CONTEXT_HANDOFF`, default `off`).
async fn context_low(Path(id): Path<String>, body: axum::body::Bytes) -> Json<serde_json::Value> {
    let payload: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    let outcome = crate::terminal::context_watcher::on_precompact_signal(&id, &payload);
    Json(serde_json::json!({
        "fired": outcome.fired,
        "mode": outcome.mode,
        "reason": outcome.reason,
        "session": id,
    }))
}

// =============================================================================
// Routes
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const GENERIC: &str = "You are an AI assistant in a session initiated from the Coordinator.";

    #[test]
    fn a_free_form_prompt_becomes_the_first_message_verbatim() {
        // The gap this closes: before, an agent that had written a task brief
        // had no way to hand it to a session — this arm did not exist and the
        // spawn fell through to the generic greeting.
        let p = initial_prompt_for(None, None, Some("Read plans/foo.md and produce a plan."));
        assert_eq!(p, "Read plans/foo.md and produce a plan.");
    }

    #[test]
    fn a_role_still_wins_and_args_still_parameterise_it() {
        assert_eq!(
            initial_prompt_for(Some("/coordinate"), None, None),
            "/coordinate"
        );
        assert_eq!(
            initial_prompt_for(Some("/implement-plan"), Some(" my-plan "), None),
            "/implement-plan my-plan"
        );
    }

    #[test]
    fn neither_role_nor_prompt_keeps_the_pre_existing_generic_session() {
        assert!(initial_prompt_for(None, None, None).starts_with(GENERIC));
    }

    #[test]
    fn a_blank_prompt_falls_back_instead_of_dispatching_an_empty_message() {
        // A caller that string-joins its way to "   " should get the honest
        // fallback, not a session whose first message is whitespace.
        assert!(initial_prompt_for(None, None, Some("   \n ")).starts_with(GENERIC));
        assert!(initial_prompt_for(None, None, Some("")).starts_with(GENERIC));
    }

    #[test]
    fn role_and_prompt_never_silently_concatenate() {
        // The handler rejects this combination with a 400; this pins the
        // precedence so that if that guard is ever removed the result is still
        // a clean role dispatch rather than a mangled half-command.
        assert_eq!(
            initial_prompt_for(Some("/auto-review"), None, Some("do something else")),
            "/auto-review"
        );
    }
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/sessions/spawn", post(spawn_session))
        .route("/sessions/{id}/message", post(send_message))
        .route("/sessions/history", get(list_history))
        .route("/sessions/{id}/touched-files", get(get_touched_files))
        .route("/sessions/{id}/transcript", get(get_transcript))
        .route(
            "/sessions/{id}/continuation-verdict",
            post(continuation_verdict),
        )
        .route("/sessions/{id}/context-low", post(context_low))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── initial_prompt_for precedence (Phase 7 seeded spawns) ──────────

    #[test]
    fn role_slash_command_wins_over_initial_prompt() {
        let p = initial_prompt_for(Some("/auto-review"), None, Some("seeded prompt"));
        assert_eq!(p, "/auto-review");
        let p = initial_prompt_for(Some("/coordinate"), Some(" repo=x "), Some("seeded"));
        assert_eq!(p, "/coordinate repo=x");
    }

    #[test]
    fn initial_prompt_seeds_roleless_spawns() {
        let p = initial_prompt_for(None, None, Some("You are the continuation of…"));
        assert_eq!(p, "You are the continuation of…");
        // Args without a role are ignored (existing behaviour).
        let p = initial_prompt_for(None, Some("args"), Some("seeded"));
        assert_eq!(p, "seeded");
    }

    #[test]
    fn empty_initial_prompt_falls_back_to_generic() {
        for ip in [None, Some(""), Some("   ")] {
            let p = initial_prompt_for(None, None, ip);
            assert!(p.contains("AI assistant"), "generic fallback for {ip:?}");
        }
    }

    #[test]
    fn spawn_request_deserializes_initial_prompt_leniently() {
        let req: SpawnSessionRequest =
            serde_json::from_str(r#"{"task_name":"t","initial_prompt":"go"}"#).unwrap();
        assert_eq!(req.initial_prompt.as_deref(), Some("go"));
        assert!(req.role.is_none());
        // Absent field defaults to None — existing callers are unaffected.
        let req: SpawnSessionRequest = serde_json::from_str(r#"{}"#).unwrap();
        assert!(req.initial_prompt.is_none());
        assert_eq!(req.task_name, "Coordinator-spawned session");
    }
}
