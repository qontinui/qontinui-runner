//! AI Session Management
//!
//! Handles AI analysis session lifecycle: starting, stopping, and monitoring
//! AI-powered sessions. Includes prompt execution, task completion tracking,
//! log migration, and MCP tool context generation.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tracing::{error, info, warn};

use crate::context;
use crate::database::CreateTaskRunInput;
use crate::mcp::shared::{
    emit_ai_output, get_workspace_paths_internal, spawn_python_with_console, FINDING_INSTRUCTIONS,
};
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::prompts;
use crate::safe_lock::safe_lock_or_recover;
use crate::settings;
use qontinui_types::scheduler::McpConnectionRef;

// Re-export AiSessionContext from the canonical location
pub use crate::execution_context::AiSessionContext;
use crate::runtime_env::{AiSessionContextExt, ExecutionContextExt};

// ============================================================================
// Inline Python Execution Types
// ============================================================================

/// Request to execute inline Python code
#[derive(Debug, Deserialize)]
pub struct InlinePythonRequest {
    /// Python code to execute
    pub code: String,
    /// Optional pip packages to install (uses uvx for isolation)
    #[serde(default)]
    pub dependencies: Option<Vec<String>>,
    /// Execution timeout in seconds (default: 30)
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// Working directory for execution (default: temp dir)
    #[serde(default)]
    pub working_directory: Option<String>,
}

/// Response from inline Python execution
#[derive(Debug, Serialize)]
pub struct InlinePythonResponse {
    /// Whether execution succeeded (exit code 0)
    pub success: bool,
    /// Stdout from the script
    pub stdout: String,
    /// Stderr from the script
    pub stderr: String,
    /// Return value if the script returned JSON via __QONTINUI_RETURN__ marker
    pub return_value: Option<serde_json::Value>,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
}

/// Request to restart the runner (for AI self-healing workflow)
#[derive(Debug, Deserialize)]
pub struct RestartRunnerRequest {
    /// Reason for restart (logged for debugging)
    pub reason: String,
    /// Delay before restart in seconds (default: 3)
    #[serde(default)]
    pub delay_seconds: Option<u64>,
}

/// Request to run a prompt
#[derive(Debug, Deserialize)]
pub struct RunPromptRequest {
    // Mode 1: Lookup prompt from database
    /// Prompt ID to lookup from database (mutually exclusive with name+content)
    #[serde(default)]
    pub prompt_id: Option<String>,

    // Mode 2: Ad-hoc prompt (used by qontinui-web)
    /// Task name for display (required for ad-hoc mode)
    #[serde(default)]
    pub name: Option<String>,
    /// Prompt content (required for ad-hoc mode)
    #[serde(default)]
    pub content: Option<String>,

    // Common options
    /// Optional session_id override (auto-generated if not provided)
    #[serde(default)]
    pub session_id: Option<String>,
    /// Optional max_sessions override (uses prompt's setting if not provided)
    #[serde(default)]
    pub max_sessions: Option<u32>,

    // Image analysis options (for multimodal analysis)
    /// Image paths to include (screenshots, etc.) - for multimodal analysis
    #[serde(default)]
    pub image_paths: Option<Vec<String>>,
    /// Video paths to extract frames from
    #[serde(default)]
    pub video_paths: Option<Vec<String>>,
    /// Path to Playwright trace ZIP file (will extract timeline and screenshots)
    #[serde(default)]
    pub trace_path: Option<String>,
    /// Maximum number of frames to extract from each video (default: 3)
    #[serde(default)]
    pub max_video_frames: Option<usize>,
    /// Maximum number of screenshots to extract from trace (default: 5)
    #[serde(default)]
    pub max_trace_screenshots: Option<usize>,

    // Context injection options
    /// Context IDs to explicitly include in the prompt
    #[serde(default)]
    pub context_ids: Option<Vec<String>>,
    /// Whether to auto-detect and include relevant contexts (default: false)
    #[serde(default)]
    pub auto_include_contexts: Option<bool>,

    // RemoteAgent / scheduler ad-hoc options (Phase D — scheduler reliability plan).
    // These plumb through to the Claude CLI invocation inside
    // spawn-independent-claude.py via --working-directory / --model /
    // --allowed-tools / --max-turns flags. `mcp_connections` is captured into
    // the prompt header for now (no MCP-config-merge wiring yet — see Phase D
    // notes in tmp_scheduler_reliability_plan.md).
    /// Working directory for the spawned Claude CLI session.
    /// `None` = runner's project root (default behavior).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Optional model override (e.g. "claude-sonnet-4-6", "sonnet", "opus").
    /// `None` = Claude CLI default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Comma/space-separated tool allow-list passed via `--allowed-tools`.
    /// `None` = inherit Claude CLI default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    /// Hard cap on Claude turns (`--max-turns`). `None` = no flag (CLI
    /// default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// MCP connection refs (resolved against runner's MCP config at dispatch
    /// time). For Phase D this is documented in the prompt header — actual
    /// per-call MCP-config merging is not yet wired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_connections: Option<Vec<McpConnectionRef>>,
}

/// Response from running a prompt
#[derive(Debug, Serialize)]
pub struct RunPromptResponse {
    pub task_run_id: String,
    pub session_id: String,
    /// Backward compatibility alias for task_run_id
    pub action_id: String,
    pub state_file: String,
    pub log_file: String,
    pub pid: Option<u32>,
}

// ============================================================================
// Idle-status (Phase 1 of stuck-session-heartbeat-plan.md)
// ============================================================================

/// Per-session idle stats returned by `GET /sessions/idle-status`.
///
/// The frontend join (`useFileLockTracking.ts`, Phase 2) keys the
/// existing `/file-locks/info` entries on `holder_task_run_id` →
/// `task_run_id` here, then attaches `idle_ms` to each waiter's
/// `LockState` so the UI can render e.g. "(holder idle 7m)".
///
/// `holder_name` is the same friendly display name the file-lock
/// dispatcher emits on `file-lock-*` events (see
/// `claude_session/dispatcher.rs:398-404` and
/// `ClaudeSession::holder_name`).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SessionIdleEntry {
    pub(crate) task_run_id: String,
    pub(crate) holder_name: String,
    /// Epoch milliseconds of the last observed stdout line.
    pub(crate) last_activity_ms: u64,
    /// `now_ms.saturating_sub(last_activity_ms)`. Clock skew or a stale
    /// `last_activity` value from the future is clamped to 0 by the
    /// saturating subtraction.
    pub(crate) idle_ms: u64,
}

/// Compute idle entries from a `SessionManager` snapshot at the given
/// `now_ms`. Pure helper extracted for testability — the HTTP handler
/// is a thin shim over this plus `SystemTime::now()`.
///
/// The atomic stores epoch SECONDS (see
/// `claude_session/session.rs:420`); we multiply by 1000 at this
/// boundary so the response is in milliseconds, matching every other
/// `*_ms` field the frontend consumes.
pub(crate) fn build_idle_entries(
    snapshot: Vec<(String, String, Arc<std::sync::atomic::AtomicU64>)>,
    now_ms: u64,
) -> Vec<SessionIdleEntry> {
    snapshot
        .into_iter()
        .map(|(task_run_id, holder_name, tracker)| {
            let last_activity_s = tracker.load(std::sync::atomic::Ordering::Relaxed);
            let last_activity_ms = last_activity_s.saturating_mul(1000);
            let idle_ms = now_ms.saturating_sub(last_activity_ms);
            SessionIdleEntry {
                task_run_id,
                holder_name,
                last_activity_ms,
                idle_ms,
            }
        })
        .collect()
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `GET /sessions/idle-status` — return per-session idle stats for
/// every currently-registered `ClaudeSession`.
///
/// Returns an empty array when no AI sessions are registered, or when
/// `SessionManager` is not available in Tauri state (which would only
/// happen during early startup). PTY workers and inline-PID
/// registrations are intentionally excluded — see
/// `SessionManager::snapshot` for the rationale.
pub async fn idle_status(State(state): State<Arc<ApiState>>) -> Json<Vec<SessionIdleEntry>> {
    use crate::claude_session::manager::SessionManager;

    let snapshot = state
        .app_handle
        .try_state::<Arc<SessionManager>>()
        .map(|s| s.inner().snapshot())
        .unwrap_or_default();

    Json(build_idle_entries(snapshot, now_epoch_ms()))
}

// ============================================================================
// Token freshness introspection (`GET /auth/freshness`)
// ============================================================================

/// Response for `GET /auth/freshness` — token staleness as *deltas from now*,
/// never the tokens or any absolute secret.
///
/// All deltas are seconds-from-now (negative = already expired). `None` means
/// the corresponding token is absent (no Cognito session, or no decodable
/// device-JWT). This lets an operator ask a running runner "how stale are
/// your tokens?" over HTTP without decrypting `auth_tokens.enc` out-of-band.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FreshnessResponse {
    /// The device-JWT/access-token `exp` minus now (seconds). `None` when the
    /// `access_token` slot is empty or holds a non-decodable (legacy opaque)
    /// bearer.
    pub access_token_exp_in_s: Option<i64>,
    /// The Cognito `oauth_expires_at` minus now (seconds). `None` when no
    /// Cognito session is present.
    pub oauth_expires_in_s: Option<i64>,
    /// Whether this runner is paired (a `paired_user.json` exists on disk).
    pub paired: bool,
}

/// Pure delta computation, extracted so it can be unit-tested without disk
/// I/O. Converts absolute unix-second expiries into seconds-from-`now`
/// deltas; `None` inputs pass through as `None`.
fn compute_freshness_deltas(
    access_token_exp: Option<i64>,
    oauth_expires_at: Option<i64>,
    now: i64,
    paired: bool,
) -> FreshnessResponse {
    FreshnessResponse {
        access_token_exp_in_s: access_token_exp.map(|exp| exp - now),
        oauth_expires_in_s: oauth_expires_at.map(|exp| exp - now),
        paired,
    }
}

/// `GET /auth/freshness` — local-only token-freshness introspection.
///
/// Returns expiry deltas for the device-JWT (`access_token` slot) and the
/// Cognito access token, plus whether the runner is paired. NEVER returns
/// tokens or absolute secrets — only seconds-from-now deltas. This is a
/// top-level local route (outside the `/ui-bridge/*` family), reachable only
/// on the runner's local server.
pub async fn auth_freshness(State(_state): State<Arc<ApiState>>) -> Json<FreshnessResponse> {
    let auth_manager = crate::auth::AuthManager::new();
    let now = chrono::Utc::now().timestamp();
    let paired = qontinui_runner_lib::pair::read_paired_user_id_from_disk().is_some();
    Json(compute_freshness_deltas(
        auth_manager.access_token_exp(),
        auth_manager.oauth_expires_at(),
        now,
        paired,
    ))
}

