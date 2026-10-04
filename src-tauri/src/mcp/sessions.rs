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
//!   `auto-review` instance, etc., without a parallel HTTP route per role.
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
/// matches the `.claude/commands/<role>.md` filenames so `/auto-review`
/// and friends can route by role string without a translation map.
fn role_slash_command(role: &str) -> Option<&'static str> {
    match role {
        "auto-review" => Some("/auto-review"),
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
                        "Unknown role '{}'. Allowed: auto-review, \
                         summarize-session, implement-plan",
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
    // fresh user. The role (`auto-review`, `implement-plan`, …) is the registry
    // key when one was given; a plain spawn resolves against the per-path row.
    let decision = crate::agent_authorization::authorize_spawn(
        req.role.as_deref().filter(|r| !r.is_empty()),
        crate::agent_authorization::SpawnPath::StandingContinuation,
        // An HTTP door: the caller's reason is not this runner's to name, so it
        // is `unknown` — autonomous, and deferred by the coord device drain.
        crate::coord_drain_state::SpawnOrigin::Unknown,
    )
    .await;
    if let Some(refusal) = decision.refusal() {
        // A drain deferral is "not now", not "forbidden": 409 so a caller can
        // retry after the drain lifts.
        let status = if decision.is_deferred_by_drain() {
            StatusCode::CONFLICT
        } else {
            StatusCode::FORBIDDEN
        };
        return Err((status, refusal));
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
    // slash command line — Claude Code resolves `/auto-review` against the
    // .claude/commands/auto-review.md body at session start. For plain
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
/// The runner-local, PATH-ADDRESSED rung of the `/finish-session` cascade: the
/// target is the URL path, so no spelling of it can land on a peer, and it
/// names no work unit. It works with coord unreachable — the marker is written
/// locally and the outbox carries it to coord when coord returns.
///
/// **Metadata only — never touches the process.** Its one local behavioural
/// effect is that `restorable_records` stops offering the session for resume,
/// which is what makes a rebuilt runner bring back only the UNFINISHED
/// sessions.
///
/// Responses:
/// - `404` — the id is unknown to this runner's lifecycle registry (a session
///   no runner plane tracks, or a different runner instance's). Only that:
///   an unreadable registry is a `500`, never a `404`.
/// - `500` — the lifecycle registry is unavailable (its lock is poisoned),
///   so whether the id is known is itself unknown.
/// - `503` — this runner manages no lifecycle store at all (not in Tauri
///   state: an early-boot window or a build/instance that never attached
///   one). Nothing was read or written.
/// - `200` otherwise, INCLUDING a re-run whose marker was already in the
///   requested state. The body ([`FinishOutcome::response_json`]) says what
///   changed (`"marker"`, `"reason_only"`, `"none"`) and whether coord was
///   told (`coord.queued`, `coord.coordSessionId`, `coord.reason`), beside the
///   record's own `finishSynced`. A `200` is never by itself evidence coord
///   heard: `coord.queued: false` means the resume set is corrected and coord
///   is NOT told. `changed: "none"` wrote nothing locally; on a mark coord
///   has not ACKed it RE-OFFERS the owed coord write (the retry door) and
///   reports that attempt's verdict, while on a synced mark it queues nothing
///   (`reason: "not_requeued"`) — read `session.finishSynced` for the current
///   verdict.
///
/// Honest local-only cases: a session whose harness id changed inside the
/// provider WITHOUT a SessionStart hook reporting `source: "clear"` — the
/// in-process `/resume` picker, or an adoption whose hook posted `startup`
/// (the Windows `.cmd` shim) or never fired — records no predecessor, so its
/// mark answers `coord.queued: false` (`reason: "no_coord_session"`) until
/// that id gets a coord row of its own.
///
/// [`FinishOutcome::response_json`]: crate::session::session_lifecycle_store::FinishOutcome::response_json
async fn finish_session(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: Option<Json<FinishSessionRequest>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let req = body.map(|Json(b)| b).unwrap_or_default();

    let store = state
        .app_handle
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        .ok_or((
            StatusCode::SERVICE_UNAVAILABLE,
            "session lifecycle store unavailable".to_string(),
        ))?;

    apply_finish(store.inner(), &id, req).map(Json)
}

/// The finish route's body, minus axum — pure over the store so the response
/// contract is unit-testable without an app handle.
fn apply_finish(
    store: &crate::session::session_lifecycle_store::SessionLifecycleStore,
    id: &str,
    req: FinishSessionRequest,
) -> Result<serde_json::Value, (StatusCode, String)> {
    let finished = req.finished.unwrap_or(true);
    let outcome = store
        .set_finished(id, finished, req.reason)
        .map_err(|e| finish_error_response(id, e))?;
    info!(
        claude_session_id = %id,
        finished,
        changed = outcome.changed.as_str(),
        coord = ?outcome.coord,
        "sessions: finished marker request"
    );
    Ok(outcome.response_json())
}

/// The finish route's error status for a [`SetFinishedError`]: an unknown id
/// is a `404`, an unreadable registry a `500` — never collapsed into each
/// other.
///
/// [`SetFinishedError`]: crate::session::session_lifecycle_store::SetFinishedError
fn finish_error_response(
    id: &str,
    e: crate::session::session_lifecycle_store::SetFinishedError,
) -> (StatusCode, String) {
    use crate::session::session_lifecycle_store::SetFinishedError;
    match e {
        SetFinishedError::UnknownSession => (
            StatusCode::NOT_FOUND,
            format!("no session `{id}` in the lifecycle registry"),
        ),
        SetFinishedError::Unavailable => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "session lifecycle registry unavailable (lock poisoned) — whether \
             the session exists is unknown"
                .to_string(),
        ),
    }
}

// =============================================================================
// /sessions/transcript-bind
// =============================================================================

/// Body for `POST /sessions/transcript-bind`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptBindRequest {
    /// The Claude Code session UUID — the JSONL stem.
    pub claude_code_session_id: String,
    /// An EXISTING `coord.sessions` id to adopt (typically from
    /// `coord_bind_self_session`). Adopted only when coord confirms that row
    /// belongs to this Claude session in the caller's tenant; otherwise the
    /// call is refused `409 adoption_unconfirmed` and nothing is bound. Absent: a
    /// session the registrar already maps keeps its row; an unmapped one gets
    /// a fresh `terminal_claude` row, exactly as the resume-sniffer registers.
    #[serde(default)]
    pub coord_session_id: Option<String>,
}

