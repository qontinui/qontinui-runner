//! Tauri commands for the Flow Designer.
//!
//! Provides CRUD operations for Flow definitions and execution tracking,
//! as well as flow execution commands that connect the Flow Designer UI
//! to actual flow execution.
//!
//! Uses SQLite database for persistence with in-memory storage for active
//! executions.

use crate::commands::AppState;
use crate::orchestrator::flow::{Condition, Flow, FlowState, FlowStatus, FlowStep};
use crate::orchestrator::flow_executor::{FlowEvent, FlowExecutor};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, error, info, warn};

/// Storage for active flow executions (in-memory for real-time updates).
/// Flow definitions are stored in the database.
struct FlowExecutionStorage {
    executions: HashMap<String, FlowState>,
    flows: HashMap<String, Flow>,
}

impl FlowExecutionStorage {
    fn new() -> Self {
        Self {
            executions: HashMap::new(),
            flows: HashMap::new(),
        }
    }
}

/// Global execution storage instance using tokio mutex for async access.
static EXECUTION_STORAGE: Lazy<TokioMutex<FlowExecutionStorage>> = Lazy::new(|| {
    TokioMutex::new(FlowExecutionStorage::new())
});

/// Summary of a flow for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowSummary {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub step_count: usize,
    pub tags: Vec<String>,
    pub version: String,
}