// ============================================================================
// Routes
// ============================================================================

/// Build the router for AI session management endpoints.
///
/// The `.without_v07_checks()` call works around an axum 0.8.8 panic on
/// the `/sessions/{session_id}/...` route: axum's v0.7-syntax detector
/// fires even though the capture uses correct `{name}` syntax, claiming
/// "Path segments must not start with `:`". The panic site is the route
/// call itself (no matter the surrounding routes), and the docs of the
/// panic message itself recommend this exact bypass for the false
/// positive. All other axum 0.8 path features still work.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .without_v07_checks()
        .route("/stop-ai-analysis", post(stop_ai_analysis))
        .route("/restart-runner", post(restart_runner))
        .route("/prompts/run", post(run_prompt))
        .route("/sessions/idle-status", get(idle_status))
        .route("/auth/freshness", get(auth_freshness))
        .route(
            "/sessions/{session_id}/promote-to-worktree",
            post(promote_session_to_worktree_handler),
        )
        .route(
            "/sessions/{session_id}/commit-progress",
            post(commit_session_progress_handler),
        )
}

// ============================================================================
// Worktree Promotion (Phase 4)
// ============================================================================

/// Response from a successful session-to-worktree promotion.
///
/// Returned by both the MCP HTTP handler (`POST /sessions/:id/promote-to-worktree`)
/// and the Tauri command (`promote_session_to_worktree`). Mirrors the fields of
/// `claude_session::WorktreeInfo` but with stringified paths so it serialises cleanly.
#[derive(Debug, Clone, Serialize)]
pub struct PromoteToWorktreeResponse {
    pub worktree_id: String,
    pub worktree_path: String,
    pub branch_name: String,
}

/// MCP HTTP handler: promote a running session to its own git worktree.
///
/// `POST /sessions/{session_id}/promote-to-worktree`
///
/// Looks up the live session via `SessionManager`, takes exclusive ownership of
/// the underlying `ClaudeSession` (since `promote_to_worktree` requires `&mut self`),
/// runs the promotion, and re-registers the new session under the same id.
///
/// Status codes:
/// - 200 — promotion succeeded
/// - 404 — no live session for that id
/// - 409 — session is already in a worktree, or another holder is preventing
///   exclusive access (e.g. concurrent caller)
/// - 500 — worktree creation, respawn, or PG persistence failed
pub async fn promote_session_to_worktree_handler(
    State(state): State<Arc<ApiState>>,
    Path(session_id): Path<String>,
) -> Result<Json<ApiResponse<PromoteToWorktreeResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: promote_to_worktree requested for session_id={}",
        session_id
    );

    promote_session_inner(&state.app_handle, &session_id)
        .await
        .map(|resp| Json(ApiResponse::success(resp)))
        .map_err(|(status, message)| (status, Json(api_error(message))))
}

/// Shared implementation behind the MCP handler and the Tauri command.
///
/// Returns `(StatusCode, error_message)` on failure so each frontend can map it
/// to the appropriate response shape (HTTP status vs `Result<_, String>`).
pub(crate) async fn promote_session_inner(
    app_handle: &tauri::AppHandle,
    session_id: &str,
) -> Result<PromoteToWorktreeResponse, (StatusCode, String)> {
    use crate::claude_session::manager::SessionManager;
    use crate::claude_session::ClaudeSession;

    // 1. Resolve the SessionManager from Tauri's managed state.
    let session_manager: Arc<SessionManager> = app_handle
        .try_state::<Arc<SessionManager>>()
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "SessionManager not available".to_string(),
            )
        })?
        .inner()
        .clone();

    // 1b. Best-effort handle to PgDb for the "Coord as Deconflicter" Phase 1
    //     emergent-task creation (§4.3). Failing to resolve AppState here
    //     must NOT block promote — every emergent-task call below is
    //     wrapped in a `.ok()` / matched `Err` so the worktree-promotion
    //     path still works against a partially-initialised runner.
    let pg_db_opt: Option<Arc<crate::database::pg::PgDb>> = {
        use crate::commands::AppState;
        app_handle
            .try_state::<Arc<AppState>>()
            .map(|s| s.inner().pg_db.clone())
    };

    // 2. Resolve repo path. `current_project_path()` returns the workspace root,
    //    which is what create_worktree expects (per worktrees.rs handlers).
    let repo_path = crate::mcp::shared::current_project_path().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "No project path available".to_string(),
    ))?;

    // 3. Take ownership of the session out of the manager. promote_to_worktree
    //    needs &mut self, but the manager hands out Arc<ClaudeSession>. Removing
    //    + try_unwrap is the only safe way to obtain exclusive ownership.
    let session_arc = session_manager.remove(session_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("No active session found for session_id: {}", session_id),
        )
    })?;

    // Already-promoted check: cheap, no need to unwrap the Arc to discover this.
    if let Some(existing) = session_arc.worktree() {
        let resp = PromoteToWorktreeResponse {
            worktree_id: existing.id.clone(),
            worktree_path: existing.path.to_string_lossy().to_string(),
            branch_name: existing.branch.clone(),
        };
        // Put it back so the frontend can keep using it.
        if let Err(e) = session_manager.register(session_id, session_arc) {
            warn!(
                "promote_to_worktree: failed to re-register already-promoted session {}: {}",
                session_id, e
            );
        }
        // §4.3: best-effort emergent-task row so the in-session advisory
        // banner has something to attach to. Idempotent via partial unique
        // index.
        if let Some(pg) = pg_db_opt.as_ref() {
            if let Err(e) = pg
                .create_emergent_task(session_id, "in_progress", "session_emergent", None)
                .await
            {
                warn!(
                    "promote_to_worktree: create_emergent_task failed for session {}: {}",
                    session_id, e
                );
            }
        }
        return Err((
            StatusCode::CONFLICT,
            format!(
                "Session {} is already in worktree {} (branch={})",
                session_id, resp.worktree_id, resp.branch_name
            ),
        ));
    }

    // 4. Try to acquire exclusive ownership. If another caller holds an Arc
    //    clone (e.g. a concurrent send_user_message), this fails — return 409
    //    so the caller can retry once the other holder releases.
    let mut session: ClaudeSession = match Arc::try_unwrap(session_arc) {
        Ok(session) => session,
        Err(arc) => {
            // Put it back so the frontend can keep using it.
            if let Err(e) = session_manager.register(session_id, arc) {
                warn!(
                    "promote_to_worktree: failed to re-register busy session {}: {}",
                    session_id, e
                );
            }
            // §4.3: best-effort emergent-task row. Idempotent.
            if let Some(pg) = pg_db_opt.as_ref() {
                if let Err(e) = pg
                    .create_emergent_task(session_id, "in_progress", "session_emergent", None)
                    .await
                {
                    warn!(
                        "promote_to_worktree: create_emergent_task failed for session {}: {}",
                        session_id, e
                    );
                }
            }
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "Session {} is busy — another caller holds a reference. Retry shortly.",
                    session_id
                ),
            ));
        }
    };

    // 5. Reconstruct AiSessionContext for the new spawn so output events keep
    //    flowing with the right task_run_id. Mirrors create_ai_session's setup
    //    path (which is what regular interactive sessions use).
    let session_ctx = AiSessionContext::setup(session_id, session_id);

    // 6. Run the promotion. promote_to_worktree handles state transitions,
    //    git worktree creation, PG persistence, kill+respawn, and replay.
    let promote_result = session
        .promote_to_worktree(
            std::path::Path::new(&repo_path),
            app_handle,
            Some(session_ctx),
        )
        .await;

    let info = match promote_result {
        Ok(info) => info,
        Err(e) => {
            // The session is back in Ready state (transition rolled back inside
            // promote_to_worktree on early failure paths). Re-register so the
            // caller can keep using it as before.
            warn!(
                "promote_to_worktree: failed for session {}: {}",
                session_id, e
            );
            let session_arc = Arc::new(session);
            if let Err(re) = session_manager.register(session_id, session_arc) {
                warn!(
                    "promote_to_worktree: failed to re-register after promote failure {}: {}",
                    session_id, re
                );
            }
            // §4.3: best-effort emergent-task row. Idempotent.
            if let Some(pg) = pg_db_opt.as_ref() {
                if let Err(ce) = pg
                    .create_emergent_task(session_id, "in_progress", "session_emergent", None)
                    .await
                {
                    warn!(
                        "promote_to_worktree: create_emergent_task failed for session {}: {}",
                        session_id, ce
                    );
                }
            }
            // Map "already promoted" race to 409, anything else to 500.
            let status = if e.contains("already") {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            return Err((status, format!("worktree promotion failed: {}", e)));
        }
    };

    // 7. Re-register the (now-mutated) session under the same id.
    let session_arc = Arc::new(session);
    if let Err(e) = session_manager.register(session_id, session_arc) {
        warn!(
            "promote_to_worktree: post-promotion register failed for {}: {}",
            session_id, e
        );
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("worktree promoted but re-register failed: {}", e),
        ));
    }
    // §4.3: best-effort emergent-task row. Idempotent.
    if let Some(pg) = pg_db_opt.as_ref() {
        if let Err(e) = pg
            .create_emergent_task(session_id, "in_progress", "session_emergent", None)
            .await
        {
            warn!(
                "promote_to_worktree: create_emergent_task failed for session {}: {}",
                session_id, e
            );
        }
    }

    info!(
        "promote_to_worktree: session {} now in worktree {} (branch={}, path={})",
        session_id,
        info.id,
        info.branch,
        info.path.display()
    );

    Ok(PromoteToWorktreeResponse {
        worktree_id: info.id,
        worktree_path: info.path.to_string_lossy().to_string(),
        branch_name: info.branch,
    })
}

// ============================================================================
// Commit Progress (Phase D)
// ============================================================================

