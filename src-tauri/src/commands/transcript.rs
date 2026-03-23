//! Tauri commands for Claude Code transcript import and workflow generation.
//!
//! Provides commands to list, read, and extract Claude Code session transcripts,
//! plus a standalone workflow generation command that doesn't require an existing task_run_id.

use crate::commands::{AppState, CommandResponse};
use crate::terminal::transcript;
use std::sync::Arc;
use tracing::{info, warn};

/// List Claude Code transcript sessions.
///
/// When `all_projects` is true, scans the workspace root **and** all immediate
/// child directories (the individual repos in the monorepo) so sessions started
/// from subdirectories are visible too.
#[tauri::command]
pub async fn transcript_list_sessions(
    project_path: Option<String>,
    all_projects: Option<bool>,
) -> Result<CommandResponse, String> {
    let workspace_root = crate::mcp::shared::get_workspace_paths_internal()
        .map(|(root, _, _)| root.to_string_lossy().to_string())
        .unwrap_or_default();

    // Build the list of project paths to scan
    let project_paths: Vec<String> = if all_projects.unwrap_or(true) {
        let mut paths = vec![workspace_root.clone()];
        // Add immediate child directories (each repo in the monorepo)
        if !workspace_root.is_empty() {
            if let Ok(entries) = std::fs::read_dir(&workspace_root) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        // Skip hidden dirs and common non-project dirs
                        if !name.starts_with('.') && name != "node_modules" {
                            paths.push(path.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
        paths
    } else {
        let project = project_path.unwrap_or(workspace_root);
        if project.is_empty() {
            return Ok(CommandResponse {
                success: false,
                message: Some("No project path available".to_string()),
                data: None,
            });
        }
        vec![project]
    };

    let config_dirs = transcript::find_claude_config_dirs();
    info!(
        "transcript_list_sessions: scanning {} project paths across {} config dirs",
        project_paths.len(),
        config_dirs.len()
    );

    if config_dirs.is_empty() {
        return Ok(CommandResponse {
            success: true,
            message: Some("No Claude Code config directories found".to_string()),
            data: Some(serde_json::json!([])),
        });
    }

    let mut all_sessions = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    for project in &project_paths {
        for dir in &config_dirs {
            match transcript::list_sessions(dir, project) {
                Ok(sessions) => {
                    for session in sessions {
                        // Deduplicate by session_id (same session won't appear twice)
                        if seen_ids.insert(session.session_id.clone()) {
                            all_sessions.push(session);
                        }
                    }
                }
                Err(e) => warn!("Failed to list sessions in {:?}: {}", dir, e),
            }
        }
    }

    // Sort all sessions by last_modified descending
    all_sessions.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));

    info!(
        "transcript_list_sessions: found {} total sessions",
        all_sessions.len()
    );

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} sessions", all_sessions.len())),
        data: Some(serde_json::to_value(&all_sessions).unwrap_or_default()),
    })
}

/// Read all messages from a specific Claude Code transcript session.
#[tauri::command]
pub async fn transcript_read_session(
    session_id: String,
    config_dir: Option<String>,
    project_path: Option<String>,
) -> Result<CommandResponse, String> {
    let project = project_path.unwrap_or_else(|| {
        crate::mcp::shared::get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_default()
    });

    // If config_dir provided, use it directly; otherwise scan all config dirs
    let config_dirs = if let Some(dir) = config_dir {
        vec![std::path::PathBuf::from(dir)]
    } else {
        transcript::find_claude_config_dirs()
    };

    for dir in &config_dirs {
        match transcript::read_session(dir, &project, &session_id) {
            Ok(messages) => {
                return Ok(CommandResponse {
                    success: true,
                    message: Some(format!("Read {} messages", messages.len())),
                    data: Some(serde_json::to_value(&messages).unwrap_or_default()),
                });
            }
            Err(_) => continue, // Try next config dir
        }
    }

    Ok(CommandResponse {
        success: false,
        message: Some(format!(
            "Session '{}' not found in any config directory",
            session_id
        )),
        data: None,
    })
}

