//! Test run reporting commands for syncing test executions to qontinui-web backend.
//!
//! **DEPRECATED**: This module is deprecated in favor of `execution_reporting`.
//! Use `execution_reporting` module for new implementations which provides a unified
//! schema supporting multiple run types: QA testing, integration testing,
//! live automation, recording sessions, and debug runs.
//!
//! This module remains for backward compatibility with the existing /api/v1/testing/*
//! endpoints. It will be removed in a future version once the backend fully supports
//! the unified /api/v1/execution/* endpoints.
//!
//! This module provides Tauri commands for reporting test run progress to the
//! QA Dashboard in qontinui-web.

use crate::auth::AuthManager;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

/// Get API base URL for qontinui-web backend
fn get_api_base_url() -> String {
    std::env::var("QONTINUI_API_URL").unwrap_or_else(|_| {
        if cfg!(debug_assertions) {
            "http://localhost:8000".to_string()
        } else {
            "https://qontinui-prod-py.eba-km2u4s23.eu-central-1.elasticbeanstalk.com".to_string()
        }
    })
}

// ============================================================================
// Request/Response Types
// ============================================================================

/// Request payload for creating a test run
#[derive(Debug, Serialize)]
pub struct CreateTestRunRequest {
    pub project_id: String,
    pub run_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub runner_metadata: RunnerMetadata,
    pub workflow_metadata: WorkflowMetadata,
    pub configuration_snapshot: ConfigurationSnapshot,
}

#[derive(Debug, Serialize)]
pub struct RunnerMetadata {
    pub runner_version: String,
    pub os: String,
    pub hostname: String,
}

