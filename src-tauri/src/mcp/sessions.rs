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
//!   nothing, so it is not a substitute.) Optional `account` and `cwd` pin the
//!   Claude account and the directory the session starts in; omitting either
//!   keeps the pre-existing defaults (global account resolution, the runner
//!   process's cwd).
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
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

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
    /// Optional working directory the spawned session starts in.
    ///
    /// Without it every spawn starts in the RUNNER PROCESS's cwd — wherever
    /// the exe happened to be launched from, which is nowhere the caller has
    /// work. That is invisible for a role spawn (the slash command carries its
    /// own context) but load-bearing for a seeded free-form `prompt`: the
    /// context-exhaustion watcher's handoff summary tells the continuation to
    /// run `git status` "in the working directory" and inspect worktrees,
    /// which only means anything if the continuation actually starts in the
    /// exhausted session's directory (session-autonomy-fabric Phase 7).
    ///
    /// Omitting it reproduces today's behaviour exactly.
    #[serde(default)]
    pub cwd: Option<String>,
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

/// Resolve the caller's requested `cwd` into the directory the spawned session
/// should start in.
///
/// `Ok(None)` ⇒ no usable request; the caller falls back to the runner
/// process's cwd (the pre-existing behaviour). `Ok(Some(dir))` ⇒ start there.
/// `Err(msg)` ⇒ 400.
///
/// Whitespace-only reads as absent, so a caller that builds the path by
/// string-joining and ends up with `"  "` gets the honest fallback rather than
/// a spawn in `"  "`. A named-but-nonexistent directory is a 400 rather than a
/// silent fallback: a caller that asked for a cwd and got the runner's install
/// directory instead would debug the wrong thing entirely — which is exactly
/// the failure this field exists to remove.
fn resolve_spawn_cwd(requested: Option<&str>) -> Result<Option<String>, String> {
    let Some(cwd) = requested.map(str::trim).filter(|c| !c.is_empty()) else {
        return Ok(None);
    };
    if !std::path::Path::new(cwd).is_dir() {
        return Err(format!(
            "`cwd` is not an existing directory: {cwd}. Omit the field to start the session in \
             the runner's working directory."
        ));
    }
    Ok(Some(cwd.to_string()))
}

/// Why the spawn closure bailed, and — the part that matters for the task-run
/// row — whether a usable session survived the failure.
///
/// The three failure points are not equivalent. `spawn failed` and
/// `register failed` leave nothing the caller can talk to, so the row is dead
/// and must be reconciled. `initial prompt failed` happens AFTER the child is
/// spawned and registered in `SessionManager`: that session is live and still
/// reachable on `POST /sessions/{id}/message`, so stamping its row
/// `failed`/`completed_at` would be a lie about a running session.
struct SpawnFailure {
    message: String,
    /// True when a registered, reachable session outlived the error.
    session_live: bool,
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

    // Validate the requested cwd BEFORE creating any state, for the same
    // reason the account override is resolved above: a bad path should 400
    // cleanly, not leave an orphaned task-run row behind.
    let requested_cwd =
        resolve_spawn_cwd(req.cwd.as_deref()).map_err(|e| (StatusCode::BAD_REQUEST, e))?;

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