/// Get the most recent Claude Code session for the current project.
#[tauri::command]
pub async fn transcript_get_latest(
    project_path: Option<String>,
) -> Result<CommandResponse, String> {
    let project = project_path.unwrap_or_else(|| {
        crate::mcp::shared::get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_default()
    });

    let config_dirs = transcript::find_claude_config_dirs();

    // Try each config dir, return first match
    for dir in &config_dirs {
        if let Some(session) = transcript::get_latest_session_id(dir, &project) {
            return Ok(CommandResponse {
                success: true,
                message: Some(format!("Latest session: {}", session.session_id)),
                data: Some(serde_json::to_value(&session).unwrap_or_default()),
            });
        }
    }

    Ok(CommandResponse {
        success: true,
        message: Some("No sessions found".to_string()),
        data: None,
    })
}

/// Generate a workflow from arbitrary text context (no task_run_id required).
///
/// This is the standalone generation entrypoint for terminal text selections
/// and transcript imports. Calls the same pipeline as `generate_workflow_from_session`
/// but doesn't need an existing AI session.
#[tauri::command]
pub async fn generate_workflow_standalone(
    app_state: tauri::State<'_, Arc<AppState>>,
    description: String,
    inline_context: String,
    include_ui_bridge: Option<bool>,
) -> Result<CommandResponse, String> {
    info!(
        "generate_workflow_standalone: desc_len={}, context_len={}",
        description.len(),
        inline_context.len()
    );

    if inline_context.trim().is_empty() {
        return Ok(CommandResponse {
            success: false,
            message: Some("No context provided for workflow generation".to_string()),
            data: None,
        });
    }

    // Enrich context with existing specs for spec-aware generation
    let existing_specs = crate::commands::ai_session::fetch_existing_specs().await;
    let enriched_context = if existing_specs != "No existing specs found" {
        crate::commands::ai_session::build_spec_aware_context(&inline_context, &existing_specs)
    } else {
        format!(
            "The following is a Claude Code conversation transcript or selected text. \
             Use this context to generate an appropriate workflow:\n\n{}",
            inline_context
        )
    };

    // Build generation request
    let request = crate::workflow_generation::GenerateWorkflowRequest {
        description,
        inline_context: Some(enriched_context),
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
        tool_tags: None,
    };

    // Get doctor handle for health monitoring
    let doctor_handle = app_state.doctor_handle.lock().await.clone();
    let db = app_state.checkpoint_db.clone();
    let db2 = db.clone();

    let gen_result = tokio::task::spawn_blocking(move || {
        let gen_result = db.with_conn(|conn| {
            let (response, artifact) = crate::workflow_generation::generate_workflow(
                request,
                doctor_handle.as_ref(),
                Some(conn),
                None,
            );
            // Save pipeline artifact (no task_run_id for standalone generation)
            if let Err(e) = db.save_pipeline_artifact(&artifact) {
                tracing::warn!("Failed to save pipeline artifact: {}", e);
            }
            Ok(response)
        });
        match gen_result {
            Ok(response) => response,
            Err(e) => {
                warn!("DB access failed for standalone workflow generation: {}", e);
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
            // Save workflow to database if generation succeeded
            if response.success {
                if let Some(ref workflow) = response.workflow {
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
                        generated_by_task_run_id: None,
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
                        rollback_policy: workflow.rollback_policy.clone(),
                    };

                    let save_result = tokio::task::spawn_blocking(move || {
                        db2.create_unified_workflow(&create_req)
                    })
                    .await;

                    match save_result {
                        Ok(Ok(saved)) => {
                            info!(
                                "Saved standalone generated workflow '{}' (id={})",
                                saved.name, saved.id
                            );
                        }
                        Ok(Err(e)) => {
                            warn!("Failed to save standalone generated workflow: {}", e);
                        }
                        Err(e) => {
                            warn!("spawn_blocking failed saving workflow: {}", e);
                        }
                    }
                }
            }

            Ok(CommandResponse {
                success: response.success,
                message: response.error.clone().or_else(|| {
                    response
                        .workflow
                        .as_ref()
                        .map(|w| format!("Generated workflow: {}", w.name))
                }),
                data: Some(serde_json::to_value(&response).unwrap_or_default()),
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Generation task failed: {}", e)),
            data: None,
        }),
    }
}
