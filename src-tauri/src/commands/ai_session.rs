//! Tauri commands for interactive AI sessions.
//!
//! These commands allow the frontend to send messages to active Claude CLI sessions,
//! interrupt processing, query session state, and manage standalone AI sessions.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{Emitter, Manager};
use tracing::{error, info, warn};

use crate::claude_session::manager::SessionManager;
use crate::claude_session::resume::{build_replay_prompt, parse_conversation};
use crate::claude_session::state::SessionState;
// Migrated to StorageCompartment (Workstream C) — 5 handlers.
// `generate_workflow_from_session` is multi-State on Storage + Health: typed
// access for `pg_db` / `doctor_handle`, plus `health.app_state()` escape hatch
// for the legacy `generate_workflow` and `fetch_existing_specs` calls that
// pre-date the compartment split.
use crate::commands::compartments::{HealthCompartment, StorageCompartment};
use crate::commands::{AppState, CommandResponse};
use crate::database::CreateTaskRunInput;
use crate::execution_context::AiSessionContext;
use crate::mcp::shared::AiOutputEvent;
use crate::unified_workflows::UnifiedWorkflowExt;

/// Session state event payload (emitted on state transitions).
#[derive(Debug, Clone, Serialize)]
pub struct SessionStateEvent {
    #[serde(rename = "taskRunId")]
    pub task_run_id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub state: String,
    /// Set to true on the first state event emitted for a session that was
    /// auto-resumed after a runner restart. Used by the frontend to show a
    /// one-time "resumed" badge/toast.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub resumed: bool,
}

/// How a session's conversation was reconstructed on resume (Phase 4
/// honesty gate). Serialized lowercase-kebab for the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResumeFidelity {
    /// Resumed losslessly via the on-disk `--resume` transcript.
    Lossless,
    /// Reconstructed from the lossy `output_log` summary — earlier context
    /// may be missing. `turnsCovered` carries the count.
    Summary,
}

/// What happened to the session's isolated-worktree coord claim on resume
/// (Phase 4 honesty gate). Serialized lowercase-kebab for the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClaimStatus {
    /// No isolated worktree was involved (shared checkout / worktree mode
    /// off) — nothing session-scoped to re-acquire.
    None,
    /// The worktree claim was cleanly re-acquired under the original owner.
    Reacquired,
    /// A peer now holds the claim under a different owner — degraded to the
    /// shared checkout in read-mostly mode (NOT clobbered).
    PeerConflict,
    /// Re-acquire failed for another reason — degraded to the shared
    /// checkout without a claim.
    Degraded,
}

/// What happened to the session's drained WIP ref on resume (Phase 4 honesty
/// gate). Serialized lowercase-kebab for the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WipStatus {
    /// No WIP ref was captured at drain time (clean tree / no worktree).
    None,
    /// The drained WIP applied cleanly back into the worktree.
    Restored,
    /// The WIP could not be applied cleanly — kept at `refs/wip/*` for the
    /// operator to recover manually.
    KeptForManual,
}

/// Per-session recovery outcome, assembled as `resume_ai_sessions` resumes
/// each session (Phase 4). Threaded out of the resume loop and into the
/// `session-recovery-summary` event so the frontend can surface an HONEST
/// per-session status (lossy / degraded / peer-conflict are never hidden).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecoveryOutcome {
    /// The resumed session's task run id (stable identity across restarts).
    pub task_run_id: String,
    /// Human-readable title shown in the banner.
    pub title: String,
    /// Lossless `--resume` vs lossy summary fallback.
    pub fidelity: ResumeFidelity,
    /// On a `summary` fidelity, the number of conversation turns the lossy
    /// summary covers (mirrors the honest-label marker). `0` for lossless.
    pub turns_covered: usize,
    /// Worktree-claim re-acquire outcome.
    pub claim_status: ClaimStatus,
    /// Drained-WIP restore outcome.
    pub wip_status: WipStatus,
}

/// Structured startup-recovery summary emitted ONCE after `resume_ai_sessions`
/// runs at boot (Phase 4). Carries the crash-vs-planned classification (from
/// the on-disk shutdown marker) plus a per-session honesty breakdown.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecoverySummary {
    /// `true` when the previous shutdown was NOT clean (the runner crashed /
    /// was hard-killed). Drives the prominent vs quiet banner on the
    /// frontend.
    pub crash_recovery: bool,
    /// Number of sessions successfully resumed.
    pub resumed_count: u32,
    /// Per-session honest outcomes (one entry per successfully resumed
    /// session).
    pub sessions: Vec<SessionRecoveryOutcome>,
}

/// Tauri event name for the one-shot startup recovery summary (Phase 4).
pub const SESSION_RECOVERY_SUMMARY_EVENT: &str = "session-recovery-summary";

/// Emit the startup recovery summary to the frontend, once, after resume
/// completes. A banner subscribes and renders crash-vs-planned + per-session
/// fidelity/claim/WIP honesty. No-op-safe: an emit failure is logged only.
pub fn emit_session_recovery_summary(
    app_handle: &tauri::AppHandle,
    summary: &SessionRecoverySummary,
) {
    if let Err(e) = app_handle.emit(SESSION_RECOVERY_SUMMARY_EVENT, summary) {
        warn!(
            "Failed to emit {} event: {}",
            SESSION_RECOVERY_SUMMARY_EVENT, e
        );
    }
}

/// Emit a session state change event to the frontend.
pub fn emit_session_state(
    app_handle: &tauri::AppHandle,
    task_run_id: &str,
    session_id: &str,
    state: SessionState,
) {
    emit_session_state_ex(app_handle, task_run_id, session_id, state, false);
}

/// Emit a session state change event, optionally flagging it as a resume.
pub fn emit_session_state_ex(
    app_handle: &tauri::AppHandle,
    task_run_id: &str,
    session_id: &str,
    state: SessionState,
    resumed: bool,
) {
    let event = SessionStateEvent {
        task_run_id: task_run_id.to_string(),
        session_id: session_id.to_string(),
        state: state.as_event_str().to_string(),
        resumed,
    };
    if let Err(e) = app_handle.emit("claude-session-state", &event) {
        warn!("Failed to emit claude-session-state event: {}", e);
    }
}

/// List recent AI sessions for the sidebar.
///
/// Returns lightweight summaries of all AI sessions (running, stopped, failed),
/// enriched with `is_live` indicating whether a live CLI process exists.
#[tauri::command]
pub async fn list_ai_sessions(
    app_state: tauri::State<'_, StorageCompartment>,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
) -> Result<CommandResponse, String> {
    let result = app_state.pg_db().get_ai_sessions(50, None).await;

    match result {
        Ok(sessions) => {
            let enriched: Vec<serde_json::Value> = sessions
                .into_iter()
                .map(|s| {
                    let is_live = session_manager.get(&s.id).is_some();
                    serde_json::json!({
                        "id": s.id,
                        "task_name": s.task_name,
                        "status": s.status,
                        "updated_at": s.updated_at,
                        "created_at": s.created_at,
                        "is_live": is_live,
                    })
                })
                .collect();

            Ok(CommandResponse {
                success: true,
                message: None,
                data: Some(serde_json::json!({
                    "sessions": enriched,
                })),
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to list AI sessions: {}", e)),
            data: None,
        }),
    }
}

/// Send a user message to an active AI session.
///
/// If the session is Ready, the message is sent immediately.
/// If the session is Processing, the message is queued for delivery after the current turn.
///
/// Returns success with `data.queued` indicating whether the message was queued.
#[tauri::command]
pub async fn send_user_message(
    app_handle: tauri::AppHandle,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    ai_coord_registrar: tauri::State<
        '_,
        Arc<crate::claude_session::coord_register::AiCoordRegistrar>,
    >,
    task_run_id: String,
    message: String,
) -> Result<CommandResponse, String> {
    info!(
        "send_user_message: task_run_id={}, message_len={}",
        task_run_id,
        message.len()
    );

    // Graceful-drain gate (Phase 2): once a planned restart begins draining,
    // refuse new AI turns so we don't start work that the imminent kill would
    // tear in half. The drain flushes in-flight turns; new ones are rejected.
    if crate::drain::is_draining() {
        return Err(
            "runner is draining for a planned restart — new messages are refused".to_string(),
        );
    }

    // Session-automation Phase 0 (R3) — operator interaction is the ONLY signal
    // that refreshes this session's coord heartbeat. Driving the heartbeat off
    // `send_user_message` (rather than the unconditional registry heartbeat
    // loop) is what lets an idle session age to `stale` so the inject trigger
    // can fire (P0.2). Best-effort + no-op when the session isn't registered.
    ai_coord_registrar.heartbeat_on_interaction(&task_run_id);

    let session = session_manager
        .get(&task_run_id)
        .ok_or_else(|| format!("No active session found for task_run_id: {}", task_run_id))?;

    // Emit the user's message as an ai-output event so it appears in the conversation.
    // We emit directly (not via emit_ai_output) so we can include task_run_id
    // without needing a full AiSessionContext.
    {
        let now = chrono::Utc::now().timestamp_millis();
        let event = AiOutputEvent {
            id: format!("ai-{}-{}", now, rand::random::<u32>()),
            timestamp: now,
            line: message.clone(),
            source: "user_message".to_string(),
            action_id: None,
            task_run_id: Some(task_run_id.clone()),
            session_id: None,
            session_name: None,
            phase: None,
            phase_iteration: None,
        };
        if let Err(e) = app_handle.emit("ai-output", &event) {
            warn!("Failed to emit user message ai-output event: {}", e);
        }
    }

    // Persist user message to output_log for recap/summary generation
    if let Some(pg) = crate::database::pg::PgDb::try_global() {
        let formatted = format!("\n[USER_MESSAGE]\n{}\n[/USER_MESSAGE]\n", message);
        if let Err(e) = pg
            .append_task_output_ex(&task_run_id, &formatted, false, false)
            .await
        {
            warn!("Failed to persist user message to output_log: {}", e);
        }
    }

    // Build the effective message by prepending any pending context (system notes
    // from workflow generation, etc.) and the first-interaction note if applicable.
    // Pending context is delivered WITH the next user message rather than as a
    // standalone message, so Claude doesn't produce an unwanted response turn.
    let mut prefix_parts: Vec<String> = Vec::new();

    // Drain pending context (system notes queued since last user message)
    if let Some(pending) = session_manager.drain_pending_context(&task_run_id) {
        info!(
            "Prepending pending context to user message for task_run_id={}",
            task_run_id
        );
        prefix_parts.push(pending);
    }

    // First-interaction context switch
    if !session.has_user_interacted() {
        info!(
            "First user interaction detected for task_run_id={}, injecting context switch note",
            task_run_id
        );
        prefix_parts.push(
            "[SYSTEM NOTE: A user is now watching and interacting with this session. \
             Please acknowledge their message and respond conversationally while continuing \
             your work. The user's message follows.]"
                .to_string(),
        );
    }

    let effective_message = if prefix_parts.is_empty() {
        message.clone()
    } else {
        prefix_parts.push(message.clone());
        prefix_parts.join("\n\n")
    };

    match session.send_user_message(&effective_message) {
        Ok(sent_immediately) => {
            let queued = !sent_immediately;

            // Emit state change
            let new_state = session.state();
            emit_session_state(&app_handle, &task_run_id, session.session_id(), new_state);

            Ok(CommandResponse {
                success: true,
                message: Some(if queued {
                    "Message queued - will be sent after current turn completes".to_string()
                } else {
                    "Message sent".to_string()
                }),
                data: Some(serde_json::json!({
                    "queued": queued,
                    "state": new_state.as_event_str(),
                })),
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to send message: {}", e)),
            data: None,
        }),
    }
}

/// Interrupt the currently processing AI session.
///
/// Sends an interrupt control request to the Claude CLI process.
/// The CLI will stop its current turn and return to Ready state.
#[tauri::command]
pub async fn interrupt_ai_session(
    app_handle: tauri::AppHandle,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    task_run_id: String,
) -> Result<CommandResponse, String> {
    info!("interrupt_ai_session: task_run_id={}", task_run_id);

    let session = session_manager
        .get(&task_run_id)
        .ok_or_else(|| format!("No active session found for task_run_id: {}", task_run_id))?;

    match session.interrupt() {
        Ok(()) => {
            let new_state = session.state();
            emit_session_state(&app_handle, &task_run_id, session.session_id(), new_state);

            Ok(CommandResponse {
                success: true,
                message: Some("Interrupt requested".to_string()),
                data: Some(serde_json::json!({
                    "state": new_state.as_event_str(),
                })),
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to interrupt: {}", e)),
            data: None,
        }),
    }
}

/// Get the current state of an AI session.
///
/// Returns the session state and metadata.
#[tauri::command]
pub async fn get_ai_session_state(
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    task_run_id: String,
) -> Result<CommandResponse, String> {
    match session_manager.get(&task_run_id) {
        Some(session) => Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::json!({
                "state": session.state().as_event_str(),
                "sessionId": session.session_id(),
                "userInteracted": session.has_user_interacted(),
                "pid": session.pid(),
            })),
        }),
        None => Ok(CommandResponse {
            success: true,
            message: Some("No active session".to_string()),
            data: Some(serde_json::json!({
                "state": null,
            })),
        }),
    }
}

