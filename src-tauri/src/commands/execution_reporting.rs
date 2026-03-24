//! Unified execution reporting commands for syncing execution runs to qontinui-web backend.
//!
//! This module provides Tauri commands for reporting execution runs to the backend API.
//! It supports multiple run types: QA testing, integration testing, live automation,
//! recording sessions, and debug runs.
//!
//! This module replaces the testing.rs module with a more flexible, unified schema.

use crate::auth::AuthManager;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

/// Get API base URL for qontinui-web backend
fn get_api_base_url() -> String {
    std::env::var("QONTINUI_API_URL").unwrap_or_else(|_| {
        if cfg!(debug_assertions) {
            "http://127.0.0.1:8000".to_string()
        } else {
            "https://qontinui-prod-py.eba-km2u4s23.eu-central-1.elasticbeanstalk.com".to_string()
        }
    })
}

// ============================================================================
// Enums
// ============================================================================

/// Type of execution run
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunType {
    QaTest,
    IntegrationTest,
    LiveAutomation,
    Recording,
    Debug,
}

/// Status of an execution run
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Timeout,
    Cancelled,
    Paused,
}

/// Status of an individual action
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Success,
    Failed,
    Timeout,
    Skipped,
    Error,
    Pending,
}

/// Type of action executed
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    Find,
    FindAll,
    WaitFor,
    WaitUntilGone,
    Click,
    DoubleClick,
    RightClick,
    Type,
    PressKey,
    Hotkey,
    Scroll,
    Drag,
    GoToState,
    Transition,
    VerifyState,
    Conditional,
    Loop,
    Parallel,
    Sequence,
    Wait,
    Screenshot,
    Log,
    Assert,
    Custom,
    AiPrompt,
    RunPromptSequence,
}

/// Type of error that occurred
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorType {
    ElementNotFound,
    Timeout,
    AssertionFailed,
    Crash,
    NetworkError,
    ValidationError,
    Other,
}

/// Severity of an issue
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Critical,
    High,
    Medium,
    Low,
    Informational,
}

/// Type of screenshot
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotType {
    Error,
    Success,
    Manual,
    Periodic,
    ActionResult,
    StateVerification,
}

// ============================================================================
// LLM Metrics
// ============================================================================

/// Token usage and cost metrics for LLM-powered actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_input: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_output: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_total: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_params: Option<serde_json::Value>,
}

// ============================================================================
// Request/Response Types
// ============================================================================

/// Metadata about the runner environment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerMetadata {
    pub runner_version: String,
    pub os: String,
    pub hostname: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_info: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

/// Metadata about the workflow being executed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowMetadata {
    pub workflow_id: String,
    pub workflow_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_states: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_transitions: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Initial active states when workflow starts (resolved from config)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_state_ids: Option<Vec<String>>,
}

/// Execution statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionStats {
    pub total_actions: i32,
    pub successful_actions: i32,
    pub failed_actions: i32,
    pub timeout_actions: i32,
    pub skipped_actions: i32,
    pub total_duration_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_action_duration_ms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens_input: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens_output: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_action_count: Option<i64>,
}

/// Coverage data for test runs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageData {
    pub coverage_percentage: f64,
    pub states_covered: i32,
    pub total_states: i32,
    pub transitions_covered: i32,
    pub total_transitions: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncovered_states: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncovered_transitions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_visit_counts: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transition_execution_counts: Option<serde_json::Value>,
}

/// Input for creating an execution run (from TypeScript)
#[derive(Debug, Deserialize)]
pub struct ExecutionRunCreateInput {
    pub project_id: String,
    pub run_type: RunType,
    pub run_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub runner_metadata: RunnerMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_metadata: Option<WorkflowMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration: Option<serde_json::Value>,
}

/// Request payload for creating an execution run (to backend)
#[derive(Debug, Serialize)]
pub struct ExecutionRunCreateRequest {
    pub project_id: String,
    pub run_type: RunType,
    pub run_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub runner_metadata: RunnerMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_metadata: Option<WorkflowMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration: Option<serde_json::Value>,
}