    let working_dir = requested_cwd.unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });

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
    let spawn_result = spawn_blocking_tracked(move || {
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
            Err(e) => {
                return Err(SpawnFailure {
                    message: format!("spawn failed: {}", e),
                    session_live: false,
                })
            }
        };

        if let Err(e) = sm.register(&trid, session.clone()) {
            // The child is spawned but unregistered, so nothing can reach it by
            // id — the row is dead even though a process leaked (pre-existing).
            return Err(SpawnFailure {
                message: format!("register failed: {}", e),
                session_live: false,
            });
        }

        crate::commands::ai_session::emit_session_state(&handle, &trid, &trid, session.state());

        if let Err(e) = session.send_initial_prompt(&initial_prompt_for_closure) {
            // Spawned AND registered: the session is live and addressable. Do
            // not reconcile the row — only the first turn failed.
            return Err(SpawnFailure {
                message: format!("initial prompt failed: {}", e),
                session_live: true,
            });
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
                req.role, task_run_id, e.message
            );
            // The task-run row is created BEFORE the spawn is attempted, so a
            // dead spawn used to leave it reading `running` forever with
            // `sessions_count: 0` and an empty `output_log` — a row that looks
            // live to every consumer while the HTTP body says `state: "error"`.
            //
            // Only reconcile when nothing usable survived: `session_live` marks
            // the `initial prompt failed` case, where the session is registered
            // and still addressable on `POST /sessions/{id}/message`.
            if !e.session_live {
                if let Err(db_err) = state
                    .app_state
                    .pg_db
                    .fail_task_run(&task_run_id, &e.message)
                    .await
                {
                    warn!(
                        "could not mark task run {} failed after spawn failure: {}",
                        task_run_id, db_err
                    );
                }
            }
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
            // Same reconcile as the spawn-failure arm above: the row exists and
            // no session ever attached to it.
            if let Err(db_err) = state
                .app_state
                .pg_db
                .fail_task_run(
                    &task_run_id,
                    &format!("spawn_blocking join error: {join_err}"),
                )
                .await
            {
                warn!(
                    "could not mark task run {} failed after join error: {}",
                    task_run_id, db_err
                );
            }
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

/// The self-describing `scope` this endpoint carries on every response.
///
/// Plan `2026-08-29-no-single-answer-to-is-it-safe-to-restart-the-runner`
/// Phase 2/D4: an operator asking *"are there sessions on this box?"* reaches
/// this endpoint by name, gets a truthful-but-narrow answer (closed sessions
/// only), and reads the empty/near-empty result as *"the runner is idle"* while
/// dozens of live agent sessions run. The fix is to make the endpoint say what
/// it covers — scoping it, not widening it.
pub const SESSIONS_HISTORY_SCOPE: &str =
    "closed terminal sessions (display-only); NOT live sessions — see /restart-readiness";

/// Build the `GET /sessions/history` response envelope: the rows under
/// `sessions`, plus the constant [`SESSIONS_HISTORY_SCOPE`] under `scope`.
///
/// Split out from the handler — and generic over the row type — so the shape is
/// unit-testable without a live `SessionLifecycleStore`.
fn history_envelope<T: serde::Serialize>(sessions: Vec<T>) -> serde_json::Value {
    serde_json::json!({
        "scope": SESSIONS_HISTORY_SCOPE,
        "sessions": sessions,
    })
}

/// `GET /sessions/history` — the DISPLAY-only "previous sessions" listing: the
/// full registry (open + closed) merged with the append-only snapshot HISTORY
/// (ids older than the 24 h registry retention), each row carrying its real
/// `--resume` name, account, resume command, and a re-probed
/// `transcriptExists` / `restorable`. Returns the same `Vec<PastSession>` JSON
/// as the `terminal_session_list_history` Tauri command, under `{ sessions }`,
/// alongside a `scope` string naming what this listing does and does NOT cover
/// ([`SESSIONS_HISTORY_SCOPE`]).
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
    // Resolve through the WRITE-side helper so this reads the same file
    // main.rs opened — see `terminal_session_list_history` and the note above
    // `read_all_snapshot_sessions`: the old port-keyed derivation pointed at a
    // different directory AND filename than the writer for every secondary.
    let snapshot_path = crate::session::session_lifecycle_store::snapshot_history_path();
    let opts = crate::session::past_sessions::PastSessionsOpts {
        since_ms: q.since_ms,
        page_id: q.page_id,
        account: q.account,
        include_shells: q.include_shells.unwrap_or(false),
        limit: q.limit,
    };
    let sessions =
        crate::session::past_sessions::build_past_sessions(store.inner(), &snapshot_path, &opts);
    Ok(Json(history_envelope(sessions)))
}

// =============================================================================
// /sessions/{id}/finish
// =============================================================================

/// Body for `POST /sessions/{id}/finish`.
#[derive(Debug, Default, Deserialize)]
pub struct FinishSessionRequest {
    /// Free-text why (e.g. `"unattended: 6 units, all landed"`). Optional.
    #[serde(default)]
    pub reason: Option<String>,
    /// `false` UNMARKS. Absent means `true` — the route is named `/finish`, so
    /// the unqualified call finishes.
    #[serde(default)]
    pub finished: Option<bool>,
}