/// Response from a successful (or no-op) `commit_session_progress_inner` call.
///
/// Mirrors `PromoteToWorktreeResponse` in shape: returned by both the MCP HTTP
/// handler (`POST /sessions/{id}/commit-progress`) and the Tauri command
/// (`commit_session_progress`). `commit_hash` is `None` when nothing actually
/// landed (empty file-set, or files matched HEAD exactly).
#[derive(Debug, Clone, Serialize)]
pub struct CommitProgressResponse {
    /// New HEAD SHA on success; `None` when no commit was created (empty
    /// tracker, or staging produced no diff vs HEAD).
    pub commit_hash: Option<String>,
    /// Number of files in the tracker at commit time. May be 0 when nothing
    /// was tracked.
    pub file_count: usize,
    /// Branch HEAD pointed at in `cwd` at commit time. Worktree branch if the
    /// session was promoted, else the user's current branch.
    pub branch: String,
    /// The commit message that was used (or would have been, on no-op).
    pub message: String,
}

/// MCP HTTP handler: commit a session's accumulated file-set to its cwd's
/// current branch.
///
/// `POST /sessions/{session_id}/commit-progress`
///
/// Looks up the live session via `SessionManager`, reads its tracked file-set
/// from PG (`session_touched_files`), runs `auto_commit::commit_files` against
/// the session's cwd (worktree path if promoted, repo root otherwise), and on
/// success clears the tracker so the next call only sees freshly-touched
/// files.
///
/// Status codes:
/// - 200 — commit succeeded (may be a no-op with `commit_hash: None`)
/// - 404 — no live session for that id
/// - 500 — git/branch lookup failed, commit failed, or PG unavailable
pub async fn commit_session_progress_handler(
    State(state): State<Arc<ApiState>>,
    Path(session_id): Path<String>,
) -> Result<Json<ApiResponse<CommitProgressResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: commit_session_progress requested for session_id={}",
        session_id
    );

    commit_session_progress_inner(&state.app_handle, &session_id)
        .await
        .map(|resp| Json(ApiResponse::success(resp)))
        .map_err(|(status, message)| (status, Json(api_error(message))))
}

/// Shared implementation behind the MCP handler and the Tauri command.
///
/// Returns `(StatusCode, error_message)` on failure so each frontend can map
/// it to its native response shape.
///
/// Behaviour:
/// 1. Resolve `SessionManager` and the session's cwd (worktree path if
///    promoted, else `current_project_path()`).
/// 2. Read tracked files from PG. Empty list short-circuits to a successful
///    no-op (`commit_hash: None`, `file_count: 0`).
/// 3. Build a default commit message: `"session-progress({id}): {N} files at
///    {RFC3339-ts}"`.
/// 4. Compute the current HEAD branch name via `git rev-parse --abbrev-ref
///    HEAD` (so the response can show "Committed to {branch}").
/// 5. Call `auto_commit::commit_files`. On success or no-op, clear the
///    tracker. On error, leave the tracker so a retry can see the same
///    file-set.
pub(crate) async fn commit_session_progress_inner(
    app_handle: &tauri::AppHandle,
    session_id: &str,
) -> Result<CommitProgressResponse, (StatusCode, String)> {
    use crate::auto_commit::{commit_files, CommitOutcome};
    use crate::claude_session::manager::SessionManager;
    use crate::commands::AppState;

    // 1. Resolve the SessionManager from Tauri's managed state.
    let session_manager: Arc<SessionManager> = app_handle
        .try_state::<Arc<SessionManager>>()
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "SessionManager not available".to_string(),
            )
        })?
        .inner()
        .clone();

    // 2. Resolve AppState (for pg_db). Available everywhere the runner is
    //    fully initialised; failure here is an internal error.
    let app_state: Arc<AppState> = app_handle
        .try_state::<Arc<AppState>>()
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "AppState not available".to_string(),
            )
        })?
        .inner()
        .clone();

    // 3. Look up the live session. We only need a read view, so the Arc clone
    //    is enough — no `try_unwrap` dance needed (unlike promote, which
    //    requires `&mut self`).
    let session = session_manager.get(session_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("No active session for session_id: {}", session_id),
        )
    })?;

    // 4. Determine cwd. Promoted sessions commit to the worktree branch;
    //    un-promoted sessions commit to whatever HEAD the runner's repo root
    //    is pointing at (the user's current branch).
    let cwd: std::path::PathBuf = if let Some(wt) = session.worktree() {
        wt.path.clone()
    } else {
        let repo = crate::mcp::shared::current_project_path().ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "No project path available".to_string(),
        ))?;
        std::path::PathBuf::from(repo)
    };

    // 5. Read the tracker. The session's session_id IS the task_run_id for
    //    live PTYs in this runner (see Phase 4 worktree-promotion notes).
    let pg = app_state.pg_db.clone();
    let files = pg.get_files_touched(session_id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to read touched-files tracker: {}", e),
        )
    })?;

    // 6. Resolve the current HEAD branch in cwd. Used both for the response
    //    and for log/UI clarity. `--abbrev-ref HEAD` returns "HEAD" on
    //    detached-HEAD, which is fine — we surface it as-is.
    let branch = crate::worktree::run_git_command(&cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to read current branch in {}: {}", cwd.display(), e),
            )
        })?;

    // 7. Build the default commit message. Includes the session id so users
    //    browsing `git log` can identify which session produced which commit.
    //    (ClaudeSession doesn't currently expose a separate "display name" —
    //    `session_id` is what the chat UI uses as the stable handle, and is
    //    what task_runs.task_name was created from at spawn time.)
    let message = format!(
        "session-progress({}): {} files at {}",
        session_id,
        files.len(),
        chrono::Utc::now().to_rfc3339(),
    );

    // 8. Empty tracker → no-op success. Skip the commit_files call entirely
    //    so we don't spam logs with "auto_commit: empty file list".
    if files.is_empty() {
        info!(
            "commit_session_progress: session {} has no tracked files; returning no-op",
            session_id
        );
        return Ok(CommitProgressResponse {
            commit_hash: None,
            file_count: 0,
            branch,
            message,
        });
    }

    // 9. Run the commit. On success or no-op, clear the tracker (best-effort:
    //    a PG failure here doesn't roll back the commit — we just warn).
    match commit_files(&cwd, &files, &message).await {
        Ok(CommitOutcome::Committed { hash }) => {
            if let Err(e) = pg.clear_files_touched(session_id).await {
                warn!(
                    "commit_session_progress: clear_files_touched failed for {} after commit: {}",
                    session_id, e
                );
            }
            info!(
                "commit_session_progress: session {} -> {} ({} files, branch={})",
                session_id,
                hash,
                files.len(),
                branch
            );
            // Tracker just got cleared → traffic light should flip to Empty.
            emit_commit_state_for_session(app_handle.clone(), session_id.to_string());
            Ok(CommitProgressResponse {
                commit_hash: Some(hash),
                file_count: files.len(),
                branch,
                message,
            })
        }
        Ok(CommitOutcome::NothingToCommit) => {
            // Files matched HEAD — clear so the tracker doesn't carry stale
            // entries forever. (If the user re-edits the same path later,
            // the dispatcher will re-register it.)
            if let Err(e) = pg.clear_files_touched(session_id).await {
                warn!(
                    "commit_session_progress: clear_files_touched failed for {} after no-op: {}",
                    session_id, e
                );
            }
            info!(
                "commit_session_progress: session {} no-op ({} tracked files matched HEAD)",
                session_id,
                files.len()
            );
            // Tracker cleared on the no-op path too (line above) — flip the
            // light to Empty for parity with the success branch.
            emit_commit_state_for_session(app_handle.clone(), session_id.to_string());
            Ok(CommitProgressResponse {
                commit_hash: None,
                file_count: files.len(),
                branch,
                message,
            })
        }
        Err(e) => {
            warn!(
                "commit_session_progress: commit_files failed for {} in {}: {}",
                session_id,
                cwd.display(),
                e
            );
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("commit failed: {}", e),
            ))
        }
    }
}

/// Probe the per-session "ready-to-commit" state without actually committing.
///
/// Sibling of `commit_session_progress_inner` — reuses its cwd-resolution +
/// tracker-read sequence, then runs the §1 dirty-subset query against the
/// touched-file set. Returns a `CommitState` describing whether the tracker
/// is empty / clean / dirty / mid-merge across one or more enclosing repos.
///
/// Used by:
/// 1. The Tauri command `get_session_commit_state` for frontend polling.
/// 2. The `commit-state-changed` event emitter that fires from
///    `auto_register_file`, the transcript-tail watcher, and post-commit
///    paths — see `emit_commit_state_for_session` below.
///
/// Errors are surfaced as `(StatusCode, String)` so callers can map them onto
/// the existing `CommandResponse { success: false, message }` shape used by
/// `commit_session_progress`.
pub(crate) async fn session_commit_state_inner(
    app_handle: &tauri::AppHandle,
    session_id: &str,
) -> Result<crate::git_status_subset::CommitState, (StatusCode, String)> {
    use crate::commands::AppState;
    use crate::git_status_subset::{
        bucket_by_repo, dirty_subset_in_repo, is_mid_merge, now_ms, CommitState, CommitStateStatus,
    };

    // 1. Resolve AppState (for pg_db). Same gating as
    //    `commit_session_progress_inner` step 2.
    let app_state: Arc<AppState> = app_handle
        .try_state::<Arc<AppState>>()
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "AppState not available".to_string(),
            )
        })?
        .inner()
        .clone();

    // 2. Read the tracker. The session's session_id IS the task_run_id for
    //    live PTYs (see `commit_session_progress_inner:527-528`). The PTY
    //    transcript watcher writes rows under the same id.
    let pg = app_state.pg_db.clone();
    let files = pg.get_files_touched(session_id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to read touched-files tracker: {}", e),
        )
    })?;

    if files.is_empty() {
        return Ok(CommitState {
            status: CommitStateStatus::Empty,
            touched_count: 0,
            dirty_count: 0,
            repo_roots: Vec::new(),
            merging_repos: Vec::new(),
            generated_at_ms: now_ms(),
        });
    }

    // 3. Bucket touched files by enclosing git toplevel. Files outside any
    //    repo are dropped silently. Empty buckets → also `Empty` for UI
    //    purposes (the user has nothing to commit).
    let buckets = bucket_by_repo(&files);
    if buckets.is_empty() {
        return Ok(CommitState {
            status: CommitStateStatus::Empty,
            touched_count: files.len(),
            dirty_count: 0,
            repo_roots: Vec::new(),
            merging_repos: Vec::new(),
            generated_at_ms: now_ms(),
        });
    }

    // 4. Probe each bucket for mid-merge state and dirty subset. Stable
    //    iteration order doesn't matter — repo_roots/merging_repos are
    //    advisory tooltip strings.
    let mut repo_roots: Vec<String> = Vec::with_capacity(buckets.len());
    let mut merging_repos: Vec<String> = Vec::new();
    let mut dirty: Vec<String> = Vec::new();

    for (repo, paths) in &buckets {
        let repo_str = repo.to_string_lossy().into_owned();
        repo_roots.push(repo_str.clone());

        if is_mid_merge(repo) {
            merging_repos.push(repo_str);
        }

        match dirty_subset_in_repo(repo, paths) {
            Ok(mut subset) => dirty.append(&mut subset),
            Err(e) => {
                warn!(
                    "session_commit_state: dirty_subset_in_repo failed for {}: {}",
                    repo.display(),
                    e
                );
            }
        }
    }

    // 5. Resolve final status. Merging precedence over Dirty (a mid-merge repo
    //    must be resolved manually — UI must disable the commit button).
    let status = if !merging_repos.is_empty() {
        CommitStateStatus::Merging
    } else if dirty.is_empty() {
        CommitStateStatus::Clean
    } else {
        CommitStateStatus::Dirty
    };

    Ok(CommitState {
        status,
        touched_count: files.len(),
        dirty_count: dirty.len(),
        repo_roots,
        merging_repos,
        generated_at_ms: now_ms(),
    })
}