/// List all saved flows.
#[tauri::command]
pub fn list_flows(state: State<'_, Arc<AppState>>) -> Result<Vec<FlowSummary>, String> {
    let flows_json = state.checkpoint_db.list_flows()?;

    let summaries = flows_json
        .into_iter()
        .map(|f| FlowSummary {
            id: f["id"].as_str().unwrap_or("").to_string(),
            name: f["name"].as_str().unwrap_or("").to_string(),
            description: f["description"].as_str().map(|s| s.to_string()),
            step_count: f["step_count"].as_i64().unwrap_or(0) as usize,
            tags: f["tags"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default(),
            version: f["version"].as_str().unwrap_or("1.0.0").to_string(),
        })
        .collect();

    Ok(summaries)
}

/// Get a flow by ID.
#[tauri::command]
pub fn get_flow(state: State<'_, Arc<AppState>>, id: String) -> Result<Option<Flow>, String> {
    let flow_json = state.checkpoint_db.get_flow(&id)?;

    if let Some(f) = flow_json {
        // Try to deserialize the flow
        match serde_json::from_value::<Flow>(f.clone()) {
            Ok(flow) => Ok(Some(flow)),
            Err(e) => {
                // Fallback: construct from JSON fields
                tracing::warn!("Could not deserialize flow: {}", e);
                Ok(None)
            }
        }
    } else {
        Ok(None)
    }
}

/// Save a flow (create or update).
#[tauri::command]
pub fn save_flow(state: State<'_, Arc<AppState>>, flow: Flow) -> Result<String, String> {
    let flow_json = serde_json::to_value(&flow)
        .map_err(|e| format!("Failed to serialize flow: {}", e))?;
    state.checkpoint_db.save_flow(&flow_json)
}

/// Delete a flow by ID.
#[tauri::command]
pub fn delete_flow(state: State<'_, Arc<AppState>>, id: String) -> Result<bool, String> {
    state.checkpoint_db.delete_flow(&id)
}

/// Validate a flow definition.
#[tauri::command]
pub fn validate_flow(flow: Flow) -> Result<Vec<String>, String> {
    match flow.validate() {
        Ok(()) => Ok(vec![]),
        Err(errors) => Ok(errors),
    }
}

/// Start a flow execution.
///
/// This initializes the flow state and stores it in memory. The flow can then
/// be executed step-by-step using `step_flow_execution` or run to completion
/// using `run_flow_execution`.
#[tauri::command]
pub async fn start_flow_execution(
    app_state: State<'_, Arc<AppState>>,
    app_handle: AppHandle,
    flow_id: String,
    inputs: HashMap<String, serde_json::Value>,
) -> Result<String, String> {
    // Get flow from database
    let flow_json = app_state.checkpoint_db.get_flow(&flow_id)?
        .ok_or_else(|| format!("Flow '{}' not found", flow_id))?;

    let flow: Flow = serde_json::from_value(flow_json)
        .map_err(|e| format!("Failed to deserialize flow: {}", e))?;

    let mut state = FlowState::new(&flow);

    // Set input values in context
    for (key, value) in inputs {
        state.set(&key, value);
    }

    let instance_id = state.instance_id.clone();

    // Save to in-memory storage for real-time access
    {
        let mut storage = EXECUTION_STORAGE.lock().await;
        storage.executions.insert(instance_id.clone(), state.clone());
        storage.flows.insert(instance_id.clone(), flow.clone());
    }

    // Persist to database
    let state_json = serde_json::to_value(&state)
        .map_err(|e| format!("Failed to serialize flow execution: {}", e))?;
    app_state.checkpoint_db.save_flow_execution(&state_json)?;

    // Emit flow started event
    emit_flow_event(&app_handle, FlowEvent::FlowStarted {
        instance_id: instance_id.clone(),
        flow_id: flow.id.clone(),
        flow_name: flow.name.clone(),
    });

    info!(
        instance_id = %instance_id,
        flow_id = %flow_id,
        "Flow execution started"
    );

    Ok(instance_id)
}

/// Execute a flow step-by-step (advance by one step).
///
/// This is useful for UI-driven execution where each step is visualized
/// before moving to the next one.
#[tauri::command]
pub async fn step_flow_execution(
    app_state: State<'_, Arc<AppState>>,
    app_handle: AppHandle,
    instance_id: String,
) -> Result<StepExecutionResult, String> {
    let mut storage = EXECUTION_STORAGE.lock().await;

    // Clone the flow first to avoid borrow conflicts
    let flow = storage.flows.get(&instance_id)
        .ok_or_else(|| format!("Flow for execution '{}' not found", instance_id))?
        .clone();

    let state = storage.executions.get_mut(&instance_id)
        .ok_or_else(|| format!("Execution '{}' not found", instance_id))?;

    // Check if flow is already finished
    if state.is_finished() {
        return Ok(StepExecutionResult {
            step_id: state.current_step.clone(),
            success: state.status == FlowStatus::Completed,
            flow_completed: true,
            outputs: HashMap::new(),
            error: state.error.clone(),
            next_step: None,
        });
    }

    // Create executor with event emitter
    let app_handle_clone = app_handle.clone();
    let executor = FlowExecutor::new()
        .with_event_callback(move |event| {
            emit_flow_event(&app_handle_clone, event);
        });

    // Execute one step
    let result = executor.step_once(&flow, state).await;

    // Persist updated state
    if let Ok(state_json) = serde_json::to_value(&*state) {
        let _ = app_state.checkpoint_db.save_flow_execution(&state_json);
    }

    match result {
        Ok(step_result) => {
            Ok(StepExecutionResult {
                step_id: Some(step_result.step_id),
                success: step_result.success,
                flow_completed: state.is_finished(),
                outputs: step_result.outputs,
                error: step_result.error,
                next_step: state.current_step.clone(),
            })
        }
        Err(e) => {
            Err(e)
        }
    }
}

/// Run a flow execution to completion.
///
/// This executes all steps in the flow until completion, failure, or
/// a human input step is reached.
#[tauri::command]
pub async fn run_flow_execution(
    app_state: State<'_, Arc<AppState>>,
    app_handle: AppHandle,
    instance_id: String,
) -> Result<FlowExecutionResult, String> {
    let (flow, mut state) = {
        let storage = EXECUTION_STORAGE.lock().await;

        let state = storage.executions.get(&instance_id)
            .ok_or_else(|| format!("Execution '{}' not found", instance_id))?
            .clone();

        let flow = storage.flows.get(&instance_id)
            .ok_or_else(|| format!("Flow for execution '{}' not found", instance_id))?
            .clone();

        (flow, state)
    };

    // Check if flow is already finished
    if state.is_finished() {
        return Ok(FlowExecutionResult {
            instance_id: instance_id.clone(),
            success: state.status == FlowStatus::Completed,
            status: format!("{:?}", state.status),
            error: state.error.clone(),
            steps_executed: state.execution_count(),
            outputs: state.context.clone(),
        });
    }

    // Create executor with event emitter
    let app_handle_clone = app_handle.clone();
    let executor = FlowExecutor::new()
        .with_event_callback(move |event| {
            emit_flow_event(&app_handle_clone, event);
        });

    // Execute the flow
    let result = executor.execute(&flow, &mut state).await;

    // Update storage with final state
    {
        let mut storage = EXECUTION_STORAGE.lock().await;
        storage.executions.insert(instance_id.clone(), state.clone());
    }

    // Persist final state
    if let Ok(state_json) = serde_json::to_value(&state) {
        let _ = app_state.checkpoint_db.save_flow_execution(&state_json);
    }

    match result {
        Ok(()) => {
            info!(
                instance_id = %instance_id,
                steps = state.execution_count(),
                "Flow execution completed successfully"
            );

            Ok(FlowExecutionResult {
                instance_id,
                success: true,
                status: "completed".to_string(),
                error: None,
                steps_executed: state.execution_count(),
                outputs: state.context,
            })
        }
        Err(e) => {
            warn!(
                instance_id = %instance_id,
                error = %e,
                "Flow execution failed"
            );

            Ok(FlowExecutionResult {
                instance_id,
                success: false,
                status: "failed".to_string(),
                error: Some(e),
                steps_executed: state.execution_count(),
                outputs: state.context,
            })
        }
    }
}

/// Provide human input to a flow waiting for input.
#[tauri::command]
pub async fn provide_flow_input(
    app_state: State<'_, Arc<AppState>>,
    _app_handle: AppHandle,
    instance_id: String,
    step_id: String,
    input: serde_json::Value,
) -> Result<bool, String> {
    let mut storage = EXECUTION_STORAGE.lock().await;

    let state = storage.executions.get_mut(&instance_id)
        .ok_or_else(|| format!("Execution '{}' not found", instance_id))?;

    if state.status != FlowStatus::WaitingForInput {
        return Err("Flow is not waiting for input".to_string());
    }

    // Store the input in context
    let key = format!("{}.input", step_id);
    state.context.insert(key, input.clone());

    // Resume execution
    state.status = FlowStatus::Running;

    info!(
        instance_id = %instance_id,
        step_id = %step_id,
        "Human input provided, resuming flow"
    );

    // Persist updated state
    if let Ok(state_json) = serde_json::to_value(&*state) {
        let _ = app_state.checkpoint_db.save_flow_execution(&state_json);
    }

    Ok(true)
}

/// Get the current state of a flow execution.
#[tauri::command]
pub async fn get_flow_execution(
    state: State<'_, Arc<AppState>>,
    instance_id: String,
) -> Result<Option<FlowState>, String> {
    // Try in-memory first for active executions
    {
        let storage = EXECUTION_STORAGE.lock().await;
        if let Some(exec) = storage.executions.get(&instance_id) {
            return Ok(Some(exec.clone()));
        }
    }

    // Fall back to database for completed executions
    let exec_json = state.checkpoint_db.get_flow_execution(&instance_id)?;
    if let Some(e) = exec_json {
        match serde_json::from_value::<FlowState>(e) {
            Ok(exec) => Ok(Some(exec)),
            Err(_) => Ok(None),
        }
    } else {
        Ok(None)
    }
}

/// List all flow executions.
#[tauri::command]
pub fn list_flow_executions(state: State<'_, Arc<AppState>>) -> Result<Vec<FlowExecutionSummary>, String> {
    let executions_json = state.checkpoint_db.list_flow_executions()?;

    let summaries = executions_json
        .into_iter()
        .map(|e| FlowExecutionSummary {
            instance_id: e["instance_id"].as_str().unwrap_or("").to_string(),
            flow_id: e["flow_id"].as_str().unwrap_or("").to_string(),
            status: e["status"].as_str().unwrap_or("Unknown").to_string(),
            current_step: e["current_step"].as_str().map(|s| s.to_string()),
            started_at: e["started_at"].as_str().unwrap_or("").to_string(),
            completed_at: e["completed_at"].as_str().map(|s| s.to_string()),
        })
        .collect();

    Ok(summaries)
}

/// Summary of a flow execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowExecutionSummary {
    pub instance_id: String,
    pub flow_id: String,
    pub status: String,
    pub current_step: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
}