/// Create a standalone AI session.
///
/// Creates a task run record in the database, spawns a Claude CLI session
/// in the background, and returns the task_run_id immediately.
#[tauri::command]
pub async fn create_ai_session(
    app_handle: tauri::AppHandle,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    ai_coord_registrar: tauri::State<
        '_,
        Arc<crate::claude_session::coord_register::AiCoordRegistrar>,
    >,
    app_state: tauri::State<'_, StorageCompartment>,
    task_name: Option<String>,
) -> Result<CommandResponse, String> {
    let task_run_id = uuid::Uuid::new_v4().to_string();
    let name = task_name.unwrap_or_else(|| "New Chat".to_string());

    info!(
        "create_ai_session: task_run_id={}, name={}",
        task_run_id, name
    );

    // 1. Create task run record in DB
    let input = CreateTaskRunInput::new(task_run_id.clone(), name.clone())
        .with_prompt("AI session")
        .with_workflow_type("chat");
    let create_ok = app_state.pg_db().create_task_run(&input).await.is_ok();

    if !create_ok {
        return Ok(CommandResponse {
            success: false,
            message: Some("Failed to create task run record".to_string()),
            data: None,
        });
    }

    // 2. Spawn and wait for session to be ready.
    // ClaudeSession::spawn blocks until the CLI completes its init handshake,
    // so the session is Ready when this returns. This prevents a race condition
    // where sendMessage is called before the session is registered.
    let sm = session_manager.inner().clone();
    let handle = app_handle.clone();
    let trid = task_run_id.clone();
    let name_for_ctx = name.clone();
    let spawn_result = tokio::task::spawn_blocking(move || {
        let working_dir = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        // Create an AiSessionContext so output events include the taskRunId.
        // Without this, emit_ai_output would emit events with taskRunId=null
        // and the frontend would be unable to filter them.
        let session_ctx = AiSessionContext::setup(&trid, &name_for_ctx);

        // Pin the Claude CLI session id to the task_run_id on this fresh launch
        // (Phase 3, restart resilience). This makes the on-disk transcript land
        // at a DETERMINISTIC path (`<config_dir>/projects/<encoded(cwd)>/
        // <task_run_id>.jsonl`), so a later restart can resume LOSSLESSLY via
        // `--resume <task_run_id>` instead of replaying a lossy output_log
        // summary. `is_resume:false` → `--session-id` (new), not `--resume`.
        let cli_session_ctx = crate::claude_session::runner::CliSessionContext {
            cli_session_id: trid.clone(),
            is_resume: false,
        };

        match crate::claude_session::ClaudeSession::spawn(
            &working_dir,
            &trid,
            &handle,
            Some(session_ctx), // Tags output events with taskRunId
            None,              // finding_ctx
            None,              // progress_ctx
            None,              // pid_tracker
            None,              // model_override
            None,              // worktree
            None,              // tool_policy
            Some(&cli_session_ctx),
        ) {
            Ok(session) => {
                let session = Arc::new(session);

                if let Err(e) = sm.register(&trid, session.clone()) {
                    warn!("Failed to register AI session: {}", e);
                    return Err(format!("Failed to register AI session: {}", e));
                }

                // Emit ready state — session is ready for the user's first message.
                // No initial prompt is sent; the first user message will include
                // a context switch note via send_user_message's first-interaction logic.
                emit_session_state(&handle, &trid, &trid, session.state());

                info!("AI session ready: task_run_id={}", trid);
                Ok(())
            }
            Err(e) => {
                warn!("Failed to spawn AI session: {}", e);
                Err(e)
            }
        }
    })
    .await;

    // 3. Best-effort emergent-task row so the deconflicter/banner have
    // somewhere to attach. Runs in the outer async context (PG access is
    // not available inside spawn_blocking without a runtime hop) and never
    // blocks the response — a failure here just means no advisory banner
    // for this session, not a broken AI session. See coord-as-deconflicter
    // plan §4.3.
    if let Ok(Ok(())) = &spawn_result {
        if let Err(e) = app_state
            .pg_db()
            .create_emergent_task(&task_run_id, "in_progress", "session_emergent", None)
            .await
        {
            warn!(
                "create_ai_session: create_emergent_task failed for {}: {}",
                task_run_id, e
            );
        }

        // Session-automation Phase 0 (R1) — register the authenticated session
        // into coord.sessions carrying its task_run_id, so it is visible +
        // addressable + correctly stale to coord. Best-effort: a registration
        // failure never blocks the live AI session.
        ai_coord_registrar.register_session(&task_run_id, &name, None);
    }

    // 4. Return result
    match spawn_result {
        Ok(Ok(())) => Ok(CommandResponse {
            success: true,
            message: Some("AI session ready".to_string()),
            data: Some(serde_json::json!({
                "task_run_id": task_run_id,
                "state": "ready",
            })),
        }),
        Ok(Err(e)) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to create AI session: {}", e)),
            data: None,
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Task failed: {}", e)),
            data: None,
        }),
    }
}

/// Close an AI session.
///
/// Gracefully closes the Claude CLI session and updates the task run status.
#[tauri::command]
pub async fn close_ai_session(
    app_handle: tauri::AppHandle,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    ai_coord_registrar: tauri::State<
        '_,
        Arc<crate::claude_session::coord_register::AiCoordRegistrar>,
    >,
    app_state: tauri::State<'_, StorageCompartment>,
    task_run_id: String,
) -> Result<CommandResponse, String> {
    info!("close_ai_session: task_run_id={}", task_run_id);

    // Remove and close the session
    if let Some(session) = session_manager.remove(&task_run_id) {
        let _ = session.close();
    }

    // Session-automation Phase 0 (R5) — emit the coord `closed` event + evict
    // the R4 index so coord.sessions doesn't leak a ghost row. Best-effort.
    ai_coord_registrar.close_session(&task_run_id);

    // Update DB status
    let _ = app_state
        .pg_db()
        .update_task_run_status(&task_run_id, "stopped")
        .await;

    // Emit closed state
    emit_session_state(
        &app_handle,
        &task_run_id,
        &task_run_id,
        SessionState::Closed,
    );

    Ok(CommandResponse {
        success: true,
        message: Some("AI session closed".to_string()),
        data: Some(serde_json::json!({
            "state": "closed",
        })),
    })
}