/// Per-session debounce store for `emit_commit_state_for_session`. 500 ms
/// window prevents N consecutive `auto_register_file` hooks from firing N
/// commit-state probes when one batch (e.g. a MultiEdit-equivalent loop in a
/// single turn) just changed the same set.
static COMMIT_STATE_DEBOUNCE: once_cell::sync::OnceCell<
    dashmap::DashMap<String, std::time::Instant>,
> = once_cell::sync::OnceCell::new();

/// Per-session debounce window for commit-state-changed emits.
const COMMIT_STATE_DEBOUNCE_MS: u128 = 500;

/// Spawn a fire-and-forget task that probes `session_commit_state_inner` and
/// emits a `commit-state-changed` event on success.
///
/// Called by:
///   - `claude_session::dispatcher::auto_register_file` (after a successful
///     Edit/Write file-lock acquire) — SDK chat sessions.
///   - `terminal::transcript_watcher::tail_session` (after PG rows landed) —
///     PTY-launched terminal AI tabs.
///   - `commit_session_progress_inner` (after a commit / no-op return) — so
///     the UI sees the post-commit `Empty` state immediately rather than
///     waiting for the next 30 s frontend poll.
///
/// Per-session debounce of 500 ms applies — repeated calls within the window
/// are dropped on the floor (the debounce store remembers each session
/// independently).
///
/// Event payload shape (snake_case at the top level — must NOT be wrapped in
/// a `#[serde(rename_all = "camelCase")]` struct, or the TS frontend will
/// silently drop the event; see auto-memory entry
/// `proj_tauri_event_payload_camelcase.md`):
///
/// ```json
/// {
///   "type": "commit-state-changed",
///   "task_run_id": "<session_id>",
///   "state": { /* CommitState */ }
/// }
/// ```
///
/// The same payload is also broadcast through `AppState.event_broadcast` so
/// non-Tauri consumers (the MCP event channel, future remote dashboards) see
/// it. Mirrors the file-lock event pattern at `dispatcher.rs:440-441` and
/// `:460-461`.
pub fn emit_commit_state_for_session(app_handle: tauri::AppHandle, session_id: String) {
    use crate::commands::AppState;

    // Debounce: if this session emitted within the last 500 ms, drop.
    let store = COMMIT_STATE_DEBOUNCE.get_or_init(dashmap::DashMap::new);
    let now = std::time::Instant::now();
    if let Some(prev) = store.get(&session_id) {
        if now.duration_since(*prev).as_millis() < COMMIT_STATE_DEBOUNCE_MS {
            return;
        }
    }
    store.insert(session_id.clone(), now);

    tauri::async_runtime::spawn(async move {
        let state = match session_commit_state_inner(&app_handle, &session_id).await {
            Ok(s) => s,
            Err((status, msg)) => {
                warn!(
                    "emit_commit_state_for_session: probe failed for session {} ([{}] {})",
                    session_id, status, msg
                );
                return;
            }
        };

        // Build the payload with explicit keys — never via a
        // camelCase-renamed struct (see camelcase trap memo above).
        let payload = serde_json::json!({
            "type": "commit-state-changed",
            "task_run_id": session_id,
            "state": state,
        });

        if let Err(e) = app_handle.emit("commit-state-changed", &payload) {
            warn!(
                "emit_commit_state_for_session: app_handle.emit failed for {}: {}",
                session_id, e
            );
        }

        // Broadcast on the shared event channel so non-Tauri consumers see
        // it. Best-effort — receivers may be empty.
        if let Some(app_state) = app_handle.try_state::<Arc<AppState>>() {
            let _ = app_state.event_broadcast.send(payload);
        }
    });
}

// ============================================================================
// Handlers
// ============================================================================

/// Stop the currently running AI analysis
///
/// This endpoint stops all running tasks by:
/// 1. Killing all tracked AI process PIDs (the actual Claude CLI processes)
/// 2. Getting running task runs from the database
/// 3. Stopping monitoring for each task
/// 4. Marking tasks as stopped in the database
pub async fn stop_ai_analysis(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stop AI analysis requested");

    // First, kill all tracked AI processes immediately
    // This is the key fix - previously we only stopped monitoring, not the actual processes
    let pids_to_kill: Vec<u32> = {
        let mut pids = safe_lock_or_recover(&state.current_ai_pids, "current_ai_pids");
        let pids_copy = pids.clone();
        pids.clear(); // Clear the tracker
        pids_copy
    };

    let mut killed_count = 0;
    for pid in &pids_to_kill {
        info!("MCP API: Killing AI process PID {}", pid);
        // Use taskkill with /T to kill the entire process tree (cmd.exe spawns node.exe for claude)
        // /F forces termination, /T terminates child processes
        let result = crate::process_helpers::no_window("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();

        match result {
            Ok(output) => {
                if output.status.success() {
                    info!("MCP API: Successfully killed process tree for PID {}", pid);
                    killed_count += 1;
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    warn!(
                        "MCP API: taskkill for PID {} returned error: {}",
                        pid, stderr
                    );
                    // Process may have already exited, which is fine
                    killed_count += 1;
                }
            }
            Err(e) => {
                error!("MCP API: Failed to execute taskkill for PID {}: {}", pid, e);
            }
        }
    }

    if !pids_to_kill.is_empty() {
        emit_ai_output(
            &state.app_handle,
            &format!("⛔ Killed {} AI process(es)", killed_count),
            "status",
            None,
            None,
        );
    }

    // Close all interactive Claude sessions via SessionManager
    if let Some(session_manager) = state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
    {
        session_manager.close_all_sessions();
    }

    // Get running tasks from the database (PG)
    let pg = &state.app_state.pg_db;

    let running_tasks = match pg.get_running_task_runs(None).await {
        Ok(tasks) => tasks,
        Err(e) => {
            error!("MCP API: Failed to get running tasks: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get running tasks: {}", e))),
            ));
        }
    };

    if running_tasks.is_empty() && pids_to_kill.is_empty() {
        info!("MCP API: No running tasks to stop");
        return Ok(Json(ApiResponse::success(())));
    }

    // Stop each running task
    for task in &running_tasks {
        // Mark as stopped in database
        if let Err(e) = pg.stop_task_run(&task.id, "user_stopped").await {
            warn!("MCP API: Failed to stop task run {}: {}", task.id, e);
        }

        // Expire any waiting breakpoint snapshots for this task (cleanup)
        let _ = pg.expire_breakpoint_snapshots(&task.id).await;

        // Release URL locks, file registry entries, and exclusive file locks
        state.app_state.url_lock_manager.release_all(&task.id).await;
        state
            .app_state
            .file_registry_manager
            .release_all(&task.id)
            .await;
        let released_paths = state
            .app_state
            .file_lock_manager
            .release_all(&task.id)
            .await;
        for released_path in &released_paths {
            use tauri::Emitter;
            let payload = serde_json::json!({
                "type": "file-lock-released",
                "file_path": released_path,
                "task_run_id": task.id,
                "holder_name": task.id,
            });
            let _ = state.app_handle.emit("file-lock-released", &payload);
        }

        info!("MCP API: Stopped task run: {}", task.id);
    }

    // Emit status to frontend
    emit_ai_output(
        &state.app_handle,
        &format!(
            "Stopped {} running task(s), killed {} process(es)",
            running_tasks.len(),
            killed_count
        ),
        "status",
        None,
        None,
    );

    info!(
        "MCP API: Stopped {} AI analysis task(s)",
        running_tasks.len()
    );
    Ok(Json(ApiResponse::success(())))
}

/// Restart the runner (for AI self-healing workflow)
///
/// This endpoint allows the AI to trigger a runner restart after applying fixes.
/// The restart is delayed to allow the response to be sent first.
pub async fn restart_runner(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RestartRunnerRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let delay_secs = request.delay_seconds.unwrap_or(3);

    info!(
        "MCP API: Runner restart requested - reason: {}, delay: {}s",
        request.reason, delay_secs
    );

    // Emit status to frontend so user knows what's happening
    emit_ai_output(
        &state.app_handle,
        &format!(
            "🔄 Restarting runner in {} seconds: {}",
            delay_secs, request.reason
        ),
        "status",
        None, // No action_id for restart status
        None, // No session context for restart status
    );

    // Spawn a task to exit after delay
    // The Tauri dev server will automatically restart the app
    let delay = std::time::Duration::from_secs(delay_secs);
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        info!("MCP API: Exiting for restart...");
        std::process::exit(0);
    });

    Ok(Json(ApiResponse::success(())))
}

// ============================================================================
// AI Developer (Persistent Mode) HTTP Endpoints
// ============================================================================

/// Check if any AI analysis tasks are currently running (sync version).
/// Uses the provided database to check for running task runs.
/// NOTE: This is a synchronous function that blocks. For async contexts,
/// use has_running_ai_tasks_async() or wrap this in spawn_blocking.
#[allow(dead_code)]
pub fn has_running_ai_tasks() -> bool {
    false
}

/// Check if any AI analysis tasks are currently running (async version).
/// Uses spawn_blocking to avoid blocking the async runtime.
pub async fn has_running_ai_tasks_async() -> bool {
    false
}

