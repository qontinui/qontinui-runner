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
///   changed (`"marker"`, `"none"`) and whether coord was told
///   (`coord.queued`, `coord.coordSessionId`, `coord.reason`), beside the
///   record's own `finishSynced`. A `200` is never by itself evidence coord
///   heard: `coord.queued: false` means the resume set is corrected and coord
///   is NOT told. A re-finish that carries a `reason` RESTAMPS an
///   already-finished mark (`changed: "marker"`, a new `finishedAt`,
///   `finishSynced: false`, the finish re-sent to coord with the new time).
///   A caller that sends a `reason` on EVERY call (a poll, say) therefore
///   restamps and re-queues on every call and keeps `finishSynced` false; a
///   poll must send no reason.
///   `changed: "none"` — a re-finish with no reason, i.e. the read-back —
///   wrote nothing locally; on a mark coord has not ACKed it RE-OFFERS the
///   owed coord write (the retry door) and reports that attempt's verdict,
///   while on a synced mark it queues nothing (`reason: "not_requeued"`) —
///   read `session.finishSynced` for the current verdict.
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

/// The request header that carries a terminal's door-scoped proof for this
/// door (lowercase, as `http::HeaderMap` stores names): the
/// [`crate::session::terminal_door_key`] derivation of the hosting terminal's
/// per-spawn secret for [`TRANSCRIPT_BIND_DOOR`]. Read only for a key that
/// names no terminal (step 6, arm 3).
pub(crate) const TERMINAL_PROOF_HEADER: &str = "x-qontinui-terminal-proof";

/// This door's name in the door-key derivation. Hashed into the proof, so a
/// proof for this door opens no other — the permission pad's included.
pub(crate) const TRANSCRIPT_BIND_DOOR: &str = "transcript-bind";

/// What the OPEN lifecycle record hosting a session says about it — the
/// deterministic ownership leg of step 6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionHost {
    /// The runner terminal hosting the session.
    pub terminal_id: String,
    /// The session's working directory, when the record knows it: where the
    /// terminal's credential lives when its cwd declares coord-mcp.
    pub working_dir: Option<String>,
    /// How the record's session id was bound — `authoritative` (the runner
    /// knows it exactly), `reconciled` (a backstop recovered it, and it "may
    /// name a foreign session"), `observed`; `None` on a legacy record, which
    /// consumers read as reconciled.
    pub origin: Option<String>,
    /// When a provider hook confirmed a real session started in the terminal;
    /// `None` is provisional.
    pub confirmed_at: Option<i64>,
}

impl SessionHost {
    /// Whether the record vouches that the session it names IS the one in its
    /// terminal: bound authoritatively AND confirmed by a provider hook.
    fn vouches_for_its_session(&self) -> bool {
        self.origin.as_deref()
            == Some(crate::session::session_lifecycle_store::ORIGIN_AUTHORITATIVE)
            && self.confirmed_at.is_some()
    }
}

/// Everything `transcript_bind_core` reads from the machine, injected so the
/// tests drive every arm without the machine's settings, credential store,
/// lifecycle store, coord, or a Tauri app. [`transcript_bind_env`] builds the
/// production closures.
pub(crate) struct TranscriptBindEnv {
    /// Transcript sync's gate for the caller's tenant: the tenant's
    /// `egress_transcript_sync` switch AND the user's own toggle
    /// ([`crate::egress::transcript_sync_gate_for`]).
    pub transcript_gate:
        Box<dyn Fn(Option<uuid::Uuid>) -> crate::egress::TranscriptGate + Send + Sync>,
    pub tailer: Option<Arc<crate::session::session_transcript_tailer::SessionTranscriptTailer>>,
    /// The config dirs the transcript watcher actually watches.
    pub watched_config_dirs: Vec<std::path::PathBuf>,
    /// nonce → the tenant its session writes under (`session_tenant_or_refuse`),
    /// or the refusal body.
    pub session_tenant:
        Box<dyn Fn(&str) -> Result<Option<uuid::Uuid>, serde_json::Value> + Send + Sync>,
    /// claude session id → what the OPEN lifecycle record hosting it says, if
    /// any record does (the deterministic ownership leg). `Err` names why the
    /// lifecycle store could not be read — UNKNOWN, which refuses, and never
    /// "not hosted", which would hand the session to the cwd arm.
    pub session_host: Box<dyn Fn(&str) -> Result<Option<SessionHost>, &'static str> + Send + Sync>,
    /// `(terminal, its session's workdir)` → the key that IS that terminal's
    /// session credential ([`crate::coord_mcp::terminal_session_credential`]),
    /// or the reason it cannot be named.
    pub session_credential:
        Box<dyn Fn(&str, Option<&str>) -> Result<String, &'static str> + Send + Sync>,
    /// `(terminal, presented proof)` → does the proof prove that terminal
    /// ([`crate::session::terminal_door_key::terminal_door_key_matches`] for
    /// [`TRANSCRIPT_BIND_DOOR`])?
    pub terminal_proven: Box<dyn Fn(&str, Option<&str>) -> bool + Send + Sync>,
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
/// 3. **Ownership** — the session must be the caller's own. Exactly three
///    paths admit it:
///    - **hosted, per-terminal key** — an open lifecycle record hosts the
///      session in the very terminal the nonce was minted for;
///    - **hosted, shared key** — the nonce names NO terminal (the key in the
///      cwd's `.mcp.json`, which every session started there shares), and all
///      three hold: the hosting record is `authoritative` AND confirmed; the
///      nonce IS the hosting terminal's session credential
///      ([`crate::coord_mcp::terminal_session_credential`], so a fresh bind
///      registers under the session's own tenant); and the request carries
///      `X-Qontinui-Terminal-Proof` ([`TERMINAL_PROOF_HEADER`]), that
///      terminal's door key ([`crate::session::terminal_door_key`]), which
///      only a holder of the terminal's per-spawn secret can derive — a
///      sibling sharing the key holds its own terminal's secret, never this
///      one's;
///    - **unhosted** — no open record hosts it, and its JSONL `cwd` is exactly
///      the nonce's workdir (so any UNHOSTED session of that workdir — not
///      strictly the calling one — can be bound by a nonce for it).
///
///    Everything else is refused `not_caller_session`, each with its own
///    `detail` and none naming a path: a nonce naming another terminal; a
///    shared key whose hosting record is not authoritative and confirmed, that
///    is not the terminal's credential, or that comes without the terminal's
///    proof (or with another's); a terminal whose credential this process
///    cannot name (an adopted holder pane holds no recorded delivery and no
///    grant); a session whose hosting cannot be read (no lifecycle store, or an
///    unreadable one — UNKNOWN, never "unhosted"); an unhosted session whose
///    `cwd` differs or is not recorded yet.
///    A nonce cannot bind another workdir's session, and a hosted session
///    cannot be bound by a peer sharing its directory's key.
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
/// - `403 {"error":"not_caller_session","detail":…}` — the session is not the caller's; the `detail` names the evidence that is missing (step 3)
/// - `409 {"error":"sync_disabled"}` — the user's own transcript-sync toggle is off; nothing written
/// - `409 {"error":"egress_off","flow":"transcript_sync"}` — the tenant's `egress_transcript_sync` switch is off; nothing written
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
    let (status, body) = transcript_bind_route(
        tailer,
        lifecycle,
        crate::terminal::transcript_watcher::watched_config_dirs(),
        &headers,
        &body,
    )
    .await;
    (status, Json(body))
}