/// Result of stepping through a flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecutionResult {
    pub step_id: Option<String>,
    pub success: bool,
    pub flow_completed: bool,
    pub outputs: HashMap<String, serde_json::Value>,
    pub error: Option<String>,
    pub next_step: Option<String>,
}

/// Result of running a flow to completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowExecutionResult {
    pub instance_id: String,
    pub success: bool,
    pub status: String,
    pub error: Option<String>,
    pub steps_executed: usize,
    pub outputs: HashMap<String, serde_json::Value>,
}

/// Cancel a flow execution.
#[tauri::command]
pub async fn cancel_flow_execution(
    app_state: State<'_, Arc<AppState>>,
    app_handle: AppHandle,
    instance_id: String,
) -> Result<bool, String> {
    let mut storage = EXECUTION_STORAGE.lock().await;

    // Clone the flow first to avoid borrow conflicts
    let flow_clone = storage.flows.get(&instance_id).cloned();

    if let Some(state) = storage.executions.get_mut(&instance_id) {
        state.fail("Cancelled by user");

        // Emit completion event
        if let Some(flow) = flow_clone {
            emit_flow_event(&app_handle, FlowEvent::FlowCompleted {
                instance_id: instance_id.clone(),
                flow_id: flow.id.clone(),
                success: false,
                error: Some("Cancelled by user".to_string()),
                total_steps: state.execution_count(),
                duration_ms: 0,
            });
        }

        // Update database
        if let Ok(state_json) = serde_json::to_value(&*state) {
            let _ = app_state.checkpoint_db.save_flow_execution(&state_json);
        }

        info!(instance_id = %instance_id, "Flow execution cancelled");
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Create a sample flow for demonstration.
#[tauri::command]
pub fn create_sample_flow() -> Result<Flow, String> {
    // Create flow
    let flow = Flow::new("sample-debug-flow")
        .with_name("Debug and Fix")
        .with_description("A sample flow that analyzes code, identifies issues, and applies fixes")
        // Add analyze step
        .add_step(
            FlowStep::agent("analyze", "code_reviewer", "Analyze the code and identify potential issues")
                .with_description("AI agent analyzes the codebase")
                .then("check-issues"),
        )
        // Add conditional step
        .add_step(
            FlowStep::conditional(
                "check-issues",
                Condition::Expression { expr: "context.issues.length > 0".to_string() },
                "fix-issues",
                "complete",
            )
            .with_description("Check if issues were found"),
        )
        // Add fix step
        .add_step(
            FlowStep::agent("fix-issues", "debugger", "Fix the identified issues one by one")
                .with_description("AI agent fixes identified issues")
                .then("verify"),
        )
        // Add verify step
        .add_step(
            FlowStep::tool("verify", "run_tests")
                .with_description("Run tests to verify fixes")
                .then("complete"),
        )
        // Add end step
        .add_step(FlowStep::end("complete").with_description("Flow completed successfully"))
        .with_start("analyze")
        .with_input("target_files", false)
        .with_output("issues_fixed", "fix-issues.fixed_count");

    Ok(flow)
}

/// Add the sample flow to storage.
#[tauri::command]
pub fn add_sample_flow(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let flow = create_sample_flow()?;
    let flow_json = serde_json::to_value(&flow)
        .map_err(|e| format!("Failed to serialize flow: {}", e))?;
    state.checkpoint_db.save_flow(&flow_json)
}

// ============================================================================
// Enhanced Flow Queries (Tag Filtering, Execution Filtering, Pagination)
// ============================================================================

/// Filter options for flow execution query.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FlowExecutionFilter {
    pub flow_id: Option<String>,
    pub status: Option<String>,
}

/// Paginated flow execution result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginatedFlowExecutionResult {
    pub items: Vec<FlowExecutionSummary>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

/// Get flows filtered by tag.
#[tauri::command]
pub fn get_flows_by_tag(
    state: State<'_, Arc<AppState>>,
    tag: String,
) -> Result<Vec<FlowSummary>, String> {
    let flows_json = state.checkpoint_db.get_flows_by_tag(&tag)?;

    let summaries = flows_json
        .into_iter()
        .map(|f| FlowSummary {
            id: f["id"].as_str().unwrap_or("").to_string(),
            name: f["name"].as_str().unwrap_or("").to_string(),
            description: f["description"].as_str().map(|s| s.to_string()),
            step_count: f["step_count"].as_i64().unwrap_or(0) as usize,
            tags: f["tags"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default(),
            version: f["version"].as_str().unwrap_or("1.0.0").to_string(),
        })
        .collect();

    Ok(summaries)
}

/// Get flow executions with optional filtering.
#[tauri::command]
pub fn get_flow_executions_filtered(
    state: State<'_, Arc<AppState>>,
    filter: FlowExecutionFilter,
) -> Result<Vec<FlowExecutionSummary>, String> {
    let executions_json = state.checkpoint_db.get_flow_executions_filtered(
        filter.flow_id.as_deref(),
        filter.status.as_deref(),
    )?;

    let summaries = executions_json
        .into_iter()
        .map(|e| FlowExecutionSummary {
            instance_id: e["instance_id"].as_str().unwrap_or("").to_string(),
            flow_id: e["flow_id"].as_str().unwrap_or("").to_string(),
            status: e["status"].as_str().unwrap_or("Unknown").to_string(),
            current_step: e["current_step"].as_str().map(|s| s.to_string()),
            started_at: e["started_at"].as_str().unwrap_or("").to_string(),
            completed_at: e["completed_at"].as_str().map(|s| s.to_string()),
        })
        .collect();

    Ok(summaries)
}

/// Get flow executions with pagination.
#[tauri::command]
pub fn get_flow_executions_paginated(
    state: State<'_, Arc<AppState>>,
    flow_id: Option<String>,
    offset: i64,
    limit: i64,
) -> Result<PaginatedFlowExecutionResult, String> {
    let executions_json = state.checkpoint_db.get_flow_executions_paginated(
        flow_id.as_deref(),
        offset,
        limit,
    )?;

    let summaries = executions_json
        .into_iter()
        .map(|e| FlowExecutionSummary {
            instance_id: e["instance_id"].as_str().unwrap_or("").to_string(),
            flow_id: e["flow_id"].as_str().unwrap_or("").to_string(),
            status: e["status"].as_str().unwrap_or("Unknown").to_string(),
            current_step: e["current_step"].as_str().map(|s| s.to_string()),
            started_at: e["started_at"].as_str().unwrap_or("").to_string(),
            completed_at: e["completed_at"].as_str().map(|s| s.to_string()),
        })
        .collect();

    let total = state.checkpoint_db.get_flow_executions_count(flow_id.as_deref())?;

    Ok(PaginatedFlowExecutionResult {
        items: summaries,
        total,
        offset,
        limit,
    })
}

/// Get total count of flow executions.
#[tauri::command]
pub fn get_flow_executions_count(
    state: State<'_, Arc<AppState>>,
    flow_id: Option<String>,
) -> Result<i64, String> {
    state.checkpoint_db.get_flow_executions_count(flow_id.as_deref())
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Emit a flow event to the frontend via Tauri.
fn emit_flow_event(app_handle: &AppHandle, event: FlowEvent) {
    let event_name = "flow-event";

    // Serialize the event
    if let Ok(payload) = serde_json::to_value(&event) {
        if let Err(e) = app_handle.emit(event_name, payload) {
            error!(error = %e, "Failed to emit flow event");
        } else {
            debug!(?event, "Emitted flow event to frontend");
        }
    }
}