/// Helper function to mark a task run as complete with retry logic.
/// Retries up to 3 times with exponential backoff (100ms, 200ms, 400ms).
/// Returns true if successfully marked complete, false otherwise.
///
/// Uses gated function - unified workflows have status managed by LoopController only.
pub async fn complete_task_run_with_retry(task_id: &str) -> bool {
    false
}

// get_workspace_paths_internal is now in crate::mcp::shared
// and re-exported at the top of this file

/// Generate MCP tool context documentation for AI sessions.
///
/// This function creates a markdown documentation string describing the available
/// MCP tools for GUI automation, including the specific workflows, states, and
/// images available in the loaded configuration.
pub fn generate_mcp_tool_context(config: &crate::config::QontinuiConfig) -> String {
    let mut context = String::from(
        r#"
## Available GUI Automation Tools

The following MCP tools are available for deterministic GUI automation.
All actions execute through the unified action service with the pre-loaded config.

### Tools

"#,
    );

    // Tool: run_workflow
    let workflows: Vec<String> = config
        .workflows
        .iter()
        .filter_map(|w| w.get("name").and_then(|n| n.as_str()))
        .map(|n| format!("- {}", n))
        .collect();

    context.push_str(&format!(
        r#"
#### run_workflow
Run a workflow by name from the loaded configuration.

**Available Workflows:**
{}

**Usage:**
```json
{{"tool": "mcp__qontinui__run_workflow", "workflow_name": "WorkflowName", "monitor": "primary"}}
```
"#,
        if workflows.is_empty() {
            "- (none loaded)".to_string()
        } else {
            workflows.join("\n")
        }
    ));

    // Tool: go_to_state
    let states: Vec<String> = config
        .states
        .iter()
        .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
        .map(|n| format!("- {}", n))
        .collect();

    context.push_str(&format!(
        r#"
#### go_to_state
Navigate to a specific state using pathfinding.

**Available States:**
{}

**Usage:**
```json
{{"tool": "mcp__qontinui__go_to_state", "state_id": "StateName"}}
```
"#,
        if states.is_empty() {
            "- (none loaded)".to_string()
        } else {
            states.join("\n")
        }
    ));

    // Tool: execute_action
    let images: Vec<String> = config
        .images
        .iter()
        .take(20) // Limit to avoid context overflow
        .filter_map(|i| i.get("id").and_then(|id| id.as_str()))
        .map(|id| format!("- {}", id))
        .collect();

    context.push_str(&format!(
        r#"
#### execute_action
Execute a single action (click, type, etc.) on a target image.

**Available Images (first 20):**
{}

**Action Types:** click, double_click, right_click, type

**Usage:**
```json
{{"tool": "mcp__qontinui__execute_action", "action_type": "click", "image_id": "image-123"}}
```
"#,
        if images.is_empty() {
            "- (none loaded)".to_string()
        } else {
            images.join("\n")
        }
    ));

    // Tool: capture_screenshot
    context.push_str(
        r#"
#### capture_screenshot
Capture a screenshot from a specified monitor.

**Usage:**
```json
{"tool": "mcp__qontinui__capture_screenshot", "monitor": 0, "delay_seconds": 1.0}
```
"#,
    );

    // SDK Tools - for interacting with UI Bridge SDK-integrated apps
    context.push_str(
        r#"
## Available SDK Tools (UI Bridge)

The following tools interact with SDK-integrated web apps via the runner's HTTP API.
Use these to inspect, interact with, and test web applications that have the UI Bridge SDK installed.

**Content Discovery:** These tools discover both **interactive elements** (buttons, inputs, links)
and **content elements** (headings, paragraphs, labels, metrics, badges, status indicators).
Content elements have a `contentType` field (e.g., `heading`, `paragraph`, `label`, `metric-value`,
`badge`, `status-message`, `description-text`, `list-item`, `table-cell`, `code-block`, `nav-text`)
and may have a `contentRole` from `data-content-role` attributes (e.g., `heading`, `body-text`,
`label`, `metric`, `badge`, `status`, `description`).

**Content filtering** (supported by element/snapshot tools):
- `includeContent` (bool) — include content elements (default: true)
- `contentOnly` (bool) — return only content elements, excluding interactive ones
- `contentRole` (string) — filter to a specific content role

This lets you read page text, find specific metrics/labels/statuses, and verify content changes without screenshots.

### Connection

#### sdk_connect
Connect to a UI Bridge SDK app for element inspection and interaction.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_connect", "url": "http://localhost:3001"}
```

#### sdk_status
Check SDK app connection status. Returns whether connected and app details.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_status"}
```

### Element Inspection

#### sdk_elements
List all registered UI elements (interactive and content) in the connected SDK app.
Returns element IDs, types, labels, state, and contentType/contentRole for content elements.
Accepts optional `includeContent`, `contentOnly`, and `contentRole` filters.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_elements"}
{"tool": "mcp__qontinui__sdk_elements", "contentOnly": true, "contentRole": "metric"}
```

#### sdk_snapshot
Get a complete UI snapshot with all elements (interactive + content) and their current state.
Includes visibility, bounds, text content, available actions, and contentType/contentRole for content elements.
Accepts optional `includeContent`, `contentOnly`, and `contentRole` filters.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_snapshot"}
{"tool": "mcp__qontinui__sdk_snapshot", "contentOnly": true}
```

### AI-Powered Interaction

#### sdk_ai_search
Search for elements (interactive or content) by natural language description.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_ai_search", "text": "Submit button"}
{"tool": "mcp__qontinui__sdk_ai_search", "text": "total revenue metric"}
```

#### sdk_ai_execute
Execute an action by natural language instruction.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_ai_execute", "instruction": "click the Submit button"}
```

#### sdk_execute_action_plan
Execute a structured action plan — an ordered sequence of typed UI actions.
Each action specifies the exact action type, element target, and parameters.
This is more efficient than sdk_ai_execute for multi-step interactions because
it skips natural language interpretation and executes actions directly.

First call sdk_snapshot or sdk_elements to get element IDs, then build the plan.

**Action types:** click, doubleClick, rightClick, type, clear, select, check, uncheck,
toggle, hover, focus, scroll, scrollIntoView, setValue, sendKeys, drag, submit,
autocomplete (type + select from suggestions), navigate, wait.

**Element targeting** (in priority order): elementId (from snapshot), testId (data-testid),
selector (CSS), searchText + elementType (fuzzy search).

**Usage:**
```json
{"tool": "mcp__qontinui__sdk_execute_action_plan", "goal": "Fill and submit login form", "actions": [
  {"action": "click", "target": {"testId": "email-input"}, "reasoning": "Focus email field", "confidence": 0.95},
  {"action": "type", "target": {"testId": "email-input"}, "params": {"text": "user@example.com"}, "reasoning": "Enter email", "confidence": 0.95},
  {"action": "click", "target": {"searchText": "Submit", "elementType": "button"}, "reasoning": "Submit the form", "confidence": 0.9}
], "confidenceThreshold": 0.5, "stopOnFailure": true}
```

#### sdk_ai_assert
Assert element state using natural language.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_ai_assert", "text": "error message", "state": "hidden"}
```

### Choosing Between sdk_ai_execute and sdk_execute_action_plan

Use **sdk_ai_execute** for:
- Single, simple actions ("click the Submit button")
- When you don't know the element structure and need AI interpretation
- Exploratory interactions where the page layout is unknown

Use **sdk_execute_action_plan** for:
- **Multi-step interactions** (2+ actions in sequence): filling forms, navigating menus, multi-field edits
- When you already have element IDs from a prior sdk_snapshot or sdk_elements call
- When precision matters: each action specifies exact type, target, and params with no ambiguity
- Performance-sensitive flows: skips the second LLM interpretation call that sdk_ai_execute requires

**Typical workflow:**
1. Call `sdk_snapshot` or `sdk_elements` to see current page state and element IDs
2. Build an action plan using the element IDs/testIds from the snapshot
3. Execute with `sdk_execute_action_plan`
4. Verify result with `sdk_ai_assert` or another `sdk_snapshot`

**Action plan caching:** Include `pageUrl` and `elementSnapshot` fields in the request
to cache successful plans. Subsequent calls with the same page and element fingerprint
can reuse the plan via GET `/ui-bridge/control/action-plan/cache?url=...&elements=...`.

#### sdk_page_summary
Get an AI-friendly summary of the current page, including layout, navigation, and key elements.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_page_summary"}
```

### Screenshots

#### sdk_screenshot
Capture a screenshot of the monitor where the SDK app is running.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_screenshot"}
```

### Per-App Analysis

These tools analyze the currently connected SDK app's page structure and data.
They work on a single app — use them independently or as building blocks.

#### sdk_analyze_data
Extract labeled data values from the page. Each value is classified by type
(text, number, currency, date, email, url, phone, percentage, boolean) and
normalized for comparison.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_analyze_data"}
```

#### sdk_analyze_regions
Segment the page into semantic regions: header, navigation, sidebar,
main-content, footer, form, table, card, modal, toolbar. Each region
includes its bounding box and contained element IDs.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_analyze_regions"}
```

#### sdk_analyze_structured_data
Detect and extract tables (with column headers and row data) and lists
(with field schemas and items) from the page based on spatial layout patterns.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_analyze_structured_data"}
```

### Cross-App Comparison

#### sdk_cross_app_compare
Compare two SDK-integrated apps by connecting to each, capturing semantic
snapshots (including content elements), and running a full analysis. Returns
scores (0-1) for data completeness, format alignment, presentation alignment,
navigation parity, action parity, and an overall score. Also returns a
prioritized issue list. Content elements enable text-level comparison across apps.

Set `include_components` to true to also fetch and compare registered
components between the two apps.

**Usage:**
```json
{"tool": "mcp__qontinui__sdk_cross_app_compare", "source_url": "http://localhost:1420", "target_url": "http://localhost:3001", "include_components": true}
```
"#,
    );

    context
}

// Prompt CRUD handlers (list, get, create, update, delete, categories, tags,
// import, export, duplicate, search) moved to crate::mcp::prompts