/// The door below the axum extraction: what [`transcript_bind`] runs once it
/// has read this runner's state — its transcript tailer, its lifecycle store,
/// and the config dirs its transcript watcher watches (each may be absent). It
/// builds the production [`TranscriptBindEnv`] and decides with
/// [`transcript_bind_core`], so a caller that enters here runs every
/// production closure the route runs.
pub(crate) async fn transcript_bind_route(
    tailer: Option<Arc<crate::session::session_transcript_tailer::SessionTranscriptTailer>>,
    lifecycle: Option<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>,
    watched_config_dirs: Vec<std::path::PathBuf>,
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> (StatusCode, serde_json::Value) {
    let env = transcript_bind_env(tailer, lifecycle, watched_config_dirs);
    transcript_bind_core(headers, body, env).await
}

/// The production [`TranscriptBindEnv`] over this runner's transcript tailer,
/// lifecycle store and watched config dirs.
fn transcript_bind_env(
    tailer: Option<Arc<crate::session::session_transcript_tailer::SessionTranscriptTailer>>,
    lifecycle: Option<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>,
    watched_config_dirs: Vec<std::path::PathBuf>,
) -> TranscriptBindEnv {
    TranscriptBindEnv {
        transcript_gate: Box::new(crate::egress::transcript_sync_gate_for),
        tailer,
        watched_config_dirs,
        session_tenant: Box::new(|nonce| {
            crate::coord_mcp::session_tenant_or_refuse(Some(nonce)).map_err(|r| r.json_body())
        }),
        // A runner with no lifecycle store, or one whose store cannot be read,
        // does not know whether a terminal hosts the session: UNKNOWN, which
        // refuses. Reading it as "not hosted" would hand every session to the
        // cwd arm, the one arm a key shared by a whole directory satisfies.
        // Only an OPEN record with a real terminal hosts a session. A blank
        // workdir is no workdir: it must never resolve `.mcp.json` against the
        // runner's own cwd.
        session_host: Box::new(move |csid| {
            let store = lifecycle.as_ref().ok_or("lifecycle_store_unavailable")?;
            Ok(store
                .try_get(csid)?
                .filter(|rec| rec.state == "open" && !rec.terminal_id.trim().is_empty())
                .map(|rec| SessionHost {
                    terminal_id: rec.terminal_id,
                    working_dir: rec.working_dir.filter(|wd| !wd.trim().is_empty()),
                    origin: rec.origin,
                    confirmed_at: rec.confirmed_at,
                }))
        }),
        session_credential: Box::new(crate::coord_mcp::terminal_session_credential),
        terminal_proven: Box::new(|terminal, proof| {
            crate::session::terminal_door_key::terminal_door_key_matches(
                terminal,
                TRANSCRIPT_BIND_DOOR,
                proof,
            )
        }),
        confirm_adoption: Box::new(|tenant, csid, coord| {
            Box::pin(confirm_adoption_with_coord(tenant, csid, coord))
        }),
    }
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

/// Step 6, arm 3: a session hosted in terminal T, presented with a key that
/// names NO terminal. Such a key — the one in the cwd's `.mcp.json` — is
/// shared by every session started in that directory, so on its own it cannot
/// say which of them is calling, and the unhosted arm's workdir-wide authority
/// must never reach a hosted session. The session is the caller's only when
/// all three hold, checked in this order:
///
/// 1. T's lifecycle record vouches for the session it names: bound
///    `authoritative` AND confirmed. A `reconciled` record may name a
///    sibling's session, and a proof of T would then bind the sibling.
/// 2. The key IS T's session credential (compared in constant time), so the
///    tenant a fresh bind registers under is the session's own.
/// 3. The request proves T: [`TERMINAL_PROOF_HEADER`] carries T's door key,
///    derived from the per-spawn secret only T's environment holds — a
///    sibling sharing the key holds its own terminal's secret, never T's.
///
/// `Err` is the refusal's reason, naming the evidence that is missing. Every
/// arm that cannot read its evidence refuses: an adopted holder pane, or a
/// terminal a previous runner process spawned, has no recorded delivery and
/// no grant here. No reason carries a path.
fn shared_key_owns_hosted_session(
    host: &SessionHost,
    nonce: &str,
    proof: Option<&str>,
    session_credential: &dyn Fn(&str, Option<&str>) -> Result<String, &'static str>,
    terminal_proven: &dyn Fn(&str, Option<&str>) -> bool,
) -> Result<(), String> {
    if !host.vouches_for_its_session() {
        // A legacy record with no origin reads as reconciled, as every
        // consumer of the field reads it.
        let origin = host
            .origin
            .as_deref()
            .unwrap_or(crate::session::session_lifecycle_store::ORIGIN_RECONCILED);
        let confirmation = if host.confirmed_at.is_some() {
            ""
        } else {
            " and unconfirmed"
        };
        return Err(format!(
            "its hosting terminal's lifecycle record is {origin}{confirmation}, not \
             authoritative and confirmed, so it cannot prove whose session it is"
        ));
    }
    match session_credential(&host.terminal_id, host.working_dir.as_deref()) {
        Err(reason) => {
            return Err(format!(
                "its hosting terminal's credential is UNKNOWN to this runner process \
                 ({reason}), so ownership is UNKNOWN and the bind is refused"
            ));
        }
        Ok(credential) if !crate::coord_mcp::secret_eq(credential.as_bytes(), nonce.as_bytes()) => {
            return Err("this key is not the credential of the session's terminal".to_string());
        }
        Ok(_) => {}
    }
    match proof {
        None => Err(
            "this key is shared by every session started in its directory, and the request \
             carries no X-Qontinui-Terminal-Proof"
                .to_string(),
        ),
        Some(proof) if !terminal_proven(&host.terminal_id, Some(proof)) => Err(
            "the X-Qontinui-Terminal-Proof does not prove the hosting terminal: a wrong key, or \
             a terminal this runner process did not spawn (an adopted pane holds no grant)"
                .to_string(),
        ),
        Some(_) => Ok(()),
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

    // 3. Transcript sync's gate — the tenant's `egress_transcript_sync` switch
    // and the user's own toggle — asked in the caller's tenant, which the
    // nonce resolves here. A nonce that resolves to no tenant (`Ok(None)`)
    // writes under the device default, so the default scope is the right one
    // to ask. A resolution that FAILS also lands on the default scope here,
    // which says nothing about the caller's tenant — harmless only because
    // step 4 then refuses (`tenant_unresolvable`) before anything is bound or
    // written. Closed writes nothing.
    let tenant_hint = (env.session_tenant)(&nonce).ok().flatten();
    match (env.transcript_gate)(tenant_hint) {
        crate::egress::TranscriptGate::Open => {}
        crate::egress::TranscriptGate::UserConsentOff => {
            return bind_error(
                StatusCode::CONFLICT,
                "sync_disabled",
                Some(
                    "transcript sync is off on this runner (the user's AI content sync toggle \
                     is off); nothing was bound or written"
                        .to_string(),
                ),
            );
        }
        crate::egress::TranscriptGate::TenantSwitchOff => {
            let mut body = serde_json::json!({
                "error": "egress_off",
                "flow": crate::egress::Flow::TranscriptSync.key(),
                "domain": crate::egress::Flow::TranscriptSync.domain(),
                "detail": "transcript sync is off for this project \
                     (egress_transcript_sync); nothing was bound or written",
            });
            body["tenant_id"] = serde_json::json!(tenant_hint);
            return (StatusCode::CONFLICT, body);
        }
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

    // 4. The caller's tenant. Establish the session's repo expectation FIRST
    // (row 1d decides from what the binding has already established): without
    // this the bind could stamp the machine tenant on a session whose coord
    // calls go to its repo's tenant.
    crate::coord_mcp::establish_session_expectation(&nonce).await;
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

    // 6. Ownership: the caller's own session only (the route doc's step 3).
    // Hosting decides first — it is the stronger evidence, and a host lookup
    // that cannot be read refuses rather than reading as "not hosted". Hosted
    // in terminal T: (1) a key minted for T is owned; (2) a key minted for ANY
    // OTHER terminal is refused, even when the cwd matches; (3) a key that
    // names NO terminal is shared by every session of its directory, so it is
    // owned only on T's own evidence ([`shared_key_owns_hosted_session`]). (4)
    // Unhosted, the JSONL's own `cwd` must equal the nonce's workdir, and a
    // file with no `cwd` record yet is refused rather than guessed.
    let nonce_terminal = crate::coord_mcp::terminal_id_for_nonce(&nonce);
    let nonce_workdir = crate::coord_mcp::workdir_for_nonce(&nonce);
    let not_caller = |why: &str| {
        bind_error(
            StatusCode::FORBIDDEN,
            "not_caller_session",
            Some(format!("session {session_key} is not this caller's: {why}")),
        )
    };
    let host = match (env.session_host)(&session_key) {
        Ok(host) => host,
        Err(reason) => {
            return not_caller(&format!(
                "whether a runner terminal hosts it is UNKNOWN to this runner process \
                 ({reason}), so ownership is UNKNOWN and the bind is refused"
            ));
        }
    };
    match host {
        Some(host) if nonce_terminal.as_deref() == Some(host.terminal_id.as_str()) => {}
        Some(_) if nonce_terminal.is_some() => {
            return not_caller(
                "this key belongs to another runner terminal than the one hosting it",
            );
        }
        Some(host) => {
            // Reading the credential reads the cwd's `.mcp.json` (and, for the
            // stdio shape, the credential file it names): file I/O, so off
            // the async runtime, as the cwd arm's JSONL read is.
            let proof = headers
                .get(TERMINAL_PROOF_HEADER)
                .and_then(|v| v.to_str().ok())
                .filter(|v| !v.is_empty())
                .map(str::to_string);
            let (session_credential, terminal_proven) =
                (env.session_credential, env.terminal_proven);
            let presented = nonce.clone();
            let verdict = spawn_blocking_tracked(move || {
                shared_key_owns_hosted_session(
                    &host,
                    &presented,
                    proof.as_deref(),
                    &*session_credential,
                    &*terminal_proven,
                )
            })
            .await
            .unwrap_or_else(|_| {
                Err(
                    "the shared-key check did not complete, so ownership is UNKNOWN and the \
                     bind is refused"
                        .to_string(),
                )
            });
            if let Err(why) = verdict {
                return not_caller(&why);
            }
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
    let joined = spawn_blocking_tracked(move || {
        tailer.bind_and_replay(
            &session_key,
            &path,
            BindRequest {
                adopt,
                tenant,
                ..BindRequest::default()
            },
            // Step 3 returned on every closed gate arm, so it is open here.
            true,
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
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Json<serde_json::Value> {
    // Permission-prompt capture (plan
    // 2026-10-14-permission-prompts-are-never-recorded-so-guard-rules-grow-on-anecdote
    // Phase 2): classify + count a `permission_prompt` payload, independently
    // of the operator-touch emit below. Fail-open; never changes the answer.
    // A browser-borne body (an `Origin` header; the hook's curl sends none) is
    // not captured — only the capture is skipped, the emit below is unchanged.
    if !has_browser_origin(&headers) {
        crate::session::permission_prompt_capture::observe_notification(&id, &body);
    }
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
// /sessions/<id>/permission-request
// =============================================================================

/// `POST /sessions/{id}/permission-request` — the Claude Code
/// `PermissionRequest` hook's landing pad (plan
/// `2026-10-14-permission-prompts-are-never-recorded-so-guard-rules-grow-on-anecdote`
/// Phase 2). Phase 0 (CLI 2.1.303) found that this hook — not `Notification`
/// — is the one that carries `tool_name` + `tool_input` for the built-in
/// command-safety prompts, in `bypassPermissions`, `default` and subagents
/// alike. `{id}` is the runner terminal id (`QONTINUI_TERMINAL_ID`), the same
/// key the `/notification` pad uses, so the dialog's later `Notification` is
/// recognized as its echo and not counted twice. Body = the raw hook payload,
/// parsed LENIENTLY. Always 200.
///
/// **It also DECIDES** (plan
/// `2026-10-23-runner-sessions-stop-at-permission-dialogs-so-accept-them-by-default-with-a-tenant-switch`
/// Phase 2, D2). The capture is recorded FIRST, exactly as before; then
/// [`crate::session::permission_accept::decide`] reads the tenant's
/// `permission_auto_accept` dial and the payload, and the answer carries
/// `"decision": "allow" | "none"`. The carrier's
/// `claude_permission_request_hook.sh` prints Claude Code's allow envelope ONLY
/// on `allow`; on `none` — and on garbage, a browser `Origin`, an unanswered
/// dial, or any never-answered class — it prints nothing and the dialog shows.
/// Each `allow` is counted (`/health` `permissionAutoAccept.acceptCount`) and
/// logged by its capture class key alone, never the command or its arguments.
///
/// An `allow` is answered only for a terminal THIS runner process spawned, and
/// only when the request carries that terminal's request key in
/// `X-Qontinui-Permreq-Key`; it then ECHOES the terminal's per-spawn nonce
/// (`"nonce"`), which the hook requires next to `"decision":"allow"`. The key
/// is derived one-way from the nonce, so whatever squats the loopback port
/// while the runner is down cannot answer a prompt, and a caller without the
/// key can neither harvest a nonce nor move the accept count. `"decision_reason"` is a
/// fixed label saying why (never payload content).
///
/// The body limit is raised to [`PERMISSION_REQUEST_BODY_LIMIT`] on this route:
/// a `Write`/`Edit` prompt carries the file content, and axum's 2 MB default
/// would turn a large one into a 413 (the dialog, fail-closed, but no capture).
async fn permission_request(
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Json<serde_json::Value> {
    use crate::session::permission_accept::{self, AcceptVerdict};
    // The hook is a `curl` from the CLI and sends no `Origin`; a browser
    // always does. A browser-borne body is never counted and never decided
    // (still 200 — the pad is fail-open).
    if has_browser_origin(&headers) {
        return Json(serde_json::json!({
            "recorded": false,
            "reason": "refused: a browser Origin header is not a hook",
            "class_key": null,
            "dossier_slug": null,
            "decision": AcceptVerdict::None.as_str(),
            "decision_reason": "a browser Origin header is not a hook",
            "session": id,
        }));
    }
    // 1. Record the capture — before, and independently of, the decision.
    let outcome = crate::session::permission_prompt_capture::observe_permission_request(&id, &body);
    let class = outcome.class();
    // 2. Decide. The hook proves which terminal it is with the request key.
    let presented_key = headers
        .get(permission_accept::REQUEST_KEY_HEADER)
        .and_then(|v| v.to_str().ok());
    let decision = permission_accept::decide(&id, presented_key, &body);
    if decision.verdict == AcceptVerdict::Allow {
        permission_accept::record_accept();
        // The class key is the ONLY thing logged about the prompt; the id is
        // Debug-formatted so a crafted path segment cannot forge a log line.
        let class_key = class
            .map(|c| c.class_key.clone())
            .unwrap_or_else(|| accept_class_key_fallback(&body));
        tracing::info!(
            terminal = ?id,
            class_key = %class_key,
            "permission auto-accept: answered a PermissionRequest dialog with allow"
        );
    }
    let mut answer = serde_json::json!({
        "recorded": outcome.counted(),
        "reason": outcome.label(),
        "class_key": class.map(|c| c.class_key.as_str()),
        "dossier_slug": class.map(|c| c.dossier_slug.as_str()),
        "decision": decision.verdict.as_str(),
        "decision_reason": decision.reason,
        "session": id,
    });
    if let Some(nonce) = decision.nonce {
        answer["nonce"] = serde_json::Value::String(nonce);
    }
    Json(answer)
}

/// The request-body ceiling for `POST /sessions/{id}/permission-request` —
/// the precedent `state_machine.rs` sets for its own large bodies.
const PERMISSION_REQUEST_BODY_LIMIT: usize = 16 * 1024 * 1024;

/// The class key for an `allow` whose payload the capture did not classify
/// (it decides on `tool_name` + `permission_mode`, the capture on its own
/// shape test). The SAME classifier, over the payload's tool and command —
/// never a second parser, and never the command itself in the log.
fn accept_class_key_fallback(body: &[u8]) -> String {
    let payload: serde_json::Value =
        serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
    let tool = payload.get("tool_name").and_then(serde_json::Value::as_str);
    let command = payload
        .get("tool_input")
        .and_then(|t| t.get("command"))
        .and_then(serde_json::Value::as_str);
    crate::session::permission_prompt_capture::classify(None, tool, command).class_key
}

/// True when the request carries an `Origin` header — a browser. The
/// permission-prompt doors serve CLI callers (`curl` from a hook or a slash
/// command), which send none.
fn has_browser_origin(headers: &axum::http::HeaderMap) -> bool {
    headers.contains_key(axum::http::header::ORIGIN)
}

/// The refusal the permission-prompt read doors give a browser.
fn browser_origin_refusal() -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "error": "refused: a browser Origin header is not a permission-prompt caller",
            "code": "ORIGIN_REFUSED",
        })),
    )
        .into_response()
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

    /// A revived session finished AGAIN (a re-finish carrying a reason, on a
    /// mark coord already ACKed) restamps and re-sends; the protocol's
    /// reason-less read-back then still observes the new mark's own ACK.
    #[test]
    fn re_finishing_with_a_reason_on_a_synced_mark_restamps_and_re_sends() {
        let coord_id = uuid::Uuid::new_v4();
        let (store, _reg, _dir) = finish_fixture(Some(coord_id));
        let first = apply_finish(&store, "s", finish_req(Some("first"))).unwrap();
        assert_eq!(first["coord"]["queued"], true);
        let first_at = first["session"]["finishedAt"].as_i64().unwrap();
        store.mark_finish_synced("s", Some(first_at));

        let again = apply_finish(&store, "s", finish_req(Some("second"))).unwrap();
        assert_eq!(
            again["changed"], "marker",
            "the mark was rewritten: {again}"
        );
        assert_eq!(again["session"]["finishReason"], "second");
        let again_at = again["session"]["finishedAt"].as_i64().unwrap();
        assert_ne!(again_at, first_at, "restamped");
        assert_eq!(
            again["coord"]["queued"], true,
            "the restamped finish is re-sent to coord: {again}"
        );
        assert_eq!(again["coord"]["coordSessionId"], coord_id.to_string());
        assert_eq!(again["session"]["finishSynced"], false);

        // The read-back (no reason): reads, re-offers the owed write, and does
        // NOT restamp — so it can observe the new mark's ACK.
        let read = apply_finish(&store, "s", finish_req(None)).unwrap();
        assert_eq!(read["changed"], "none");
        assert_eq!(read["session"]["finishedAt"], again_at);
        assert_eq!(read["coord"]["queued"], true);
        store.mark_finish_synced("s", Some(first_at)); // the OLD row's late ACK
        assert_eq!(
            apply_finish(&store, "s", finish_req(None)).unwrap()["session"]["finishSynced"],
            false,
            "a stale ACK does not sync the restamped mark"
        );
        store.mark_finish_synced("s", Some(again_at));
        let read = apply_finish(&store, "s", finish_req(None)).unwrap();
        assert_eq!(read["session"]["finishSynced"], true, "{read}");
        assert_eq!(read["coord"]["reason"], "not_requeued");
        assert_eq!(read["session"]["finishedAt"], again_at);
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
                "transcript_sync_permitted",
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
            assert!(
                body["transcript_sync_permitted"].is_null(),
                "no append observed yet"
            );
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
        /// terminal, adoption confirmed iff `confirm`. The hosted-arm evidence
        /// fails closed: no terminal's credential can be named and no proof
        /// proves anything.
        fn env(
            t: Option<Arc<SessionTranscriptTailer>>,
            cfg: Vec<PathBuf>,
            confirm: bool,
        ) -> TranscriptBindEnv {
            TranscriptBindEnv {
                transcript_gate: Box::new(|_| crate::egress::TranscriptGate::Open),
                tailer: t,
                watched_config_dirs: cfg,
                session_tenant: Box::new(|_| Ok(None)),
                session_host: Box::new(|_| Ok(None)),
                session_credential: Box::new(|_, _| Err("coord_mcp_delivery_unrecorded")),
                terminal_proven: Box::new(|_, _| false),
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

        /// Hosting is stronger evidence than cwd: a session hosted in one
        /// runner terminal is refused for a key minted for ANOTHER terminal,
        /// even though its cwd is that key's workdir. The key is a real
        /// per-terminal device key from the seam — the case the name states.
        /// (This test used to present a terminal-less agent key, which is the
        /// shared-key arm, now T9.)
        #[tokio::test]
        async fn a_session_hosted_in_another_terminal_is_refused() {
            let amb = crate::test_env::isolated_ambient();
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let key = per_terminal_key(&amb, &terminal());
            let wd = crate::coord_mcp::workdir_for_nonce(&key).expect("workdir");
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &wd);
            let mut e = env(Some(t), vec![cfg], true);
            let hosting = host(&terminal(), &wd);
            e.session_host = Box::new(move |_| Ok(Some(hosting.clone())));
            let (status, v) = transcript_bind_core(&bearer(&key), &body(&csid), e).await;
            let detail = refused(status, &v, &outbox, &[wd.as_str()]);
            assert!(
                detail.contains("belongs to another runner terminal"),
                "{detail}"
            );
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
            e.transcript_gate = Box::new(|_| crate::egress::TranscriptGate::UserConsentOff);
            let (status, v) = transcript_bind_core(&bearer(&nonce), &body(&csid), e).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(v["error"], "sync_disabled");
            assert!(outbox.pending().unwrap().is_empty());
        }

        /// M1: the TENANT's switch off answers 409 `egress_off`, asks the gate
        /// in the caller's tenant scope, and writes nothing.
        #[tokio::test]
        async fn tenant_switch_off_is_egress_off_and_writes_nothing() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (nonce, wd) = registered_nonce();
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &wd);
            let tenant = uuid::Uuid::from_u128(0x7e);
            let asked = Arc::new(std::sync::Mutex::new(None));
            let seen = asked.clone();
            let mut e = env(Some(t), vec![cfg], true);
            e.session_tenant = Box::new(move |_| Ok(Some(tenant)));
            e.transcript_gate = Box::new(move |t| {
                *seen.lock().unwrap() = Some(t);
                crate::egress::TranscriptGate::TenantSwitchOff
            });
            let (status, v) = transcript_bind_core(&bearer(&nonce), &body(&csid), e).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(v["error"], "egress_off");
            assert_eq!(v["flow"], "transcript_sync");
            assert_eq!(*asked.lock().unwrap(), Some(Some(tenant)));
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

        // =====================================================================
        // Step 6 — a HOSTED session presented with a key that names no
        // terminal (plan
        // 2026-11-05-transcript-bind-refuses-a-hosted-session-whose-only-key-is-the-workdir-nonce,
        // Phase 2). T1-T11 enumerate every arm of the ownership step, the
        // failure arms included. Each refusal asserts the outbox is empty and
        // that its detail names no path.
        // =====================================================================

        /// The port the real config writers record; nothing listens on it.
        const PORT: u16 = 23_917;

        fn terminal() -> String {
            format!("term-{}", uuid::Uuid::new_v4())
        }

        /// A fresh directory under `dir`, so no other test's keys share it.
        fn fresh_workdir(dir: &std::path::Path) -> String {
            let wd = dir.join(format!("wd-{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&wd).unwrap();
            wd.to_string_lossy().to_string()
        }

        /// The coord-mcp key `<wd>/.mcp.json` carries, read the way the runner
        /// reads it (through the stdio credential file when there is one).
        fn cwd_key(wd: &str) -> String {
            let raw = std::fs::read_to_string(std::path::Path::new(wd).join(".mcp.json"))
                .expect("the writer wrote .mcp.json");
            let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
            crate::coord_mcp_config::proxy_nonce_from_config_doc(&doc).expect("a runner key")
        }

        /// The SHARED key: what the cwd `.mcp.json` writer mints — a DEVICE
        /// key that names no terminal, read by every session started in that
        /// directory. Written by the real writer into a fresh directory under
        /// `dir`; returns `(key, the workdir it is bound to)`.
        fn shared_cwd_key(dir: &std::path::Path) -> (String, String) {
            let wd = fresh_workdir(dir);
            crate::coord_mcp::write_coord_mcp_proxy_config(&wd, PORT, None);
            let key = cwd_key(&wd);
            assert_eq!(crate::coord_mcp::terminal_id_for_nonce(&key), None);
            assert_eq!(
                crate::coord_mcp::proxy_principal_for_nonce(&key),
                Some(crate::coord_mcp::ProxyPrincipal::Device)
            );
            let bound = crate::coord_mcp::workdir_for_nonce(&key).expect("workdir");
            (key, bound)
        }

        /// A PER-TERMINAL key: what the seam mints for `terminal` when its cwd
        /// declares no coord-mcp. Its config file lands in app data, hence the
        /// ambient fixture.
        fn per_terminal_key(amb: &crate::test_env::IsolatedAmbient, terminal: &str) -> String {
            let wd = fresh_workdir(amb.dir());
            let delivered =
                crate::coord_mcp::deliver_terminal_coord_mcp(&wd, terminal, None, Some(PORT))
                    .expect("an unpinned spawn is admitted");
            assert_eq!(
                delivered.delivery,
                crate::coord_mcp::CoordMcpDelivery::Provisioned
            );
            let path = delivered
                .config_path
                .expect("a bound port provisions a file");
            let doc: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            let key = crate::coord_mcp_config::proxy_nonce_from_config_doc(&doc).unwrap();
            assert_eq!(
                crate::coord_mcp::terminal_id_for_nonce(&key).as_deref(),
                Some(terminal)
            );
            key
        }

        /// A grant this test registered — the record the spawn path makes for
        /// a terminal it spawned, carrying its per-spawn `secret` — forgotten
        /// when the guard drops, on a failing assertion's unwind too, so a red
        /// test never leaves a slot behind in the process-wide grant registry.
        struct Granted {
            terminal: String,
            secret: String,
        }

        impl Drop for Granted {
            fn drop(&mut self) {
                crate::session::permission_accept::forget_terminal_grant(
                    &self.terminal,
                    &self.secret,
                );
            }
        }

        /// Record `terminal` as spawned by this process.
        fn grant(terminal: &str) -> Granted {
            let secret = crate::session::permission_accept::new_nonce();
            crate::session::permission_accept::register_terminal_grant(
                terminal,
                secret.clone(),
                None,
            );
            Granted {
                terminal: terminal.to_string(),
                secret,
            }
        }

        /// `headers` plus this door's proof for `secret`, derived the way the
        /// CLIENT derives it from `$QONTINUI_PERMREQ_NONCE`: the literal label,
        /// door and header, not the runner's own derivation, so these tests
        /// present what a client presents. The door-key unit tests pin the
        /// runner's derivation to the same shared vector.
        fn with_proof(mut headers: HeaderMap, secret: &str) -> HeaderMap {
            use sha2::Digest as _;
            let mut h = sha2::Sha256::new();
            h.update(b"qontinui-terminal-door-key\0");
            h.update(b"transcript-bind");
            h.update(b"\0");
            h.update(secret.as_bytes());
            headers.insert(
                "x-qontinui-terminal-proof",
                hex::encode(h.finalize()).parse().unwrap(),
            );
            headers
        }

        /// What an `authoritative`, confirmed open record says about a session
        /// hosted in `terminal` — the shape every open lifecycle record had on
        /// nilan when plan
        /// 2026-11-05-transcript-bind-refuses-a-hosted-session-whose-only-key-is-the-workdir-nonce
        /// measured them on 2026-11-05.
        fn host(terminal: &str, working_dir: &str) -> SessionHost {
            SessionHost {
                terminal_id: terminal.to_string(),
                working_dir: Some(working_dir.to_string()),
                origin: Some(
                    crate::session::session_lifecycle_store::ORIGIN_AUTHORITATIVE.to_string(),
                ),
                confirmed_at: Some(1),
            }
        }

        /// [`env`] for a session hosted per `host`, whose terminal's
        /// credential is `credential` (every other terminal's is UNKNOWN), and
        /// with the REAL door-key check over this process's grants.
        fn hosted_env(
            t: Option<Arc<SessionTranscriptTailer>>,
            cfg: Vec<PathBuf>,
            host: SessionHost,
            credential: Result<String, &'static str>,
        ) -> TranscriptBindEnv {
            let mut e = env(t, cfg, true);
            let host_terminal = host.terminal_id.clone();
            e.session_host = Box::new(move |_| Ok(Some(host.clone())));
            e.session_credential = Box::new(move |terminal, _| {
                if terminal == host_terminal {
                    credential.clone()
                } else {
                    Err("coord_mcp_delivery_unrecorded")
                }
            });
            e.terminal_proven = Box::new(|terminal, proof| {
                crate::session::terminal_door_key::terminal_door_key_matches(
                    terminal,
                    TRANSCRIPT_BIND_DOOR,
                    proof,
                )
            });
            e
        }

        /// A `not_caller_session` refusal that wrote nothing and whose detail
        /// names none of `paths`; returns the detail.
        fn refused(
            status: StatusCode,
            v: &serde_json::Value,
            outbox: &OutboxWriter,
            paths: &[&str],
        ) -> String {
            assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
            assert_eq!(v["error"], "not_caller_session", "{v}");
            let detail = v["detail"].as_str().expect("every refusal has a detail");
            for path in paths {
                assert!(!detail.contains(path), "a detail named a path: {detail}");
            }
            assert!(
                outbox.pending().unwrap().is_empty(),
                "a refusal wrote: {detail}"
            );
            detail.to_string()
        }

        /// T1, the live shape: a hosted session whose only key is its cwd's
        /// shared key — its terminal's credential — with its terminal's proof
        /// binds and replays; a second call is `already_bound` and replays
        /// nothing.
        #[tokio::test]
        async fn t1_a_hosted_sessions_shared_key_with_its_terminals_proof_binds() {
            let dir = tempfile::tempdir().unwrap();
            let (t, _outbox) = tailer(dir.path());
            let (key, wd) = shared_cwd_key(dir.path());
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &wd);
            let file_len =
                std::fs::metadata(cfg.join("projects").join("p").join(format!("{csid}.jsonl")))
                    .unwrap()
                    .len();
            let term = terminal();
            let granted = grant(&term);
            let mk = || {
                hosted_env(
                    Some(t.clone()),
                    vec![cfg.clone()],
                    host(&term, &wd),
                    Ok(key.clone()),
                )
            };
            let headers = with_proof(bearer(&key), &granted.secret);

            let (status, v) = call(&headers, &body(&csid), mk).await;
            assert_eq!(status, StatusCode::OK, "{v}");
            assert_eq!(v["bound"], true);
            assert_eq!(v["already_bound"], false);
            assert_eq!(v["replayed_bytes"], file_len);
            assert_eq!(v["replayed_chunks"], 1);
            assert!(v["replay_stopped_at"].is_null());
            let first_row = v["coord_session_id"].clone();

            let (status, v) = call(&headers, &body(&csid), mk).await;
            assert_eq!(status, StatusCode::OK, "{v}");
            assert_eq!(v["already_bound"], true);
            assert_eq!(v["coord_session_id"], first_row, "no second row");
            assert_eq!(v["replayed_bytes"], 0);
            assert_eq!(v["replayed_chunks"], 0);
        }

        /// T2: T1 without the proof — today's refusal, now honest about what
        /// is missing. An empty header is no proof, and the terminal's own
        /// permission-pad request key is not this door's proof either.
        #[tokio::test]
        async fn t2_a_hosted_sessions_shared_key_without_its_terminals_proof_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (key, wd) = shared_cwd_key(dir.path());
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &wd);
            let term = terminal();
            let granted = grant(&term);
            let mk = || {
                hosted_env(
                    Some(t.clone()),
                    vec![cfg.clone()],
                    host(&term, &wd),
                    Ok(key.clone()),
                )
            };
            let dir_path = dir.path().to_string_lossy().to_string();
            let paths = [wd.as_str(), dir_path.as_str()];

            let (status, v) = transcript_bind_core(&bearer(&key), &body(&csid), mk()).await;
            let detail = refused(status, &v, &outbox, &paths);
            assert!(
                detail.contains("carries no X-Qontinui-Terminal-Proof"),
                "{detail}"
            );

            let mut empty = bearer(&key);
            empty.insert(TERMINAL_PROOF_HEADER, "".parse().unwrap());
            let (status, v) = transcript_bind_core(&empty, &body(&csid), mk()).await;
            let detail = refused(status, &v, &outbox, &paths);
            assert!(detail.contains("carries no"), "{detail}");

            let mut pad_key = bearer(&key);
            pad_key.insert(
                TERMINAL_PROOF_HEADER,
                crate::session::permission_accept::request_key_for(&granted.secret)
                    .parse()
                    .unwrap(),
            );
            let (status, v) = transcript_bind_core(&pad_key, &body(&csid), mk()).await;
            let detail = refused(status, &v, &outbox, &paths);
            assert!(
                detail.contains("does not prove the hosting terminal"),
                "{detail}"
            );
        }

        /// T3, a sibling: B's session is hosted in terminal B, in the same
        /// directory, so the shared key IS terminal B's credential too — and
        /// the request carries terminal A's proof. Refused, nothing written:
        /// the peer-binding case the proof exists for.
        #[tokio::test]
        async fn t3_a_sibling_presenting_another_terminals_proof_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (key, wd) = shared_cwd_key(dir.path());
            let sibling = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &sibling, &wd);
            let (a, b) = (terminal(), terminal());
            let (granted_a, _granted_b) = (grant(&a), grant(&b));

            let e = hosted_env(Some(t), vec![cfg], host(&b, &wd), Ok(key.clone()));
            let (status, v) = transcript_bind_core(
                &with_proof(bearer(&key), &granted_a.secret),
                &body(&sibling),
                e,
            )
            .await;
            let detail = refused(status, &v, &outbox, &[wd.as_str()]);
            assert!(
                detail.contains("does not prove the hosting terminal"),
                "{detail}"
            );
        }

        /// T4: a key that is not the hosting terminal's credential (the
        /// terminal's credential is ANOTHER key), with a valid proof of that
        /// terminal — refused.
        #[tokio::test]
        async fn t4_a_shared_key_that_is_not_the_terminals_credential_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (key, wd) = shared_cwd_key(dir.path());
            let (credential, _) = shared_cwd_key(dir.path());
            assert_ne!(key, credential);
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &wd);
            let term = terminal();
            let granted = grant(&term);

            let e = hosted_env(Some(t), vec![cfg], host(&term, &wd), Ok(credential));
            let (status, v) =
                transcript_bind_core(&with_proof(bearer(&key), &granted.secret), &body(&csid), e)
                    .await;
            let detail = refused(status, &v, &outbox, &[wd.as_str()]);
            assert!(
                detail.contains("not the credential of the session's terminal"),
                "{detail}"
            );
        }

        /// T5: a key minted for ANOTHER terminal, presented with a valid proof
        /// of the host — and built so every OTHER condition of the shared-key
        /// arm holds (the record vouches, and the host's credential IS the
        /// presented key), so only the another-terminal arm can refuse it. A
        /// peer's per-terminal key stays refused.
        #[tokio::test]
        async fn t5_another_terminals_key_is_refused_even_when_all_else_holds() {
            let amb = crate::test_env::isolated_ambient();
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let other = terminal();
            let key = per_terminal_key(&amb, &other);
            let wd = crate::coord_mcp::workdir_for_nonce(&key).expect("workdir");
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &wd);
            let term = terminal();
            let granted = grant(&term);

            let e = hosted_env(Some(t), vec![cfg], host(&term, &wd), Ok(key.clone()));
            let (status, v) =
                transcript_bind_core(&with_proof(bearer(&key), &granted.secret), &body(&csid), e)
                    .await;
            let detail = refused(status, &v, &outbox, &[wd.as_str()]);
            assert!(
                detail.contains("belongs to another runner terminal"),
                "{detail}"
            );
        }

        /// T6: the hosting terminal's credential cannot be named — every
        /// reason the resolver gives — with a valid proof. Refused, and the
        /// detail carries the reason: ownership is UNKNOWN, never assumed.
        #[tokio::test]
        async fn t6_an_unknown_terminal_credential_is_refused_with_its_reason() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (key, wd) = shared_cwd_key(dir.path());
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &wd);
            let term = terminal();
            let granted = grant(&term);

            for reason in [
                "coord_mcp_delivery_unrecorded",
                "no_session_nonce",
                "no_runner_nonce_in_cwd_file",
            ] {
                let e = hosted_env(
                    Some(t.clone()),
                    vec![cfg.clone()],
                    host(&term, &wd),
                    Err(reason),
                );
                let (status, v) = transcript_bind_core(
                    &with_proof(bearer(&key), &granted.secret),
                    &body(&csid),
                    e,
                )
                .await;
                let detail = refused(status, &v, &outbox, &[wd.as_str()]);
                assert!(detail.contains(reason), "{detail}");
                assert!(detail.contains("UNKNOWN"), "{detail}");
            }
        }

        /// T7: the host's OWN per-terminal key needs no proof — the first arm
        /// is unchanged. It consults none of the shared-key evidence: the
        /// record here is reconciled and unconfirmed, no credential can be
        /// named and the terminal holds no grant, and it still binds.
        #[tokio::test]
        async fn t7_the_hosts_own_per_terminal_key_binds_without_a_proof() {
            let amb = crate::test_env::isolated_ambient();
            let dir = tempfile::tempdir().unwrap();
            let (t, _outbox) = tailer(dir.path());
            let term = terminal();
            let key = per_terminal_key(&amb, &term);
            let wd = crate::coord_mcp::workdir_for_nonce(&key).expect("workdir");
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &wd);
            let mut record = host(&term, &wd);
            record.origin =
                Some(crate::session::session_lifecycle_store::ORIGIN_RECONCILED.to_string());
            record.confirmed_at = None;

            let (status, v) = call(&bearer(&key), &body(&csid), || {
                hosted_env(
                    Some(t.clone()),
                    vec![cfg.clone()],
                    record.clone(),
                    Err("coord_mcp_delivery_unrecorded"),
                )
            })
            .await;
            assert_eq!(status, StatusCode::OK, "{v}");
            assert_eq!(v["bound"], true);
        }

        /// T8, the resume shapes: a session id no open lifecycle record holds
        /// (an in-process `/resume` the store does not know) takes the cwd
        /// arm, unchanged — the shared key binds it when its JSONL `cwd` is
        /// the key's workdir, and is refused when it is not.
        #[tokio::test]
        async fn t8_an_unhosted_session_keeps_the_cwd_arm() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (key, wd) = shared_cwd_key(dir.path());

            let elsewhere = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &elsewhere, "/some/other/workdir");
            let (status, v) = transcript_bind_core(
                &bearer(&key),
                &body(&elsewhere),
                env(Some(t.clone()), vec![cfg.clone()], true),
            )
            .await;
            let detail = refused(status, &v, &outbox, &[wd.as_str()]);
            assert!(
                detail.contains("its JSONL's cwd is not this nonce's workdir"),
                "{detail}"
            );

            let resumed = uuid::Uuid::new_v4().to_string();
            config_with(dir.path(), &resumed, &wd);
            let (status, v) = call(&bearer(&key), &body(&resumed), || {
                env(Some(t.clone()), vec![cfg.clone()], true)
            })
            .await;
            assert_eq!(status, StatusCode::OK, "{v}");
            assert_eq!(v["bound"], true);
        }

        /// T9: a terminal-less AGENT key that is NOT the hosting terminal's
        /// credential (that is the cwd's device key), with a valid proof of
        /// the host — refused. (An agent key that IS the credential is
        /// admitted; T10's agent-worktree variant pins that through the
        /// production seam.)
        #[tokio::test]
        async fn t9_an_agent_key_that_is_not_the_terminals_credential_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (agent_key, agent_wd) = registered_nonce();
            let (device_key, _) = shared_cwd_key(dir.path());
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &agent_wd);
            let term = terminal();
            let granted = grant(&term);

            let e = hosted_env(Some(t), vec![cfg], host(&term, &agent_wd), Ok(device_key));
            let (status, v) = transcript_bind_core(
                &with_proof(bearer(&agent_key), &granted.secret),
                &body(&csid),
                e,
            )
            .await;
            let detail = refused(status, &v, &outbox, &[agent_wd.as_str()]);
            assert!(
                detail.contains("not the credential of the session's terminal"),
                "{detail}"
            );
        }

        /// T11: T1's inputs, but the hosting record cannot vouch for the
        /// session it names — `reconciled` (it "may name a foreign session"),
        /// a legacy record with no origin, `observed`, or `authoritative` but
        /// never confirmed. Refused, and the detail says which.
        #[tokio::test]
        async fn t11_a_host_record_that_is_not_authoritative_and_confirmed_is_refused() {
            use crate::session::session_lifecycle_store::{
                ORIGIN_AUTHORITATIVE, ORIGIN_OBSERVED, ORIGIN_RECONCILED,
            };
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (key, wd) = shared_cwd_key(dir.path());
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &wd);
            let term = terminal();
            let granted = grant(&term);

            for (origin, confirmed_at, says) in [
                (Some(ORIGIN_RECONCILED), Some(1), "is reconciled,"),
                (None, Some(1), "is reconciled,"),
                (Some(ORIGIN_OBSERVED), Some(1), "is observed,"),
                (
                    Some(ORIGIN_AUTHORITATIVE),
                    None,
                    "is authoritative and unconfirmed,",
                ),
                (
                    Some(ORIGIN_RECONCILED),
                    None,
                    "is reconciled and unconfirmed,",
                ),
            ] {
                let mut record = host(&term, &wd);
                record.origin = origin.map(str::to_string);
                record.confirmed_at = confirmed_at;
                let e = hosted_env(Some(t.clone()), vec![cfg.clone()], record, Ok(key.clone()));
                let (status, v) = transcript_bind_core(
                    &with_proof(bearer(&key), &granted.secret),
                    &body(&csid),
                    e,
                )
                .await;
                let detail = refused(status, &v, &outbox, &[wd.as_str()]);
                assert!(
                    detail.contains(says),
                    "{origin:?}/{confirmed_at:?}: {detail}"
                );
                assert!(
                    detail.contains("cannot prove whose session it is"),
                    "{detail}"
                );
            }
        }

        /// The terminal proof is evidence of a LOCAL terminal, so the door
        /// must stay loopback-only: on the origin guard's credential-door list
        /// (no browser caller reaches it) and off the backend relay's
        /// allowlist, whose header filter (`RELAY_SKIP_REQUEST_HEADERS`) does
        /// not strip `X-Qontinui-Terminal-Proof`. A relay that ever admits this
        /// door must strip that header first; this is the tripwire.
        #[test]
        fn the_door_stays_loopback_only_so_no_relayed_caller_presents_a_proof() {
            assert!(crate::mcp::origin_guard::is_credential_door(
                "POST",
                "/sessions/transcript-bind"
            ));
            assert_eq!(
                crate::mcp::relay_path_policy::relay_path_verdict(
                    "POST",
                    "/sessions/transcript-bind"
                ),
                crate::mcp::relay_path_policy::RelayPathVerdict::NotAllowed
            );
        }

        /// Which writer put the cwd's key in place.
        #[derive(Debug, Clone, Copy)]
        enum CwdWriter {
            /// `write_coord_mcp_proxy_config` — the operator's root, a gate
            /// continuation's cwd: a terminal-less DEVICE key.
            Device,
            /// `write_coord_mcp_agent_proxy_config` — an agent worktree's
            /// in-cwd key: a terminal-less AGENT key.
            Agent,
        }

        /// The device's active tenant in the route tests' machine fixture.
        const ROUTE_TENANT: uuid::Uuid =
            uuid::Uuid::from_u128(0x7b1d_0000_0000_4000_8000_0000_0000_0001);

        /// The machine state the production route reads besides this door's
        /// own closures, set the way the machine sets it: the device's active
        /// tenant in `machine.json`, and coord's answer that transcript sync is
        /// on for it (an egress pin reads as that answer, on this thread, which
        /// is where the route asks). The user's own sync toggle stays at its
        /// shipped default, on.
        fn route_machine(
            amb: &crate::test_env::IsolatedAmbient,
        ) -> crate::egress::test_support::EgressPin {
            amb.write_active_tenant_id(ROUTE_TENANT);
            crate::egress::test_support::pin(
                crate::egress::Flow::TranscriptSync,
                crate::egress::Level::On,
            )
        }

        /// POST through [`transcript_bind_route`] — the door's production entry
        /// below the axum extraction, every production closure included —
        /// retrying across the process-global registration kill switch the
        /// coord_register suite toggles, as [`call`] does for the core.
        async fn call_route(
            tailer: &Arc<SessionTranscriptTailer>,
            lifecycle: Option<&Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>,
            watched: &[PathBuf],
            headers: &HeaderMap,
            body: &[u8],
        ) -> (StatusCode, serde_json::Value) {
            for _ in 0..200 {
                let (status, v) = transcript_bind_route(
                    Some(tailer.clone()),
                    lifecycle.cloned(),
                    watched.to_vec(),
                    headers,
                    body,
                )
                .await;
                if status == StatusCode::CONFLICT && v["error"] == "registration_disabled" {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    continue;
                }
                return (status, v);
            }
            panic!("registration stayed disabled")
        }

        /// T10, production wiring, entered ONLY through [`transcript_bind_route`]:
        /// two terminals spawned in ONE directory that declares coord-mcp,
        /// through the real seam (`deliver_terminal_coord_mcp` records the
        /// declared delivery for both), with real grants and a real tempdir
        /// `SessionLifecycleStore` holding both sessions as open, authoritative,
        /// confirmed records. The request carries the cwd's key and terminal
        /// A's proof, derived as a client derives it. B's session is refused
        /// and nothing is written (T3); A's session binds (T1).
        async fn production_wiring(
            amb: &crate::test_env::IsolatedAmbient,
            writer: CwdWriter,
            stdio: bool,
        ) {
            use crate::session::session_lifecycle_store::{
                test_open_record, SessionLifecycleStore, ORIGIN_AUTHORITATIVE,
            };
            let _sync_on = route_machine(amb);
            let wd = fresh_workdir(amb.dir());
            match writer {
                CwdWriter::Device => {
                    crate::coord_mcp::write_coord_mcp_proxy_config(&wd, PORT, None)
                }
                CwdWriter::Agent => crate::coord_mcp::write_coord_mcp_agent_proxy_config(
                    &wd,
                    PORT,
                    uuid::Uuid::new_v4(),
                ),
            }
            let doc: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(std::path::Path::new(&wd).join(".mcp.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                crate::coord_mcp_config::config_doc_is_stdio_shim(&doc),
                stdio,
                "the document this test reads is not the shape it names: {doc}"
            );
            let key = cwd_key(&wd);
            let (a, b) = (terminal(), terminal());
            for term in [&a, &b] {
                let delivered =
                    crate::coord_mcp::deliver_terminal_coord_mcp(&wd, term, None, Some(PORT))
                        .expect("a tenant-less spawn into a declared cwd is admitted");
                assert_eq!(
                    delivered.delivery,
                    crate::coord_mcp::CoordMcpDelivery::WorkdirDeclared
                );
            }
            let (granted_a, _granted_b) = (grant(&a), grant(&b));

            let state_dir = amb
                .dir()
                .join(format!("state-{}", uuid::Uuid::new_v4().simple()));
            let store = Arc::new(
                SessionLifecycleStore::open(state_dir.join("terminal-sessions.json")).unwrap(),
            );
            let (csid_a, csid_b) = (
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
            );
            for (csid, term) in [(&csid_a, &a), (&csid_b, &b)] {
                let mut rec = test_open_record(csid);
                rec.terminal_id = term.clone();
                rec.working_dir = Some(wd.clone());
                rec.origin = Some(ORIGIN_AUTHORITATIVE.to_string());
                rec.confirmed_at = Some(1);
                store.record_open(rec);
            }
            let watched = vec![config_with(&state_dir, &csid_a, &wd)];
            config_with(&state_dir, &csid_b, &wd);
            let (t, outbox) = tailer(&state_dir);
            let proof_a = with_proof(bearer(&key), &granted_a.secret);

            let (status, v) =
                call_route(&t, Some(&store), &watched, &proof_a, &body(&csid_b)).await;
            let detail = refused(status, &v, &outbox, &[wd.as_str()]);
            assert!(
                detail.contains("does not prove the hosting terminal"),
                "{writer:?}: {detail}"
            );

            let (status, v) =
                call_route(&t, Some(&store), &watched, &proof_a, &body(&csid_a)).await;
            assert_eq!(status, StatusCode::OK, "{writer:?}: {v}");
            assert_eq!(v["bound"], true);
        }

        /// Force the stdio document every observed case presents (a
        /// `coord-mcp-shim.py --credential <abs>` entry). `production_wiring`
        /// asserts the shape of the very `.mcp.json` it reads, so the stdio
        /// variants cannot pass on the http arm `cfg(test)` defaults to.
        fn open_stdio_gate(
            amb: &crate::test_env::IsolatedAmbient,
        ) -> crate::coord_mcp::StdioShimGateGuard {
            use crate::coord_mcp::{StdioShimGate, StdioShimGateGuard, StdioShimGateOverride};
            StdioShimGateGuard::install(StdioShimGateOverride::Fixed(StdioShimGate::Open {
                interpreter: amb.dir().join("fake-python"),
                shim: amb
                    .dir()
                    .join("fake-scripts")
                    .join(crate::coord_mcp_config::COORD_MCP_STDIO_SHIM_FILE),
            }))
        }

        #[tokio::test]
        async fn t10_production_wiring_http_shape() {
            let amb = crate::test_env::isolated_ambient();
            production_wiring(&amb, CwdWriter::Device, false).await;
        }

        #[tokio::test]
        async fn t10_production_wiring_stdio_shape() {
            let amb = crate::test_env::isolated_ambient();
            let _gate = open_stdio_gate(&amb);
            production_wiring(&amb, CwdWriter::Device, true).await;
        }

        /// An agent worktree: the agent writer's in-cwd key IS the hosting
        /// terminal's credential (`session_nonce` reads the declared file with
        /// no principal filter), so a hosted session there hands off its own
        /// transcript — and its sibling still cannot.
        #[tokio::test]
        async fn t10_production_wiring_agent_worktree() {
            let amb = crate::test_env::isolated_ambient();
            production_wiring(&amb, CwdWriter::Agent, false).await;
        }

        #[tokio::test]
        async fn t10_production_wiring_agent_worktree_stdio_shape() {
            let amb = crate::test_env::isolated_ambient();
            let _gate = open_stdio_gate(&amb);
            production_wiring(&amb, CwdWriter::Agent, true).await;
        }

        /// T12: a host lookup that cannot be read is UNKNOWN and refuses — even
        /// where the cwd arm WOULD admit (the session's JSONL `cwd` is the
        /// key's workdir) — for each reason the production lookup gives.
        #[tokio::test]
        async fn t12_an_unreadable_host_lookup_refuses_instead_of_taking_the_cwd_arm() {
            let dir = tempfile::tempdir().unwrap();
            let (t, outbox) = tailer(dir.path());
            let (key, wd) = shared_cwd_key(dir.path());
            let csid = uuid::Uuid::new_v4().to_string();
            let cfg = config_with(dir.path(), &csid, &wd);
            for reason in ["lifecycle_store_unavailable", "lifecycle_store_unreadable"] {
                let mut e = env(Some(t.clone()), vec![cfg.clone()], true);
                e.session_host = Box::new(move |_| Err(reason));
                let (status, v) = transcript_bind_core(&bearer(&key), &body(&csid), e).await;
                let detail = refused(status, &v, &outbox, &[wd.as_str()]);
                assert!(detail.contains(reason), "{detail}");
                assert!(detail.contains("UNKNOWN"), "{detail}");
            }
            // The control: the same request, with a lookup that answers "not
            // hosted", IS the cwd arm's, and binds.
            let (status, v) = call(&bearer(&key), &body(&csid), || {
                env(Some(t.clone()), vec![cfg.clone()], true)
            })
            .await;
            assert_eq!(status, StatusCode::OK, "{v}");
        }

        /// T12 through the production route: a runner with NO lifecycle store
        /// refuses with the UNKNOWN detail rather than reading every session as
        /// unhosted. The control: with a store that holds no record for the
        /// session, the same request is the cwd arm's and binds.
        #[tokio::test]
        async fn t12_a_runner_with_no_lifecycle_store_refuses_through_the_route() {
            use crate::session::session_lifecycle_store::SessionLifecycleStore;
            let amb = crate::test_env::isolated_ambient();
            let _sync_on = route_machine(&amb);
            let wd = fresh_workdir(amb.dir());
            crate::coord_mcp::write_coord_mcp_proxy_config(&wd, PORT, None);
            let key = cwd_key(&wd);
            let bound = crate::coord_mcp::workdir_for_nonce(&key).expect("workdir");
            let state_dir = amb
                .dir()
                .join(format!("state-{}", uuid::Uuid::new_v4().simple()));
            let csid = uuid::Uuid::new_v4().to_string();
            let watched = vec![config_with(&state_dir, &csid, &bound)];
            let (t, outbox) = tailer(&state_dir);

            let (status, v) = call_route(&t, None, &watched, &bearer(&key), &body(&csid)).await;
            let detail = refused(status, &v, &outbox, &[wd.as_str(), bound.as_str()]);
            assert!(detail.contains("lifecycle_store_unavailable"), "{detail}");
            assert!(detail.contains("UNKNOWN"), "{detail}");

            let store = Arc::new(
                SessionLifecycleStore::open(state_dir.join("terminal-sessions.json")).unwrap(),
            );
            let (status, v) =
                call_route(&t, Some(&store), &watched, &bearer(&key), &body(&csid)).await;
            assert_eq!(status, StatusCode::OK, "{v}");
            assert_eq!(v["bound"], true);
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

// =============================================================================
// /permission-prompts/classify
// =============================================================================

/// Body of `POST /permission-prompts/classify`.
#[derive(Debug, Default, Deserialize)]
struct ClassifyPermissionPromptRequest {
    /// The prompt text — pasted out of a blocked pane, or a `Notification`
    /// `message`.
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    command: Option<String>,
}

/// `POST /permission-prompts/classify` — the D3 classifier as a pure,
/// side-effect-free loopback door (plan
/// `2026-10-14-permission-prompts-are-never-recorded-so-guard-rules-grow-on-anecdote`
/// Phase 2), so the `/permission-prompts record` command keys a pasted prompt
/// exactly as the Notification intake keys a captured one. Records nothing.
/// Body is parsed leniently (empty / non-JSON → all fields absent → the
/// `permission-prompt-unknown` class); 200, except 403 to a request carrying
/// a browser `Origin`.
async fn classify_permission_prompt(
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if has_browser_origin(&headers) {
        return browser_origin_refusal();
    }
    let req: ClassifyPermissionPromptRequest = serde_json::from_slice(&body).unwrap_or_default();
    let class = crate::session::permission_prompt_capture::classify(
        req.text.as_deref(),
        req.tool_name.as_deref(),
        req.command.as_deref(),
    );
    let redacted = req
        .text
        .as_deref()
        .map(crate::session::permission_prompt_capture::redact_pasted);
    Json(serde_json::json!({
        "class_key": class.class_key,
        "dossier_slug": class.dossier_slug,
        "redacted": redacted,
        "reason": class.reason.as_deref().map(crate::session::permission_prompt_capture::redact_pasted),
        "tool": class.tool,
    }))
    .into_response()
}

/// `GET /permission-prompts/snapshot` — read-only view of the in-memory
/// aggregator (plan
/// `2026-10-14-permission-prompts-are-never-recorded-so-guard-rules-grow-on-anecdote`
/// Phase 2), so a live box can observe the capture arm before Phase 3's flush
/// exists. Every field is already redacted (D6). 403 to a browser `Origin`.
async fn permission_prompts_snapshot(headers: axum::http::HeaderMap) -> axum::response::Response {
    use axum::response::IntoResponse;
    if has_browser_origin(&headers) {
        return browser_origin_refusal();
    }
    let buckets: Vec<serde_json::Value> = crate::session::permission_prompt_capture::snapshot()
        .iter()
        .map(|b| b.to_json())
        .collect();
    Json(serde_json::json!({
        "bucket_count": buckets.len(),
        "dropped": crate::session::permission_prompt_capture::dropped(),
        "buckets": buckets,
    }))
    .into_response()
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
        .route(
            "/sessions/{id}/permission-request",
            post(permission_request).layer(axum::extract::DefaultBodyLimit::max(
                PERMISSION_REQUEST_BODY_LIMIT,
            )),
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
        // The D3 permission-prompt classifier, pure (plan 2026-10-14 Phase 2).
        .route(
            "/permission-prompts/classify",
            post(classify_permission_prompt),
        )
        .route(
            "/permission-prompts/snapshot",
            get(permission_prompts_snapshot),
        )
}

/// Permission-prompt capture through the routes the population uses (plan
/// `2026-10-14-permission-prompts-are-never-recorded-so-guard-rules-grow-on-anecdote`
/// Phase 2). The handlers take no `State`, so each test mounts the REAL
/// handler fn on its REAL path; `both_routes_are_registered_by_routes` pins
/// that `routes()` registers those same paths.
#[cfg(test)]
mod permission_prompt_capture_route_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt as _;

    async fn post_json(app: Router, uri: &str, body: String) -> (StatusCode, serde_json::Value) {
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    fn notification_app() -> Router {
        Router::new().route(
            "/sessions/{id}/notification",
            post(operator_touch_notification),
        )
    }

    fn permission_request_app() -> Router {
        Router::new().route(
            "/sessions/{id}/permission-request",
            post(permission_request).layer(axum::extract::DefaultBodyLimit::max(
                PERMISSION_REQUEST_BODY_LIMIT,
            )),
        )
    }

    fn class_count(slug: &str) -> u64 {
        crate::session::permission_prompt_capture::snapshot()
            .iter()
            .filter(|b| b.dossier_slug == slug)
            .map(|b| b.count)
            .sum()
    }

    /// ARMING (detect_deliver), primary entry: a `PermissionRequest` payload —
    /// the Phase 0 raw shape — posted to `/sessions/{id}/permission-request`
    /// increments the aggregator's class count, and the same dialog's later
    /// `Notification` does NOT count it again.
    #[tokio::test]
    async fn a_permission_request_increments_the_class_count_once_per_dialog() {
        // A command token no other test uses, so this class's global bucket
        // is this test's alone.
        let command = "qzx7armingprobe $PROBE_EMPTY_VAR/";
        let payload = serde_json::json!({
            "session_id": "11111111-2222-3333-4444-555555555555",
            "cwd": "C:\\work\\repo",
            "prompt_id": "p1",
            "permission_mode": "bypassPermissions",
            "effort": {"level": "medium"},
            "hook_event_name": "PermissionRequest",
            "tool_name": "Qzx7EchoTool",
            "tool_input": {"command": command, "description": "probe"},
            "permission_suggestions": [],
        });
        let class = crate::session::permission_prompt_capture::parse_payload(&payload)
            .unwrap()
            .class;
        assert_eq!(
            class.class_key,
            "permission-prompt-qzx7echotool-qzx7armingprobe"
        );
        let before = class_count(&class.dossier_slug);

        let (status, body) = post_json(
            permission_request_app(),
            "/sessions/term-pr-arming/permission-request",
            payload.to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["recorded"], true, "{body}");
        assert_eq!(body["class_key"], class.class_key.as_str());
        assert_eq!(body["dossier_slug"], class.dossier_slug.as_str());
        assert_eq!(class_count(&class.dossier_slug), before + 1);
        let bucket = crate::session::permission_prompt_capture::snapshot()
            .into_iter()
            .find(|b| b.dossier_slug == class.dossier_slug)
            .unwrap();
        assert_eq!(bucket.safety_class(), Some(true));

        // The dialog's Notification echo on the same terminal is not counted.
        // Its own class would be `permission-prompt-qzx7echotool` — a tool no
        // other test names, so the global count cannot collide.
        let bash_only = crate::session::permission_prompt_capture::classify(
            Some("Claude needs your permission to use Qzx7EchoTool"),
            None,
            None,
        );
        let bash_before = class_count(&bash_only.dossier_slug);
        let (status, body) = post_json(
            notification_app(),
            "/sessions/term-pr-arming/notification",
            serde_json::json!({
                "hook_event_name": "Notification",
                "notification_type": "permission_prompt",
                "message": "Claude needs your permission to use Qzx7EchoTool",
            })
            .to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            class_count(&bash_only.dossier_slug),
            bash_before,
            "echo counted"
        );
        assert_eq!(class_count(&class.dossier_slug), before + 1);
    }

    /// POST `body` carrying `key` in the hook's request-key header.
    async fn post_keyed(
        app: Router,
        uri: &str,
        body: String,
        key: &str,
    ) -> (StatusCode, serde_json::Value) {
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header(crate::session::permission_accept::REQUEST_KEY_HEADER, key)
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn send(
        app: Router,
        method: &str,
        uri: &str,
        origin: Option<&str>,
        body: String,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(o) = origin {
            req = req.header("origin", o);
        }
        let resp = app
            .oneshot(req.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    /// A browser-borne body is never counted, and the read doors refuse a
    /// browser outright.
    #[tokio::test]
    async fn a_browser_origin_is_refused_by_every_permission_prompt_door() {
        let payload = serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Qzx7OriginTool",
            "tool_input": {"command": "rm -f x"},
        });
        let class = crate::session::permission_prompt_capture::parse_payload(&payload)
            .unwrap()
            .class;
        let before = class_count(&class.dossier_slug);
        let (status, body) = send(
            permission_request_app(),
            "POST",
            "/sessions/term-origin/permission-request",
            Some("https://evil.example"),
            payload.to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["recorded"], false);
        assert_eq!(
            class_count(&class.dossier_slug),
            before,
            "browser body counted"
        );

        // The Notification pad: the capture is skipped for a browser body,
        // the operator-touch answer is unchanged.
        let note = serde_json::json!({
            "hook_event_name": "Notification",
            "notification_type": "permission_prompt",
            "message": "Claude needs your permission to use Qzx7OriginNoteTool",
        });
        let note_class = crate::session::permission_prompt_capture::parse_payload(&note)
            .unwrap()
            .class;
        let note_before = class_count(&note_class.dossier_slug);
        let (status, body) = send(
            notification_app(),
            "POST",
            "/sessions/term-origin-note/notification",
            Some("https://evil.example"),
            note.to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["session"], "term-origin-note");
        assert_eq!(
            class_count(&note_class.dossier_slug),
            note_before,
            "browser Notification captured"
        );

        let classify_app = Router::new().route(
            "/permission-prompts/classify",
            post(classify_permission_prompt),
        );
        let (status, body) = send(
            classify_app,
            "POST",
            "/permission-prompts/classify",
            Some("http://localhost:3001"),
            "{}".to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["code"], "ORIGIN_REFUSED");

        let snap = || {
            Router::new().route(
                "/permission-prompts/snapshot",
                get(permission_prompts_snapshot),
            )
        };
        let (status, _) = send(
            snap(),
            "GET",
            "/permission-prompts/snapshot",
            Some("https://evil.example"),
            String::new(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    /// The snapshot door shows what the intake counted.
    #[tokio::test]
    async fn the_snapshot_route_shows_a_counted_class() {
        let payload = serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Qzx7SnapshotTool",
            "tool_input": {"command": "rmdir $X/"},
            "permission_suggestions": [],
        });
        let (status, _) = post_json(
            permission_request_app(),
            "/sessions/term-snapshot/permission-request",
            payload.to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = send(
            Router::new().route(
                "/permission-prompts/snapshot",
                get(permission_prompts_snapshot),
            ),
            "GET",
            "/permission-prompts/snapshot",
            None,
            String::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["dropped"].is_u64(), "{body}");
        let found = body["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["class_key"] == "permission-prompt-qzx7snapshottool-rmdir")
            .cloned()
            .unwrap_or_else(|| panic!("class not in snapshot: {body}"));
        assert!(found["count"].as_u64().unwrap() >= 1);
        assert_eq!(found["safety_class"], true);
    }

    #[tokio::test]
    async fn the_permission_request_route_answers_200_on_garbage() {
        for body in ["", "not json", "{}", "{\"hook_event_name\":\"Stop\"}"] {
            let (status, json) = post_json(
                permission_request_app(),
                "/sessions/term-garbage/permission-request",
                body.to_string(),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body:?} -> {json}");
            assert_eq!(json["recorded"], false);
            assert_eq!(json["session"], "term-garbage");
        }
    }

    // ---------------------------------------------------------------------
    // The decision (plan 2026-10-23-runner-sessions-stop-at-permission-
    // dialogs-so-accept-them-by-default-with-a-tenant-switch, Phase 2)
    // ---------------------------------------------------------------------

    /// A Phase 0-shaped `PermissionRequest` in manual mode, keyed on a tool no
    /// other test names.
    fn decision_payload(tool: &str, mode: &str) -> serde_json::Value {
        serde_json::json!({
            "session_id": "11111111-2222-3333-4444-555555555555",
            "cwd": "/w/repo",
            "prompt_id": "p1",
            "permission_mode": mode,
            "hook_event_name": "PermissionRequest",
            "tool_name": tool,
            "tool_input": {"command": "rmdir $PROBE_EMPTY_VAR/", "description": "probe"},
            "permission_suggestions": [],
        })
    }

    /// ARMING (gate): with the dial answered `on`, the pad records the capture
    /// AND answers `decision: allow`, and the accept count climbs. With it
    /// answered `off` the SAME prompt is still recorded but answered `none`,
    /// and nothing is counted (no test can produce an `allow` while the pin
    /// holds the dial off, so the count is exact there).
    #[tokio::test]
    async fn the_capture_is_recorded_before_the_decision_is_answered() {
        use crate::session::permission_accept::accept_count;
        let pin = crate::mcp::fleet_policy_poller::pin_permission_auto_accept_for_test();
        let payload = decision_payload("Qzx7DecideTool", "default");
        let class = crate::session::permission_prompt_capture::parse_payload(&payload)
            .unwrap()
            .class;

        // Each terminal is one THIS process spawned: it holds a grant.
        let nonce = crate::session::permission_accept::new_nonce();
        for term in ["term-decide-on", "term-decide-off", "term-decide-cold"] {
            crate::session::permission_accept::register_terminal_grant(term, nonce.clone(), None);
        }

        let key = crate::session::permission_accept::request_key_for(&nonce);

        pin.set("on");
        // Without the terminal's request key the SAME prompt is recorded but
        // answered none: no nonce is echoed and nothing is counted.
        let before_accepts = accept_count();
        let (status, body) = post_json(
            permission_request_app(),
            "/sessions/term-decide-on/permission-request",
            payload.to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["decision"], "none", "keyless: {body}");
        assert!(body.get("nonce").is_none(), "keyless: {body}");
        assert_eq!(
            body["decision_reason"],
            "request key does not match the terminal's grant"
        );
        assert_eq!(
            accept_count(),
            before_accepts,
            "a keyless post is not counted"
        );

        let before_count = class_count(&class.dossier_slug);
        let before_accepts = accept_count();
        let (status, body) = post_keyed(
            permission_request_app(),
            "/sessions/term-decide-on/permission-request",
            payload.to_string(),
            &key,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["decision"], "allow", "{body}");
        assert_eq!(body["nonce"], nonce.as_str(), "the allow echoes the nonce");
        assert_eq!(body["recorded"], true, "the capture rode the same request");
        assert_eq!(body["class_key"], class.class_key.as_str());
        assert_eq!(class_count(&class.dossier_slug), before_count + 1);
        assert!(accept_count() > before_accepts, "an allow is counted");

        pin.set("off");
        let before_accepts = accept_count();
        let (status, body) = post_keyed(
            permission_request_app(),
            "/sessions/term-decide-off/permission-request",
            payload.to_string(),
            &key,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["decision"], "none", "{body}");
        assert!(body.get("nonce").is_none(), "a none never echoes the nonce");
        assert_eq!(body["recorded"], true, "an off dial still records");
        assert_eq!(class_count(&class.dossier_slug), before_count + 2);
        assert_eq!(accept_count(), before_accepts, "a none is not counted");

        pin.set_unanswered();
        let (_, body) = post_keyed(
            permission_request_app(),
            "/sessions/term-decide-cold/permission-request",
            payload.to_string(),
            &key,
        )
        .await;
        assert_eq!(body["decision"], "none", "unanswered is none: {body}");
        assert_eq!(accept_count(), before_accepts);

        // The ORDER, pinned on the handler's source: the capture call comes
        // before the decision call, so a decision can never be answered for a
        // prompt that was not recorded.
        let src = include_str!("sessions.rs");
        let handler = src
            .split_once("async fn permission_request(")
            .expect("the pad handler is in this file")
            .1
            .split_once("\nfn accept_class_key_fallback(")
            .expect("the handler is followed by its fallback")
            .0;
        let capture_at = handler
            .find("observe_permission_request(&id, &body)")
            .expect("the pad records the capture");
        let decide_at = handler
            .find("permission_accept::decide(&id, presented_key, &body)")
            .expect("the pad decides");
        assert!(capture_at < decide_at, "the capture is recorded FIRST");
        for term in ["term-decide-on", "term-decide-off", "term-decide-cold"] {
            crate::session::permission_accept::forget_terminal_grant(term, &nonce);
        }
    }

    /// RISK 5: a terminal this process did not spawn (no grant) is never
    /// answered `allow`, even with the dial answered `on`.
    #[tokio::test]
    async fn an_unknown_terminal_is_answered_none() {
        let pin = crate::mcp::fleet_policy_poller::pin_permission_auto_accept_for_test();
        pin.set("on");
        let (status, body) = post_json(
            permission_request_app(),
            "/sessions/term-decide-nobody/permission-request",
            decision_payload("Qzx7NobodyTool", "default").to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["decision"], "none", "{body}");
        assert_eq!(
            body["decision_reason"],
            "terminal was not spawned by this runner process"
        );
        assert!(body.get("nonce").is_none());
    }

    /// ARMING (black box; compiles against the BASE 5571ce756 — only base-era
    /// symbols): the pad handler the hook's curl reaches answers a `decision`,
    /// and garbage is `none`. It CALLS the handler itself — the entry point the
    /// population uses — rather than a router built around it. The base pad
    /// answers no `decision` key, so this fails on the assertion there.
    #[tokio::test]
    async fn arming_the_pad_answers_decision_none_on_garbage() {
        let Json(body) = permission_request(
            Path("term-arming-decision".to_string()),
            axum::http::HeaderMap::new(),
            axum::body::Bytes::from_static(b"not json"),
        )
        .await;
        assert_eq!(body["decision"], "none", "{body}");
        assert_eq!(body["session"], "term-arming-decision", "{body}");
    }

    /// BUG 1 + RISK 5, end to end: the REAL bundled hook, the REAL curl and
    /// the REAL pad on a loopback listener. A payload over 1 MB (a `Write`
    /// carrying file content) is streamed by curl itself and still answered
    /// `allow`; the hook prints the envelope because the pad echoed THIS
    /// terminal's nonce. Over 2 MB proves the route's raised body limit.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_real_hook_gets_allow_from_the_real_pad_for_a_payload_over_one_megabyte() {
        const HOOK: &str =
            include_str!("../../resources/session-restore/claude_permission_request_hook.sh");
        const ALLOW: &str = r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#;
        let pin = crate::mcp::fleet_policy_poller::pin_permission_auto_accept_for_test();
        pin.set("on");
        let term = "term-e2e-large";
        let nonce = crate::session::permission_accept::new_nonce();
        crate::session::permission_accept::register_terminal_grant(term, nonce.clone(), None);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, permission_request_app()).await;
        });

        let tmp = tempfile::tempdir().unwrap();
        let hook = tmp.path().join("claude_permission_request_hook.sh");
        std::fs::write(&hook, HOOK).unwrap();
        for size in [1_200_000usize, 3_000_000] {
            let payload = serde_json::json!({
                "hook_event_name": "PermissionRequest",
                "permission_mode": "default",
                "tool_name": "Write",
                "tool_input": {"file_path": "/w/big.txt", "content": "a".repeat(size)},
            })
            .to_string();
            assert!(payload.len() > size);
            let payload_file = tmp.path().join(format!("payload-{size}.json"));
            std::fs::write(&payload_file, &payload).unwrap();
            let mut cmd = std::process::Command::new("bash");
            cmd.arg("-c")
                .arg(format!(
                    "exec bash '{}' < '{}'",
                    hook.display(),
                    payload_file.display()
                ))
                .env_clear()
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("QONTINUI_RUNNER_API_PORT", port.to_string())
                .env("QONTINUI_TERMINAL_ID", term)
                .env(crate::session::permission_accept::NONCE_ENV, &nonce)
                .env(
                    crate::session::permission_accept::REQUEST_KEY_ENV,
                    crate::session::permission_accept::request_key_for(&nonce),
                );
            let out = tokio::task::spawn_blocking(move || {
                crate::process_helpers::output_with_timeout(cmd, std::time::Duration::from_secs(30))
            })
            .await
            .unwrap()
            .expect("the hook runs inside its budget");
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                format!("{ALLOW}\n"),
                "a {size}-byte payload must still be answered allow; stderr: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        server.abort();
        crate::session::permission_accept::forget_terminal_grant(term, &nonce);
        drop(pin);
    }

    /// The pad stays lenient: garbage and a browser `Origin` answer 200 with
    /// `decision: none`, even with the dial answered `on`.
    #[tokio::test]
    async fn the_pad_answers_none_on_garbage_and_on_a_browser_origin() {
        let pin = crate::mcp::fleet_policy_poller::pin_permission_auto_accept_for_test();
        pin.set("on");
        // Both terminals hold a grant and every post carries the right key, so
        // ONLY the payload can make these `none` (without a grant the grant
        // check alone would answer none, and the D3 cases would prove nothing).
        let nonce = crate::session::permission_accept::new_nonce();
        let key = crate::session::permission_accept::request_key_for(&nonce);
        for term in ["term-decide-garbage", "term-decide-never"] {
            crate::session::permission_accept::register_terminal_grant(term, nonce.clone(), None);
        }
        for body in ["", "not json", "{}", "[]", "{\"hook_event_name\":\"Stop\"}"] {
            let (status, json) = post_keyed(
                permission_request_app(),
                "/sessions/term-decide-garbage/permission-request",
                body.to_string(),
                &key,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body:?} -> {json}");
            assert_eq!(json["decision"], "none", "{body:?} -> {json}");
        }
        let (status, json) = send(
            permission_request_app(),
            "POST",
            "/sessions/term-decide-origin/permission-request",
            Some("https://evil.example"),
            decision_payload("Qzx7DecideOriginTool", "default").to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["decision"], "none", "{json}");
        assert_eq!(json["recorded"], false, "{json}");

        // The never-answered classes travel through the pad too.
        let mut absent_mode = decision_payload("Bash", "default");
        absent_mode
            .as_object_mut()
            .unwrap()
            .remove("permission_mode");
        for payload in [
            decision_payload("AskUserQuestion", "default"),
            decision_payload("ExitPlanMode", "plan"),
            decision_payload("Bash", "plan"),
            decision_payload("Bash", "dontAsk"),
            absent_mode,
        ] {
            let (_, json) = post_keyed(
                permission_request_app(),
                "/sessions/term-decide-never/permission-request",
                payload.to_string(),
                &key,
            )
            .await;
            assert_eq!(json["decision"], "none", "{payload} -> {json}");
            assert_eq!(
                json["decision_reason"], "verdict none",
                "{payload} -> {json}"
            );
        }
        // The control: the same terminal, key and dial answer an ordinary
        // prompt allow, so the `none`s above came from the payload alone.
        let (_, json) = post_keyed(
            permission_request_app(),
            "/sessions/term-decide-never/permission-request",
            decision_payload("Qzx7NeverControlTool", "default").to_string(),
            &key,
        )
        .await;
        assert_eq!(json["decision"], "allow", "control: {json}");
        for term in ["term-decide-garbage", "term-decide-never"] {
            crate::session::permission_accept::forget_terminal_grant(term, &nonce);
        }
    }

    /// The allow log carries the class key; when the capture did not classify
    /// the payload, the same classifier names it from the tool + command.
    #[test]
    fn the_fallback_class_key_is_the_capture_classifier() {
        let body = serde_json::json!({
            "tool_name": "Bash",
            "permission_mode": "default",
            "tool_input": {"command": "rmdir $X/ --secret-arg"},
        })
        .to_string();
        let key = accept_class_key_fallback(body.as_bytes());
        assert_eq!(key, "permission-prompt-bash-rmdir");
        assert!(!key.contains("secret"), "never the command's arguments");
    }

    /// ARMING (detect_deliver): a `permission_prompt` Notification posted to
    /// `/sessions/{id}/notification` increments the aggregator's class count.
    #[tokio::test]
    async fn a_permission_prompt_notification_increments_the_class_count() {
        // A reason no other test uses, so the global aggregator's bucket for
        // this class is this test's alone.
        let message = "Arming probe qzx7 capture reason for the notification route: $v in `rm $v`";
        let class = crate::session::permission_prompt_capture::classify(Some(message), None, None);
        let before = class_count(&class.dossier_slug);
        let payload = serde_json::json!({
            "hook_event_name": "Notification",
            "notification_type": "permission_prompt",
            "message": message,
            "session_id": "11111111-2222-3333-4444-555555555555",
            "cwd": "/home/u/ws/repo",
        });
        let (status, body) = post_json(
            notification_app(),
            "/sessions/term-arming/notification",
            payload.to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        // The operator-touch half answers as before (no app handle in a test).
        assert_eq!(body["session"], "term-arming");
        assert_eq!(body["recorded"], false);
        assert_eq!(class_count(&class.dossier_slug), before + 1);

        // A second prompt of the same class with a different variable counts
        // in the SAME class. Another terminal: on the same one it would be
        // inside the dedup window and read as the same dialog.
        let variant = payload.to_string().replace("$v", "$other");
        let _ = post_json(
            notification_app(),
            "/sessions/term-arming-2/notification",
            variant,
        )
        .await;
        assert_eq!(class_count(&class.dossier_slug), before + 2);
    }

    #[tokio::test]
    async fn the_notification_route_still_answers_200_on_garbage() {
        for body in ["", "not json", "{\"notification_type\":\"idle_prompt\"}"] {
            let (status, json) = post_json(
                notification_app(),
                "/sessions/term-garbage/notification",
                body.to_string(),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body:?} -> {json}");
            assert_eq!(json["session"], "term-garbage");
        }
    }

    #[tokio::test]
    async fn the_classify_route_matches_the_classifier_and_records_nothing() {
        let app = || {
            Router::new().route(
                "/permission-prompts/classify",
                post(classify_permission_prompt),
            )
        };
        let text = "Dangerous rm operation on possibly-empty variable path: $f in `rm -f $f`";
        let want = crate::session::permission_prompt_capture::classify(Some(text), None, None);
        let before = class_count(&want.dossier_slug);
        let (status, body) = post_json(
            app(),
            "/permission-prompts/classify",
            serde_json::json!({"text": text}).to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["class_key"], want.class_key);
        assert_eq!(body["dossier_slug"], want.dossier_slug);
        assert_eq!(body["redacted"], text, "nothing secret to redact here");

        // A paste carrying its own parsed command: the command line is not
        // taken for the reason, and the response text is runner-redacted.
        let paste = format!(
            "TOKEN={} rmdir $X/\n\nClaude needs your permission to use Bash",
            "ghp_zzz"
        );
        let (_, body) = post_json(
            app(),
            "/permission-prompts/classify",
            serde_json::json!({"text": paste, "tool_name": "Bash", "command": "rmdir $X/"})
                .to_string(),
        )
        .await;
        assert_eq!(body["class_key"], "permission-prompt-bash-rmdir", "{body}");
        assert!(
            !body["redacted"].as_str().unwrap().contains("ghp_zzz"),
            "{body}"
        );
        assert_eq!(class_count(&want.dossier_slug), before, "classify is pure");

        let (_, body) = post_json(
            app(),
            "/permission-prompts/classify",
            serde_json::json!({"tool_name": "Bash", "command": "rm -f x"}).to_string(),
        )
        .await;
        assert_eq!(body["class_key"], "permission-prompt-bash-rm");

        let (status, body) =
            post_json(app(), "/permission-prompts/classify", "garbage".to_string()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["class_key"], "permission-prompt-unknown");
    }

    /// The tests above mount the handlers themselves, so a tree-wide route
    /// scan would find these paths in THIS module and prove nothing. Read the
    /// body of `routes()` alone instead.
    #[test]
    fn both_routes_are_registered_by_routes() {
        let src = include_str!("sessions.rs");
        let (_, from_routes) = src
            .split_once("pub fn routes() -> Router<Arc<ApiState>> {")
            .expect("routes() is in this file");
        let (body, _) = from_routes.split_once("\n}\n").expect("routes() closes");
        for (path, handler) in [
            (
                "\"/sessions/{id}/permission-request\"",
                "post(permission_request)",
            ),
            (
                "\"/sessions/{id}/notification\"",
                "post(operator_touch_notification)",
            ),
            (
                "\"/permission-prompts/classify\"",
                "post(classify_permission_prompt)",
            ),
            (
                "\"/permission-prompts/snapshot\"",
                "get(permission_prompts_snapshot)",
            ),
        ] {
            let (_, tail) = body
                .split_once(path)
                .unwrap_or_else(|| panic!("{path} not in routes()"));
            let registration = tail.split(".route(").next().unwrap_or(tail);
            assert!(
                registration.contains(handler),
                "{path} not routed to {handler}"
            );
        }
    }
}
