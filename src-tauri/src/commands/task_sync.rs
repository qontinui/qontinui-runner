//! Task Sync commands for syncing task runs to qontinui-web backend.
//!
//! This module provides Tauri commands for reporting task runs, sessions,
//! and findings to the backend API. It enables team visibility of local
//! task execution workflows (AI, automation, or mixed).
//!
//! This module follows the same patterns as execution_reporting.rs for consistency.
//!
//! Note: Renamed from ai_task_reporting.rs as part of the unified TaskRun architecture.
//! The "AI" prefix was removed since task runs can now be pure automation or mixed.

use crate::auth::AuthManager;
use crate::commands::compartments::StorageCompartment;
use crate::database::pg::PgDb;
use crate::database::TaskRun;
use crate::findings::types::{Finding, FindingStatus};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::{error, info, warn};

use crate::api_config::get_api_base_url;

// ============================================================================
// Enums (matching backend types)
// ============================================================================

/// AI Task status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AITaskStatus {
    Running,
    Complete,
    Failed,
    Stopped,
}

impl From<&str> for AITaskStatus {
    fn from(s: &str) -> Self {
        match s {
            "complete" | "completed" => Self::Complete,
            "failed" => Self::Failed,
            "stopped" => Self::Stopped,
            _ => Self::Running,
        }
    }
}

/// AI Task finding category
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AITaskFindingCategory {
    CodeBug,
    Security,
    Performance,
    Todo,
    Enhancement,
    ConfigIssue,
    TestIssue,
    Documentation,
    RuntimeIssue,
    AlreadyFixed,
    ExpectedBehavior,
    Warning,
    DataMigration,
}

impl From<&crate::findings::types::FindingCategory> for AITaskFindingCategory {
    fn from(cat: &crate::findings::types::FindingCategory) -> Self {
        use crate::findings::types::FindingCategory;
        match cat {
            FindingCategory::CodeBug => Self::CodeBug,
            FindingCategory::Security => Self::Security,
            FindingCategory::Performance => Self::Performance,
            FindingCategory::Todo => Self::Todo,
            FindingCategory::Enhancement => Self::Enhancement,
            FindingCategory::ConfigIssue => Self::ConfigIssue,
            FindingCategory::TestIssue => Self::TestIssue,
            FindingCategory::Documentation => Self::Documentation,
            FindingCategory::RuntimeIssue => Self::RuntimeIssue,
            FindingCategory::AlreadyFixed => Self::AlreadyFixed,
            FindingCategory::ExpectedBehavior => Self::ExpectedBehavior,
            FindingCategory::Warning => Self::Warning,
            FindingCategory::DataMigration => Self::DataMigration,
        }
    }
}

/// AI Task finding severity
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AITaskFindingSeverity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl From<&crate::findings::types::FindingSeverity> for AITaskFindingSeverity {
    fn from(sev: &crate::findings::types::FindingSeverity) -> Self {
        use crate::findings::types::FindingSeverity;
        match sev {
            FindingSeverity::Critical => Self::Critical,
            FindingSeverity::High => Self::High,
            FindingSeverity::Medium => Self::Medium,
            FindingSeverity::Low => Self::Low,
            FindingSeverity::Info => Self::Info,
        }
    }
}

/// AI Task finding status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AITaskFindingStatus {
    Detected,
    InProgress,
    NeedsInput,
    Resolved,
    WontFix,
    Deferred,
}

impl From<&FindingStatus> for AITaskFindingStatus {
    fn from(status: &FindingStatus) -> Self {
        match status {
            FindingStatus::Detected => Self::Detected,
            FindingStatus::InProgress => Self::InProgress,
            FindingStatus::NeedsInput => Self::NeedsInput,
            FindingStatus::Resolved => Self::Resolved,
            FindingStatus::WontFix => Self::WontFix,
            FindingStatus::Deferred => Self::Deferred,
        }
    }
}