/// A typed JSON answer: `{"error": code}` plus an optional `detail`.
fn bind_error(
    status: StatusCode,
    code: &str,
    detail: Option<String>,
) -> (StatusCode, serde_json::Value) {
    let mut body = serde_json::json!({ "error": code });
    if let Some(d) = detail {
        body["detail"] = serde_json::Value::String(d);
    }
    (status, body)
}

/// Future type of [`TranscriptBindEnv::confirm_adoption`].
type AdoptionCheck =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;

/// Everything `transcript_bind_core` reads from the machine, injected so the
/// tests drive every arm without the machine's settings, credential store,
/// lifecycle store, coord, or a Tauri app.
pub(crate) struct TranscriptBindEnv {
    /// Gate 1, `Settings.cloud_sync_enabled`.
    pub cloud_sync_enabled: bool,
    pub tailer: Option<Arc<crate::session::session_transcript_tailer::SessionTranscriptTailer>>,
    /// The config dirs the transcript watcher actually watches.
    pub watched_config_dirs: Vec<std::path::PathBuf>,
    /// nonce → the tenant its session writes under (`session_tenant_or_refuse`),
    /// or the refusal body.
    pub session_tenant:
        Box<dyn Fn(&str) -> Result<Option<uuid::Uuid>, serde_json::Value> + Send + Sync>,
    /// claude session id → the runner terminal an OPEN lifecycle record hosts
    /// it in, if any (the deterministic ownership leg).
    pub session_terminal: Box<dyn Fn(&str) -> Option<String> + Send + Sync>,
    /// `(tenant, claude session, coord session)` → does coord say that coord
    /// row belongs to that Claude session in that tenant? `Err` carries why it
    /// could not be confirmed.
    pub confirm_adoption:
        Box<dyn Fn(Option<uuid::Uuid>, uuid::Uuid, uuid::Uuid) -> AdoptionCheck + Send + Sync>,
}

/// `POST /sessions/transcript-bind` — bind the CALLER'S OWN Claude Code session
/// into the interactive transcript tailer and replay the part of its JSONL not
/// yet emitted. This is the ONLY door that sends a session's unsent prefix: the
/// live tailer tracks a resumed pane from where it first meets it and never
/// backfills history on its own, so the hand-off is what makes the author's
/// whole conversation reach coord. Plan
/// `2026-09-28-an-author-session-holds-its-worktree-slot-until-its-pr-lands-so-idle-sessions-starve-coord-fixers`
/// Phase 4.4; mechanics in `session::session_transcript_tailer` ("Binding on
/// request").
///
/// **Authorization (security-surface content trigger 5).** This door decides
/// whose conversation leaves the machine, so it authorizes in four steps, none
/// of them "the request came from loopback":
/// 1. **Nonce** — a registered coord-mcp proxy nonce, found by the same
///    `proxy_nonce_from_request` → `proxy_principal_for_nonce` resolution the
///    coord-mcp forwarder and the plan-library write door use, checked before
///    the body is parsed.
/// 2. **Tenant** — the nonce's session tenant from `session_tenant_or_refuse`,
///    the resolver the forwarder uses for its bearer; an unresolvable one is
///    refused, and the resolved tenant is what a fresh registration stamps.
/// 3. **Ownership** — the session must belong to the nonce: an open lifecycle
///    record hosting it in the nonce's own terminal is accepted, one hosting it
///    in any other terminal is refused; an UNHOSTED session is accepted when
///    its JSONL `cwd` is exactly the nonce's workdir (so any session of that
///    workdir — not strictly the calling one — can be bound by a nonce for
///    it), and no `cwd` record yet is refused. A nonce cannot bind another
///    workdir's session.
/// 4. **Adoption** — a supplied `coord_session_id` is adopted only after coord
///    confirms (over the runner's device credential for that tenant) that the
///    row belongs to this Claude session; an unconfirmed id is refused and
///    nothing is bound, and an id another key holds here is refused.
///
/// Answers (the client contract; later fields are additive):
/// - `200 {"bound":true,"already_bound":b,"adopted":b,"replayed_bytes":n,"replayed_chunks":n,"coord_session_id":id,"replay_stopped_at":n|null}`
///   — `adopted` is true iff a supplied `coord_session_id` was adopted; also
///   `"prefix_truncated_bytes":n` (older unsent-prefix bytes skipped by the
///   8 MiB prefix cap, 0 when none) and `"prefix_after_chunk_offset":n|null`
///   (the lane offset where arrival order stops matching file order)
/// - `401 {"error":"nonce_required"}` — no registered proxy nonce
/// - `400 {"error":"malformed_request"|"malformed_id"}`
/// - `403 {"error":"tenant_unresolvable"}` — the nonce's tenant is not knowable
/// - `403 {"error":"not_caller_session"}` — the session is not the caller's
/// - `409 {"error":"sync_disabled"}` — `Settings.cloud_sync_enabled` is off; nothing written
/// - `409 {"error":"registration_disabled"}` — the registrar declined the binding
/// - `409 {"error":"coord_session_in_use"}` — the id to adopt is bound to another session
/// - `409 {"error":"bound_to_other_row","bound":x,"requested":y}` — already bound to another row; nothing written
/// - `409 {"error":"adoption_unconfirmed"}` — coord could not confirm the supplied id is this session's; nothing written
/// - `422 {"error":"jsonl_not_watched","detail":…}` — no watched JSONL for the id
/// - `500 {"error":"replay_failed","detail":…}` — bound, but the JSONL could not be read
/// - `503 {"error":"tailer_unavailable"}` — this runner booted without the session outbox
///
/// Tenant consent and coord's quotas are enforced coord-side on ingest, as for
/// every transcript chunk; nothing here bypasses them.
async fn transcript_bind(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    let tailer = state
        .app_handle
        .try_state::<Arc<crate::session::session_transcript_tailer::SessionTranscriptTailer>>()
        .map(|s| s.inner().clone());
    let lifecycle = state
        .app_handle
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        .map(|s| s.inner().clone());
    let env = TranscriptBindEnv {
        cloud_sync_enabled: crate::settings::get_cloud_sync_enabled(),
        tailer,
        watched_config_dirs: crate::terminal::transcript_watcher::watched_config_dirs(),
        session_tenant: Box::new(|nonce| {
            crate::coord_mcp::session_tenant_or_refuse(Some(nonce)).map_err(|r| r.json_body())
        }),
        session_terminal: Box::new(move |csid| {
            lifecycle
                .as_ref()
                .and_then(|store| store.get(csid))
                .filter(|rec| rec.state == "open" && !rec.terminal_id.trim().is_empty())
                .map(|rec| rec.terminal_id)
        }),
        confirm_adoption: Box::new(|tenant, csid, coord| {
            Box::pin(confirm_adoption_with_coord(tenant, csid, coord))
        }),
    };
    let (status, body) = transcript_bind_core(&headers, &body, env).await;
    (status, Json(body))
}