/// Run a prompt by spawning a Claude session
///
/// Supports two modes:
/// 1. Lookup prompt from database: provide `prompt_id`
/// 2. Ad-hoc prompt: provide `name` and `content`
///
/// Optional image analysis: provide `image_paths`, `video_paths`, or `trace_path`
/// to enhance the prompt with visual analysis data.
pub async fn run_prompt(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RunPromptRequest>,
) -> Result<Json<ApiResponse<RunPromptResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Determine mode and get prompt name + content + orchestrator config
    // Orchestrator config is extracted from saved prompts (system-level setting, not user-controllable)
    let (
        prompt_name,
        prompt_content,
        prompt_id,
        prompt_max_sessions,
        requires_orchestrator,
        _orchestrator_goal,
        _orchestrator_max_iterations,
        _orchestrator_verification_first,
    ) = if let Some(ref id) = request.prompt_id {
        // Mode 1: Lookup from database
        let prompt = prompts::get_prompt(id).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Prompt not found: {}", id))),
            )
        })?;
        (
            prompt.name.clone(),
            prompt.content.clone(),
            Some(prompt.id.clone()),
            prompt.max_sessions,
            prompt.requires_orchestrator,
            prompt.orchestrator_goal.clone(),
            prompt.orchestrator_max_iterations,
            prompt.orchestrator_verification_first,
        )
    } else if let (Some(name), Some(content)) = (&request.name, &request.content) {
        // Mode 2: Ad-hoc prompt (no orchestrator by default)
        (
            name.clone(),
            content.clone(),
            None,
            None,
            false,
            None,
            None,
            None,
        )
    } else {
        // Invalid: neither mode satisfied
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "Must provide either prompt_id OR (name AND content)",
            )),
        ));
    };

    // Generate session_id if not provided
    let session_id = request.session_id.unwrap_or_else(|| {
        format!(
            "{}-{}",
            chrono::Utc::now().format("%Y%m%d-%H%M%S"),
            rand::random::<u16>()
        )
    });

    // Use override or prompt's setting (None = unlimited sessions)
    let max_sessions = request.max_sessions.or(prompt_max_sessions);

    // Use session_id as task_run_id (they are the same)
    let task_run_id = session_id.clone();

    // Auto-load last config if not already loaded and auto_load_last_config is enabled
    // This ensures GUI automation tasks have access to workflows
    let config_was_loaded = {
        let config_lock = safe_lock_or_recover(&state.app_state.current_config, "current_config");
        config_lock.is_some()
    };

    let mut config_info: Option<(String, Option<String>, Option<i32>)> = None;
    if !config_was_loaded && settings::get_auto_load_last_config() {
        if let Some(config_path) = settings::get_last_config_path() {
            if std::path::Path::new(&config_path).exists() {
                info!(
                    "MCP API: Auto-loading last config for prompt execution: {}",
                    config_path
                );

                // Load the config
                match crate::config::ConfigLoader::load_from_file(&config_path) {
                    Ok(config) => {
                        // Store the config
                        let mut config_lock =
                            safe_lock_or_recover(&state.app_state.current_config, "current_config");
                        *config_lock = Some(config);

                        let workflow_id = settings::get_last_workflow_id();
                        let monitor_index = settings::get_last_monitor_index();
                        config_info = Some((config_path.clone(), workflow_id, monitor_index));

                        info!(
                            "MCP API: Auto-loaded config: {:?}, workflow: {:?}, monitor: {:?}",
                            config_path,
                            config_info.as_ref().map(|c| &c.1),
                            config_info.as_ref().map(|c| &c.2)
                        );
                    }
                    Err(e) => {
                        warn!("MCP API: Failed to auto-load config: {}", e);
                    }
                }
            }
        }
    }

    // RemoteAgent / scheduler ad-hoc knobs (Phase D — scheduler reliability
    // plan). Captured before mutation so they can be plumbed into the
    // spawn-independent-claude.py invocation below. Each is forwarded as an
    // optional CLI flag the Python wrapper passes verbatim to `claude`.
    let remote_working_directory = request.working_directory.clone();
    let remote_model = request.model.clone();
    let remote_allowed_tools = request
        .allowed_tools
        .as_ref()
        .filter(|v| !v.is_empty())
        .map(|tools| tools.join(","));
    let remote_max_turns = request.max_turns;
    let remote_mcp_connections = request.mcp_connections.clone().unwrap_or_default();

    // Collect images for analysis if provided
    let image_paths = request.image_paths.unwrap_or_default();
    let video_paths = request.video_paths.unwrap_or_default();
    let max_video_frames = request.max_video_frames.unwrap_or(3) as u32;
    let max_trace_screenshots = request.max_trace_screenshots.unwrap_or(5) as u32;

    let (all_images, trace_timeline) = super::trace_verification::collect_images_for_analysis(
        &image_paths,
        &video_paths,
        request.trace_path.as_deref(),
        max_video_frames,
        max_trace_screenshots,
    );

    // Build enhanced prompt with trace timeline and image references if available
    let mut enhanced_prompt = prompt_content.clone();

    // Inject contexts into the prompt if requested
    let context_ids = request.context_ids.unwrap_or_default();
    let auto_include_contexts = request.auto_include_contexts.unwrap_or(false);

    // Extract action types from loaded config for auto-detection
    let action_types: Vec<String> = {
        let config_lock = safe_lock_or_recover(&state.app_state.current_config, "current_config");
        if let Some(ref config) = *config_lock {
            // Extract action types from workflows
            config
                .workflows
                .iter()
                .flat_map(|w| {
                    w.get("actions")
                        .and_then(|a| a.as_array())
                        .map(|actions| {
                            actions
                                .iter()
                                .filter_map(|action| {
                                    action
                                        .get("type")
                                        .and_then(|t| t.as_str())
                                        .map(String::from)
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .collect()
        } else {
            Vec::new()
        }
    };

    // For now, we pass an empty error list for auto-detection
    // In the future, this could be populated from recent log errors
    let recent_errors: Vec<String> = Vec::new();

    // Inject contexts and track which ones were used
    let (prompt_with_contexts, used_context_ids) =
        if !context_ids.is_empty() || auto_include_contexts {
            let (enhanced, used_ids) = context::inject_contexts(
                &enhanced_prompt,
                &context_ids,
                auto_include_contexts,
                &prompt_content, // Use original prompt for auto-detection matching
                &action_types,
                &recent_errors,
                None, // CWD-based project contexts (no explicit workspace path here)
            );

            if !used_ids.is_empty() {
                info!(
                    "MCP API: Injected {} contexts into prompt: {:?}",
                    used_ids.len(),
                    used_ids
                );
            }

            (enhanced, used_ids)
        } else {
            (enhanced_prompt.clone(), Vec::new())
        };
    enhanced_prompt = prompt_with_contexts;

    // Inject observation memory from past sessions (if PG available).
    // Use the prompt name (concise) rather than full prompt content (noisy) for search.
    let memory_query = prompt_name.as_str();
    if !memory_query.is_empty() {
        if let Some(memory_section) = context::format_observation_memory_for_prompt(
            &state.app_state.pg_db,
            None,
            Some(memory_query),
        )
        .await
        {
            enhanced_prompt = format!("{}{}", memory_section, enhanced_prompt);
        }
    }

    // Check file registry for conflicts and warn this session about files under active development
    {
        let conflicts = state
            .app_state
            .file_registry_manager
            .check_conflicts(&task_run_id)
            .await;
        if !conflicts.is_empty() {
            let mut warning = String::from("## Active File Conflicts Warning\n\n");
            warning.push_str(
                "The following files are currently being worked on by other active sessions. \
                 Avoid modifying these files to prevent merge conflicts:\n\n",
            );
            for conflict in &conflicts {
                let holders: Vec<String> = conflict
                    .other_holders
                    .iter()
                    .map(|h| format!("'{}'", h.holder_name))
                    .collect();
                warning.push_str(&format!(
                    "- **{}** (active in: {})\n",
                    conflict.file_path,
                    holders.join(", ")
                ));
            }
            warning.push_str("\nIf you must edit these files, coordinate with the other session(s) first.\n\n---\n\n");
            enhanced_prompt = format!("{}{}", warning, enhanced_prompt);
            info!(
                "Injected {} file conflict warning(s) into session {} prompt",
                conflicts.len(),
                task_run_id
            );
        }
    }

    // Prepend runner-triggered context and supervisor instructions
    // This tells the AI session how to safely restart the runner if needed
    let supervisor_available = super::auto_continue::check_supervisor_available();
    let runner_context = if supervisor_available {
        r#"## IMPORTANT: Runner-Triggered Session Context

You are being run BY the qontinui-runner. You are a child process of the runner.

**CRITICAL RULES:**
1. Do NOT restart the qontinui-runner directly - it will kill your session
2. You CAN restart backend and frontend without issues
3. If the runner needs to be restarted, USE THE SUPERVISOR API

**Restarting Runner via Supervisor (SAFE):**
```powershell
# Simple restart (no rebuild)
Invoke-RestMethod -Uri "http://localhost:9875/runner/restart" -Method Post -ContentType "application/json" -Body '{"trigger_auto_continue": true}'

# Restart with REBUILD (use after modifying runner Rust code)
Invoke-RestMethod -Uri "http://localhost:9875/runner/restart" -Method Post -ContentType "application/json" -Body '{"rebuild": true, "trigger_auto_continue": true}'
```

**Supervisor API (port 9875):**
- GET /health - Check if supervisor is running
- POST /runner/stop - Stop the runner
- POST /runner/restart - Restart runner (options: rebuild, trigger_auto_continue, wait_timeout_seconds)
- POST /workflow-loop/signal-restart - Signal that runner restart is needed (use during workflow loops)

**IMPORTANT:** If you modified qontinui-runner Rust code, use `"rebuild": true` to recompile before restart.

**Workflow Loop Signal:** If you are running inside a supervisor workflow loop and you modify runner code, call:
```powershell
Invoke-RestMethod -Uri "http://localhost:9875/workflow-loop/signal-restart" -Method Post
```
This tells the supervisor to restart the runner between iterations. If you don't signal, the loop skips the restart (saving time when only non-runner repos were changed).

---

"#
    } else {
        r#"## IMPORTANT: Runner-Triggered Session Context

You are being run BY the qontinui-runner. You are a child process of the runner.

**CRITICAL RULES:**
1. Do NOT restart the qontinui-runner directly - it will kill your session
2. You CAN restart backend and frontend without issues
3. The supervisor is NOT currently running - if runner restart is needed, inform the user

**If runner restart is needed:**
Tell the user: "The qontinui-runner needs to be restarted manually to apply changes."

---

"#
    };

    enhanced_prompt = format!("{}{}", runner_context, enhanced_prompt);

    // RemoteAgent: surface declared MCP connection refs in the prompt
    // header. Phase D does not yet merge these into a per-call MCP config
    // file; the runner inherits whatever MCP config the user has registered.
    // The header documents the requested connections so the agent (and any
    // log readers) can verify the right MCP servers are available.
    if !remote_mcp_connections.is_empty() {
        let mut mcp_section = String::from(
            "## Requested MCP Connections\n\nThis scheduled task declared the following MCP connection refs. They are resolved at dispatch time against the runner's existing MCP config; per-call overrides are not yet wired.\n\n",
        );
        for conn in &remote_mcp_connections {
            match &conn.url {
                Some(url) => {
                    mcp_section.push_str(&format!("- **{}** (override URL: {})\n", conn.name, url))
                }
                None => mcp_section.push_str(&format!(
                    "- **{}** (use runner's configured URL)\n",
                    conn.name
                )),
            }
        }
        mcp_section.push_str("\n---\n\n");
        enhanced_prompt = format!("{}{}", mcp_section, enhanced_prompt);
    }

    // Inject Multi-Step Task Guide context (user override takes precedence)
    let multi_step_guide = context::get_multi_step_guide();
    let multi_step_section = format!(
        "## Multi-Session Task Context\n\n{}\n\n---\n\n",
        context::format_single_context(&multi_step_guide)
    );
    enhanced_prompt = format!("{}{}", multi_step_section, enhanced_prompt);

    // Inject Service Restart Commands context (user override takes precedence)
    // Replace {{WORKSPACE}} placeholder with actual workspace path
    let service_restart = context::get_service_restart_commands();
    let workspace_path = get_workspace_paths_internal()
        .map(|(root, _, _)| root.to_string_lossy().to_string())
        .unwrap_or_else(|_| "{{WORKSPACE}}".to_string());
    let service_restart_content = service_restart
        .content
        .replace("{{WORKSPACE}}", &workspace_path);
    let mut service_restart_with_path = service_restart.clone();
    service_restart_with_path.content = service_restart_content;
    let service_restart_section = format!(
        "{}\n\n---\n\n",
        context::format_single_context(&service_restart_with_path)
    );
    enhanced_prompt = format!("{}{}", service_restart_section, enhanced_prompt);

    // Inject configured log sources from global settings
    // This tells the AI where to find logs for debugging
    {
        let global_settings = crate::settings::get_global_log_source_settings();
        let enabled_sources: Vec<_> = global_settings
            .sources
            .iter()
            .filter(|s| s.enabled)
            .map(|s| format!("- **{}**: `{}`", s.name, s.path))
            .collect();

        if !enabled_sources.is_empty() {
            let log_sources_section = format!(
                r#"## Configured Log Sources

The following log files have been configured for monitoring. Use these paths to check for errors:

{}

---

"#,
                enabled_sources.join("\n")
            );
            enhanced_prompt = format!("{}{}", log_sources_section, enhanced_prompt);
        }
    }

    // Add GUI automation context if config was auto-loaded
    if let Some((config_path, workflow_id, monitor_index)) = &config_info {
        let workflow_info = workflow_id
            .as_ref()
            .map(|w| format!("- Last workflow: {}", w))
            .unwrap_or_else(|| "- No last workflow saved".to_string());
        let monitor_info = monitor_index
            .map(|m| format!("- Last monitor index: {}", m))
            .unwrap_or_else(|| "- No last monitor index saved".to_string());

        let gui_context = format!(
            r#"
## GUI Automation Available

A workflow configuration has been auto-loaded:
- Config path: {}
{}
{}

**Runner MCP API (port 9876):**
- GET /status - Check runner and config status
- POST /run-workflow - Run a workflow by name
  Example: `Invoke-RestMethod -Uri "http://localhost:9876/run-workflow" -Method Post -ContentType "application/json" -Body '{{"workflow_id": "workflow-name", "monitor_index": 0}}'`
- GET /monitors - List available monitors

If your task requires running visual automation, use the Runner API to execute workflows.

---

"#,
            config_path, workflow_info, monitor_info
        );

        enhanced_prompt = format!("{}{}", gui_context, enhanced_prompt);
    }

    // Add MCP tool context if config is loaded (either pre-loaded or auto-loaded)
    {
        let config_lock = safe_lock_or_recover(&state.app_state.current_config, "current_config");
        if let Some(config) = config_lock.as_ref() {
            let tool_context = generate_mcp_tool_context(config);
            enhanced_prompt = format!("{}\n{}", enhanced_prompt, tool_context);
        }
    }

    if let Some(timeline) = &trace_timeline {
        enhanced_prompt = format!("{}\n\n{}", enhanced_prompt, timeline);
    }

    // Add image paths to prompt if there are any
    if !all_images.is_empty() {
        enhanced_prompt = format!(
            "{}\n\n## Images for Analysis\n\nThe following images are available for analysis. Use the Read tool to view them:\n{}",
            enhanced_prompt,
            all_images.iter().map(|p| format!("- {}", p)).collect::<Vec<_>>().join("\n")
        );
    }

    // Add structured finding output instructions
    enhanced_prompt = format!("{}{}", enhanced_prompt, FINDING_INSTRUCTIONS);

    info!(
        "MCP API: Running prompt '{}' (session: {}, max_sessions: {:?}, requires_orchestrator: {}, images: {})",
        prompt_name,
        session_id,
        max_sessions,
        requires_orchestrator,
        all_images.len()
    );

    // Create TaskRun record in database (PG)
    {
        let mut input = CreateTaskRunInput::new(&task_run_id, &prompt_name)
            .with_prompt(&enhanced_prompt)
            .with_task_type("task");
        if let Some(ms) = max_sessions {
            input = input.with_max_sessions(ms);
        }
        state.app_state.pg_db.create_task_run(&input).await
    }
    .map_err(|e| {
        error!("MCP API: Failed to create task run: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to create task run: {}", e))),
        )
    })?;

    info!("MCP API: Created task run with ID: {}", task_run_id);

    // Create session context for AI output events so frontend can display the task name
    // This is the first turn (iteration 1), so turn_count = 1
    let session_ctx = AiSessionContext::agentic(&task_run_id, &prompt_name, 1)
        .with_runtime_env()
        .with_new_trace()
        .with_ai_settings()
        .with_turn_count(1);

    // Emit prompt to frontend (use original prompt content for display)
    emit_ai_output(
        &state.app_handle,
        &prompt_content,
        "prompt",
        Some(&task_run_id),
        Some(&session_ctx),
    );

    // Emit status indicator
    emit_ai_output(
        &state.app_handle,
        "AI session spawned - check task runs for status",
        "status",
        Some(&task_run_id),
        Some(&session_ctx),
    );

    // Record context usage now that the session is starting
    if !used_context_ids.is_empty() {
        context::record_contexts_used(&used_context_ids);
    }

    // =========================================================================
    // EXECUTION PATH ROUTING
    // =========================================================================
    // When requires_orchestrator is true, route through the unified session API
    // which has full orchestrator support (planning, verification, feedback loops).
    // When false, use the simpler direct spawn path.
    // =========================================================================

    // NOTE: The orchestrator path was removed when run_unified_session_loop was deleted.
    // All paths now use the direct spawn path. Orchestrator functionality will be
    // re-integrated via LoopController in a future update.
    if requires_orchestrator {
        warn!(
            "MCP API: Orchestrator path requested but session loop was removed. Falling through to direct spawn path for prompt '{}' (session: {})",
            prompt_name, session_id
        );
    }

    // Always use the direct spawn path for now
    {
        // =====================================================================
        // DIRECT SPAWN PATH
        // =====================================================================
        // DIRECT SPAWN PATH
        // =====================================================================
        // Use the simpler direct spawn path.
        // Orchestrator functionality will be re-integrated via LoopController.
        // =====================================================================

        info!(
            "MCP API: Using direct spawn path for prompt '{}' (session: {})",
            prompt_name, session_id
        );

        let prompt_name_for_state = prompt_name.clone();
        let remote_working_directory_for_spawn = remote_working_directory.clone();
        let remote_model_for_spawn = remote_model.clone();
        let remote_allowed_tools_for_spawn = remote_allowed_tools.clone();
        let remote_max_turns_for_spawn = remote_max_turns;
        let result = tokio::task::spawn_blocking(move || {
            let (workspace_root, dev_logs_path, scripts_path) = get_workspace_paths_internal()?;
            let spawn_script = scripts_path.join("spawn-independent-claude.py");
            let state_file = dev_logs_path.join(format!("ai-developer-{}.json", session_id));
            let prompt_file = dev_logs_path.join(format!("ai-developer-{}-prompt.txt", session_id));
            let log_file = dev_logs_path.join(format!("claude-session-{}.log", session_id));

            // Ensure .dev-logs directory exists
            std::fs::create_dir_all(&dev_logs_path)
                .map_err(|e| format!("Failed to create dev-logs directory: {}", e))?;

            // Create initial state file
            let initial_state = serde_json::json!({
                "session_id": session_id,
                "task_run_id": session_id,
                "prompt_id": prompt_id,
                "prompt_name": prompt_name_for_state,
                "session_count": 1,
                "max_sessions": max_sessions,
                "status": "starting",
                "started_at": chrono::Utc::now().to_rfc3339(),
                "stop_requested": false,
                "current_action": "Initializing",
                "errors_fixed": [],
                "errors_remaining": [],
                "activity_log": [],
                // Orchestrator not used in direct spawn path
                "requires_orchestrator": false,
                "orchestrator_goal": null,
                "orchestrator_max_iterations": null
            });

            let state_json = serde_json::to_string_pretty(&initial_state)
                .map_err(|e| format!("Failed to serialize state: {}", e))?;
            std::fs::write(&state_file, state_json)
                .map_err(|e| format!("Failed to write state file: {}", e))?;

            // Write enhanced prompt content to file
            std::fs::write(&prompt_file, &enhanced_prompt)
                .map_err(|e| format!("Failed to write prompt file: {}", e))?;

            info!("MCP API: State file created: {:?}", state_file);
            info!("MCP API: Prompt file created: {:?}", prompt_file);

            // Build base CLI args. New RemoteAgent knobs (--working-directory,
            // --model, --allowed-tools, --max-turns) are appended below when
            // set. The Python wrapper forwards each verbatim to `claude`.
            //
            // We hold the formatted strings (max_turns_str) in this scope so
            // their `OsStr` borrow stays valid until spawn_python_with_console
            // returns.
            let max_turns_str = remote_max_turns_for_spawn.map(|n| n.to_string());

            let mut spawn_args: Vec<&std::ffi::OsStr> = vec![
                spawn_script.as_os_str(),
                std::ffi::OsStr::new("--file"),
                prompt_file.as_os_str(),
                std::ffi::OsStr::new("--session-id"),
                std::ffi::OsStr::new(&session_id),
            ];
            if let Some(ref wd) = remote_working_directory_for_spawn {
                spawn_args.push(std::ffi::OsStr::new("--working-directory"));
                spawn_args.push(std::ffi::OsStr::new(wd.as_str()));
            }
            if let Some(ref m) = remote_model_for_spawn {
                spawn_args.push(std::ffi::OsStr::new("--model"));
                spawn_args.push(std::ffi::OsStr::new(m.as_str()));
            }
            if let Some(ref tools) = remote_allowed_tools_for_spawn {
                spawn_args.push(std::ffi::OsStr::new("--allowed-tools"));
                spawn_args.push(std::ffi::OsStr::new(tools.as_str()));
            }
            if let Some(ref mt) = max_turns_str {
                spawn_args.push(std::ffi::OsStr::new("--max-turns"));
                spawn_args.push(std::ffi::OsStr::new(mt.as_str()));
            }

            // Spawn Claude independently using the spawn script
            // Use spawn_python_with_console to ensure Claude CLI gets a console window
            let spawn_result = spawn_python_with_console("python", &spawn_args, &workspace_root);

            match spawn_result {
                Ok(child) => {
                    info!(
                        "MCP API: AI Developer spawned with PID: {} for prompt '{}'",
                        child.id(),
                        prompt_name_for_state
                    );
                    Ok((
                        RunPromptResponse {
                            task_run_id: session_id.clone(),
                            action_id: session_id.clone(), // Backward compatibility
                            session_id,
                            state_file: state_file.to_string_lossy().to_string(),
                            log_file: log_file.to_string_lossy().to_string(),
                            pid: Some(child.id()),
                        },
                        log_file,
                        dev_logs_path,
                    ))
                }
                Err(e) => {
                    error!("MCP API: Failed to spawn AI Developer: {}", e);
                    Err(format!("Failed to spawn AI Developer: {}", e))
                }
            }
        })
        .await
        .map_err(|e| {
            error!("MCP API: spawn_blocking error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Internal error: {}", e))),
            )
        })?;

        match result {
            Ok((response, _log_file, _dev_logs_path)) => {
                // NOTE: TaskMonitor was removed - task completion is now tracked by LoopController
                Ok(Json(ApiResponse::success(response)))
            }
            Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
        }
    }
}