/// AI Task finding action type
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AITaskFindingActionType {
    AutoFix,
    NeedsUserInput,
    Manual,
    Informational,
}

impl From<&crate::findings::types::FindingActionType> for AITaskFindingActionType {
    fn from(action: &crate::findings::types::FindingActionType) -> Self {
        use crate::findings::types::FindingActionType;
        match action {
            FindingActionType::AutoFix => Self::AutoFix,
            FindingActionType::NeedsUserInput => Self::NeedsUserInput,
            FindingActionType::Manual => Self::Manual,
            FindingActionType::Informational => Self::Informational,
        }
    }
}

// ============================================================================
// Request/Response Types
// ============================================================================

/// Request to create a new AI task
#[derive(Debug, Serialize)]
pub struct AITaskCreateRequest {
    /// Allow runner to specify ID for direct mapping
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    /// Workspace/organization ID (from web backend)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// User ID who triggered the task (from web backend)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,
    pub task_name: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_sessions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_continue: Option<bool>,
}

/// Response from creating an AI task
#[derive(Debug, Deserialize, Serialize)]
pub struct AITaskResponse {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub task_name: String,
    pub status: String,
    pub sessions_count: u32,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Request to update an AI task
#[derive(Debug, Serialize)]
pub struct AITaskUpdateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<AITaskStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

/// Request to create a session
#[derive(Debug, Serialize)]
pub struct AITaskSessionCreateRequest {
    pub session_number: u32,
}

/// Response from creating a session
#[derive(Debug, Deserialize, Serialize)]
pub struct AITaskSessionResponse {
    pub id: String,
    pub task_id: String,
    pub session_number: u32,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
}

/// Request to update a session
#[derive(Debug, Serialize)]
pub struct AITaskSessionUpdateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_summary: Option<String>,
}

/// Request to create a finding
#[derive(Debug, Clone, Serialize)]
pub struct AITaskFindingCreateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub category: AITaskFindingCategory,
    pub severity: AITaskFindingSeverity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<AITaskFindingStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_type: Option<AITaskFindingActionType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_hash: Option<String>,
    pub title: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_snippet: Option<String>,
    pub detected_in_session: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_input: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_options: Option<Vec<String>>,
}

/// Request to batch sync findings
#[derive(Debug, Serialize)]
pub struct AITaskFindingsBatchRequest {
    pub findings: Vec<AITaskFindingCreateRequest>,
}

/// Request to sync a single deferred question
#[derive(Debug, Serialize)]
pub struct DeferredQuestionSyncRequest {
    pub id: String,
    pub iteration: i32,
    pub question: String,
    pub context_json: String,
    pub auto_decision_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_decision_detail: Option<String>,
    pub confidence: f64,
    pub risk_level: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_checkpoint: Option<String>,
    pub contingent_iterations: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer_comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<String>,
}

/// Request to batch sync deferred questions
#[derive(Debug, Serialize)]
pub struct DeferredQuestionBatchRequest {
    pub questions: Vec<DeferredQuestionSyncRequest>,
}

/// Response from syncing findings
#[derive(Debug, Deserialize, Serialize)]
pub struct AITaskFindingSyncResponse {
    pub id: String,
    pub task_id: String,
    pub title: String,
    pub status: String,
}

/// Request to create a verification result
#[derive(Debug, Serialize)]
pub struct VerificationResultCreateRequest {
    pub iteration: u32,
    pub result: serde_json::Value,
}

/// Request to batch upsert verification results
#[derive(Debug, Serialize)]
pub struct VerificationResultsBatchRequest {
    pub results: Vec<VerificationResultCreateRequest>,
}

// ============================================================================
// Conversion helpers
// ============================================================================