#[derive(Debug, Serialize)]
pub struct WorkflowMetadata {
    pub workflow_id: String,
    pub workflow_name: String,
    pub total_states: i32,
    pub total_transitions: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct ConfigurationSnapshot {
    pub strategy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_duration_seconds: Option<i32>,
}

/// Response from creating a test run
#[derive(Debug, Deserialize, Serialize)]
pub struct CreateTestRunResponse {
    pub run_id: String,
    pub project_id: String,
    pub run_name: String,
    pub status: String,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
}

/// Transition data for reporting
/// Field names match the backend `TransitionCreate` schema
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionData {
    pub sequence_number: i32,
    pub from_state: String,
    pub to_state: String,
    pub transition_name: String,
    pub status: String, // "success", "failed", "skipped", "timeout"
    pub started_at: String,
    pub completed_at: String,
    pub duration_ms: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_id: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Request payload for reporting transitions
#[derive(Debug, Serialize)]
pub struct ReportTransitionsRequest {
    pub transitions: Vec<TransitionData>,
}

/// Response from reporting transitions
#[derive(Debug, Deserialize, Serialize)]
pub struct ReportTransitionsResponse {
    pub recorded: i32,
    pub run_id: String,
}

/// Request payload for completing a test run
/// Field names match the backend `TestRunComplete` schema
#[derive(Debug, Serialize)]
pub struct CompleteTestRunRequest {
    pub status: String, // "completed", "failed", "timeout", "aborted", "crashed"
    pub ended_at: String,
    pub final_metrics: FinalMetrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Final metrics for test run completion
#[derive(Debug, Serialize)]
pub struct FinalMetrics {
    pub total_transitions_executed: i32,
    pub successful_transitions: i32,
    pub failed_transitions: i32,
    pub timeout_transitions: i32,
    pub unique_transitions_covered: i32,
    pub coverage_percentage: f64,
    pub total_deficiencies_found: i32,
    pub total_screenshots_captured: i32,
    pub total_duration_seconds: i32,
}

/// Response from completing a test run
#[derive(Debug, Deserialize, Serialize)]
pub struct CompleteTestRunResponse {
    pub run_id: String,
    pub status: String,
    pub duration_seconds: f64,
}

// ============================================================================
// Input types from TypeScript
// ============================================================================

/// Input from TypeScript for creating a test run
#[derive(Debug, Deserialize)]
pub struct CreateTestRunInput {
    pub project_id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    #[serde(default)]
    pub total_states: i32,
    #[serde(default)]
    pub total_transitions: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Input from TypeScript for completing a test run
#[derive(Debug, Deserialize)]
pub struct CompleteTestRunInput {
    pub run_id: String,
    pub success: bool,
    pub total_transitions: i32,
    pub successful_transitions: i32,
    pub failed_transitions: i32,
    #[serde(default)]
    pub timeout_transitions: i32,
    #[serde(default)]
    pub unique_transitions_covered: i32,
    #[serde(default)]
    pub coverage_percentage: f64,
    #[serde(default)]
    pub states_covered: i32,
    #[serde(default)]
    pub total_states: i32,
    #[serde(default)]
    pub deficiencies_found: i32,
    #[serde(default)]
    pub screenshots_captured: i32,
    #[serde(default)]
    pub duration_seconds: i32,
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Create a new test run when execution starts.
///
/// # Arguments
///
/// * `input` - Test run creation parameters
///
/// # Returns
///
/// The created test run with its assigned ID
#[tauri::command]
pub async fn create_test_run(input: CreateTestRunInput) -> Result<CreateTestRunResponse, String> {
    info!(
        "Creating test run for workflow '{}' in project {}",
        input.workflow_name, input.project_id
    );

    let auth_manager = AuthManager::new();

    // Check authentication
    if !auth_manager.has_tokens() {
        warn!("Cannot create test run: not authenticated");
        return Err("Not authenticated. Please log in.".to_string());
    }

    // Get access token
    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    // Get system info
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let os = std::env::consts::OS.to_string();

    let request = CreateTestRunRequest {
        project_id: input.project_id,
        run_name: format!("{} - {}", input.workflow_name, chrono::Utc::now().format("%Y-%m-%d %H:%M")),
        description: input.description,
        runner_metadata: RunnerMetadata {
            runner_version: env!("CARGO_PKG_VERSION").to_string(),
            os,
            hostname,
        },
        workflow_metadata: WorkflowMetadata {
            workflow_id: input.workflow_id,
            workflow_name: input.workflow_name,
            total_states: input.total_states,
            total_transitions: input.total_transitions,
            tags: None,
        },
        configuration_snapshot: ConfigurationSnapshot {
            strategy: "workflow_execution".to_string(),
            max_duration_seconds: Some(3600),
        },
    };

    // Send to backend
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/v1/testing/runs", get_api_base_url()))
        .bearer_auth(&access_token)
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to create test run: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!("Create test run failed with status {}: {}", status, error_text);
        return Err(format!("Failed to create test run: {}", error_text));
    }

    let test_run: CreateTestRunResponse = response.json().await.map_err(|e| {
        error!("Failed to parse create test run response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!("Test run created successfully: {}", test_run.run_id);

    Ok(test_run)
}

/// Report transitions that occurred during a test run.
///
/// # Arguments
///
/// * `run_id` - The test run ID
/// * `transitions` - List of transitions to report
#[tauri::command]
pub async fn report_test_transitions(
    run_id: String,
    transitions: Vec<TransitionData>,
) -> Result<ReportTransitionsResponse, String> {
    if transitions.is_empty() {
        return Ok(ReportTransitionsResponse {
            recorded: 0,
            run_id,
        });
    }

    info!(
        "Reporting {} transitions for test run {}",
        transitions.len(),
        run_id
    );

    let auth_manager = AuthManager::new();

    if !auth_manager.has_tokens() {
        warn!("Cannot report transitions: not authenticated");
        return Err("Not authenticated".to_string());
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    let request = ReportTransitionsRequest { transitions };

    let client = reqwest::Client::new();
    let response = client
        .post(format!(
            "{}/api/v1/testing/runs/{}/transitions",
            get_api_base_url(),
            run_id
        ))
        .bearer_auth(&access_token)
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to report transitions: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Report transitions failed with status {}: {}",
            status, error_text
        );
        return Err(format!("Failed to report transitions: {}", error_text));
    }

    let result: ReportTransitionsResponse = response.json().await.map_err(|e| {
        error!("Failed to parse transitions response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!("Transitions reported successfully: {} recorded", result.recorded);

    Ok(result)
}

/// Complete a test run with final status and summary.
///
/// # Arguments
///
/// * `input` - Test run completion parameters
#[tauri::command]
pub async fn complete_test_run(input: CompleteTestRunInput) -> Result<CompleteTestRunResponse, String> {
    info!(
        "Completing test run {} with status: {}",
        input.run_id,
        if input.success { "completed" } else { "failed" }
    );

    let auth_manager = AuthManager::new();

    if !auth_manager.has_tokens() {
        warn!("Cannot complete test run: not authenticated");
        return Err("Not authenticated".to_string());
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    let request = CompleteTestRunRequest {
        status: if input.success {
            "completed".to_string()
        } else {
            "failed".to_string()
        },
        ended_at: chrono::Utc::now().to_rfc3339(),
        final_metrics: FinalMetrics {
            total_transitions_executed: input.total_transitions,
            successful_transitions: input.successful_transitions,
            failed_transitions: input.failed_transitions,
            timeout_transitions: input.timeout_transitions,
            unique_transitions_covered: input.unique_transitions_covered,
            coverage_percentage: input.coverage_percentage,
            total_deficiencies_found: input.deficiencies_found,
            total_screenshots_captured: input.screenshots_captured,
            total_duration_seconds: input.duration_seconds,
        },
        summary: None,
    };

    let client = reqwest::Client::new();
    let response = client
        .put(format!(
            "{}/api/v1/testing/runs/{}/complete",
            get_api_base_url(),
            input.run_id
        ))
        .bearer_auth(&access_token)
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to complete test run: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Complete test run failed with status {}: {}",
            status, error_text
        );
        return Err(format!("Failed to complete test run: {}", error_text));
    }

    let result: CompleteTestRunResponse = response.json().await.map_err(|e| {
        error!("Failed to parse complete response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!(
        "Test run {} completed with duration: {}s",
        result.run_id, result.duration_seconds
    );

    Ok(result)
}

// ============================================================================
// Image Recognition Reporting
// ============================================================================

/// Individual image recognition action data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRecognitionAction {
    pub action_id: String,
    pub pattern_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern_name: Option<String>,
    pub action_type: String,
    pub active_states: Vec<String>,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_match_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_x: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_y: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_width: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_height: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence_number: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_data: Option<serde_json::Value>,
}

/// Input from TypeScript for reporting image recognitions
#[derive(Debug, Deserialize)]
pub struct ReportRecognitionsInput {
    pub run_id: String,
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<i32>,
    pub test_run_id: String,
    pub actions: Vec<ImageRecognitionAction>,
}

/// Request payload for reporting image recognitions to backend
#[derive(Debug, Serialize)]
pub struct ReportRecognitionsRequest {
    pub run_id: String,
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_run_id: Option<String>,
    pub actions: Vec<ImageRecognitionAction>,
}

/// Response from reporting image recognitions
#[derive(Debug, Deserialize, Serialize)]
pub struct ReportRecognitionsResponse {
    pub indexed: i32,
    pub run_id: String,
}

/// Report image recognition results for historical storage.
///
/// # Arguments
///
/// * `input` - Image recognition data to report
#[tauri::command]
pub async fn report_image_recognitions(
    input: ReportRecognitionsInput,
) -> Result<ReportRecognitionsResponse, String> {
    if input.actions.is_empty() {
        return Ok(ReportRecognitionsResponse {
            indexed: 0,
            run_id: input.run_id,
        });
    }

    info!(
        "Reporting {} image recognitions for test run {}",
        input.actions.len(),
        input.run_id
    );

    let auth_manager = AuthManager::new();

    if !auth_manager.has_tokens() {
        warn!("Cannot report recognitions: not authenticated");
        return Err("Not authenticated".to_string());
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    let request = ReportRecognitionsRequest {
        run_id: input.run_id.clone(),
        project_id: input.project_id,
        workflow_id: input.workflow_id,
        test_run_id: Some(input.test_run_id),
        actions: input.actions,
    };

    let client = reqwest::Client::new();
    let response = client
        .post(format!(
            "{}/api/v1/historical/actions",
            get_api_base_url()
        ))
        .bearer_auth(&access_token)
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to report recognitions: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Report recognitions failed with status {}: {}",
            status, error_text
        );
        return Err(format!("Failed to report recognitions: {}", error_text));
    }

    let result: ReportRecognitionsResponse = response.json().await.map_err(|e| {
        error!("Failed to parse recognitions response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!(
        "Image recognitions reported successfully: {} indexed",
        result.indexed
    );

    Ok(result)
}