/// Response from creating an execution run
#[derive(Debug, Deserialize, Serialize)]
pub struct ExecutionRunResponse {
    /// The backend returns 'id' but we expose it as 'run_id' for clarity
    #[serde(alias = "id")]
    pub run_id: String,
    pub project_id: String,
    pub run_type: RunType,
    pub run_name: String,
    pub status: RunStatus,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    /// Additional fields from backend (ignored during deserialization)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

/// Match location for vision actions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchLocation {
    pub x: i32,
    pub y: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<i32>,
}

/// Input for creating an action execution record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionExecutionCreate {
    pub sequence_number: i32,
    pub action_type: ActionType,
    pub action_name: String,
    pub status: ActionStatus,
    pub started_at: String,
    pub completed_at: String,
    pub duration_ms: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_states: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_location: Option<MatchLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<ErrorType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_stack: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_action_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_metrics: Option<LLMMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

/// Request payload for reporting action executions
#[derive(Debug, Serialize)]
pub struct ActionExecutionBatchRequest {
    pub actions: Vec<ActionExecutionCreate>,
}

/// Response from reporting action executions
#[derive(Debug, Deserialize, Serialize)]
pub struct ActionExecutionResponse {
    pub recorded: i32,
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_ids: Option<Vec<String>>,
}