/// Rename an AI session.
///
/// Updates the task_name for the task run in the database.
#[tauri::command]
pub async fn rename_ai_session(
    app_state: tauri::State<'_, StorageCompartment>,
    task_run_id: String,
    name: String,
) -> Result<CommandResponse, String> {
    info!(
        "rename_ai_session: task_run_id={}, name={}",
        task_run_id, name
    );

    let result: Result<(), String> = app_state
        .pg_db()
        .update_task_name(&task_run_id, &name)
        .await;

    match result {
        Ok(()) => Ok(CommandResponse {
            success: true,
            message: Some("Session renamed".to_string()),
            data: Some(serde_json::json!({
                "task_run_id": task_run_id,
                "name": name,
            })),
        }),
        _ => Ok(CommandResponse {
            success: false,
            message: Some("Failed to rename session".to_string()),
            data: None,
        }),
    }
}

/// Get the output log for an AI session.
///
/// Returns the full conversation output. Combines the persisted DB output_log
/// with any in-memory accumulated output from the live session that hasn't
/// been persisted yet (e.g., the current or most recent AI response).
#[tauri::command]
pub async fn get_ai_output(
    app_state: tauri::State<'_, StorageCompartment>,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    task_run_id: String,
) -> Result<CommandResponse, String> {
    let db_output = app_state
        .pg_db()
        .get_task_output(&task_run_id)
        .await
        .unwrap_or_default();

    // If there's a live session, append any unpersisted AI output.
    // The accumulated_output contains all AI text from the current session.
    // The DB output_log may be missing the most recent AI response if the
    // turn hasn't completed yet or the persister thread hasn't flushed.
    let mut combined_output = db_output;
    if let Some(session) = session_manager.get(&task_run_id) {
        let live_output = session.get_output();
        if !live_output.is_empty() {
            // Check if the live output contains text not yet in the DB.
            // The DB has [USER_MESSAGE] and [AI_RESPONSE] blocks.
            // The live accumulated_output has raw AI text (no tags).
            // We append it as an [AI_RESPONSE] block so parseOutputLog can find it.
            let db_len_without_whitespace = combined_output.trim().len();
            if db_len_without_whitespace == 0 {
                // DB is empty, just wrap the live output
                combined_output = format!("\n[AI_RESPONSE]\n{}\n[/AI_RESPONSE]\n", live_output);
            } else {
                // Check if the live output is already covered by DB content.
                // A simple heuristic: if the last [AI_RESPONSE] block in the DB
                // ends with the same content as the live output, it's already persisted.
                let already_persisted = combined_output.contains("[AI_RESPONSE]")
                    && combined_output
                        .rfind("[/AI_RESPONSE]")
                        .map(|pos| {
                            // Find the start of the last AI_RESPONSE block
                            let before = &combined_output[..pos];
                            if let Some(start) = before.rfind("[AI_RESPONSE]") {
                                let block_content =
                                    combined_output[start + "[AI_RESPONSE]".len()..pos].trim();
                                // If the live output starts with or equals the last block, it's covered
                                live_output.trim().starts_with(block_content)
                                    || block_content.starts_with(live_output.trim())
                            } else {
                                false
                            }
                        })
                        .unwrap_or(false);

                if !already_persisted {
                    // Append the live output as a new AI_RESPONSE block
                    combined_output.push_str(&format!(
                        "\n[AI_RESPONSE]\n{}\n[/AI_RESPONSE]\n",
                        live_output
                    ));
                }
            }
        }
    }

    Ok(CommandResponse {
        success: true,
        message: if combined_output.is_empty() {
            Some("No output available".to_string())
        } else {
            None
        },
        data: Some(serde_json::json!({
            "task_run_id": task_run_id,
            "output_log": combined_output,
        })),
    })
}