// Remaining prompt CRUD handlers (categories, tags, import, export, duplicate, search)
// moved to crate::mcp::prompts

// Macro handlers moved to crate::mcp::macros
// Playwright script handlers moved to crate::mcp::playwright
// Prompt snippet handlers moved to crate::mcp::prompt_snippets

#[cfg(test)]
mod tests {
    // Phase 1 of stuck-session-heartbeat-plan.md — `GET /sessions/idle-status`.
    //
    // The HTTP handler takes `Arc<ApiState>`, which can't be constructed in
    // a unit test (real `tauri::AppHandle`). Following the pattern used by
    // `mcp::file_registry::tests` (see `request_yield_valid_payload_broadcasts_event`
    // and friends), we drive the same composition the handler produces
    // against a real `SessionManager` snapshot via the `build_idle_entries`
    // pure helper — exactly the substrate the handler depends on.
    //
    // The handler body itself is a four-line shim
    // (`try_state` → `snapshot` → `build_idle_entries` → `Json`), so
    // covering the helper covers every interesting branch.
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Mirrors the friendly-name derivation in
    /// `claude_session/dispatcher.rs:398-404` and
    /// `ClaudeSession::holder_name`. Kept as a test-local helper so a
    /// future change to that derivation will surface here if the two
    /// paths diverge.
    fn derive_holder_name(session_id: &str, session_name: Option<&str>) -> String {
        session_name
            .map(|s| s.to_string())
            .unwrap_or_else(|| session_id.to_string())
    }