/// Annotation for screenshots
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotAnnotation {
    #[serde(rename = "type")]
    pub annotation_type: String,
    pub x: i32,
    pub y: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Input for creating an execution screenshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionScreenshotCreate {
    pub screenshot_id: String,
    pub sequence_number: i32,
    pub screenshot_type: ScreenshotType,
    pub timestamp: String,
    pub width: i32,
    pub height: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_sequence_number: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_states: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<ScreenshotAnnotation>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Response from uploading a screenshot
#[derive(Debug, Deserialize, Serialize)]
pub struct ExecutionScreenshotResponse {
    pub screenshot_id: String,
    pub run_id: String,
    pub image_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    pub uploaded_at: String,
    pub file_size_bytes: i64,
}

/// Input for creating an execution issue
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionIssueCreate {
    pub title: String,
    pub description: String,
    pub severity: IssueSeverity,
    pub issue_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_sequence_number: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reproduction_steps: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_behavior: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_behavior: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Request payload for reporting issues
#[derive(Debug, Serialize)]
pub struct ExecutionIssueBatchRequest {
    pub issues: Vec<ExecutionIssueCreate>,
}

/// Response from reporting issues
#[derive(Debug, Deserialize, Serialize)]
pub struct ExecutionIssueResponse {
    pub recorded: i32,
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_ids: Option<Vec<String>>,
}

/// Input for completing an execution run
#[derive(Debug, Deserialize)]
pub struct ExecutionRunCompleteInput {
    pub status: RunStatus,
    pub ended_at: String,
    pub stats: ExecutionStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Request payload for completing an execution run
#[derive(Debug, Serialize)]
pub struct ExecutionRunCompleteRequest {
    pub status: RunStatus,
    pub ended_at: String,
    pub stats: ExecutionStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Response from completing an execution run
#[derive(Debug, Deserialize, Serialize)]
pub struct ExecutionRunCompleteResponse {
    pub run_id: String,
    pub status: RunStatus,
    pub started_at: String,
    pub ended_at: String,
    pub duration_seconds: f64,
    pub stats: ExecutionStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageData>,
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Create a new execution run.
///
/// # Arguments
///
/// * `input` - Execution run creation parameters
///
/// # Returns
///
/// The created execution run with its assigned ID
#[tauri::command]
pub async fn create_execution_run(
    input: ExecutionRunCreateInput,
) -> Result<ExecutionRunResponse, String> {
    info!(
        "Creating {} execution run '{}' for project {}",
        format!("{:?}", input.run_type).to_lowercase(),
        input.run_name,
        input.project_id
    );

    // Debug: log workflow metadata including initial_state_ids
    if let Some(ref wm) = input.workflow_metadata {
        info!(
            "[CREATE_EXECUTION_RUN] workflow_metadata: workflow_id={}, workflow_name={}, initial_state_ids={:?}",
            wm.workflow_id,
            wm.workflow_name,
            wm.initial_state_ids
        );
    } else {
        info!("[CREATE_EXECUTION_RUN] No workflow_metadata provided");
    }

    let auth_manager = AuthManager::new();

    // Check authentication
    if !auth_manager.has_tokens() {
        warn!("Cannot create execution run: not authenticated");
        return Err("Not authenticated. Please log in.".to_string());
    }

    // Get access token
    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    let request = ExecutionRunCreateRequest {
        project_id: input.project_id,
        run_type: input.run_type,
        run_name: input.run_name,
        description: input.description,
        runner_metadata: input.runner_metadata,
        workflow_metadata: input.workflow_metadata,
        configuration: input.configuration,
    };

    // Debug: log the serialized request JSON
    if let Ok(json_str) = serde_json::to_string_pretty(&request) {
        info!("[CREATE_EXECUTION_RUN] Request JSON: {}", json_str);
    }

    // Send to backend
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/v1/execution/runs", get_api_base_url()))
        .bearer_auth(&access_token)
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to create execution run: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Create execution run failed with status {}: {}",
            status, error_text
        );
        return Err(format!("Failed to create execution run: {}", error_text));
    }

    let run: ExecutionRunResponse = response.json().await.map_err(|e| {
        error!("Failed to parse execution run response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!("Execution run created successfully: {}", run.run_id);

    Ok(run)
}

/// Report action executions for an execution run.
///
/// # Arguments
///
/// * `run_id` - The execution run ID
/// * `actions` - List of action executions to report
#[tauri::command]
pub async fn report_action_executions(
    run_id: String,
    actions: Vec<ActionExecutionCreate>,
) -> Result<ActionExecutionResponse, String> {
    if actions.is_empty() {
        return Ok(ActionExecutionResponse {
            recorded: 0,
            run_id,
            action_ids: None,
        });
    }

    info!(
        "Reporting {} action executions for run {}",
        actions.len(),
        run_id
    );

    let auth_manager = AuthManager::new();

    if !auth_manager.has_tokens() {
        warn!("Cannot report actions: not authenticated");
        return Err("Not authenticated".to_string());
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    let request = ActionExecutionBatchRequest { actions };

    let client = reqwest::Client::new();
    let response = client
        .post(format!(
            "{}/api/v1/execution/runs/{}/actions",
            get_api_base_url(),
            run_id
        ))
        .bearer_auth(&access_token)
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to report actions: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Report actions failed with status {}: {}",
            status, error_text
        );
        return Err(format!("Failed to report actions: {}", error_text));
    }

    let result: ActionExecutionResponse = response.json().await.map_err(|e| {
        error!("Failed to parse actions response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!(
        "Actions reported successfully: {} recorded",
        result.recorded
    );

    Ok(result)
}

/// Upload a screenshot for an execution run.
///
/// # Arguments
///
/// * `run_id` - The execution run ID
/// * `screenshot` - Screenshot metadata
/// * `image_data` - Raw image data (PNG or JPEG)
#[tauri::command]
pub async fn upload_execution_screenshot(
    run_id: String,
    screenshot: ExecutionScreenshotCreate,
    image_data: Vec<u8>,
) -> Result<ExecutionScreenshotResponse, String> {
    info!(
        "Uploading screenshot {} for run {}",
        screenshot.screenshot_id, run_id
    );

    let auth_manager = AuthManager::new();

    if !auth_manager.has_tokens() {
        warn!("Cannot upload screenshot: not authenticated");
        return Err("Not authenticated".to_string());
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    // Determine content type from image data
    let content_type = if image_data.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        "image/png"
    } else if image_data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else {
        "application/octet-stream"
    };

    // Build multipart form
    let metadata_json = serde_json::to_string(&screenshot).map_err(|e| {
        error!("Failed to serialize screenshot metadata: {}", e);
        format!("Failed to serialize metadata: {}", e)
    })?;

    let image_part = reqwest::multipart::Part::bytes(image_data)
        .file_name("screenshot")
        .mime_str(content_type)
        .map_err(|e| format!("Failed to create image part: {}", e))?;

    let metadata_part = reqwest::multipart::Part::text(metadata_json)
        .mime_str("application/json")
        .map_err(|e| format!("Failed to create metadata part: {}", e))?;

    let form = reqwest::multipart::Form::new()
        .part("image", image_part)
        .part("metadata", metadata_part);

    let client = reqwest::Client::new();
    let response = client
        .post(format!(
            "{}/api/v1/execution/runs/{}/screenshots",
            get_api_base_url(),
            run_id
        ))
        .bearer_auth(&access_token)
        .multipart(form)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to upload screenshot: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Upload screenshot failed with status {}: {}",
            status, error_text
        );
        return Err(format!("Failed to upload screenshot: {}", error_text));
    }

    let result: ExecutionScreenshotResponse = response.json().await.map_err(|e| {
        error!("Failed to parse screenshot response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!("Screenshot uploaded successfully: {}", result.image_url);

    Ok(result)
}

/// Report issues found during an execution run.
///
/// # Arguments
///
/// * `run_id` - The execution run ID
/// * `issues` - List of issues to report
#[tauri::command]
pub async fn report_execution_issues(
    run_id: String,
    issues: Vec<ExecutionIssueCreate>,
) -> Result<ExecutionIssueResponse, String> {
    if issues.is_empty() {
        return Ok(ExecutionIssueResponse {
            recorded: 0,
            run_id,
            issue_ids: None,
        });
    }

    info!("Reporting {} issues for run {}", issues.len(), run_id);

    let auth_manager = AuthManager::new();

    if !auth_manager.has_tokens() {
        warn!("Cannot report issues: not authenticated");
        return Err("Not authenticated".to_string());
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    let request = ExecutionIssueBatchRequest { issues };

    let client = reqwest::Client::new();
    let response = client
        .post(format!(
            "{}/api/v1/execution/runs/{}/issues",
            get_api_base_url(),
            run_id
        ))
        .bearer_auth(&access_token)
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to report issues: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Report issues failed with status {}: {}",
            status, error_text
        );
        return Err(format!("Failed to report issues: {}", error_text));
    }

    let result: ExecutionIssueResponse = response.json().await.map_err(|e| {
        error!("Failed to parse issues response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!("Issues reported successfully: {} recorded", result.recorded);

    Ok(result)
}

/// Complete an execution run with final status and statistics.
///
/// # Arguments
///
/// * `run_id` - The execution run ID
/// * `input` - Completion parameters
#[tauri::command]
pub async fn complete_execution_run(
    run_id: String,
    input: ExecutionRunCompleteInput,
) -> Result<ExecutionRunCompleteResponse, String> {
    info!(
        "Completing execution run {} with status: {:?}",
        run_id, input.status
    );

    let auth_manager = AuthManager::new();

    if !auth_manager.has_tokens() {
        warn!("Cannot complete execution run: not authenticated");
        return Err("Not authenticated".to_string());
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    let request = ExecutionRunCompleteRequest {
        status: input.status,
        ended_at: input.ended_at,
        stats: input.stats,
        coverage: input.coverage,
        summary: input.summary,
        error_message: input.error_message,
    };

    let client = reqwest::Client::new();
    let response = client
        .put(format!(
            "{}/api/v1/execution/runs/{}/complete",
            get_api_base_url(),
            run_id
        ))
        .bearer_auth(&access_token)
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to complete execution run: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Complete execution run failed with status {}: {}",
            status, error_text
        );
        return Err(format!("Failed to complete execution run: {}", error_text));
    }

    let result: ExecutionRunCompleteResponse = response.json().await.map_err(|e| {
        error!("Failed to parse complete response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!(
        "Execution run {} completed with duration: {}s",
        result.run_id, result.duration_seconds
    );

    Ok(result)
}