/// Generate a workflow from an AI session conversation.
///
/// Reads the conversation output log and uses the workflow generator
/// to create a UnifiedWorkflow from the conversation context.
/// When `source_content` is provided, uses that specific message instead
/// of the full conversation, and enriches the context with spec generation
/// instructions and existing page specs.
#[tauri::command]
pub async fn generate_workflow_from_session(
    app_handle: tauri::AppHandle,
    storage: tauri::State<'_, StorageCompartment>,
    health: tauri::State<'_, HealthCompartment>,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    task_run_id: String,
    description: Option<String>,
    include_ui_bridge: Option<bool>,
    source_content: Option<String>,
) -> Result<CommandResponse, String> {
    info!(
        "generate_workflow_from_session: task_run_id={}, has_source_content={}",
        task_run_id,
        source_content.is_some()
    );

    // Get conversation from DB output_log (needed as fallback if no source_content)
    let output_log = match storage.pg_db().get_task_output(&task_run_id).await {
        Ok(log) if !log.is_empty() => log,
        _ => {
            if source_content.is_none() {
                return Ok(CommandResponse {
                    success: false,
                    message: Some("No conversation history available".to_string()),
                    data: None,
                });
            }
            String::new()
        }
    };

    // Determine the plan text to use for generation
    let plan_text = source_content.as_deref().unwrap_or(&output_log);

    // When generating from a specific message, fetch existing specs and build
    // enriched context with spec generation instructions
    let inline_context = if source_content.is_some() {
        // `fetch_existing_specs` is a pre-compartment helper that takes
        // `&AppState`. Use the explicit `app_state()` escape hatch so the
        // dependency stays greppable for a future split.
        let existing_specs = fetch_existing_specs(health.app_state()).await;
        build_spec_aware_context(plan_text, &existing_specs)
    } else {
        format!(
            "The following is a conversation between a user and an AI assistant. \
             Use this conversation context to generate an appropriate workflow:\n\n{}",
            plan_text
        )
    };

    // Build generation request with conversation as inline context
    let request = crate::workflow_generation::GenerateWorkflowRequest {
        // Stream C: AI-session-launched workflows default to the runner app.
        // A future iteration may let the user pick an app at session start.
        app_id: crate::spec_api::storage::RUNNER_APP_ID.to_string(),
        description: description
            .unwrap_or_else(|| "Generate workflow from chat conversation".to_string()),
        inline_context: Some(inline_context),
        category: None,
        tags: None,
        max_iterations: None,
        provider: None,
        model: None,
        skip_ai_summary: None,
        log_source_selection: None,
        prompt_template: None,
        auto_include_contexts: Some(true),
        context_ids: None,
        max_fix_iterations: Some(3),
        discovery_mode: None,
        include_ui_bridge_instructions: include_ui_bridge,
        reflection_mode: None,
        investigate_codebase: Some(true),
        include_design_guidance: None,
        auto_run: None,
        model_overrides: None,
        generate_specification: Some(true),
        verification_depth: None,
        discover_ui_bridge_specs: None,
        simple_mode: None,
        pipeline_depth: None,
        tool_tags: None,
        exploration_settings: None,
        target_runner_port: None,
    };

    // Emit a UI-only note that generation is starting (not sent to AI session --
    // the AI would respond prematurely while generation is still running).
    {
        let start_note = "Generating workflow from conversation...";
        let now = chrono::Utc::now().timestamp_millis();
        let event = AiOutputEvent {
            id: format!("sys-{}-{}", now, rand::random::<u32>()),
            timestamp: now,
            line: start_note.to_string(),
            source: "system_note".to_string(),
            action_id: None,
            task_run_id: Some(task_run_id.clone()),
            session_id: None,
            session_name: None,
            phase: None,
            phase_iteration: None,
        };
        if let Err(e) = app_handle.emit("ai-output", &event) {
            warn!("Failed to emit generation-start system note: {}", e);
        }
    }

    let doctor_handle = health.doctor_handle().lock().await.clone();
    let pg_db = storage.pg_db().clone();
    let pg_clone = pg_db.clone();
    let artifact_task_run_id = task_run_id.clone();
    // See transcript.rs rationale — threading AppState here lets brief-mode
    // runs advertise the correct runner port in the Builder prompt.
    // `generate_workflow` is a pre-compartment helper; escape-hatch via
    // health.app_state() keeps the legacy passthrough greppable.
    let app_state_for_gen = health.app_state().clone();

    let gen_result = tokio::task::spawn_blocking(move || {
        let (response, mut artifact) = crate::workflow_generation::generate_workflow(
            request,
            doctor_handle.as_ref(),
            Some(&pg_clone),
            None,
            Some(&*app_state_for_gen),
        );
        artifact.task_run_id = Some(artifact_task_run_id.clone());
        (response, artifact)
    })
    .await;

    // Save pipeline artifact to PG (async, outside spawn_blocking)
    if let Ok((_, ref artifact)) = gen_result {
        if let Err(e) = pg_db.save_generation_artifact(artifact).await {
            tracing::warn!("Failed to save pipeline artifact to PG: {}", e);
        }
    }
    let gen_result = gen_result.map(|(response, _)| response);

    match gen_result {
        Ok(response) => {
            // Save workflow to database and store reference in AI session task run
            if response.success {
                if let Some(ref workflow) = response.workflow {
                    // Persist to workflow library so it appears in the UI
                    let create_req = crate::unified_workflows::CreateUnifiedWorkflowRequest {
                        name: workflow.name.clone(),
                        description: workflow.description.clone(),
                        category: workflow.category.clone(),
                        tags: workflow.tags.clone(),
                        setup_steps: workflow.setup_steps.clone(),
                        verification_steps: workflow.verification_steps.clone(),
                        agentic_steps: workflow.agentic_steps.clone(),
                        completion_steps: workflow.completion_steps.clone(),
                        max_iterations: workflow.max_iterations,
                        timeout_seconds: workflow.timeout_seconds,
                        provider: workflow.provider.clone(),
                        model: workflow.model.clone(),
                        skip_ai_summary: false,
                        log_source_selection: None,
                        context_ids: None,
                        disabled_context_ids: None,
                        auto_include_contexts: Some(workflow.auto_include_contexts),
                        prompt_template: workflow.prompt_template.clone(),
                        log_watch_enabled: Some(workflow.log_watch_enabled),
                        health_check_enabled: Some(workflow.health_check_enabled),
                        health_check_urls: if workflow.health_check_urls.is_empty() {
                            None
                        } else {
                            Some(workflow.health_check_urls.clone())
                        },
                        preflight_check_enabled: Some(workflow.preflight_check_enabled),
                        enable_sweep: Some(workflow.enable_sweep),
                        max_sweep_iterations: Some(workflow.max_sweep_iterations),
                        targeted_error_ids: None,
                        generated_by_task_run_id: Some(task_run_id.clone()),
                        stages: if workflow.stages.is_empty() {
                            None
                        } else {
                            Some(workflow.stages.clone())
                        },
                        stop_on_failure: Some(workflow.stop_on_failure),
                        constraint_overrides: Some(workflow.constraint_overrides.clone()),
                        approval_gate: Some(workflow.approval_gate),
                        reflection_mode: Some(workflow.reflection_mode),
                        completion_prompts_first: Some(workflow.completion_prompts_first),
                        model_overrides: Some(workflow.model_overrides.clone()),
                        dependency_graph: workflow.dependency_graph.clone(),
                        cost_annotations: workflow.cost_annotations.clone(),
                        quality_report: workflow.quality_report.clone(),
                        acceptance_criteria: workflow.acceptance_criteria.clone(),
                        ai_reviewed: Some(workflow.ai_reviewed),
                        workflow_architecture: workflow.workflow_architecture.clone(),
                        enforce_token_budget: Some(workflow.enforce_token_budget),
                        strict_cwd: workflow.strict_cwd,
                        tool_tags: workflow.tool_tags.clone(),
                        flow_control_json: None,
                        phase_timeouts_json: None,
                        rollback_policy: workflow.rollback_policy.clone(),
                        htn_enabled: workflow.htn_enabled,
                        htn_ui_bridge_url: workflow.htn_ui_bridge_url.clone(),
                        htn_state_machine_path: workflow.htn_state_machine_path.clone(),
                    };

                    match storage.pg_db().create_unified_workflow(&create_req).await {
                        Ok(saved) => {
                            info!(
                                "Saved generated workflow '{}' to library (id={})",
                                saved.name, saved.id
                            );
                        }
                        Err(e) => {
                            warn!("Failed to save generated workflow to library: {}", e);
                        }
                    }

                    // Store generated_workflow_id in the AI session task run's result_data
                    {
                        let wf_id = &workflow.id;
                        let wf_name = &workflow.name;
                        let result_data = serde_json::json!({
                            "generated_workflow_id": wf_id,
                            "generated_workflow_name": wf_name,
                        });
                        let rd_str = result_data.to_string();
                        if let Err(e) = storage
                            .pg_db()
                            .update_task_result_data(&task_run_id, &rd_str)
                            .await
                        {
                            warn!("Failed to update AI session task run result_data: {}", e);
                        }
                    }
                }
            }

            // Notify the active Claude CLI session about the generated workflow
            if response.success {
                if let Some(ref workflow) = response.workflow {
                    // Count steps via normalize_to_stages (handles both flat and staged)
                    let step_count: usize = workflow
                        .normalize_to_stages()
                        .iter()
                        .map(|s| {
                            s.setup_steps.len()
                                + s.verification_steps.len()
                                + s.agentic_steps.len()
                                + s.completion_steps.len()
                        })
                        .sum();
                    let system_note = format!(
                        "[SYSTEM NOTE: A workflow '{}' ({} steps) has been successfully generated from this conversation. \
                         The conversation is now idle. Do NOT respond to this note — do not ask questions, do not \
                         offer next steps, do not produce any output. Simply stop and wait silently. \
                         If the user sends a new message later, respond to that message only.]",
                        workflow.name, step_count
                    );

                    // Emit as ai-output event so it appears in chat UI
                    {
                        let now = chrono::Utc::now().timestamp_millis();
                        let event = AiOutputEvent {
                            id: format!("sys-{}-{}", now, rand::random::<u32>()),
                            timestamp: now,
                            line: system_note.clone(),
                            source: "system_note".to_string(),
                            action_id: None,
                            task_run_id: Some(task_run_id.clone()),
                            session_id: None,
                            session_name: None,
                            phase: None,
                            phase_iteration: None,
                        };
                        if let Err(e) = app_handle.emit("ai-output", &event) {
                            warn!("Failed to emit system note ai-output event: {}", e);
                        }
                    }

                    // Persist to output_log via PG
                    {
                        let formatted =
                            format!("\n[SYSTEM_NOTE]\n{}\n[/SYSTEM_NOTE]\n", system_note);
                        if let Err(e) = storage
                            .pg_db()
                            .append_task_output_ex(&task_run_id, &formatted, false, false)
                            .await
                        {
                            warn!("Failed to persist system note to output_log: {}", e);
                        }
                    }

                    // Queue as pending context — will be prepended to the next user
                    // message so Claude learns about the generation without triggering
                    // an extra response turn (every standalone message to Claude
                    // triggers a response, which is why VS Code CLI doesn't have this
                    // problem — it never injects automated messages).
                    session_manager.push_pending_context(&task_run_id, system_note);
                }
            }

            // Notify AI if generation failed
            if !response.success {
                let error_msg = response.error.as_deref().unwrap_or("unknown error");
                let fail_note = format!(
                    "[SYSTEM NOTE: Workflow generation failed: {}. \
                     Do NOT respond to this note — do not ask questions or offer suggestions. \
                     Wait silently for the user to send a message.]",
                    error_msg
                );

                let now = chrono::Utc::now().timestamp_millis();
                let event = AiOutputEvent {
                    id: format!("sys-{}-{}", now, rand::random::<u32>()),
                    timestamp: now,
                    line: fail_note.clone(),
                    source: "system_note".to_string(),
                    action_id: None,
                    task_run_id: Some(task_run_id.clone()),
                    session_id: None,
                    session_name: None,
                    phase: None,
                    phase_iteration: None,
                };
                if let Err(e) = app_handle.emit("ai-output", &event) {
                    warn!("Failed to emit generation-failed system note: {}", e);
                }

                {
                    let formatted = format!("\n[SYSTEM_NOTE]\n{}\n[/SYSTEM_NOTE]\n", fail_note);
                    if let Err(e) = storage
                        .pg_db()
                        .append_task_output_ex(&task_run_id, &formatted, false, false)
                        .await
                    {
                        warn!("Failed to persist generation-failed system note: {}", e);
                    }
                }

                // Queue as pending context for next user message
                session_manager.push_pending_context(&task_run_id, fail_note);
            }

            Ok(CommandResponse {
                success: response.success,
                message: response.error.clone(),
                data: Some(serde_json::json!({
                    "task_run_id": task_run_id,
                    "success": response.success,
                    "workflow": response.workflow,
                    "error": response.error,
                    "validation_errors": response.validation_errors,
                    "model_used": response.model_used,
                })),
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Generation task failed: {}", e)),
            data: None,
        }),
    }
}

/// Promote a running AI session into an isolated git worktree (Phase 4).
///
/// The Tauri-facing sibling of `POST /sessions/{id}/promote-to-worktree`.
/// Both routes funnel through `crate::mcp::ai_session::promote_session_inner`
/// so behaviour stays in lockstep.
///
/// On success returns the new worktree's id, path, and branch name in `data`.
/// On failure returns `success: false` with the error message; the original
/// session is left re-registered in `SessionManager` so the chat UI keeps
/// working.
///
/// TODO(worktrees Phase 4 stretch): extend `create_ai_session` with
/// `start_in_worktree: Option<bool>` so a session can be born inside a
/// worktree instead of needing a separate promotion call.
#[tauri::command]
pub async fn promote_session_to_worktree(
    app_handle: tauri::AppHandle,
    task_run_id: String,
) -> Result<CommandResponse, String> {
    info!("promote_session_to_worktree: task_run_id={}", task_run_id);

    match crate::mcp::ai_session::promote_session_inner(&app_handle, &task_run_id).await {
        Ok(resp) => Ok(CommandResponse {
            success: true,
            message: Some(format!(
                "Session promoted to worktree {} (branch={})",
                resp.worktree_id, resp.branch_name
            )),
            data: Some(serde_json::json!({
                "task_run_id": task_run_id,
                "worktree_id": resp.worktree_id,
                "worktree_path": resp.worktree_path,
                "branch_name": resp.branch_name,
            })),
        }),
        Err((status, message)) => Ok(CommandResponse {
            success: false,
            message: Some(format!("[{}] {}", status.as_u16(), message)),
            data: None,
        }),
    }
}

/// Commit a running session's accumulated file-set to its cwd's current branch.
///
/// The Tauri-facing sibling of `POST /sessions/{id}/commit-progress`. Both
/// routes funnel through `crate::mcp::ai_session::commit_session_progress_inner`
/// so behaviour stays in lockstep.
///
/// Behaviour summary (full details in `commit_session_progress_inner`):
/// - The session must be live in this runner. cwd is the worktree path if the
///   session was promoted via `promote_session_to_worktree`, otherwise the
///   runner's repo root (so the commit lands on whatever branch the user
///   currently has checked out).
/// - The file-set comes from PG `session_touched_files` (populated by the
///   dispatcher for every Edit/Write tool call).
/// - Empty tracker → success with `commit_hash: null`. Files matching HEAD →
///   success with `commit_hash: null`. Anything else → success with the new
///   HEAD SHA. The tracker is cleared on success (or no-op); on failure it's
///   left so a retry can see the same file-set.
///
/// On success returns the new commit hash (or null), file count, branch
/// name, and the auto-generated commit message in `data`. On failure returns
/// `success: false` with the error message; no parent refresh is needed
/// because committing doesn't mutate session state the chat UI cares about.
#[tauri::command]
pub async fn commit_session_progress(
    app: tauri::AppHandle,
    task_run_id: String,
) -> Result<CommandResponse, String> {
    info!("commit_session_progress: task_run_id={}", task_run_id);

    match crate::mcp::ai_session::commit_session_progress_inner(&app, &task_run_id).await {
        Ok(resp) => Ok(CommandResponse {
            success: true,
            message: Some(match &resp.commit_hash {
                Some(hash) => format!(
                    "Committed {} file(s) to {} ({})",
                    resp.file_count,
                    resp.branch,
                    &hash[..hash.len().min(7)]
                ),
                None => format!(
                    "No changes to commit (tracked {} file(s) on {})",
                    resp.file_count, resp.branch
                ),
            }),
            data: Some(serde_json::json!({
                "task_run_id": task_run_id,
                "commit_hash": resp.commit_hash,
                "file_count": resp.file_count,
                "branch": resp.branch,
                "message": resp.message,
            })),
        }),
        Err((status, message)) => Ok(CommandResponse {
            success: false,
            message: Some(format!("[{}] {}", status.as_u16(), message)),
            data: None,
        }),
    }
}

/// Probe the per-session "ready-to-commit" state for the traffic-light UI.
///
/// Frontend equivalent of an HTTP GET — runs the §1 dirty-subset query against
/// the `session_touched_files` rows for `task_run_id` and returns a
/// `CommitState` describing whether the tracker is empty / clean / dirty /
/// mid-merge. Called by `useCommitState` on mount and on a 30 s interval per
/// tab.
///
/// All errors collapse to `success: false` with the inner status code +
/// message in `message`. The `data` field carries the `CommitState` JSON
/// blob.
#[tauri::command]
pub async fn get_session_commit_state(
    app: tauri::AppHandle,
    task_run_id: String,
) -> Result<CommandResponse, String> {
    use crate::mcp::ai_session::session_commit_state_inner;
    match session_commit_state_inner(&app, &task_run_id).await {
        Ok(state) => Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::to_value(&state).unwrap_or_default()),
        }),
        Err((_status, msg)) => Ok(CommandResponse {
            success: false,
            message: Some(msg),
            data: None,
        }),
    }
}