impl From<&TaskRun> for AITaskCreateRequest {
    fn from(task: &TaskRun) -> Self {
        Self {
            id: Some(task.id.clone()),
            project_id: None, // Set by caller if available
            runner_id: Some(get_runner_id()),
            workspace_id: task.workspace_id.clone(),
            triggered_by: task.triggered_by.clone(),
            task_name: task.task_name.clone(),
            prompt: task
                .prompt
                .clone()
                .unwrap_or_else(|| "(Automation only)".to_string()),
            max_sessions: task.max_sessions,
            auto_continue: Some(task.auto_continue),
        }
    }
}

impl From<&Finding> for AITaskFindingCreateRequest {
    fn from(finding: &Finding) -> Self {
        Self {
            id: Some(finding.id.clone()),
            category: AITaskFindingCategory::from(&finding.category),
            severity: AITaskFindingSeverity::from(&finding.severity),
            status: Some(AITaskFindingStatus::from(&finding.status)),
            action_type: Some(AITaskFindingActionType::from(&finding.action_type)),
            signature_hash: Some(finding.signature_hash.clone()),
            title: finding.title.clone(),
            description: finding.description.clone(),
            resolution: finding.resolution.clone(),
            file_path: finding.code_context.as_ref().and_then(|c| c.file.clone()),
            line_number: finding.code_context.as_ref().and_then(|c| c.line),
            column_number: finding.code_context.as_ref().and_then(|c| c.column),
            code_snippet: finding
                .code_context
                .as_ref()
                .and_then(|c| c.snippet.clone()),
            detected_in_session: finding.session_num,
            needs_input: finding.user_input.is_some().then_some(true),
            question: finding.user_input.as_ref().map(|u| u.question.clone()),
            input_options: finding.user_input.as_ref().and_then(|u| u.options.clone()),
        }
    }
}

/// Convert a JSON row from PgDb into a DeferredQuestionSyncRequest.
pub fn json_to_deferred_question_sync(q: &serde_json::Value) -> DeferredQuestionSyncRequest {
    DeferredQuestionSyncRequest {
        id: q["id"].as_str().unwrap_or("").to_string(),
        iteration: q["iteration"].as_i64().unwrap_or(0) as i32,
        question: q["question"].as_str().unwrap_or("").to_string(),
        context_json: q["context_json"].as_str().unwrap_or("{}").to_string(),
        auto_decision_type: q["auto_decision_type"]
            .as_str()
            .unwrap_or("proceeded")
            .to_string(),
        auto_decision_detail: q["auto_decision_detail"].as_str().map(String::from),
        confidence: q["confidence"].as_f64().unwrap_or(0.0),
        risk_level: q["risk_level"].as_str().unwrap_or("low").to_string(),
        status: q["status"].as_str().unwrap_or("pending").to_string(),
        git_checkpoint: q["git_checkpoint"].as_str().map(String::from),
        contingent_iterations: q["contingent_iterations"]
            .as_str()
            .unwrap_or("[]")
            .to_string(),
        reviewer_comment: q["reviewer_comment"].as_str().map(String::from),
        created_at: q["created_at"].as_str().map(String::from),
        reviewed_at: q["reviewed_at"].as_str().map(String::from),
    }
}

/// Get a unique runner ID for this machine
fn get_runner_id() -> String {
    // Use hostname as a simple runner identifier
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown-runner".to_string())
}

/// Calculate duration in seconds from created_at to completed_at
fn calculate_duration(created_at: &str, completed_at: Option<&str>) -> Option<i64> {
    use chrono::{DateTime, Utc};

    let created: DateTime<Utc> = created_at.parse().ok()?;
    let completed: DateTime<Utc> = completed_at?.parse().ok()?;

    Some((completed - created).num_seconds())
}

/// Generate a summary from the output log (truncate if too long)
fn summarize_output(output_log: &str, max_chars: usize) -> String {
    if output_log.len() <= max_chars {
        output_log.to_string()
    } else {
        // Take last N characters (most recent output is usually most relevant)
        let start = output_log.len() - max_chars;
        format!("...[truncated]...\n{}", &output_log[start..])
    }
}

// ============================================================================
// AI Task Sync Service
// ============================================================================