    #[test]
    fn idle_status_returns_empty_when_no_sessions() {
        // Empty snapshot — what `SessionManager::snapshot()` returns when
        // no `ClaudeSession`s are registered.
        let entries = build_idle_entries(Vec::new(), 1_700_000_000_000);
        assert!(
            entries.is_empty(),
            "expected empty Vec for empty snapshot, got {} entries",
            entries.len()
        );
    }

    #[test]
    fn idle_status_returns_entry_for_registered_session() {
        // Synthetic snapshot: one session, last_activity 2 seconds ago.
        // last_activity is stored as epoch SECONDS (per
        // `claude_session/session.rs:81,420`), so the tracker holds
        // (now_ms / 1000) - 2.
        let now_ms = 1_700_000_000_000_u64;
        let last_activity_s = (now_ms / 1000) - 2;
        let tracker = Arc::new(AtomicU64::new(last_activity_s));

        let snapshot = vec![(
            "task-A".to_string(),
            "Session A".to_string(),
            tracker.clone(),
        )];

        let entries = build_idle_entries(snapshot, now_ms);

        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.task_run_id, "task-A");
        assert_eq!(entry.holder_name, "Session A");
        // last_activity_ms = last_activity_s * 1000 = now_ms - 2000
        assert_eq!(entry.last_activity_ms, now_ms - 2_000);
        // idle_ms = now - last_activity = 2 seconds = 2000 ms
        assert_eq!(entry.idle_ms, 2_000);
    }

    #[test]
    fn idle_status_idle_ms_zero_when_activity_at_now() {
        // Activity timestamp matches the moment we compute idle_ms —
        // idle_ms must be 0, not negative (saturating sub).
        let now_ms = 1_700_000_000_000_u64;
        let tracker = Arc::new(AtomicU64::new(now_ms / 1000));
        let snapshot = vec![("t".to_string(), "T".to_string(), tracker)];

        let entries = build_idle_entries(snapshot, now_ms);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].idle_ms, 0);
    }

    #[test]
    fn idle_status_idle_ms_saturates_on_future_activity() {
        // last_activity in the future (clock skew between threads, or a
        // test using a synthetic now): idle_ms must clamp to 0 rather
        // than wrap around.
        let now_ms = 1_700_000_000_000_u64;
        let future_s = (now_ms / 1000) + 60;
        let tracker = Arc::new(AtomicU64::new(future_s));
        let snapshot = vec![("t".to_string(), "T".to_string(), tracker)];

        let entries = build_idle_entries(snapshot, now_ms);
        assert_eq!(entries[0].idle_ms, 0);
    }

    #[test]
    fn idle_status_holder_name_matches_dispatcher_emit() {
        // Cross-check that holder_name comes through the snapshot with
        // the exact friendly-name shape the file-lock dispatcher emits
        // on `file-lock-*` events (claude_session/dispatcher.rs:437).
        //
        // Workflow path: holder_name = session_ctx.session_name
        let workflow = derive_holder_name("task-w-1", Some("My Workflow - Iteration 3"));
        // Terminal path: session_ctx is None → falls back to session_id
        let terminal = derive_holder_name("term-task-123", None);

        let now_ms = 1_700_000_000_000_u64;
        let snapshot = vec![
            (
                "task-w-1".to_string(),
                workflow.clone(),
                Arc::new(AtomicU64::new(now_ms / 1000)),
            ),
            (
                "term-task-123".to_string(),
                terminal.clone(),
                Arc::new(AtomicU64::new(now_ms / 1000)),
            ),
        ];

        let entries = build_idle_entries(snapshot, now_ms);
        assert_eq!(entries.len(), 2);
        // Order is preserved from snapshot input.
        assert_eq!(entries[0].holder_name, "My Workflow - Iteration 3");
        assert_eq!(entries[1].holder_name, "term-task-123");
        // Equivalent to what `ClaudeSession::holder_name()` returns:
        assert_eq!(workflow, "My Workflow - Iteration 3");
        assert_eq!(terminal, "term-task-123");
    }

    #[test]
    fn idle_status_sees_live_atomic_updates() {
        // The snapshot hands out `Arc<AtomicU64>`s, not snapshots of the
        // value. A second build_idle_entries call after the tracker
        // advances must reflect the new value — this is how the live
        // /sessions/idle-status endpoint stays fresh between requests
        // without a new snapshot.
        let now_ms_1 = 1_700_000_000_000_u64;
        let tracker = Arc::new(AtomicU64::new(now_ms_1 / 1000 - 5));
        let snapshot = vec![("t".to_string(), "T".to_string(), tracker.clone())];

        let first = build_idle_entries(snapshot.clone(), now_ms_1);
        assert_eq!(first[0].idle_ms, 5_000);

        // Simulate a new stdout line landing — bumps the tracker to
        // "now_ms_1's second" (1 sec idle relative to a slightly later now).
        tracker.store(now_ms_1 / 1000, Ordering::Relaxed);
        let now_ms_2 = now_ms_1 + 1_000;
        let second = build_idle_entries(snapshot, now_ms_2);
        assert_eq!(second[0].idle_ms, 1_000);
    }

    // ------------------------------------------------------------------
    // `GET /auth/freshness` — compute_freshness_deltas (item 4)
    // ------------------------------------------------------------------

    #[test]
    fn freshness_computes_positive_and_negative_deltas() {
        let now = 1_700_000_000_i64;
        // access token expires 1h from now; oauth already expired 5m ago.
        let r = compute_freshness_deltas(Some(now + 3_600), Some(now - 300), now, true);
        assert_eq!(r.access_token_exp_in_s, Some(3_600));
        assert_eq!(r.oauth_expires_in_s, Some(-300));
        assert!(r.paired);
    }

    #[test]
    fn freshness_passes_none_through() {
        let now = 1_700_000_000_i64;
        let r = compute_freshness_deltas(None, None, now, false);
        assert_eq!(r.access_token_exp_in_s, None);
        assert_eq!(r.oauth_expires_in_s, None);
        assert!(!r.paired);
    }

    #[test]
    fn freshness_never_leaks_absolute_expiry() {
        // The delta must be relative to `now`, not the absolute unix-seconds
        // expiry that lives in storage.
        let now = 1_700_000_000_i64;
        let r = compute_freshness_deltas(Some(1_700_000_050), Some(1_700_000_010), now, true);
        assert_eq!(r.access_token_exp_in_s, Some(50));
        assert_eq!(r.oauth_expires_in_s, Some(10));
    }
}