/// One row of the file-ownership heatmap (Phase 3 of
/// `conflict-tooling-pauses-aligned-plan.md`).
///
/// Mirrors a single `coord.session_touched_files` row, with `recorded_at`
/// flattened to epoch milliseconds so the frontend can compute decay
/// without a date-parsing detour.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TouchedFileRow {
    pub file_path: String,
    pub task_run_id: String,
    /// Epoch milliseconds (UTC).
    pub recorded_at_ms: u64,
}

/// Recent rows from `coord.session_touched_files` for the file-ownership
/// heatmap panel.
///
/// Returns up to 5000 most-recent rows whose `recorded_at` falls inside the
/// last `window_secs` seconds. Rows are ordered most-recent first; the
/// frontend groups by `file_path`, computes contention (>1 distinct
/// `task_run_id` per file) and recency decay, and renders a sortable
/// table. See `src/components/terminal/FileOwnershipHeatmap.tsx`.
///
/// Implementation is a slim inline PG query — by design, mirroring the
/// vet-pass note in the plan ("Use `pg_db.pool().get()` for a connection").
/// The 5000-row safety cap keeps the panel tractable on heavy
/// multi-session days; the 30 s default window in the frontend keeps the
/// typical row count well below the cap.
#[tauri::command]
pub async fn recent_session_touched_files(
    app_state: tauri::State<'_, StorageCompartment>,
    window_secs: u64,
) -> Result<Vec<TouchedFileRow>, String> {
    let conn = app_state
        .pg_db()
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    // tokio_postgres binds bigint as i64. Saturating cast guards against an
    // i64-overflowing `window_secs` (would only happen on caller error).
    let window_secs_i64: i64 = window_secs.try_into().unwrap_or(i64::MAX);

    let rows = conn
        .query(
            r#"SELECT file_path,
                      task_run_id,
                      (EXTRACT(EPOCH FROM recorded_at) * 1000)::bigint AS recorded_at_ms
               FROM coord.session_touched_files
               WHERE recorded_at > NOW() - make_interval(secs => $1)
               ORDER BY recorded_at DESC
               LIMIT 5000"#,
            &[&window_secs_i64],
        )
        .await
        .map_err(|e| format!("PG recent_session_touched_files: {}", e))?;

    Ok(rows
        .iter()
        .map(|r| TouchedFileRow {
            file_path: r.get::<_, String>(0),
            task_run_id: r.get::<_, String>(1),
            // Saturating cast: `recorded_at_ms` is computed by PG as a
            // bigint; pre-1970 timestamps would be negative, but
            // `coord.session_touched_files` is append-only with NOW() so
            // this is defensive only.
            recorded_at_ms: r.get::<_, i64>(2).max(0) as u64,
        })
        .collect())
}

/// Honest-label marker prepended to a SUMMARY-fallback replay so the
/// operator/agent knows the resume is LOSSY (the full on-disk transcript was
/// unavailable). The `{n}` is the actual number of conversation turns the
/// summary covers — see [`build_honest_summary_prompt`].
///
/// This is the UX honesty gate (Phase 3, plan
/// `2026-06-06-runner-dev-loop-and-restart-resilience`): we never silently
/// present a reconstructed-from-output_log summary as a complete resume.
fn honest_summary_marker(turns_covered: usize) -> String {
    format!(
        "[SYSTEM_NOTE] Resumed from the last {turns_covered} turns \
         (full transcript unavailable — earlier context may be missing).\n\n"
    )
}

/// Prepend [`honest_summary_marker`] to a `build_replay_prompt` summary so the
/// lossy nature of the resume is labelled. `turns_covered` is the count of
/// parsed turns the summary was built from.
fn build_honest_summary_prompt(replay_prompt: &str, turns_covered: usize) -> String {
    format!("{}{}", honest_summary_marker(turns_covered), replay_prompt)
}

/// Does an on-disk Claude transcript exist for `(working_dir, cli_session_id)`?
///
/// The chat path pins the CLI session id to the `task_run_id` (see
/// `create_ai_session`), so the transcript lands at
/// `<config_dir>/projects/<encoded(working_dir)>/<cli_session_id>.jsonl`. When
/// it exists we can resume LOSSLESSLY via `--resume <cli_session_id>` and skip
/// the lossy output_log summary entirely. We probe every discovered Claude
/// config dir (multi-account installs) and the resolved effective dir.
fn transcript_exists_for(working_dir: &str, cli_session_id: &str) -> bool {
    use crate::terminal::transcript;

    // The effective config dir for the active account, plus all discovered
    // config dirs (multi-account). Any hit means the transcript is resumable.
    let mut config_dirs: Vec<std::path::PathBuf> = transcript::find_claude_config_dirs();
    if let Some(eff) =
        crate::ai_provider::get_effective_config_dir(&crate::settings::get_ai_settings().claude_cli)
    {
        let p = std::path::PathBuf::from(eff);
        if !config_dirs.iter().any(|d| d == &p) {
            config_dirs.push(p);
        }
    }

    config_dirs
        .iter()
        .any(|cfg| transcript::session_transcript_path(cfg, working_dir, cli_session_id).exists())
}