/// Service for syncing AI tasks to qontinui-web backend.
///
/// This service handles:
/// - Creating AI tasks in the backend when a new task starts
/// - Updating task status and output as it progresses
/// - Recording session starts and ends
/// - Syncing findings (issues, bugs, questions)
pub struct AITaskSyncService {
    auth_manager: AuthManager,
    client: reqwest::Client,
}

impl AITaskSyncService {
    pub fn new() -> Self {
        Self {
            auth_manager: AuthManager::new(),
            client: reqwest::Client::new(),
        }
    }

    /// Check if authenticated and can sync
    fn check_auth(&self) -> Result<String, String> {
        if !self.auth_manager.has_tokens() {
            return Err("Not authenticated. Please log in to sync AI tasks.".to_string());
        }

        self.auth_manager.get_access_token().map_err(|e| {
            error!("Failed to get access token: {}", e);
            format!("Failed to get access token: {}", e)
        })
    }

    /// Sync a newly created task to the backend.
    pub async fn sync_task_created(
        &self,
        task: &TaskRun,
        project_id: Option<&str>,
    ) -> Result<AITaskResponse, String> {
        info!("Syncing new AI task to backend: {}", task.id);

        let access_token = self.check_auth()?;

        let mut request = AITaskCreateRequest::from(task);
        request.project_id = project_id.map(String::from);

        let response = self
            .client
            .post(format!("{}/api/v1/task-runs", get_api_base_url()))
            .bearer_auth(&access_token)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                error!("Failed to sync task creation: {}", e);
                format!("Network error: {}", e)
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!(
                "Sync task creation failed with status {}: {}",
                status, error_text
            );
            return Err(format!("Failed to sync task: {}", error_text));
        }

        let result: AITaskResponse = response.json().await.map_err(|e| {
            error!("Failed to parse task response: {}", e);
            format!("Invalid response from server: {}", e)
        })?;