/// Ask coord which row it resolves this Claude session to, in this tenant,
/// over the runner's device credential for that tenant. `GET
/// /sessions/:id/output` accepts a Claude Code session id as `:id`
/// (`load_session_scoped_or_claude`, tenant-filtered) and answers the matched
/// row's coord `session_id`; the adoption is confirmed only when that is the
/// requested row. Any failure to ask is "not confirmed", never "confirmed".
async fn confirm_adoption_with_coord(
    tenant: Option<uuid::Uuid>,
    claude_session: uuid::Uuid,
    coord_session: uuid::Uuid,
) -> Result<(), String> {
    let jwt = crate::coord_mcp::read_usable_device_jwt_for(tenant)
        .await
        .ok_or_else(|| "no usable device credential for the session's tenant".to_string())?;
    let (base, _) = crate::coord_mcp::coord_base_url_with_source();
    let url = format!("{base}/sessions/{claude_session}/output?stream=transcript&limit=1");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp = client
        .get(&url)
        .bearer_auth(jwt)
        .send()
        .await
        .map_err(|e| format!("coord unreachable: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("coord answered {status} for this claude session"));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("coord answer unparseable: {e}"))?;
    match v.get("session_id").and_then(|s| s.as_str()) {
        Some(id) if id == coord_session.to_string() => Ok(()),
        Some(id) => Err(format!(
            "coord resolves this claude session to row {id}, not {coord_session}"
        )),
        None => Err("coord's answer names no session_id".to_string()),
    }
}

/// The route's logic over an injected [`TranscriptBindEnv`].
pub(crate) async fn transcript_bind_core(
    headers: &axum::http::HeaderMap,
    body: &[u8],
    env: TranscriptBindEnv,
) -> (StatusCode, serde_json::Value) {
    use crate::session::session_transcript_tailer::{
        jsonl_ownership, locate_session_jsonl, BindRefusal, BindRequest, Ownership,
    };

    // 1. The nonce — before anything else is read.
    let nonce = crate::coord_mcp::proxy_nonce_from_request(headers);
    let Some(nonce) = nonce.filter(|n| crate::coord_mcp::proxy_principal_for_nonce(n).is_some())
    else {
        let presented = crate::coord_mcp::proxy_nonce_from_request(headers);
        crate::coord_mcp::spawn_log_proxy_nonce_rejected(
            presented.as_deref(),
            "missing, unregistered, or expired proxy key on POST /sessions/transcript-bind (401)",
        );
        let detail = match presented {
            None => crate::coord_mcp::missing_proxy_key_error(),
            Some(_) => {
                crate::coord_mcp::stale_proxy_key_error(crate::coord_mcp::STALE_PROXY_KEY_CAUSE)
            }
        };
        return bind_error(StatusCode::UNAUTHORIZED, "nonce_required", Some(detail));
    };

    // 2. The body and its ids.
    let req: TranscriptBindRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => {
            return bind_error(
                StatusCode::BAD_REQUEST,
                "malformed_request",
                Some(e.to_string()),
            );
        }
    };
    let Ok(csid) = uuid::Uuid::parse_str(req.claude_code_session_id.trim()) else {
        return bind_error(
            StatusCode::BAD_REQUEST,
            "malformed_id",
            Some("claude_code_session_id is not a UUID".to_string()),
        );
    };
    let requested_adopt = match req.coord_session_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => match uuid::Uuid::parse_str(raw) {
            Ok(id) => Some(id),
            Err(_) => {
                return bind_error(
                    StatusCode::BAD_REQUEST,
                    "malformed_id",
                    Some("coord_session_id is not a UUID".to_string()),
                );
            }
        },
    };

    // 3. Gate 1 — the runner's transcript-sync toggle. Off writes nothing.
    if !env.cloud_sync_enabled {
        return bind_error(
            StatusCode::CONFLICT,
            "sync_disabled",
            Some(
                "transcript sync is off on this runner (Settings.cloud_sync_enabled = false); \
                 nothing was bound or written"
                    .to_string(),
            ),
        );
    }
    let Some(tailer) = env.tailer else {
        return bind_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "tailer_unavailable",
            Some(
                "this runner booted without its session outbox, so it has no transcript tailer"
                    .to_string(),
            ),
        );
    };

    // 4. The caller's tenant.
    let tenant = match (env.session_tenant)(&nonce) {
        Ok(t) => t,
        Err(refusal) => {
            let mut body = serde_json::json!({ "error": "tenant_unresolvable" });
            body["detail"] = refusal;
            return (StatusCode::FORBIDDEN, body);
        }
    };

    // 5. The file — only under a root the watcher actually watches.
    let session_key = csid.to_string();
    let path = match locate_session_jsonl(&env.watched_config_dirs, &session_key) {
        Ok(p) => p,
        Err(r) => {
            return bind_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "jsonl_not_watched",
                Some(r.detail()),
            )
        }
    };

    // 6. Ownership: the caller's own session only. Hosting decides first —
    // it is the stronger evidence: hosted in the nonce's own terminal is
    // owned; hosted in ANY OTHER terminal is refused even when the cwd
    // matches. Unhosted, the JSONL's own `cwd` must equal the nonce's workdir,
    // and a file with no `cwd` record yet is refused rather than guessed.
    let nonce_terminal = crate::coord_mcp::terminal_id_for_nonce(&nonce);
    let nonce_workdir = crate::coord_mcp::workdir_for_nonce(&nonce);
    let not_caller = |why: &str| {
        bind_error(
            StatusCode::FORBIDDEN,
            "not_caller_session",
            Some(format!("session {session_key} is not this caller's: {why}")),
        )
    };
    match (env.session_terminal)(&session_key) {
        Some(host) if nonce_terminal.as_deref() == Some(host.as_str()) => {}
        Some(_) => {
            return not_caller("it is hosted in a different runner terminal than this nonce's");
        }
        None => {
            let (p, wd) = (path.clone(), nonce_workdir.clone());
            let verdict = spawn_blocking_tracked(move || {
                wd.map(|wd| jsonl_ownership(&p, &wd))
                    .unwrap_or(Ownership::CwdDiffers)
            })
            .await
            .unwrap_or(Ownership::NoCwdRecord);
            match verdict {
                Ownership::Owned => {}
                Ownership::CwdDiffers => {
                    return not_caller("its JSONL's cwd is not this nonce's workdir");
                }
                Ownership::NoCwdRecord => {
                    return not_caller(
                        "no cwd record yet in its JSONL, so ownership cannot be established \
                         (retry once the session has written a turn)",
                    );
                }
            }
        }
    }

    // 7. Adoption — only a row coord confirms is this session's. An
    // unconfirmed id is refused outright: binding anyway would mint a second
    // row beside the one the caller named, the duplicate-row defect the client
    // exists to avoid.
    let adopt = match requested_adopt {
        None => None,
        Some(id) => match (env.confirm_adoption)(tenant, csid, id).await {
            Ok(()) => Some(id),
            Err(why) => {
                return bind_error(
                    StatusCode::CONFLICT,
                    "adoption_unconfirmed",
                    Some(format!(
                        "coord could not confirm coord session {id} belongs to Claude session \
                         {csid} in this tenant ({why}); nothing was bound or written"
                    )),
                );
            }
        },
    };

    // 8. Bind + replay — file I/O and fsyncs, off the async runtime.
    let sync_enabled = env.cloud_sync_enabled;
    let joined = spawn_blocking_tracked(move || {
        tailer.bind_and_replay(
            &session_key,
            &path,
            BindRequest { adopt, tenant },
            sync_enabled,
        )
    })
    .await;

    match joined {
        Ok(Ok(o)) => {
            let body = serde_json::json!({
                "bound": true,
                "already_bound": o.already_bound,
                "adopted": o.adopted,
                "replayed_bytes": o.replayed_bytes,
                "replayed_chunks": o.replayed_chunks,
                "coord_session_id": o.coord_session_id,
                "replay_stopped_at": o.replay_stopped_at,
                "prefix_truncated_bytes": o.prefix_truncated_bytes,
                "prefix_after_chunk_offset": o.prefix_after_chunk_offset,
            });
            (StatusCode::OK, body)
        }
        Ok(Err(BindRefusal::SyncDisabled)) => {
            bind_error(StatusCode::CONFLICT, "sync_disabled", None)
        }
        Ok(Err(BindRefusal::RegistrationDisabled)) => bind_error(
            StatusCode::CONFLICT,
            "registration_disabled",
            Some(
                "the runner's coord session registration declined the binding \
                 (QONTINUI_SESSION_AUTOMATION_REGISTER is off, or its outbox write failed)"
                    .to_string(),
            ),
        ),
        Ok(Err(BindRefusal::CoordSessionInUse)) => bind_error(
            StatusCode::CONFLICT,
            "coord_session_in_use",
            Some(
                "the coord_session_id to adopt is already bound to a different Claude session \
                 on this runner"
                    .to_string(),
            ),
        ),
        Ok(Err(BindRefusal::BoundToOtherRow { bound, requested })) => (
            StatusCode::CONFLICT,
            serde_json::json!({
                "error": "bound_to_other_row",
                "bound": bound,
                "requested": requested,
                "detail": "this session is already bound to a different coord row on this \
                           runner; nothing was written",
            }),
        ),
        Ok(Err(BindRefusal::Unreadable(d))) => bind_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "replay_failed",
            Some(format!(
                "the session IS bound and later appends are tailed, but its JSONL could not be \
                 read for the replay: {d}"
            )),
        ),
        Err(e) => bind_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "replay_failed",
            Some(format!("bind task failed: {e}")),
        ),
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
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let key = resolve_session_key(&state, &id, &serde_json::Value::Null);
    // Parse STRICTLY, and never fall back to `key` on failure. `key` is the
    // terminal id; seating it in coord's durable `claude_session_id` column
    // would be a fabricated provenance value that the compliance signal then
    // reads as fact. An unparseable id is simply no id.
    let attribution =
        crate::mcp::policy_context::parse_attribution_session(q.claude_session_id.as_deref());
    // The session's spawn-time delivered-SHA marker (`QONTINUI_POLICY_DELIVERED_SHA`),
    // forwarded by the hook script in a HEADER so it never reaches the trace
    // log's request URI. Parsed strictly; anything but a 64-hex SHA-256 is no
    // marker and gets the full render.
    let delivered_sha = crate::mcp::policy_context::delivered_sha_from_headers(&headers);
    match crate::mcp::policy_context::policy_context(
        &key,
        q.source.as_deref(),
        attribution,
        delivered_sha.as_deref(),
    )
    .await
    {
        Some(envelope) => Json(envelope).into_response(),
        None => StatusCode::OK.into_response(),
    }
}