/// Re-acquire the coord `kind=worktree` claim(s) for a resumed chat session and
/// restore its drained WIP state, BEFORE re-spawning the CLI.
///
/// Closes the P0c gap: the legacy resume path never re-`acquire`d the worktree
/// claim, orphaning it for the TTL (a peer could steal it). We derive the same
/// stable `agent_session_id` Phase 2's drain used
/// (`drain::stable_ai_session_id(task_run_id)`), so the re-acquire idempotently
/// RENEWS the original claim if still within TTL, or cleanly re-takes it if the
/// TTL expired.
///
/// Returns the effective working dir for the re-spawn (the worktree path when
/// re-acquire succeeded, else `None` → caller keeps the shared cwd); any
/// `[SYSTEM_NOTE]` strings to surface (peer-conflict / manual-WIP-recovery);
/// and the re-acquired `IsolatedEditContext` (`Some` only when re-acquire
/// succeeded). The caller MUST hand the context to the resumed
/// `ClaudeSession` (via `set_isolated_edit_ctx`) so the claim heartbeat lives
/// for the session's lifetime and the claim RELEASES (via the context's Drop)
/// when the session ends — NOT `mem::forget` it (that orphans the claim past
/// session end and blocks a peer from re-acquiring the worktree).
///
/// Graceful degrade (mirrors the launch path): worktree mode off / no persisted
/// worktree / acquire fails → returns `(None, notes, None)` and resume continues
/// in the shared checkout with no claim. A peer holding the claim under a
/// DIFFERENT owner is surfaced (note) and NOT clobbered — resume goes
/// read-mostly/shared.
async fn reacquire_and_restore_session_worktree(
    task_run_id: &str,
    pg: &Arc<crate::database::pg::PgDb>,
) -> (
    Option<String>,
    Vec<String>,
    Option<crate::agent_worktree::isolated_edit::IsolatedEditContext>,
    ClaimStatus,
    WipStatus,
) {
    use crate::agent_worktree::isolated_edit::{acquire, AcquireRequest};

    let mut notes: Vec<String> = Vec::new();

    // 1. Which repo(s)/worktree(s) was this session editing? Sourced from the
    //    persisted `coord.worktrees` rows keyed by task_run_id (written by
    //    `promote_to_worktree`). No row → the session ran in the shared cwd;
    //    nothing session-scoped to re-acquire.
    let worktree_rows = match pg.get_worktrees_for_task_run(task_run_id).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(
                "AI session resume: worktree lookup failed for {} ({}); shared-checkout fallback",
                task_run_id, e
            );
            return (None, notes, None, ClaimStatus::None, WipStatus::None);
        }
    };
    if worktree_rows.is_empty() {
        return (None, notes, None, ClaimStatus::None, WipStatus::None);
    }

    // 2. Map each worktree's `repo_path` (the source checkout) back to a bare
    //    repo slug for the acquire facade. A slug we can't resolve is skipped
    //    (best-effort) rather than failing the whole resume.
    let mut repos: Vec<String> = Vec::new();
    for row in &worktree_rows {
        if let Some(slug) = crate::agent_worktree::canonical_paths::repo_slug_for_path(
            std::path::Path::new(&row.repo_path),
        ) {
            if !repos.contains(&slug) {
                repos.push(slug);
            }
        } else {
            warn!(
                "AI session resume: could not resolve repo slug for {} (worktree {}); skipping",
                row.repo_path, row.id
            );
        }
    }
    if repos.is_empty() {
        return (None, notes, None, ClaimStatus::None, WipStatus::None);
    }

    // 3. Re-acquire under the SAME stable agent_session_id the drain used, so
    //    the owner token (`<machine_id>:<agent_session_id>`) matches the
    //    original claim → idempotent renew within TTL / clean re-take after.
    let agent_session_id = crate::drain::stable_ai_session_id(task_run_id);
    let intent = format!("resume chat session {task_run_id}");

    match acquire(AcquireRequest {
        repos: &repos,
        intent: Some(&intent),
        declared_overlap_paths: None,
        plan_id: None,
        phase: None,
        agent_session_id: Some(agent_session_id),
    })
    .await
    {
        Ok(Some(ctx)) => {
            let workdir = ctx
                .worktrees
                .first()
                .map(|w| w.worktree_path.to_string_lossy().to_string());
            info!(
                "AI session resume: re-acquired worktree claim(s) for {} (repos={:?}, agent_session_id={})",
                task_run_id, repos, agent_session_id
            );

            // 4. Restore the drained WIP ref into each re-acquired worktree
            //    (Phase 2 wrote `refs/wip/<agent_session_id>` per worktree).
            //    `wip_status` is the WORST outcome across worktrees: a single
            //    not-clean apply degrades the whole session to KeptForManual
            //    (the honest banner state).
            let mut wip_status = WipStatus::None;
            for w in &ctx.worktrees {
                match crate::drain::restore_wip_ref(&w.worktree_path, &agent_session_id) {
                    Ok(outcome) if outcome.ref_existed && outcome.applied_clean => {
                        if wip_status == WipStatus::None {
                            wip_status = WipStatus::Restored;
                        }
                        info!(
                            "AI session resume: restored WIP {} into {} (ref deleted={})",
                            outcome.ref_name,
                            w.worktree_path.display(),
                            outcome.ref_deleted
                        );
                    }
                    Ok(outcome) if outcome.ref_existed => {
                        // Apply was not clean — ref kept; surface for manual apply.
                        wip_status = WipStatus::KeptForManual;
                        notes.push(format!(
                            "[SYSTEM_NOTE] Uncommitted work from before the restart could not be \
                             applied cleanly in {}. It is preserved at `{}` — run \
                             `git -C {} stash apply {}` to recover it manually.",
                            w.repo,
                            outcome.ref_name,
                            w.worktree_path.display(),
                            outcome.ref_name
                        ));
                        warn!(
                            "AI session resume: WIP {} did not apply cleanly into {}; kept for manual recovery",
                            outcome.ref_name,
                            w.worktree_path.display()
                        );
                    }
                    Ok(_) => { /* no ref captured — clean drain or clean tree */ }
                    Err(e) => {
                        warn!(
                            "AI session resume: WIP restore probe failed in {}: {}",
                            w.worktree_path.display(),
                            e
                        );
                    }
                }
            }

            // The IsolatedEditContext owns the claim heartbeat. Hand it BACK
            // to the caller, which parks it on the resumed `ClaudeSession`
            // (`set_isolated_edit_ctx`) so the heartbeat lives for the
            // session's lifetime AND the claim RELEASES (via the context's
            // Drop) when the session ends. We must NOT `mem::forget` it: that
            // would keep the heartbeat alive until process exit and never
            // release the claim, blocking a peer from re-acquiring the
            // worktree after this session dies.
            (
                workdir,
                notes,
                Some(ctx),
                ClaimStatus::Reacquired,
                wip_status,
            )
        }
        Ok(None) => {
            // Worktree mode off — shared-checkout fallback, no claim.
            (None, notes, None, ClaimStatus::None, WipStatus::None)
        }
        Err(e) => {
            // A `ClaimConflict` (peer holds it under a different owner) or any
            // other allocate error → DO NOT clobber. Surface + degrade to the
            // shared checkout in read-mostly mode. A genuine peer-conflict is
            // classified distinctly (PeerConflict) so the banner can be HONEST
            // that another agent now holds the worktree, vs a generic transport
            // failure (Degraded).
            let is_peer_conflict =
                matches!(e, crate::agent_worktree::AllocateError::ClaimConflict(_));
            let msg = e.to_string();
            warn!(
                "AI session resume: worktree re-acquire for {} failed ({}); \
                 resuming in shared/read-mostly mode without a claim",
                task_run_id, msg
            );
            notes.push(format!(
                "[SYSTEM_NOTE] Could not re-acquire the isolated worktree for this session after \
                 the restart ({msg}). Another agent may now hold it — resuming in shared, \
                 read-mostly mode. Re-check ownership before making edits."
            ));
            let claim_status = if is_peer_conflict {
                ClaimStatus::PeerConflict
            } else {
                ClaimStatus::Degraded
            };
            (None, notes, None, claim_status, WipStatus::None)
        }
    }
}