        info!("AI task synced successfully: {}", result.id);
        Ok(result)
    }

    /// Sync session start to the backend.
    pub async fn sync_session_started(
        &self,
        task_id: &str,
        session_number: u32,
    ) -> Result<AITaskSessionResponse, String> {
        info!(
            "Syncing session start for task {}, session {}",
            task_id, session_number
        );

        let access_token = self.check_auth()?;

        let request = AITaskSessionCreateRequest { session_number };

        let response = self
            .client
            .post(format!(
                "{}/api/v1/task-runs/{}/sessions",
                get_api_base_url(),
                task_id
            ))
            .bearer_auth(&access_token)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                error!("Failed to sync session start: {}", e);
                format!("Network error: {}", e)
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!(
                "Sync session start failed with status {}: {}",
                status, error_text
            );
            return Err(format!("Failed to sync session: {}", error_text));
        }

        let result: AITaskSessionResponse = response.json().await.map_err(|e| {
            error!("Failed to parse session response: {}", e);
            format!("Invalid response from server: {}", e)
        })?;

        info!("Session synced successfully: {}", result.id);
        Ok(result)
    }

    /// Sync session end to the backend.
    pub async fn sync_session_ended(
        &self,
        task_id: &str,
        session_number: u32,
        duration_seconds: i64,
        output_summary: Option<&str>,
    ) -> Result<AITaskSessionResponse, String> {
        info!(
            "Syncing session end for task {}, session {} ({}s)",
            task_id, session_number, duration_seconds
        );

        let access_token = self.check_auth()?;

        let now = chrono::Utc::now().to_rfc3339();
        let request = AITaskSessionUpdateRequest {
            ended_at: Some(now),
            duration_seconds: Some(duration_seconds),
            output_summary: output_summary.map(String::from),
        };

        let response = self
            .client
            .put(format!(
                "{}/api/v1/task-runs/{}/sessions/{}",
                get_api_base_url(),
                task_id,
                session_number
            ))
            .bearer_auth(&access_token)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                error!("Failed to sync session end: {}", e);
                format!("Network error: {}", e)
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!(
                "Sync session end failed with status {}: {}",
                status, error_text
            );
            return Err(format!("Failed to sync session: {}", error_text));
        }

        let result: AITaskSessionResponse = response.json().await.map_err(|e| {
            error!("Failed to parse session response: {}", e);
            format!("Invalid response from server: {}", e)
        })?;

        info!("Session end synced successfully");
        Ok(result)
    }

    /// Sync findings to the backend.
    pub async fn sync_findings(
        &self,
        task_id: &str,
        findings: &[Finding],
    ) -> Result<Vec<AITaskFindingSyncResponse>, String> {
        if findings.is_empty() {
            return Ok(vec![]);
        }

        info!("Syncing {} findings for task {}", findings.len(), task_id);

        let access_token = self.check_auth()?;

        let request = AITaskFindingsBatchRequest {
            findings: findings
                .iter()
                .map(AITaskFindingCreateRequest::from)
                .collect(),
        };

        let response = self
            .client
            .post(format!(
                "{}/api/v1/task-runs/{}/findings",
                get_api_base_url(),
                task_id
            ))
            .bearer_auth(&access_token)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                error!("Failed to sync findings: {}", e);
                format!("Network error: {}", e)
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!(
                "Sync findings failed with status {}: {}",
                status, error_text
            );
            return Err(format!("Failed to sync findings: {}", error_text));
        }

        let result: Vec<AITaskFindingSyncResponse> = response.json().await.map_err(|e| {
            error!("Failed to parse findings response: {}", e);
            format!("Invalid response from server: {}", e)
        })?;

        info!("Synced {} findings successfully", result.len());
        Ok(result)
    }

    /// Sync deferred questions to the backend.
    pub async fn sync_deferred_questions(
        &self,
        task_id: &str,
        questions: Vec<DeferredQuestionSyncRequest>,
    ) -> Result<(), String> {
        if questions.is_empty() {
            return Ok(());
        }

        info!(
            "Syncing {} deferred questions for task {}",
            questions.len(),
            task_id
        );

        let access_token = self.check_auth()?;

        let batch = DeferredQuestionBatchRequest { questions };

        let response = self
            .client
            .post(format!(
                "{}/api/v1/task-runs/{}/deferred-questions",
                get_api_base_url(),
                task_id
            ))
            .bearer_auth(&access_token)
            .json(&batch)
            .send()
            .await
            .map_err(|e| {
                error!("Failed to sync deferred questions: {}", e);
                format!("Network error: {}", e)
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!(
                "Sync deferred questions failed with status {}: {}",
                status, error_text
            );
            return Err(format!("Failed to sync deferred questions: {}", error_text));
        }

        info!(
            "Deferred questions synced successfully for task {}",
            task_id
        );
        Ok(())
    }

    /// Sync a verification phase result to the backend.
    pub async fn sync_verification_result(
        &self,
        task_id: &str,
        iteration: u32,
        result_json: &serde_json::Value,
    ) -> Result<(), String> {
        info!(
            "Syncing verification result for task {}, iteration {}",
            task_id, iteration
        );

        let access_token = self.check_auth()?;

        let request = VerificationResultsBatchRequest {
            results: vec![VerificationResultCreateRequest {
                iteration,
                result: result_json.clone(),
            }],
        };

        let response = self
            .client
            .post(format!(
                "{}/api/v1/task-runs/{}/verification-results",
                get_api_base_url(),
                task_id
            ))
            .bearer_auth(&access_token)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                error!("Failed to sync verification result: {}", e);
                format!("Network error: {}", e)
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!(
                "Sync verification result failed with status {}: {}",
                status, error_text
            );
            return Err(format!(
                "Failed to sync verification result: {}",
                error_text
            ));
        }

        info!(
            "Verification result synced successfully for iteration {}",
            iteration
        );
        Ok(())
    }

    /// Sync task completion to the backend.
    pub async fn sync_task_completed(&self, task: &TaskRun) -> Result<AITaskResponse, String> {
        info!("Syncing task completion: {}", task.id);

        let access_token = self.check_auth()?;

        let status = AITaskStatus::from(task.status.as_str());
        let duration = calculate_duration(&task.created_at, task.completed_at.as_deref());

        // Create a summary from output (limit to 10KB for summary)
        let output_summary = if !task.output_log.is_empty() {
            Some(summarize_output(&task.output_log, 10240))
        } else {
            None
        };

        let request = AITaskUpdateRequest {
            status: Some(status),
            sessions_count: Some(task.sessions_count),
            output_summary,
            error_message: task.error_message.clone(),
            duration_seconds: duration,
            completed_at: task.completed_at.clone(),
        };

        let response = self
            .client
            .put(format!(
                "{}/api/v1/task-runs/{}",
                get_api_base_url(),
                task.id
            ))
            .bearer_auth(&access_token)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                error!("Failed to sync task completion: {}", e);
                format!("Network error: {}", e)
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!(
                "Sync task completion failed with status {}: {}",
                status, error_text
            );
            return Err(format!("Failed to sync task completion: {}", error_text));
        }

        let result: AITaskResponse = response.json().await.map_err(|e| {
            error!("Failed to parse task response: {}", e);
            format!("Invalid response from server: {}", e)
        })?;

        info!("Task completion synced successfully: {}", result.id);
        Ok(result)
    }

    /// Full sync: sync a task with all its findings and verification results.
    ///
    /// This is useful for syncing a completed task that was created while offline.
    pub async fn full_sync_task(
        &self,
        db: &Arc<PgDb>,
        task_id: &str,
        project_id: Option<&str>,
    ) -> Result<(), String> {
        info!("Performing full sync for task: {}", task_id);

        // Get task from local database
        let task = db
            .get_task_run(task_id)
            .await?
            .ok_or_else(|| format!("Task {} not found in local database", task_id))?;

        // 1. Create or update task in backend
        let _task_response = self.sync_task_created(&task, project_id).await?;

        // 2. Sync all findings
        let findings = db.get_findings_for_task(task_id).await?;
        if !findings.is_empty() {
            self.sync_findings(task_id, &findings).await?;
        }

        // 3. Sync all deferred questions
        let dq_rows = db
            .get_deferred_questions_for_task_run(task_id)
            .await
            .unwrap_or_default();
        if !dq_rows.is_empty() {
            let sync_questions: Vec<DeferredQuestionSyncRequest> =
                dq_rows.iter().map(json_to_deferred_question_sync).collect();
            self.sync_deferred_questions(task_id, sync_questions)
                .await?;
        }

        // 4. Verification results sync removed — was checkpoint_db-only

        // 5. If task is complete, sync completion status
        if task.status != "running" {
            self.sync_task_completed(&task).await?;
        }

        info!("Full sync completed for task: {}", task_id);
        Ok(())
    }
}