/// `POST /sessions/{id}/finish` — mark a session's WORK as complete, or unmark
/// it with `{"finished": false}`.
///
/// This is the LOCAL rung of the `/finish-session` transport cascade: it works
/// with coord unreachable, writing the marker locally with `finishSynced:false`
/// so the outbox carries it to coord when coord returns. A local-only mark is a
/// legitimate terminal state, not a failure.
///
/// **Metadata only — never touches the process.** Its one behavioural effect is
/// that `restorable_records` stops offering the session for resume, which is
/// what makes a rebuilt runner bring back only the UNFINISHED sessions.
///
/// `404` when the id is unknown or the marker was already in the requested
/// state; the store reports a no-op as `None`, and a no-op must not read as a
/// successful write.
async fn finish_session(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: Option<Json<FinishSessionRequest>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let finished = req.finished.unwrap_or(true);

    let store = state
        .app_handle
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        .ok_or((
            StatusCode::SERVICE_UNAVAILABLE,
            "session lifecycle store unavailable".to_string(),
        ))?;

    match store.set_finished(&id, finished, req.reason) {
        Some(rec) => {
            info!(
                claude_session_id = %id,
                finished,
                "sessions: finished marker updated"
            );
            Ok(Json(serde_json::json!({
                "success": true,
                "session": rec,
            })))
        }
        None => Err((
            StatusCode::NOT_FOUND,
            format!(
                "no session `{id}` in the lifecycle registry, or its finished marker was \
                 already in that state"
            ),
        )),
    }
}

// =============================================================================
// /sessions/tree-resets
// =============================================================================

/// Query params for `GET /sessions/tree-resets` — snake_case keys
/// (`since_ms`, `min_mount_number`, `limit`), mapped onto
/// [`crate::session::snapshot_history::TreeResetQuery`].
#[derive(Debug, Default, Deserialize)]
pub struct TreeResetsQuery {
    pub since_ms: Option<i64>,
    pub min_mount_number: Option<u32>,
    pub limit: Option<usize>,
}

