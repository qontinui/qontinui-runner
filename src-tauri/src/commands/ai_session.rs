//! Tauri commands for interactive AI sessions.
//!
//! These commands allow the frontend to send messages to active Claude CLI sessions,
//! interrupt processing, query session state, and manage standalone AI sessions.

use std::sync::Arc;

use serde::Serialize;
use tauri::Emitter;
use tracing::{error, info, warn};

use crate::claude_session::manager::SessionManager;
use crate::claude_session::resume::{build_replay_prompt, parse_conversation};
use crate::claude_session::state::SessionState;
use crate::commands::{AppState, CommandResponse};
use crate::database::{CheckpointDb, CreateTaskRunInput};
use crate::execution_context::AiSessionContext;
use crate::mcp::shared::AiOutputEvent;

/// Session state event payload (emitted on state transitions).
#[derive(Debug, Clone, Serialize)]
pub struct SessionStateEvent {
    #[serde(rename = "taskRunId")]
    pub task_run_id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub state: String,
}

/// Emit a session state change event to the frontend.
pub fn emit_session_state(
    app_handle: &tauri::AppHandle,
    task_run_id: &str,
    session_id: &str,
    state: SessionState,
) {
    let event = SessionStateEvent {
        task_run_id: task_run_id.to_string(),
        session_id: session_id.to_string(),
        state: state.as_event_str().to_string(),
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
    app_state: tauri::State<'_, Arc<AppState>>,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
) -> Result<CommandResponse, String> {
    let db = app_state.checkpoint_db.clone();
    let result = tokio::task::spawn_blocking(move || db.get_ai_sessions(50)).await;

    match result {
        Ok(Ok(sessions)) => {
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
        Ok(Err(e)) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to list AI sessions: {}", e)),
            data: None,
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Task failed: {}", e)),
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
    task_run_id: String,
    message: String,
) -> Result<CommandResponse, String> {
    info!(
        "send_user_message: task_run_id={}, message_len={}",
        task_run_id,
        message.len()
    );

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
    if let Ok(db) = CheckpointDb::new() {
        let formatted = format!("\n[USER_MESSAGE]\n{}\n[/USER_MESSAGE]\n", message);
        if let Err(e) = db.append_task_output_ex(&task_run_id, &formatted, false, false) {
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
    app_state: tauri::State<'_, Arc<AppState>>,
    task_name: Option<String>,
) -> Result<CommandResponse, String> {
    let task_run_id = uuid::Uuid::new_v4().to_string();
    let name = task_name.unwrap_or_else(|| "New Chat".to_string());

    info!(
        "create_ai_session: task_run_id={}, name={}",
        task_run_id, name
    );

    // 1. Create task run record in DB
    let db = app_state.checkpoint_db.clone();
    let id_clone = task_run_id.clone();
    let name_clone = name.clone();
    let create_result = tokio::task::spawn_blocking(move || {
        db.create_task_run(
            &CreateTaskRunInput::new(id_clone, name_clone)
                .with_prompt("AI session")
                .with_workflow_type("chat"),
        )
    })
    .await;

    if create_result.is_err() || create_result.as_ref().unwrap().is_err() {
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

        match crate::claude_session::ClaudeSession::spawn(
            &working_dir,
            &trid,
            &handle,
            Some(session_ctx), // Tags output events with taskRunId
            None,              // finding_ctx
            None,              // progress_ctx
            None,              // pid_tracker
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

    // 3. Return result
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
    app_state: tauri::State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<CommandResponse, String> {
    info!("close_ai_session: task_run_id={}", task_run_id);

    // Remove and close the session
    if let Some(session) = session_manager.remove(&task_run_id) {
        let _ = session.close();
    }

    // Update DB status
    let db = app_state.checkpoint_db.clone();
    let id_clone = task_run_id.clone();
    let _ =
        tokio::task::spawn_blocking(move || db.update_task_run_status(&id_clone, "stopped")).await;

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
    app_state: tauri::State<'_, Arc<AppState>>,
    task_run_id: String,
    name: String,
) -> Result<CommandResponse, String> {
    info!(
        "rename_ai_session: task_run_id={}, name={}",
        task_run_id, name
    );

    let db = app_state.checkpoint_db.clone();
    let id_clone = task_run_id.clone();
    let name_clone = name.clone();
    let result =
        tokio::task::spawn_blocking(move || db.update_task_run_name(&id_clone, &name_clone)).await;

    match result {
        Ok(Ok(())) => Ok(CommandResponse {
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
    app_state: tauri::State<'_, Arc<AppState>>,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    task_run_id: String,
) -> Result<CommandResponse, String> {
    let db = app_state.checkpoint_db.clone();
    let id_clone = task_run_id.clone();
    let result = tokio::task::spawn_blocking(move || db.get_task_run_output(&id_clone)).await;

    let db_output = match result {
        Ok(Ok(Some(output))) => output,
        Ok(Ok(None)) => String::new(),
        _ => {
            return Ok(CommandResponse {
                success: false,
                message: Some("Failed to get output".to_string()),
                data: None,
            });
        }
    };

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
    app_state: tauri::State<'_, Arc<AppState>>,
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
    let db = app_state.checkpoint_db.clone();
    let id_clone = task_run_id.clone();
    let output_log =
        match tokio::task::spawn_blocking(move || db.get_task_run_output(&id_clone)).await {
            Ok(Ok(Some(log))) => log,
            Ok(Ok(None)) => {
                if source_content.is_none() {
                    return Ok(CommandResponse {
                        success: false,
                        message: Some("No conversation history available".to_string()),
                        data: None,
                    });
                }
                String::new()
            }
            _ => {
                if source_content.is_none() {
                    return Ok(CommandResponse {
                        success: false,
                        message: Some("Failed to read conversation".to_string()),
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
        let existing_specs = fetch_existing_specs().await;
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

    // Get doctor handle for health monitoring
    let doctor_handle = app_state.doctor_handle.lock().await.clone();
    let db2 = app_state.checkpoint_db.clone();
    let artifact_task_run_id = task_run_id.clone();

    let gen_result = tokio::task::spawn_blocking(move || {
        let gen_result = db2.with_conn(|conn| {
            let (response, mut artifact) = crate::workflow_generation::generate_workflow(
                request,
                doctor_handle.as_ref(),
                Some(conn),
                None,
            );
            // Save pipeline artifact for generator evaluation
            artifact.task_run_id = Some(artifact_task_run_id.clone());
            if let Err(e) = db2.save_pipeline_artifact(&artifact) {
                tracing::warn!("Failed to save pipeline artifact: {}", e);
            }
            Ok(response)
        });
        match gen_result {
            Ok(response) => response,
            Err(e) => {
                warn!("DB access failed for session workflow generation: {}", e);
                crate::workflow_generation::GenerateWorkflowResponse {
                    workflow: None,
                    validation_errors: vec![],
                    success: false,
                    error: Some(format!("Database error during generation: {}", e)),
                    model_used: None,
                    verification_iterations: vec![],
                    hardening_summary: None,
                    discovery_calls: vec![],
                    acceptance_criteria: None,
                    quality_report: None,
                    confidence_score: None,
                }
            }
        }
    })
    .await;

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
                    };

                    let db_save = app_state.checkpoint_db.clone();
                    let save_result = tokio::task::spawn_blocking(move || {
                        db_save.create_unified_workflow(&create_req)
                    })
                    .await;

                    match save_result {
                        Ok(Ok(saved)) => {
                            info!(
                                "Saved generated workflow '{}' to library (id={})",
                                saved.name, saved.id
                            );
                        }
                        Ok(Err(e)) => {
                            warn!("Failed to save generated workflow to library: {}", e);
                        }
                        Err(e) => {
                            warn!("spawn_blocking failed saving workflow: {}", e);
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
                        let db3 = app_state.checkpoint_db.clone();
                        let trid = task_run_id.clone();
                        let rd_str = result_data.to_string();
                        if let Err(e) = tokio::task::spawn_blocking(move || {
                            db3.update_task_run_result_data(&trid, &rd_str)
                        })
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()))
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

                    // Persist to output_log
                    if let Ok(db) = CheckpointDb::new() {
                        let formatted =
                            format!("\n[SYSTEM_NOTE]\n{}\n[/SYSTEM_NOTE]\n", system_note);
                        if let Err(e) =
                            db.append_task_output_ex(&task_run_id, &formatted, false, false)
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

                if let Ok(db) = CheckpointDb::new() {
                    let formatted = format!("\n[SYSTEM_NOTE]\n{}\n[/SYSTEM_NOTE]\n", fail_note);
                    if let Err(e) = db.append_task_output_ex(&task_run_id, &formatted, false, false)
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

/// Resume interrupted AI sessions on startup.
///
/// Queries for task runs with status='running' and workflow_type='chat',
/// parses their conversation history from the output_log, spawns new
/// Claude CLI sessions with a replay prompt, and re-registers them in
/// the SessionManager.
///
/// Returns the number of sessions successfully resumed.
pub async fn resume_ai_sessions(
    db: Arc<CheckpointDb>,
    session_manager: Arc<SessionManager>,
    app_handle: tauri::AppHandle,
) -> u32 {
    // Query running AI sessions
    let db_for_query = db.clone();
    let ai_sessions =
        match tokio::task::spawn_blocking(move || db_for_query.get_running_ai_sessions()).await {
            Ok(Ok(sessions)) => sessions,
            Ok(Err(e)) => {
                warn!("Failed to query running AI sessions: {}", e);
                return 0;
            }
            Err(e) => {
                warn!("Task panicked querying AI sessions: {}", e);
                return 0;
            }
        };

    if ai_sessions.is_empty() {
        return 0;
    }

    info!(
        "Found {} interrupted AI session(s) to resume",
        ai_sessions.len()
    );

    let mut resumed_count = 0u32;

    for task_run in ai_sessions {
        let task_run_id = task_run.id.clone();
        let task_name = task_run.task_name.clone();

        // Skip stale sessions older than 24 hours -- mark as stopped
        if let Ok(ts) =
            chrono::NaiveDateTime::parse_from_str(&task_run.updated_at, "%Y-%m-%d %H:%M:%S")
        {
            let age = chrono::Utc::now().naive_utc() - ts;
            if age > chrono::Duration::hours(24) {
                info!(
                    "AI session {} is stale ({} hours old), marking as stopped",
                    task_run_id,
                    age.num_hours()
                );
                let db_for_stop = db.clone();
                let id_for_stop = task_run_id.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    db_for_stop.update_task_run_status(&id_for_stop, "stopped")
                })
                .await;
                continue;
            }
        }

        // Read full output_log
        let db_for_output = db.clone();
        let id_for_output = task_run_id.clone();
        let output_log = match tokio::task::spawn_blocking(move || {
            db_for_output.get_task_run_output(&id_for_output)
        })
        .await
        {
            Ok(Ok(Some(log))) => log,
            _ => {
                info!(
                    "No output_log for AI session {}, marking as stopped",
                    task_run_id
                );
                let db_for_stop = db.clone();
                let id_for_stop = task_run_id.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    db_for_stop.update_task_run_status(&id_for_stop, "stopped")
                })
                .await;
                continue;
            }
        };

        // Parse conversation history
        let turns = parse_conversation(&output_log);
        if turns.is_empty() {
            info!(
                "Empty conversation for AI session {}, marking as stopped",
                task_run_id
            );
            let db_for_stop = db.clone();
            let id_for_stop = task_run_id.clone();
            let _ = tokio::task::spawn_blocking(move || {
                db_for_stop.update_task_run_status(&id_for_stop, "stopped")
            })
            .await;
            continue;
        }

        // Build replay prompt
        let replay_prompt = build_replay_prompt(&turns, None);

        // Write a [CHAT_RESUMED] marker to output_log and increment sessions_count
        {
            let db_for_marker = db.clone();
            let id_for_marker = task_run_id.clone();
            let marker = format!(
                "\n[CHAT_RESUMED]\nSession resumed after runner restart ({} turns replayed)\n[/CHAT_RESUMED]\n",
                turns.len()
            );
            let _ = tokio::task::spawn_blocking(move || {
                // increment_session=true to track how many times this session has been (re)started
                db_for_marker.append_task_output_ex(&id_for_marker, &marker, true, false)
            })
            .await;
        }

        // Spawn new Claude session
        let working_dir = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        let session_ctx = AiSessionContext::setup(&task_run_id, &task_name);

        match crate::claude_session::ClaudeSession::spawn(
            &working_dir,
            &task_run_id,
            &app_handle,
            Some(session_ctx),
            None, // finding_ctx
            None, // progress_ctx
            None, // pid_tracker
        ) {
            Ok(session) => {
                // Send the replay prompt as the initial message
                match session.send_initial_prompt(&replay_prompt) {
                    Ok(()) => {
                        let session = Arc::new(session);
                        if let Err(e) = session_manager.register(&task_run_id, session.clone()) {
                            warn!(
                                "Failed to register resumed AI session {}: {}",
                                task_run_id, e
                            );
                            let _ = session.close();
                            continue;
                        }

                        // Emit processing state so frontend knows it's alive
                        emit_session_state(
                            &app_handle,
                            &task_run_id,
                            &task_run_id,
                            SessionState::Processing,
                        );

                        info!("Resumed AI session: {} (\"{}\")", task_run_id, task_name);
                        resumed_count += 1;
                    }
                    Err(e) => {
                        error!(
                            "Failed to send replay prompt for AI session {}: {}",
                            task_run_id, e
                        );
                        let _ = session.close();
                        let db_for_fail = db.clone();
                        let id_for_fail = task_run_id.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            db_for_fail.update_task_run_status(&id_for_fail, "failed")
                        })
                        .await;
                    }
                }
            }
            Err(e) => {
                error!(
                    "Failed to spawn Claude session for resume {}: {}",
                    task_run_id, e
                );
                let db_for_fail = db.clone();
                let id_for_fail = task_run_id.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    db_for_fail.update_task_run_status(&id_for_fail, "failed")
                })
                .await;
            }
        }
    }

    resumed_count
}

/// Fetch existing page specs from the runner's UI Bridge endpoints.
///
/// Tries both the cached external app specs and the runner's own specs.
/// Returns a JSON string of whatever specs are available, or an empty message.
pub(crate) async fn fetch_existing_specs() -> String {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_default();

    let mut specs_parts: Vec<String> = Vec::new();

    let self_base = crate::mcp::types::get_self_base_url_from_env();

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