impl Default for AITaskSyncService {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Sync an AI task to the backend.
///
/// Called when a task is created to sync it to the web backend for team visibility.
#[tauri::command]
pub async fn sync_ai_task_created(
    task_id: String,
    project_id: Option<String>,
    storage: tauri::State<'_, StorageCompartment>,
) -> Result<AITaskResponse, String> {
    let task = storage
        .pg_db()
        .get_task_run(&task_id)
        .await?
        .ok_or_else(|| format!("Task {} not found", task_id))?;

    let service = AITaskSyncService::new();
    service
        .sync_task_created(&task, project_id.as_deref())
        .await
}

/// Sync session start to the backend.
#[tauri::command]
pub async fn sync_ai_session_started(
    task_id: String,
    session_number: u32,
) -> Result<AITaskSessionResponse, String> {
    let service = AITaskSyncService::new();
    service.sync_session_started(&task_id, session_number).await
}

/// Sync session end to the backend.
#[tauri::command]
pub async fn sync_ai_session_ended(
    task_id: String,
    session_number: u32,
    duration_seconds: i64,
    output_summary: Option<String>,
) -> Result<AITaskSessionResponse, String> {
    let service = AITaskSyncService::new();
    service
        .sync_session_ended(
            &task_id,
            session_number,
            duration_seconds,
            output_summary.as_deref(),
        )
        .await
}

/// Sync findings to the backend.
#[tauri::command]
pub async fn sync_ai_findings(
    task_id: String,
    storage: tauri::State<'_, StorageCompartment>,
) -> Result<Vec<AITaskFindingSyncResponse>, String> {
    let findings = storage.pg_db().get_findings_for_task(&task_id).await?;

    if findings.is_empty() {
        return Ok(vec![]);
    }

    let service = AITaskSyncService::new();
    service.sync_findings(&task_id, &findings).await
}

/// Sync deferred questions to the backend.
#[tauri::command]
pub async fn sync_deferred_questions(
    task_id: String,
    storage: tauri::State<'_, StorageCompartment>,
) -> Result<(), String> {
    let questions = storage
        .pg_db()
        .get_deferred_questions_for_task_run(&task_id)
        .await?;

    if questions.is_empty() {
        return Ok(());
    }

    let sync_questions: Vec<DeferredQuestionSyncRequest> = questions
        .iter()
        .map(json_to_deferred_question_sync)
        .collect();

    let service = AITaskSyncService::new();
    service
        .sync_deferred_questions(&task_id, sync_questions)
        .await
}

/// Sync task completion to the backend.
#[tauri::command]
pub async fn sync_ai_task_completed(
    task_id: String,
    storage: tauri::State<'_, StorageCompartment>,
) -> Result<AITaskResponse, String> {
    let task = storage
        .pg_db()
        .get_task_run(&task_id)
        .await?
        .ok_or_else(|| format!("Task {} not found", task_id))?;

    let service = AITaskSyncService::new();
    service.sync_task_completed(&task).await
}

/// Perform a full sync of a task (create + findings + completion).
///
/// Useful for syncing tasks that were created while offline.
#[tauri::command]
pub async fn full_sync_ai_task(
    task_id: String,
    project_id: Option<String>,
    storage: tauri::State<'_, StorageCompartment>,
) -> Result<(), String> {
    let service = AITaskSyncService::new();
    service
        .full_sync_task(storage.pg_db(), &task_id, project_id.as_deref())
        .await
}

/// Sync all pending (unsynced) tasks to the backend.
///
/// This scans for tasks that haven't been synced and syncs them.
/// Returns the number of tasks successfully synced.
#[tauri::command]
pub async fn sync_all_pending_ai_tasks(
    project_id: Option<String>,
    storage: tauri::State<'_, StorageCompartment>,
) -> Result<u32, String> {
    let service = AITaskSyncService::new();

    // Check auth first
    if !service.auth_manager.has_tokens() {
        return Err("Not authenticated. Please log in to sync.".to_string());
    }

    // Get recent task runs (limit to last 50)
    let tasks = storage.pg_db().get_recent_task_runs(50, None).await?;

    let mut synced_count = 0u32;

    for task in tasks {
        match service
            .full_sync_task(storage.pg_db(), &task.id, project_id.as_deref())
            .await
        {
            Ok(_) => {
                synced_count += 1;
                info!("Synced task: {}", task.id);
            }
            Err(e) => {
                warn!("Failed to sync task {}: {}", task.id, e);
                // Continue with other tasks
            }
        }
    }

    info!("Synced {} tasks to backend", synced_count);
    Ok(synced_count)
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_task_sync")
        .invoke_handler(tauri::generate_handler![
            sync_ai_task_created,
            sync_ai_session_started,
            sync_ai_session_ended,
            sync_ai_findings,
            sync_deferred_questions,
            sync_ai_task_completed,
            full_sync_ai_task,
            sync_all_pending_ai_tasks,
        ])
        .build()
}