/// `GET /sessions/tree-resets` — the read side of the P0 tree-reset
/// observability log.
///
/// `terminal_report_tree_reset` has appended a durable row per terminal-tree
/// mount since P0, but nothing read the file: verifying that an auth flip no
/// longer remounts the tree (the P2 fix) meant locating `tree-resets.jsonl`
/// under the session-restore dir on the runner host by hand. This exposes it
/// over the same API surface as `/sessions/history`.
///
/// Returns `{ treeResets, count, remountCount }`, chronological (oldest
/// first). `remountCount` counts rows with `mountNumber > 1` — the
/// genuine-REmount filter the report type documents, and the number that must
/// stay flat across an auth flip for P2 to hold.
///
/// Read-only and infallible by construction: the reader fails open, so a
/// runner that has never reported returns an empty list rather than an error,
/// and there is no error arm to return at all (unlike `list_history`, which
/// can fail to resolve the lifecycle store).
async fn list_tree_resets(Query(q): Query<TreeResetsQuery>) -> Json<serde_json::Value> {
    // Port-scoped path, matching the write side in `terminal_report_tree_reset`.
    let port = crate::mcp::types::get_mcp_api_port();
    let path = crate::session::snapshot_history::tree_reset_path_for_port(port);
    let rows = crate::session::snapshot_history::read_tree_resets(
        &path,
        &crate::session::snapshot_history::TreeResetQuery {
            since_ms: q.since_ms,
            min_mount_number: q.min_mount_number,
            limit: q.limit,
        },
    );
    let remount_count = rows.iter().filter(|r| r.report.mount_number > 1).count();
    Json(serde_json::json!({
        "treeResets": rows,
        "count": rows.len(),
        "remountCount": remount_count,
    }))
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
// /sessions/<id>/policy-context
// =============================================================================

/// Query for [`policy_context`]. `source` is the Claude `SessionStart` payload's
/// `source` (`startup` | `resume` | `compact`), forwarded by the hook script so
/// the injection can be labelled with WHY the session started. Optional: an
/// absent or unrecognised value normalizes to `startup` in
/// [`crate::mcp::policy_context::normalize_source`], because an unrecognised
/// start is still a start.
///
/// `claude_session_id` is the Claude session UUID from the SAME hook payload,
/// and it is a SECOND, independent identity from `{id}` in the path. `{id}` is
/// the runner TERMINAL id — which is what [`resolve_session_key`] returns and
/// what addresses the route — while this is the session coord attributes the
/// policy read to. One runner terminal can host several Claude sessions in
/// sequence, so attributing a read to the terminal id would file every one of
/// them under the same session. Optional and never fabricated: absent or
/// unparseable ⇒ the coord fetch goes out WITHOUT the attribution header and
/// coord records `claude_session_id = NULL`, which the compliance signal reads
/// as `unavailable`, never as non-compliance.
#[derive(Debug, Deserialize)]
struct PolicyContextQuery {
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    claude_session_id: Option<String>,
}

/// `GET /sessions/{id}/policy-context` — the SessionStart policy-injection
/// endpoint (plan `2026-08-08-runner-enforced-policy-pull.md` Phase 1).
///
/// Sibling of [`continuation_verdict`] in every respect that matters: `{id}` is
/// the runner terminal id the hook script sends (`QONTINUI_TERMINAL_ID`,
/// falling back to the Claude session id), resolved through the same
/// [`resolve_session_key`]; all policy lives in
/// [`crate::mcp::policy_context`]; and it is flag-gated
/// (`QONTINUI_POLICY_INJECTION`, default **`on`** — only the literal `off`
/// disables it).
///
/// **Always 200, never 5xx.** Two distinct 200s:
///
/// - a JSON `hookSpecificOutput` envelope ⇒ the hook prints it and Claude
///   splices `additionalContext` into the session's context;
/// - an EMPTY body ⇒ inject nothing. That is the answer in `off` and `observe`
///   mode, and it is what the hook script's `[ -z "$resp" ]` guard already
///   treats as "decline", so the dark path and the unreachable path coincide.
///
/// A coord failure does NOT produce an empty body — it produces an envelope
/// carrying the fail-open notice, because a session that silently receives
/// nothing is in exactly the pre-plan state the phase exists to end.
async fn policy_context(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(q): Query<PolicyContextQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let key = resolve_session_key(&state, &id, &serde_json::Value::Null);
    // Parse STRICTLY, and never fall back to `key` on failure. `key` is the
    // terminal id; seating it in coord's durable `claude_session_id` column
    // would be a fabricated provenance value that the compliance signal then
    // reads as fact. An unparseable id is simply no id.
    let attribution =
        crate::mcp::policy_context::parse_attribution_session(q.claude_session_id.as_deref());
    match crate::mcp::policy_context::policy_context(&key, q.source.as_deref(), attribution).await {
        Some(envelope) => Json(envelope).into_response(),
        None => StatusCode::OK.into_response(),
    }
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
// /sessions/compliance-coverage
// =============================================================================

/// `GET /sessions/compliance-coverage` — the §A1a coverage bound: an honest,
/// STATIC statement of which sessions the compliance check can see.
///
/// Consumed by the qontinui-web enforcement panel so the operator is told the
/// boundary directly instead of inferring it. It is deliberately not a
/// computed number — see
/// [`crate::mcp::session_compliance::coverage_bound`] for why deriving it from
/// the runner's `liveUntracked` tracking-health metric would be confidently
/// wrong.
async fn compliance_coverage() -> Json<crate::mcp::session_compliance::CoverageBound> {
    Json(crate::mcp::session_compliance::coverage_bound())
}

// =============================================================================
// Routes
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const GENERIC: &str = "You are an AI assistant in a session initiated from the Coordinator.";

    // =========================================================================
    // GET /sessions/history — the scope key
    //
    // Plan `2026-08-29-no-single-answer-to-is-it-safe-to-restart-the-runner`
    // Phase 2/D4.
    // =========================================================================

    #[test]
    fn the_history_scope_disclaims_being_a_live_session_listing() {
        assert!(!SESSIONS_HISTORY_SCOPE.is_empty());
        assert!(
            SESSIONS_HISTORY_SCOPE.contains("NOT live sessions"),
            "the scope must disclaim covering live sessions: {SESSIONS_HISTORY_SCOPE}"
        );
        assert!(
            SESSIONS_HISTORY_SCOPE.contains("/restart-readiness"),
            "the scope must point at the surface that DOES answer it: {SESSIONS_HISTORY_SCOPE}"
        );
    }

    #[test]
    fn the_history_response_carries_scope_alongside_the_rows() {
        let body = history_envelope(vec![
            serde_json::json!({ "claudeSessionId": "a" }),
            serde_json::json!({ "claudeSessionId": "b" }),
        ]);

        assert!(body.is_object());
        assert_eq!(
            body.get("scope").and_then(|v| v.as_str()),
            Some(SESSIONS_HISTORY_SCOPE)
        );

        // `sessions` keeps its name and position — that is precisely why adding
        // `scope` is non-breaking for `usePastSessions.ts`, which reads the key
        // by name rather than treating the body as an array.
        let rows = body
            .get("sessions")
            .and_then(|v| v.as_array())
            .expect("`sessions` must be present and an array");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].get("claudeSessionId").and_then(|v| v.as_str()),
            Some("a")
        );
    }

    #[test]
    fn an_empty_history_still_says_what_it_covers() {
        let body = history_envelope(Vec::<serde_json::Value>::new());

        assert!(body
            .get("scope")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty()));
        assert_eq!(
            body.get("sessions").and_then(|v| v.as_array()),
            Some(&vec![]),
            "an empty history must serialize as `[]`, not null or absent: {body}"
        );
    }

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

    #[test]
    fn the_context_handoff_watcher_payload_deserializes_verbatim() {
        // Pins the exact body `terminal::context_watcher::spawn_continuation`
        // POSTs (session-autonomy-fabric Phase 7). The field is snake_case
        // `prompt` — this struct has no `rename_all` — and carries NO `role`,
        // because the handler 400s on role+prompt. An earlier revision of the
        // watcher sent `initial_prompt`, which silently deserialized to
        // `prompt: None` and handed the continuation session the generic
        // greeting instead of its handoff summary. This is that regression.
        let req: SpawnSessionRequest =
            serde_json::from_str(r#"{"task_name":"Continuation of x","prompt":"handoff summary"}"#)
                .unwrap();
        assert_eq!(req.prompt.as_deref(), Some("handoff summary"));
        assert!(req.role.is_none());
        assert_eq!(
            initial_prompt_for(None, req.args.as_deref(), req.prompt.as_deref()),
            "handoff summary"
        );
    }

    // ── spawn cwd (session-autonomy-fabric Phase 7 follow-up) ──────────

    #[test]
    fn absent_cwd_defers_to_the_runner_process_dir() {
        // `Ok(None)` is the "use the pre-existing default" signal, so every
        // caller that never heard of this field behaves exactly as before.
        assert_eq!(resolve_spawn_cwd(None), Ok(None));
        assert_eq!(resolve_spawn_cwd(Some("")), Ok(None));
        assert_eq!(resolve_spawn_cwd(Some("   ")), Ok(None));
    }

    #[test]
    fn existing_cwd_is_accepted_and_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().to_string();
        assert_eq!(resolve_spawn_cwd(Some(&path)), Ok(Some(path.clone())));
        assert_eq!(
            resolve_spawn_cwd(Some(&format!("  {path}  "))),
            Ok(Some(path))
        );
    }

    #[test]
    fn nonexistent_cwd_is_a_400_not_a_silent_fallback() {
        // The whole point of the field is that the session lands where the
        // caller said. Falling back to the runner's install dir on a typo
        // would reproduce the bug this fixes, one layer down.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-dir").to_string_lossy().to_string();
        let err = resolve_spawn_cwd(Some(&missing)).unwrap_err();
        assert!(err.contains(&missing), "error names the offending path");

        // A FILE is not a working directory either.
        let file = dir.path().join("f.txt");
        std::fs::write(&file, b"x").unwrap();
        assert!(resolve_spawn_cwd(Some(&file.to_string_lossy())).is_err());
    }

    #[test]
    fn spawn_request_deserializes_cwd_and_defaults_it_to_none() {
        let req: SpawnSessionRequest = serde_json::from_str(
            r#"{"task_name":"t","prompt":"p","cwd":"D:/qontinui-root","account":"hotmail"}"#,
        )
        .unwrap();
        assert_eq!(req.cwd.as_deref(), Some("D:/qontinui-root"));
        assert_eq!(req.account.as_deref(), Some("hotmail"));

        // Absent field defaults to None — existing callers are unaffected.
        let req: SpawnSessionRequest = serde_json::from_str(r#"{"task_name":"t"}"#).unwrap();
        assert!(req.cwd.is_none());
    }
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/sessions/spawn", post(spawn_session))
        .route("/sessions/{id}/message", post(send_message))
        .route("/sessions/history", get(list_history))
        .route("/sessions/compliance-coverage", get(compliance_coverage))
        .route("/sessions/tree-resets", get(list_tree_resets))
        .route("/sessions/{id}/touched-files", get(get_touched_files))
        .route("/sessions/{id}/transcript", get(get_transcript))
        .route(
            "/sessions/{id}/continuation-verdict",
            post(continuation_verdict),
        )
        .route("/sessions/{id}/context-low", post(context_low))
        .route("/sessions/{id}/policy-context", get(policy_context))
        // Mark a session's WORK finished (or unmark it). NOTE: this family has
        // no `route_entries()` and `manifest_matches_route_calls` does not reach
        // it — that test scans `src/mcp/ui_bridge` only, and its regex is
        // anchored to `"/ui-bridge/…"`. So this route needs no manifest entry
        // and gets no drift guard from one; its contract is covered by the
        // handler tests below instead.
        .route("/sessions/{id}/finish", post(finish_session))
}