// =============================================================================
// /sessions/policy-context-stats
// =============================================================================

/// `GET /sessions/policy-context-stats` — how many INJECTIONS carried the FULL
/// policy body since this runner started, and why (plan
/// `2026-09-21-policy-body-still-crosses-the-sessionstart-boundary`, Phase 2).
///
/// **Injections, not sessions** — see the warning below; the distinction is the
/// difference between reading this route right and reading it backwards, so it
/// belongs in the first sentence rather than only in a note further down.
///
/// The honesty gate in [`crate::mcp::policy_context::render_for_session`] fails
/// OPEN: anything it cannot confirm gets the full body. That is correct and it
/// was also invisible — a marker that never arrived looked exactly like a
/// marker deliberately withheld on a `resume`, in every log and every metric,
/// and the feature regressed to its pre-change behaviour for five days with
/// nothing saying so.
///
/// One row per [`crate::mcp::policy_context::PolicyRenderReason`], plus
/// `full_body_total` and `injections`, so the ratio is computable from this one
/// read — without grepping a transcript or a log. Counts are process-lifetime:
/// a restart zeroes them, and the durable per-session record remains coord's
/// `session_policy_reads`, which this route deliberately does not duplicate.
///
/// ⚠️ **The unit is an INJECTION, not a session.** `compact` is a confirmable
/// source and this route fires on every `SessionStart`, so one long-lived
/// session contributes one count per compaction and the `confirmed` share is
/// inflated by exactly the longest-running sessions. Read the per-source split
/// before drawing a per-session conclusion — the full rationale, and the
/// worked example that inverts a naive ratio, is on
/// [`crate::mcp::policy_context::PolicyRenderStats`].
async fn policy_context_stats() -> Json<crate::mcp::policy_context::PolicyRenderStats> {
    Json(crate::mcp::policy_context::render_stats())
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
// /sessions/<id>/notification
// =============================================================================

/// `POST /sessions/{id}/notification` — the Claude Code `Notification` hook's
/// landing pad (plan `2026-08-27-operator-touch-observation-runner-emitter`,
/// Phase B2, §2a4's resolution: ship via the `qontinui-claude-config`
/// installer, landing HERE rather than in the runner's own bundled
/// `--settings`). `{id}` is the runner terminal id the hook script sends
/// (`QONTINUI_TERMINAL_ID`) and is looked up as an exact terminal id — there
/// is NO Claude-session-id fallback here (unlike `/sessions/{id}/context-low`,
/// which resolves its key as a terminal id but degrades rather than refusing
/// on a miss): an unknown key answers
/// `recorded:false`, so the hook stands down when it has no terminal id
/// (plan vet 2026-09-24, D1). Body = the raw Claude
/// `Notification` hook payload (parsed LENIENTLY — empty/non-JSON reads as
/// `{}` so a curl probe works). Always 200: fail-open by design (a broken
/// watcher must never break a hook), and all policy lives in
/// `terminal::operator_touch_watch::on_notification_signal` (kill-switched by
/// `QONTINUI_OPERATOR_TOUCH_HOOK`, default ARMED).
async fn operator_touch_notification(
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Json<serde_json::Value> {
    let payload: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    let outcome = crate::terminal::operator_touch_watch::on_notification_signal(&id, &payload);
    Json(serde_json::json!({
        "recorded": outcome.recorded,
        "kind": outcome.kind,
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
// /sessions/transcript-coverage
// =============================================================================

/// `GET /sessions/transcript-coverage` — the interactive transcript tailer's
/// [`CoverageReport`](crate::session::session_transcript_tailer::CoverageReport):
/// how many panes it reaches, which ids it is dropping for want of a coord
/// binding (the ones `POST /sessions/transcript-bind` exists to fix), and the
/// `transcript_holes` / `held_batches` counts that make a lost or deferred
/// range visible. Until now the report reached only the periodic log line.
///
/// - `200` — the report. Every counter is since this runner process started.
/// - `503 {"error":"tailer_unavailable"}` — this runner booted without its
///   session outbox, so it has no tailer and NO coverage answer: UNKNOWN, not
///   zero.
async fn transcript_coverage(
    State(state): State<Arc<ApiState>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let tailer = state
        .app_handle
        .try_state::<Arc<crate::session::session_transcript_tailer::SessionTranscriptTailer>>()
        .map(|s| s.inner().clone());
    let (status, body) = transcript_coverage_body(tailer.as_deref());
    (status, Json(body))
}

/// The route's answer for an optional tailer, split out so it is testable
/// without a Tauri app.
fn transcript_coverage_body(
    tailer: Option<&crate::session::session_transcript_tailer::SessionTranscriptTailer>,
) -> (StatusCode, serde_json::Value) {
    match tailer {
        Some(t) => match serde_json::to_value(t.coverage()) {
            Ok(v) => (StatusCode::OK, v),
            Err(e) => bind_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "coverage_unserializable",
                Some(e.to_string()),
            ),
        },
        None => bind_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "tailer_unavailable",
            Some(
                "this runner booted without its session outbox, so it has no transcript \
                 tailer and no coverage to report (UNKNOWN, not zero)"
                    .to_string(),
            ),
        ),
    }
}

// =============================================================================
// Routes
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // POST /sessions/{id}/finish — the response contract
    //
    // Plan `2026-09-20-the-one-finish-door-that-names-its-target-in-the-path-
    // is-disowned-by-both-closeout-commands` Phase 1.
    // =========================================================================

    use crate::claude_session::coord_register::AiCoordRegistrar;
    use crate::session::session_lifecycle_store::{self as lifecycle, SessionLifecycleStore};

    /// A store wired the way main.rs wires it: the finish observer forwards to
    /// a real registrar, whose terminal-plane lookup answers `terminal_coord`
    /// for every id (or nothing).
    fn finish_fixture(
        terminal_coord: Option<uuid::Uuid>,
    ) -> (
        Arc<SessionLifecycleStore>,
        AiCoordRegistrar,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Arc::new(
            crate::session::local_store::OutboxWriter::open(dir.path().join("outbox.jsonl"))
                .unwrap(),
        );
        let reg = AiCoordRegistrar::with_tenant_resolver(outbox, uuid::Uuid::new_v4(), || None);
        reg.attach_terminal_coord_lookup(move |_| terminal_coord);
        let store = Arc::new(
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap(),
        );
        {
            let reg = reg.clone();
            store.attach_finish_observer(move |rec| reg.forward_finish_change(rec));
        }
        store.record_open(lifecycle::test_open_record("s"));
        (store, reg, dir)
    }

    /// An unreadable registry is a 500 that says "unknown", never the 404
    /// an unknown id gets.
    #[test]
    fn finish_error_response_maps_unavailable_to_500_and_unknown_to_404() {
        use crate::session::session_lifecycle_store::SetFinishedError;
        let (status, msg) = finish_error_response("s", SetFinishedError::Unavailable);
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(msg.contains("unknown"), "{msg}");
        let (status, msg) = finish_error_response("s", SetFinishedError::UnknownSession);
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(msg.contains("`s`"), "{msg}");
    }

    fn finish_req(reason: Option<&str>) -> FinishSessionRequest {
        FinishSessionRequest {
            reason: reason.map(str::to_string),
            finished: None,
        }
    }

    #[test]
    fn finish_response_names_a_local_only_mark() {
        let (store, _reg, _dir) = finish_fixture(None);

        let body = apply_finish(&store, "s", finish_req(None)).expect("a known id is 200");
        assert_eq!(body["success"], true);
        assert_eq!(body["changed"], "marker");
        assert_eq!(
            body["coord"]["queued"], false,
            "no plane knows a coord row — coord was NOT told: {body}"
        );
        assert_eq!(body["coord"]["reason"], "no_coord_session");
        assert!(body["coord"]["coordSessionId"].is_null());
        assert!(
            body["session"]["finishedAt"].is_i64(),
            "the local mark landed"
        );
        assert_eq!(body["session"]["finishSynced"], false);
    }

    #[test]
    fn a_terminal_plane_finish_reports_the_coord_row_it_queued_for() {
        let coord_id = uuid::Uuid::new_v4();
        let (store, _reg, _dir) = finish_fixture(Some(coord_id));

        let body = apply_finish(&store, "s", finish_req(Some("done"))).unwrap();
        assert_eq!(body["changed"], "marker");
        assert_eq!(body["coord"]["queued"], true);
        assert_eq!(body["coord"]["coordSessionId"], coord_id.to_string());
        assert!(body["coord"]["reason"].is_null());
    }

    #[test]
    fn re_finishing_is_200_changed_none_not_404() {
        let (store, _reg, _dir) = finish_fixture(None);
        apply_finish(&store, "s", finish_req(None)).unwrap();

        let again = apply_finish(&store, "s", finish_req(None))
            .expect("a re-run on a known id is 200, not 404");
        assert_eq!(again["changed"], "none", "nothing was written: {again}");
        assert_eq!(again["coord"]["queued"], false);
        assert_eq!(
            again["coord"]["reason"], "no_coord_session",
            "the unsynced mark's owed write was re-offered, and still no coord row \
             resolves: {again}"
        );
        assert_eq!(
            again["session"]["finishSynced"], false,
            "the re-run reads the CURRENT sync verdict"
        );

        let unmark = FinishSessionRequest {
            reason: None,
            finished: Some(false),
        };
        assert_eq!(
            apply_finish(&store, "s", unmark).unwrap()["changed"],
            "marker"
        );
        let unmark_again = FinishSessionRequest {
            reason: None,
            finished: Some(false),
        };
        let again = apply_finish(&store, "s", unmark_again).unwrap();
        assert_eq!(again["changed"], "none");
        assert_eq!(
            again["coord"]["reason"], "not_requeued",
            "a no-op unmark owes coord nothing: {again}"
        );

        let (status, _) = apply_finish(&store, "ghost", finish_req(None))
            .expect_err("an unknown id is still an error");
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// The retry door: a no-op re-finish on a mark coord has NOT ACKed
    /// re-offers the owed write instead of reporting `not_requeued` forever.
    /// A mark that went local-only (no coord row yet) is pushed once a row
    /// resolves, by re-running the same call.
    #[test]
    fn re_finishing_an_unsynced_mark_retries_the_coord_write() {
        let coord_id = uuid::Uuid::new_v4();
        let resolves = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dir = tempfile::tempdir().unwrap();
        let outbox = Arc::new(
            crate::session::local_store::OutboxWriter::open(dir.path().join("outbox.jsonl"))
                .unwrap(),
        );
        let reg = AiCoordRegistrar::with_tenant_resolver(outbox, uuid::Uuid::new_v4(), || None);
        {
            let resolves = resolves.clone();
            reg.attach_terminal_coord_lookup(move |_| {
                resolves
                    .load(std::sync::atomic::Ordering::SeqCst)
                    .then_some(coord_id)
            });
        }
        let store = Arc::new(
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap(),
        );
        {
            let reg = reg.clone();
            store.attach_finish_observer(move |rec| reg.forward_finish_change(rec));
        }
        store.record_open(lifecycle::test_open_record("s"));

        let first = apply_finish(&store, "s", finish_req(None)).unwrap();
        assert_eq!(first["coord"]["queued"], false, "no coord row yet: {first}");

        resolves.store(true, std::sync::atomic::Ordering::SeqCst);
        let again = apply_finish(&store, "s", finish_req(None)).unwrap();
        assert_eq!(
            again["changed"], "none",
            "nothing local was written: {again}"
        );
        assert_eq!(
            again["coord"]["queued"], true,
            "the owed write is retried on a no-op re-run of an UNSYNCED mark: {again}"
        );
        assert_eq!(again["coord"]["coordSessionId"], coord_id.to_string());
        assert_eq!(
            again["session"]["finishedAt"], first["session"]["finishedAt"],
            "the retry does not restamp the mark"
        );
    }

    #[test]
    fn re_finishing_with_a_reason_on_a_synced_mark_says_coord_was_not_told() {
        let coord_id = uuid::Uuid::new_v4();
        let (store, _reg, _dir) = finish_fixture(Some(coord_id));
        let first = apply_finish(&store, "s", finish_req(Some("first"))).unwrap();
        assert_eq!(first["coord"]["queued"], true);
        store.mark_finish_synced("s", first["session"]["finishedAt"].as_i64());

        let again = apply_finish(&store, "s", finish_req(Some("second"))).unwrap();
        assert_eq!(again["changed"], "reason_only");
        assert_eq!(
            again["session"]["finishReason"], "second",
            "the local rewrite landed"
        );
        assert_eq!(
            again["coord"]["queued"], false,
            "an ACKed mark is not re-queued, so coord never hears the new reason: {again}"
        );
        assert_eq!(again["coord"]["reason"], "not_requeued");
        assert_eq!(again["session"]["finishSynced"], true);
    }

    const GENERIC: &str = "You are an AI assistant in a session initiated from the Coordinator.";

    // =========================================================================
    // POST /sessions/transcript-bind — the route contract (plan 2026-09-28
    // author-session slot, Phase 4.4). The bind/replay mechanics are tested in
    // `session::session_transcript_tailer`; these pin the door.
    // =========================================================================

    mod transcript_bind {
        use super::*;
        use crate::session::local_store::OutboxWriter;
        use crate::session::session_transcript_tailer::SessionTranscriptTailer;
        use crate::session::transcript_emitter::TranscriptEmitter;
        use axum::http::HeaderMap;
        use std::path::PathBuf;

        fn tailer(dir: &std::path::Path) -> (Arc<SessionTranscriptTailer>, Arc<OutboxWriter>) {
            let outbox = Arc::new(OutboxWriter::open(dir.join("outbox.jsonl")).unwrap());
            let machine_id = uuid::Uuid::new_v4();
            let registrar = Arc::new(
                crate::claude_session::coord_register::AiCoordRegistrar::with_tenant_resolver(
                    outbox.clone(),
                    machine_id,
                    || None,
                ),
            );
            let emitter = Arc::new(TranscriptEmitter::new(
                outbox.clone(),
                machine_id,
                registrar.clone(),
            ));
            (
                Arc::new(SessionTranscriptTailer::new(emitter, registrar)),
                outbox,
            )
        }

        /// `GET /sessions/transcript-coverage`: no tailer is UNKNOWN (503),
        /// never a zero report; a tailer serves its report with the hole and
        /// held-batch counters a client reads.
        #[test]
        fn coverage_route_is_unknown_without_a_tailer_and_serves_the_report() {
            let (status, body) = transcript_coverage_body(None);
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(body["error"], "tailer_unavailable");
            assert!(body.get("appends_emitted").is_none());

            let dir = tempfile::tempdir().unwrap();
            let (t, _outbox) = tailer(dir.path());
            let (status, body) = transcript_coverage_body(Some(&t));
            assert_eq!(status, StatusCode::OK);
            for key in [
                "cloud_sync_enabled",
                "sessions_tailed",
                "sessions_unbound",
                "unbound_session_ids",
                "appends_emitted",
                "transcript_holes",
                "held_batches",
            ] {
                assert!(body.get(key).is_some(), "missing {key}: {body}");
            }
            assert_eq!(body["transcript_holes"], 0);
            assert!(body["cloud_sync_enabled"].is_null(), "no append observed yet");
        }

        /// A registered nonce and the workdir it is bound to, via the one
        /// `pub(crate)` registration helper the plan-library door tests use.
        fn registered_nonce() -> (String, String) {
            let wd = format!("/tmp/transcript-bind-door-test-{}", uuid::Uuid::now_v7());
            let nonce = crate::coord_mcp::register_agent_proxy_nonce(&wd, uuid::Uuid::new_v4());
            let bound = crate::coord_mcp::workdir_for_nonce(&nonce).expect("workdir");
            (nonce, bound)
        }

        fn bearer(nonce: &str) -> HeaderMap {
            let mut h = HeaderMap::new();
            h.insert(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {nonce}").parse().unwrap(),
            );
            h
        }

        fn body(csid: &str) -> Vec<u8> {
            serde_json::json!({ "claude_code_session_id": csid })
                .to_string()
                .into_bytes()
        }

        fn body_adopting(csid: &str, coord: uuid::Uuid) -> Vec<u8> {
            serde_json::json!({ "claude_code_session_id": csid, "coord_session_id": coord })
                .to_string()
                .into_bytes()
        }

        /// `<dir>/cfg/projects/p/<csid>.jsonl` whose records carry `cwd`;
        /// returns the config dir.
        fn config_with(dir: &std::path::Path, csid: &str, cwd: &str) -> PathBuf {
            let cfg = dir.join("cfg");
            let p = cfg.join("projects").join("p");
            std::fs::create_dir_all(&p).unwrap();
            let text = format!(
                "{}\n{}\n",
                serde_json::json!({"type": "user", "cwd": cwd, "n": 1}),
                serde_json::json!({"type": "assistant", "cwd": cwd, "n": 2}),
            );
            std::fs::write(p.join(format!("{csid}.jsonl")), text).unwrap();
            cfg
        }

        /// A permissive environment: sync on, tenant resolvable, no hosting
        /// terminal, adoption confirmed iff `confirm`.
        fn env(
            t: Option<Arc<SessionTranscriptTailer>>,
            cfg: Vec<PathBuf>,
            confirm: bool,
        ) -> TranscriptBindEnv {
            TranscriptBindEnv {
                cloud_sync_enabled: true,
                tailer: t,
                watched_config_dirs: cfg,
                session_tenant: Box::new(|_| Ok(None)),
                session_terminal: Box::new(|_| None),
                confirm_adoption: Box::new(move |_, _, _| {
                    Box::pin(async move {
                        if confirm {
                            Ok(())
                        } else {
                            Err("coord unreachable (test)".to_string())
                        }
                    })
                }),
            }
        }

        /// Drive the core, retrying across the process-global registration
        /// kill switch the coord_register suite toggles.
        async fn call(
            headers: &HeaderMap,
            body: &[u8],
            mk: impl Fn() -> TranscriptBindEnv,
        ) -> (StatusCode, serde_json::Value) {
            for _ in 0..200 {
                let (status, v) = transcript_bind_core(headers, body, mk()).await;
                if status == StatusCode::CONFLICT && v["error"] == "registration_disabled" {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    continue;
                }
                return (status, v);
            }
            panic!("registration stayed disabled")
        }

        /// No nonce, or one this runner never minted, under EITHER header:
        /// 401 `nonce_required`, and nothing reaches the outbox — even with a
        /// valid body and a watched JSONL behind it.
        #[tokio::test]
        async fn refuses_a_call_without_a_registered_nonce() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, "/anywhere");

            let stranger = format!("{}", uuid::Uuid::new_v4().simple());
            let mut legacy = HeaderMap::new();
            legacy.insert("x-coord-mcp-proxy-key", stranger.parse().unwrap());
            for headers in [HeaderMap::new(), bearer(&stranger), legacy] {
                let (status, v) = transcript_bind_core(
                    &headers,
                    &body(&csid),
                    env(Some(t.clone()), vec![cfg.clone()], true),
                )
                .await;
                assert_eq!(status, StatusCode::UNAUTHORIZED);
                assert_eq!(v["error"], "nonce_required");
            }
            assert!(outbox.pending().unwrap().is_empty());
        }

        /// A nonce for workdir A cannot bind session B (whose cwd is another
        /// workdir): 403 `not_caller_session`, nothing written.
        #[tokio::test]
        async fn a_nonce_cannot_bind_another_workdirs_session() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (nonce, _wd) = registered_nonce();
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, "/some/other/workdir");

            let (status, v) = call(&bearer(&nonce), &body(&csid), || {
                env(Some(t.clone()), vec![cfg.clone()], true)
            })
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
            assert_eq!(v["error"], "not_caller_session");
            assert!(outbox.pending().unwrap().is_empty());
        }

        /// Hosting is stronger evidence than cwd: a session hosted in ANOTHER
        /// runner terminal is refused even though its cwd is the nonce's
        /// workdir.
        #[tokio::test]
        async fn a_session_hosted_in_another_terminal_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (nonce, wd) = registered_nonce();
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &wd);
            let mut e = env(Some(t), vec![cfg], true);
            e.session_terminal = Box::new(|_| Some("some-other-terminal".to_string()));
            let (status, v) = transcript_bind_core(&bearer(&nonce), &body(&csid), e).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
            assert_eq!(v["error"], "not_caller_session");
            assert!(outbox.pending().unwrap().is_empty());
        }

        /// No `cwd` record yet: refused, never guessed from the directory name.
        #[tokio::test]
        async fn a_session_with_no_cwd_record_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (nonce, _wd) = registered_nonce();
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = dir.path().join("cfg");
            let p = cfg.join("projects").join("p");
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join(format!("{csid}.jsonl")), "{\"type\":\"summary\"}\n").unwrap();
            let (status, v) =
                transcript_bind_core(&bearer(&nonce), &body(&csid), env(Some(t), vec![cfg], true))
                    .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
            assert_eq!(v["error"], "not_caller_session");
            assert!(v["detail"].as_str().unwrap().contains("no cwd record yet"));
            assert!(outbox.pending().unwrap().is_empty());
        }

        #[tokio::test]
        async fn unresolvable_tenant_is_403() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (nonce, wd) = registered_nonce();
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &wd);
            let mut e = env(Some(t), vec![cfg], true);
            e.session_tenant = Box::new(|_| Err(serde_json::json!({"code": "test"})));
            let (status, v) = transcript_bind_core(&bearer(&nonce), &body(&csid), e).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(v["error"], "tenant_unresolvable");
            assert!(outbox.pending().unwrap().is_empty());
        }

        /// The toggle off answers 409 `sync_disabled` and writes nothing.
        #[tokio::test]
        async fn disabled_toggle_is_sync_disabled_and_writes_nothing() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (nonce, wd) = registered_nonce();
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &wd);
            let mut e = env(Some(t), vec![cfg], true);
            e.cloud_sync_enabled = false;
            let (status, v) = transcript_bind_core(&bearer(&nonce), &body(&csid), e).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(v["error"], "sync_disabled");
            assert!(outbox.pending().unwrap().is_empty());
        }

        #[tokio::test]
        async fn malformed_ids_are_400() {
            let dir = tempfile::tempdir().unwrap();
            let (t, _outbox) = tailer(dir.path());
            let (nonce, _wd) = registered_nonce();
            for raw in [
                serde_json::json!({ "claude_code_session_id": "../../etc/passwd" }),
                serde_json::json!({
                    "claude_code_session_id": uuid::Uuid::new_v4().to_string(),
                    "coord_session_id": "nope",
                }),
            ] {
                let (status, v) = transcript_bind_core(
                    &bearer(&nonce),
                    raw.to_string().as_bytes(),
                    env(Some(t.clone()), Vec::new(), true),
                )
                .await;
                assert_eq!(status, StatusCode::BAD_REQUEST);
                assert_eq!(v["error"], "malformed_id");
            }
            let (status, v) =
                transcript_bind_core(&bearer(&nonce), b"{", env(Some(t), Vec::new(), true)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(v["error"], "malformed_request");
        }

        #[tokio::test]
        async fn unwatched_jsonl_is_422() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (nonce, _wd) = registered_nonce();
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = dir.path().join("empty-cfg");
            let (status, v) =
                transcript_bind_core(&bearer(&nonce), &body(&csid), env(Some(t), vec![cfg], true))
                    .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(v["error"], "jsonl_not_watched");
            // No absolute paths before the ownership check.
            assert!(!v["detail"].as_str().unwrap().contains("empty-cfg"));
            assert!(outbox.pending().unwrap().is_empty());
        }

        /// The happy path end to end through the door, then a re-bind that
        /// returns the SAME row and replays nothing.
        #[tokio::test]
        async fn binds_and_replays_then_rebind_replays_nothing() {
            let dir = tempfile::tempdir().unwrap();
            let (t, _outbox) = tailer(dir.path());
            let (nonce, wd) = registered_nonce();
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &wd);
            let file_len =
                std::fs::metadata(cfg.join("projects").join("p").join(format!("{csid}.jsonl")))
                    .unwrap()
                    .len();

            let mk = || env(Some(t.clone()), vec![cfg.clone()], true);
            let (status, v) = call(&bearer(&nonce), &body(&csid), mk).await;
            assert_eq!(status, StatusCode::OK, "{v}");
            assert_eq!(v["bound"], true);
            assert_eq!(v["already_bound"], false);
            assert_eq!(v["adopted"], false);
            assert_eq!(v["replayed_bytes"], file_len);
            assert_eq!(v["replayed_chunks"], 1);
            assert!(v["replay_stopped_at"].is_null());
            let first_row = v["coord_session_id"].clone();

            let (status, v) = call(&bearer(&nonce), &body(&csid), mk).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(v["already_bound"], true);
            assert_eq!(v["coord_session_id"], first_row, "no second row");
            assert_eq!(v["replayed_bytes"], 0);
            assert_eq!(v["replayed_chunks"], 0);
        }

        /// A supplied coord session coord cannot confirm is refused
        /// `adoption_unconfirmed`, and NOTHING is bound or written — no fresh
        /// row beside the one the caller named.
        #[tokio::test]
        async fn unconfirmed_adoption_is_refused_and_writes_nothing() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (nonce, wd) = registered_nonce();
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &wd);
            let requested = uuid::Uuid::new_v4();

            let (status, v) = call(&bearer(&nonce), &body_adopting(&csid, requested), || {
                env(Some(t.clone()), vec![cfg.clone()], false)
            })
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{v}");
            assert_eq!(v["error"], "adoption_unconfirmed");
            assert!(outbox.pending().unwrap().is_empty(), "no row, no chunk");
        }

        /// A confirmed coord session is adopted; the same id for ANOTHER
        /// session of the caller's is then refused `coord_session_in_use`.
        #[tokio::test]
        async fn confirmed_adoption_is_adopted_and_cannot_be_adopted_twice() {
            let dir = tempfile::tempdir().unwrap();
            let (t, _outbox) = tailer(dir.path());
            let (nonce, wd) = registered_nonce();
            let (a, b) = (
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
            );
            let cfg = config_with(dir.path(), &a, &wd);
            config_with(dir.path(), &b, &wd);
            let requested = uuid::Uuid::new_v4();
            let mk = || env(Some(t.clone()), vec![cfg.clone()], true);

            let (status, v) = call(&bearer(&nonce), &body_adopting(&a, requested), mk).await;
            assert_eq!(status, StatusCode::OK, "{v}");
            assert_eq!(v["adopted"], true);
            assert_eq!(v["coord_session_id"], serde_json::json!(requested));

            let (status, v) = call(&bearer(&nonce), &body_adopting(&b, requested), mk).await;
            assert_eq!(status, StatusCode::CONFLICT, "{v}");
            assert_eq!(v["error"], "coord_session_in_use");
        }
    }

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
            initial_prompt_for(Some("/auto-review"), None, None),
            "/auto-review"
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
        .route(
            "/sessions/{id}/notification",
            post(operator_touch_notification),
        )
        .route("/sessions/{id}/policy-context", get(policy_context))
        .route("/sessions/policy-context-stats", get(policy_context_stats))
        // Mark a session's WORK finished (or unmark it). NOTE: this family has
        // no `route_entries()` and `manifest_matches_route_calls` does not reach
        // it — that test scans `src/mcp/ui_bridge` only, and its regex is
        // anchored to `"/ui-bridge/…"`. So this route needs no manifest entry
        // and gets no drift guard from one; its contract is covered by the
        // handler tests below instead.
        .route("/sessions/{id}/finish", post(finish_session))
        // Phase 4.4 (plan 2026-09-28 author-session slot): bind a Claude Code
        // session into the transcript tailer and replay its prefix. A
        // credential door (`origin_guard::CREDENTIAL_DOORS`) that authorizes
        // on the coord-mcp proxy nonce — see `transcript_bind`.
        .route("/sessions/transcript-bind", post(transcript_bind))
        // The tailer's coverage report — the read side of transcript-bind.
        .route("/sessions/transcript-coverage", get(transcript_coverage))
}