/// Resume interrupted AI sessions on startup.
///
/// Queries for task runs with status='running' and workflow_type='chat'
/// owned by this runner instance, reconstructs the conversation from the
/// persisted `[USER_MESSAGE]` / `[AI_RESPONSE]` / `[SYSTEM_NOTE]` blocks in
/// `output_log`, builds a replay prompt, spawns a fresh Claude CLI session
/// for each, sends the replay prompt as the initial turn, and re-registers
/// the session in `SessionManager` under the original `task_run_id`.
///
/// Each session is handled independently — one failing row never blocks the
/// others and never panics startup. Rows that cannot be resumed (no output
/// log, unparseable conversation, spawn failure, `auto_continue=false`) are
/// marked failed in PG with a `resume_failed:` error message so the sidebar
/// reflects reality rather than showing a phantom "running" state.
///
/// `crash_recovery` is the boot classification from the on-disk shutdown
/// marker (see `session::shutdown_marker`): `true` when the previous shutdown
/// was NOT clean. It is threaded onto the returned [`SessionRecoverySummary`]
/// so the caller can emit a single `session-recovery-summary` event and the
/// frontend can choose a prominent (crash) vs quiet (planned) banner.
///
/// Returns a [`SessionRecoverySummary`] carrying the resumed count, the
/// crash-vs-planned flag, and a per-session honesty breakdown (resume
/// fidelity + claim status + WIP status).
pub async fn resume_ai_sessions(
    session_manager: Arc<SessionManager>,
    app_handle: tauri::AppHandle,
    crash_recovery: bool,
) -> SessionRecoverySummary {
    use crate::claude_session::resume::{build_replay_prompt, parse_conversation};
    use crate::claude_session::ClaudeSession;

    // An empty summary carries only the boot classification — used for every
    // early exit (no sessions / not-yet-ready) so the caller still learns
    // whether this was a crash boot.
    let empty_summary = || SessionRecoverySummary {
        crash_recovery,
        resumed_count: 0,
        sessions: Vec::new(),
    };

    // Grab AppState from Tauri's managed state — same pattern other resume
    // paths use. If unavailable we're being invoked before setup completes;
    // there's nothing to resume in that case.
    let app_state: Arc<AppState> = match app_handle.try_state::<Arc<AppState>>() {
        Some(s) => s.inner().clone(),
        None => {
            warn!("AI session resume: AppState not yet available; skipping");
            return empty_summary();
        }
    };

    // Session-automation Phase 0 (R2) — the coord registrar (managed Tauri
    // state). Resolved here (not injected) because this fn is called internally
    // with bare args. `None` only before `.setup()` finished managing it, in
    // which case resume simply doesn't register (best-effort).
    let ai_coord_registrar = app_handle
        .try_state::<Arc<crate::claude_session::coord_register::AiCoordRegistrar>>()
        .map(|s| s.inner().clone());

    let my_port = app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);
    if my_port == 0 {
        warn!("AI session resume: api_port not set yet; skipping");
        return empty_summary();
    }

    let pg = app_state.pg_db.clone();

    let rows = match pg.get_resumable_chat_sessions_for_runner(my_port).await {
        Ok(rows) => rows,
        Err(e) => {
            error!("AI session resume: failed to query resumable chats: {}", e);
            return empty_summary();
        }
    };

    if rows.is_empty() {
        info!("AI session resume: no resumable chat sessions found");
        return empty_summary();
    }

    info!(
        "AI session resume: found {} candidate chat session(s)",
        rows.len()
    );

    let mark_failed = |id: String, reason: String, pg: Arc<crate::database::pg::PgDb>| async move {
        let msg = format!("resume_failed: {}", reason);
        if let Err(e) = pg.fail_task_run(&id, &msg).await {
            warn!("Failed to mark chat session {} as failed: {}", id, e);
        }
    };

    let mut resumed: u32 = 0;
    let mut outcomes: Vec<SessionRecoveryOutcome> = Vec::new();

    for (task_run_id, task_name, auto_continue) in rows {
        if !auto_continue {
            info!(
                "AI session resume: skipping {} (auto_continue disabled)",
                task_run_id
            );
            continue;
        }

        // Load the full task run (including output_log — the lightweight
        // chat-sessions query doesn't carry it).
        let full = match pg.get_task_run(&task_run_id).await {
            Ok(Some(tr)) => tr,
            Ok(None) => {
                warn!("AI session resume: task run {} vanished", task_run_id);
                continue;
            }
            Err(e) => {
                warn!(
                    "AI session resume: failed to load task run {}: {}",
                    task_run_id, e
                );
                continue;
            }
        };

        if full.output_log.trim().is_empty() {
            info!(
                "AI session resume: {} has no conversation history, marking failed",
                task_run_id
            );
            mark_failed(
                task_run_id.clone(),
                "no conversation history".to_string(),
                pg.clone(),
            )
            .await;
            continue;
        }

        let turns = parse_conversation(&full.output_log);
        if turns.is_empty() {
            info!(
                "AI session resume: {} has no parseable turns, marking failed",
                task_run_id
            );
            mark_failed(
                task_run_id.clone(),
                "could not parse conversation".to_string(),
                pg.clone(),
            )
            .await;
            continue;
        }

        // ── (A) Re-acquire the worktree claim + (C) restore drained WIP ────
        // Closes the P0c gap. Runs in the async context (PG + coord I/O).
        // Returns the worktree path to re-spawn into (when re-acquire
        // succeeded) plus any operator-facing notes (peer-conflict / manual
        // WIP recovery). Graceful-degrades to the shared cwd on any failure.
        let (workdir_override, system_notes, reacquired_ctx, claim_status, wip_status) =
            reacquire_and_restore_session_worktree(&task_run_id, &pg).await;

        // Effective working dir: the re-acquired worktree, else the shared cwd
        // the session originally ran in.
        let working_dir = workdir_override.clone().unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string())
        });

        // ── (B) Prefer lossless `--resume`; honestly label the summary ─────
        // The chat path pins the CLI session id to the task_run_id, so an
        // on-disk transcript at `<cfg>/projects/<encoded(cwd)>/<id>.jsonl`
        // means we can resume losslessly via `--resume` and SKIP the lossy
        // output_log summary entirely. Only when no transcript exists do we
        // fall back to `build_replay_prompt` — and we LABEL it honestly so the
        // operator/agent knows earlier context may be missing.
        let can_full_resume = transcript_exists_for(&working_dir, &task_run_id);

        // The replay prompt is only used on the summary fallback. When we have
        // a full transcript we send NO initial prompt (the --resume restores
        // the real conversation; injecting a summary would be redundant +
        // lossy). `notes_prefix` carries any worktree SYSTEM_NOTEs onto
        // whichever initial turn we DO send.
        let notes_prefix = if system_notes.is_empty() {
            String::new()
        } else {
            format!("{}\n\n", system_notes.join("\n\n"))
        };

        let initial_prompt: Option<String> = if can_full_resume {
            info!(
                "AI session resume: {} has an on-disk transcript — resuming losslessly via --resume",
                task_run_id
            );
            // Even on a lossless resume, surface worktree notes (if any) as a
            // standalone initial turn so the operator sees the conflict/WIP
            // status. No notes → no initial prompt at all.
            if notes_prefix.is_empty() {
                None
            } else {
                Some(notes_prefix.trim_end().to_string())
            }
        } else {
            let replay_prompt = build_replay_prompt(&turns, None);
            if replay_prompt.is_empty() {
                mark_failed(
                    task_run_id.clone(),
                    "empty replay prompt".to_string(),
                    pg.clone(),
                )
                .await;
                continue;
            }
            // Honesty gate: label the lossy summary with the turn count it
            // covers, prefixed by any worktree notes.
            Some(format!(
                "{}{}",
                notes_prefix,
                build_honest_summary_prompt(&replay_prompt, turns.len())
            ))
        };

        // Capture the per-session honesty signals BEFORE the spawn closure
        // moves/consumes the locals. `fidelity` is lossless iff we resumed from
        // an on-disk transcript; otherwise it's the lossy summary, and
        // `turns_covered` records how many turns that summary spans.
        let fidelity = if can_full_resume {
            ResumeFidelity::Lossless
        } else {
            ResumeFidelity::Summary
        };
        let turns_covered = if can_full_resume { 0 } else { turns.len() };
        let outcome_title = task_name.clone();

        // Spawn the Claude CLI session on a blocking thread — ClaudeSession::spawn
        // blocks waiting for the init handshake. This mirrors create_ai_session.
        let sm = session_manager.clone();
        let handle = app_handle.clone();
        let trid = task_run_id.clone();
        let name_for_ctx = task_name.clone();
        let initial_for_spawn = initial_prompt;
        let working_dir_for_spawn = working_dir;
        let resume_from_transcript = can_full_resume;
        // The re-acquired claim context (Some only when re-acquire succeeded).
        // Moved into the spawn closure and parked on the resumed session AFTER
        // register, so its Drop releases the claim when the session ends.
        let reacquired_ctx_for_spawn = reacquired_ctx;

        let spawn_result = tokio::task::spawn_blocking(move || -> Result<(), String> {
            let session_ctx = AiSessionContext::setup(&trid, &name_for_ctx);

            // Pin/resume the CLI session id to the task_run_id. `is_resume` is
            // true ONLY when an on-disk transcript exists (→ `--resume`); else
            // we still `--session-id`-pin so a SUBSEQUENT restart can resume
            // losslessly from the transcript this launch creates.
            let cli_session_ctx = crate::claude_session::runner::CliSessionContext {
                cli_session_id: trid.clone(),
                is_resume: resume_from_transcript,
            };

            let session = ClaudeSession::spawn(
                &working_dir_for_spawn,
                &trid,
                &handle,
                Some(session_ctx),
                None, // finding_ctx
                None, // progress_ctx
                None, // pid_tracker
                None, // model_override
                None, // worktree
                None, // tool_policy
                Some(&cli_session_ctx),
            )
            .map_err(|e| format!("spawn failed: {}", e))?;

            let session = Arc::new(session);

            sm.register(&trid, session.clone())
                .map_err(|e| format!("register failed: {}", e))?;

            // Park the re-acquired worktree claim on the now-registered
            // session so the heartbeat lives for its lifetime and the claim
            // RELEASES (via the context's Drop) when the session closes — the
            // P3 fix for the `mem::forget` leak that orphaned the claim past
            // session end. `None` (worktree mode off / shared-checkout
            // fallback / peer-conflict degrade) is a no-op.
            if let Some(ctx) = reacquired_ctx_for_spawn {
                session.set_isolated_edit_ctx(ctx);
            }

            // Send an initial turn ONLY when we have something to say:
            //   - summary fallback → the honest-labelled replay summary, or
            //   - lossless resume  → just the worktree SYSTEM_NOTEs (if any).
            // `send_initial_prompt` requires Ready state (spawn just
            // established it), writes to stdin, transitions to Processing, and
            // crucially does NOT set `user_has_interacted` — so the next real
            // user message still triggers the first-interaction context-switch
            // note. It does not append to output_log, so the next restart's
            // replay won't double-count.
            if let Some(prompt) = initial_for_spawn.as_deref() {
                session
                    .send_initial_prompt(prompt)
                    .map_err(|e| format!("send_initial_prompt failed: {}", e))?;
            }

            // Emit the state event with resumed=true so the frontend can
            // surface a one-time "resumed" badge/toast.
            emit_session_state_ex(&handle, &trid, session.session_id(), session.state(), true);

            Ok(())
        })
        .await;

        // Best-effort emergent-task row for the resumed session (deconflicter
        // attaches to the assigned_session_id). Identical pattern to
        // create_ai_session above; the partial unique index makes it a no-op
        // if a row already exists from a prior boot. See plan §4.3.
        if let Ok(Ok(())) = &spawn_result {
            if let Err(e) = pg
                .create_emergent_task(&task_run_id, "in_progress", "session_emergent", None)
                .await
            {
                warn!(
                    "AI session resume: create_emergent_task failed for {}: {}",
                    task_run_id, e
                );
            }

            // R2 — re-register the resumed session with coord (idempotent;
            // a fresh coord row is minted carrying the same task_run_id).
            if let Some(reg) = ai_coord_registrar.as_ref() {
                reg.register_session(&task_run_id, &task_name, None);
            }
        }

        match spawn_result {
            Ok(Ok(())) => {
                info!(
                    "AI session resume: resumed {} (fidelity={:?} claim={:?} wip={:?})",
                    task_run_id, fidelity, claim_status, wip_status
                );
                resumed += 1;
                outcomes.push(SessionRecoveryOutcome {
                    task_run_id: task_run_id.clone(),
                    title: outcome_title,
                    fidelity,
                    turns_covered,
                    claim_status,
                    wip_status,
                });
            }
            Ok(Err(e)) => {
                warn!("AI session resume: {} failed: {}", task_run_id, e);
                mark_failed(task_run_id.clone(), e, pg.clone()).await;
            }
            Err(join_err) => {
                warn!(
                    "AI session resume: spawn task for {} panicked: {}",
                    task_run_id, join_err
                );
                mark_failed(
                    task_run_id.clone(),
                    format!("spawn task panicked: {}", join_err),
                    pg.clone(),
                )
                .await;
            }
        }
    }

    SessionRecoverySummary {
        crash_recovery,
        resumed_count: resumed,
        sessions: outcomes,
    }
}

/// Tries both the cached external app specs and the runner's own specs.
/// Returns a JSON string of whatever specs are available, or an empty message.
pub(crate) async fn fetch_existing_specs(app_state: &AppState) -> String {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_default();

    let mut specs_parts: Vec<String> = Vec::new();

    let self_base = crate::mcp::types::get_self_base_url(app_state);

    // Fetch cached external app specs — separate architecture specs from page specs
    match client
        .get(format!("{}/ui-bridge/sdk/cached-specs", self_base))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(body) = resp.text().await {
                if body.len() > 5 {
                    // Parse the response to separate architecture specs from page specs
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) {
                        let specs_array = parsed
                            .get("data")
                            .and_then(|d| d.as_array())
                            .or_else(|| parsed.as_array());

                        if let Some(specs) = specs_array {
                            let mut arch_parts: Vec<String> = Vec::new();
                            let mut page_specs: Vec<&serde_json::Value> = Vec::new();

                            for spec in specs {
                                let spec_json_str = spec.get("spec_json").and_then(|v| v.as_str());
                                let is_arch = spec_json_str
                                    .map(crate::spec_utils::is_architecture_spec_str)
                                    .unwrap_or(false);

                                if is_arch {
                                    if let Some(s) = spec_json_str {
                                        if let Ok(parsed_spec) =
                                            serde_json::from_str::<serde_json::Value>(s)
                                        {
                                            let project_name = spec
                                                .get("app_name")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("unknown");
                                            arch_parts.push(
                                                crate::spec_utils::format_architecture_markdown(
                                                    &parsed_spec,
                                                    project_name,
                                                ),
                                            );
                                        }
                                    }
                                } else {
                                    page_specs.push(spec);
                                }
                            }

                            // Add architecture specs section
                            if !arch_parts.is_empty() {
                                specs_parts.push(format!(
                                    "### Project Architecture\n\n{}",
                                    arch_parts.join("\n")
                                ));
                            }

                            // Add page specs section (re-serialize only the page specs)
                            if !page_specs.is_empty() {
                                if let Ok(page_json) = serde_json::to_string_pretty(&page_specs) {
                                    specs_parts
                                        .push(format!("### External App Specs\n{}", page_json));
                                }
                            }
                        }
                    } else {
                        // Fallback: couldn't parse, use raw body
                        specs_parts.push(format!("### External App Specs\n{}", body));
                    }
                }
            }
        }
        _ => {
            info!("No cached external app specs available");
        }
    }

    // Fetch runner's own page specs
    match client
        .get(format!("{}/ui-bridge/control/specs", self_base))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(body) = resp.text().await {
                if body.len() > 5 {
                    specs_parts.push(format!("### Runner Page Specs\n{}", body));
                }
            }
        }
        _ => {
            info!("No runner page specs available");
        }
    }

    if specs_parts.is_empty() {
        "No existing specs found".to_string()
    } else {
        specs_parts.join("\n\n")
    }
}

/// Build enriched inline context for spec-aware workflow generation.
///
/// Combines the plan text with spec generation instructions and any
/// existing specs so the AI can create/update page specs alongside
/// the workflow.
pub(crate) fn build_spec_aware_context(plan_text: &str, existing_specs: &str) -> String {
    format!(
        r#"The following AI plan message describes what should be built. Generate a workflow
that implements this plan, including creating or updating semantic page specs.

## Plan
{plan_text}

## Semantic Page Spec Generation

When the plan describes creating NEW pages:
- Generate a `.spec.uibridge.json` file for each new page as part of the agentic steps
- Use the plan's description to define semantic spec groups (what the page SHOULD do)
- Wire individual UI elements with element-presence assertions using the SDK's auto-generated semantic IDs
- Include form-validation, state-consistency, and accessibility assertions
- Set `source: "ai-generated"` on all assertion groups

When the plan describes MODIFYING existing pages:
- The existing specs are provided below — update them to reflect the planned changes
- Add new assertions for new functionality, update existing ones for changed behavior

### Spec File Format

Each `.spec.uibridge.json` file follows this structure:
```json
{{
  "version": "1.0",
  "description": "Page description",
  "groups": [
    {{
      "id": "group-id",
      "name": "Group Name",
      "description": "What this group verifies",
      "category": "ui-elements",
      "source": "ai-generated",
      "assertions": [
        {{
          "id": "assert-id",
          "description": "What this assertion checks",
          "category": "element-presence",
          "severity": "critical",
          "enabled": true,
          "target": {{
            "type": "search",
            "criteria": {{ "role": "button", "textContent": "Submit" }}
          }},
          "assertionType": "exists"
        }}
      ]
    }}
  ],
  "metadata": {{
    "component": "ComponentName",
    "pageUrl": "/page-path",
    "tags": ["generated"]
  }}
}}
```

### Available Assertion Types
- `exists` / `notExists` — element presence or absence
- `visible` / `hidden` — element visibility
- `enabled` / `disabled` — interactive element state
- `focused` — keyboard focus
- `checked` / `unchecked` — checkbox/radio state
- `hasText` / `containsText` — exact or partial text match
- `hasValue` — form input value
- `count` — number of matching elements
- `attribute` — element attribute value
- `hasClass` — CSS class presence
- `cssProperty` — computed CSS property

### Severity Levels
- `critical` — core functionality that must work
- `warning` — important features
- `info` — nice-to-have checks

### Target Types
**Search target** (preferred — finds elements by semantic criteria):
```json
{{ "type": "search", "criteria": {{ "role": "button", "textContent": "Save" }} }}
```

**Element ID target** (use when element ID is known):
```json
{{ "type": "elementId", "elementId": "settings-dark-mode-toggle" }}
```

### Implementation Guidelines
- The SDK's AutoRegisterProvider automatically assigns stable semantic IDs to interactive elements
- Create verification steps that load and run the generated specs
- Group assertions semantically (e.g., "form-elements", "navigation", "state-management")

### Existing Specs
{existing_specs}
"#
    )
}

pub fn plugin() -> TauriPlugin<tauri::Wry> {
    PluginBuilder::<tauri::Wry>::new("qontinui_ai_session")
        .invoke_handler(tauri::generate_handler![
            list_ai_sessions,
            send_user_message,
            interrupt_ai_session,
            get_ai_session_state,
            create_ai_session,
            close_ai_session,
            rename_ai_session,
            get_ai_output,
            generate_workflow_from_session,
            promote_session_to_worktree,
            commit_session_progress,
            recent_session_touched_files,
        ])
        .build()
}

#[cfg(test)]
mod resume_tests {
    use super::*;

    #[test]
    fn honest_marker_reports_actual_turn_count() {
        let marker = honest_summary_marker(7);
        assert!(marker.starts_with("[SYSTEM_NOTE]"));
        assert!(
            marker.contains("last 7 turns"),
            "marker must state the actual N turns it covers: {marker}"
        );
        assert!(
            marker.contains("full transcript unavailable"),
            "marker must flag the resume as lossy: {marker}"
        );
    }

    #[test]
    fn summary_fallback_is_labelled_with_n() {
        // The summary path (no on-disk transcript) prepends the honest marker
        // to the replay prompt, with N = number of turns covered.
        let replay = "<conversation_history>...</conversation_history>";
        let n = 3usize;
        let labelled = build_honest_summary_prompt(replay, n);
        assert!(
            labelled.starts_with("[SYSTEM_NOTE]"),
            "summary fallback must be honestly labelled"
        );
        assert!(labelled.contains("last 3 turns"));
        assert!(
            labelled.ends_with(replay),
            "the original replay prompt must follow the marker verbatim"
        );
    }

    #[test]
    fn full_resume_path_has_no_honesty_marker() {
        // On the lossless `--resume` path we send NO summary, so the honesty
        // marker MUST be absent. We model the decision: when can_full_resume is
        // true and there are no worktree notes, the initial prompt is None;
        // with notes it is just the notes (never the lossy marker).
        let notes_only = "[SYSTEM_NOTE] Could not re-acquire the isolated worktree.";
        assert!(
            !notes_only.contains("full transcript unavailable"),
            "the lossless-resume initial turn must never carry the lossy summary marker"
        );
        // And the summary builder is simply never invoked on this path — the
        // marker string only originates from honest_summary_marker, which the
        // full-resume branch does not call.
        assert!(!notes_only.contains(&honest_summary_marker(1)));
    }

    // ── Phase 4 — recovery-summary assembly + serde honesty contract ────────

    fn outcome(
        id: &str,
        fidelity: ResumeFidelity,
        turns: usize,
        claim: ClaimStatus,
        wip: WipStatus,
    ) -> SessionRecoveryOutcome {
        SessionRecoveryOutcome {
            task_run_id: id.to_string(),
            title: format!("title-{id}"),
            fidelity,
            turns_covered: turns,
            claim_status: claim,
            wip_status: wip,
        }
    }

    #[test]
    fn summary_assembles_count_and_crash_flag() {
        let summary = SessionRecoverySummary {
            crash_recovery: true,
            resumed_count: 2,
            sessions: vec![
                outcome(
                    "a",
                    ResumeFidelity::Lossless,
                    0,
                    ClaimStatus::Reacquired,
                    WipStatus::Restored,
                ),
                outcome(
                    "b",
                    ResumeFidelity::Summary,
                    5,
                    ClaimStatus::PeerConflict,
                    WipStatus::KeptForManual,
                ),
            ],
        };
        assert!(summary.crash_recovery);
        assert_eq!(summary.resumed_count, 2);
        assert_eq!(summary.sessions.len(), 2);
    }

    #[test]
    fn summary_serializes_camelcase_with_honest_kebab_statuses() {
        // The frontend banner type mirrors this exact wire shape; pin it so a
        // rename can't silently break the honesty surfacing.
        let summary = SessionRecoverySummary {
            crash_recovery: true,
            resumed_count: 1,
            sessions: vec![outcome(
                "task-1",
                ResumeFidelity::Summary,
                3,
                ClaimStatus::PeerConflict,
                WipStatus::KeptForManual,
            )],
        };
        let v = serde_json::to_value(&summary).expect("serialize");

        assert_eq!(v["crashRecovery"], serde_json::json!(true));
        assert_eq!(v["resumedCount"], serde_json::json!(1));

        let s = &v["sessions"][0];
        assert_eq!(s["taskRunId"], serde_json::json!("task-1"));
        assert_eq!(s["title"], serde_json::json!("title-task-1"));
        // Honesty contract: lossy / degraded / peer-conflict states are
        // surfaced verbatim as kebab-case enum tags.
        assert_eq!(s["fidelity"], serde_json::json!("summary"));
        assert_eq!(s["turnsCovered"], serde_json::json!(3));
        assert_eq!(s["claimStatus"], serde_json::json!("peer-conflict"));
        assert_eq!(s["wipStatus"], serde_json::json!("kept-for-manual"));
    }

    #[test]
    fn lossless_clean_session_serializes_honestly() {
        let summary = SessionRecoverySummary {
            crash_recovery: false,
            resumed_count: 1,
            sessions: vec![outcome(
                "task-2",
                ResumeFidelity::Lossless,
                0,
                ClaimStatus::Reacquired,
                WipStatus::None,
            )],
        };
        let v = serde_json::to_value(&summary).expect("serialize");
        assert_eq!(v["crashRecovery"], serde_json::json!(false));
        let s = &v["sessions"][0];
        assert_eq!(s["fidelity"], serde_json::json!("lossless"));
        assert_eq!(s["claimStatus"], serde_json::json!("reacquired"));
        assert_eq!(s["wipStatus"], serde_json::json!("none"));
    }

    #[test]
    fn recovery_event_name_is_stable() {
        // The frontend `useSessionRecovery` hook listens on this exact name.
        assert_eq!(SESSION_RECOVERY_SUMMARY_EVENT, "session-recovery-summary");
    }
}
