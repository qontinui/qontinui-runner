//! Web Backend Workflow Client
//!
//! Communicates with the qontinui-web backend for workflow CRUD operations.
//! The web backend (PostgreSQL) is the source of truth for workflow definitions.
//! The runner maintains a local SQLite cache for offline execution.

use crate::auth::AuthManager;
use crate::str_utils::truncate_str;
use crate::unified_workflows::UnifiedWorkflow;
use serde::Deserialize;
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

#[derive(Debug, Deserialize)]
struct PaginatedResponse {
    items: Vec<UnifiedWorkflow>,
    /// Pagination metadata from the backend (not used, but must be accepted to avoid parse failures)
    #[serde(default)]
    #[allow(dead_code)]
    pagination: Option<serde_json::Value>,
}

/// Client for communicating with the web backend's workflow API.
pub struct WebBackendWorkflowClient {
    auth_manager: AuthManager,
    client: reqwest::Client,
}

impl WebBackendWorkflowClient {
    pub fn new() -> Self {
        Self {
            auth_manager: AuthManager::new(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        }
    }

    fn check_auth(&self) -> Result<String, String> {
        if !self.auth_manager.has_tokens() {
            return Err("Not authenticated. Please log in to sync workflows.".to_string());
        }
        self.auth_manager.get_access_token().map_err(|e| {
            error!("Failed to get access token: {}", e);
            format!("Failed to get access token: {}", e)
        })
    }

    /// Fetch a single workflow from the web backend.
    pub async fn fetch_workflow(&self, id: &str) -> Result<UnifiedWorkflow, String> {
        let access_token = self.check_auth()?;

        let response = self
            .client
            .get(format!(
                "{}/api/v1/unified-workflows/{}",
                get_api_base_url(),
                id
            ))
            .bearer_auth(&access_token)
            .send()
            .await
            .map_err(|e| format!("Network error fetching workflow: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(format!(
                "Failed to fetch workflow {} ({}): {}",
                id, status, error_text
            ));
        }

        response
            .json::<UnifiedWorkflow>()
            .await
            .map_err(|e| format!("Failed to parse workflow response: {}", e))
    }

    /// Fetch all workflows from the web backend.
    pub async fn fetch_all_workflows(&self) -> Result<Vec<UnifiedWorkflow>, String> {
        let access_token = self.check_auth()?;

        let response = self
            .client
            .get(format!(
                "{}/api/v1/unified-workflows?limit=200",
                get_api_base_url()
            ))
            .bearer_auth(&access_token)
            .send()
            .await
            .map_err(|e| format!("Network error fetching workflows: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(format!(
                "Failed to fetch workflows ({}): {}",
                status, error_text
            ));
        }

        // Parse as text first so we can log the body on failure
        let body = response
            .text()
            .await
            .map_err(|e| format!("Failed to read workflows response body: {}", e))?;

        let paginated: PaginatedResponse = serde_json::from_str(&body).map_err(|e| {
            // Log a truncated snippet of the body to help debug deserialization issues
            let snippet = if body.len() > 500 {
                format!("{}...", truncate_str(&body, 500))
            } else {
                body.clone()
            };
            warn!(
                "Failed to parse workflows response: {}. Body snippet: {}",
                e, snippet
            );
            format!("Failed to parse workflows response: {}", e)
        })?;

        Ok(paginated.items)
    }

    /// Save a new workflow to the web backend.
    pub async fn save_workflow(
        &self,
        workflow: &UnifiedWorkflow,
    ) -> Result<UnifiedWorkflow, String> {
        let access_token = self.check_auth()?;

        let response = self
            .client
            .post(format!("{}/api/v1/unified-workflows", get_api_base_url()))
            .bearer_auth(&access_token)
            .json(workflow)
            .send()
            .await
            .map_err(|e| format!("Network error saving workflow: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(format!(
                "Failed to save workflow ({}): {}",
                status, error_text
            ));
        }

        response
            .json::<UnifiedWorkflow>()
            .await
            .map_err(|e| format!("Failed to parse save response: {}", e))
    }

    /// Update an existing workflow on the web backend.
    pub async fn update_workflow(
        &self,
        id: &str,
        workflow: &UnifiedWorkflow,
    ) -> Result<UnifiedWorkflow, String> {
        let access_token = self.check_auth()?;

        let response = self
            .client
            .put(format!(
                "{}/api/v1/unified-workflows/{}",
                get_api_base_url(),
                id
            ))
            .bearer_auth(&access_token)
            .json(workflow)
            .send()
            .await
            .map_err(|e| format!("Network error updating workflow: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(format!(
                "Failed to update workflow {} ({}): {}",
                id, status, error_text
            ));
        }

        response
            .json::<UnifiedWorkflow>()
            .await
            .map_err(|e| format!("Failed to parse update response: {}", e))
    }

    /// Delete a workflow from the web backend.
    pub async fn delete_workflow(&self, id: &str) -> Result<(), String> {
        let access_token = self.check_auth()?;

        let response = self
            .client
            .delete(format!(
                "{}/api/v1/unified-workflows/{}",
                get_api_base_url(),
                id
            ))
            .bearer_auth(&access_token)
            .send()
            .await
            .map_err(|e| format!("Network error deleting workflow: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(format!(
                "Failed to delete workflow {} ({}): {}",
                id, status, error_text
            ));
        }

        Ok(())
    }
}

/// Sync all workflows from the web backend to local SQLite cache.
/// Called on startup and periodically.
pub async fn sync_workflows_from_backend(
    db: &crate::database::CheckpointDb,
) -> Result<usize, String> {
    let client = WebBackendWorkflowClient::new();

    let workflows = match client.fetch_all_workflows().await {
        Ok(w) => w,
        Err(e) => {
            warn!("Could not sync workflows from backend: {}", e);
            return Err(e);
        }
    };

    let count = workflows.len();
    info!(
        "Syncing {} workflows from web backend to local cache",
        count
    );

    for workflow in &workflows {
        // Check if workflow exists locally
        match db.get_unified_workflow(&workflow.id) {
            Ok(Some(_existing)) => {
                // Update local cache
                let update_req = crate::unified_workflows::UpdateUnifiedWorkflowRequest {
                    name: Some(workflow.name.clone()),
                    description: Some(workflow.description.clone()),
                    category: Some(workflow.category.clone()),
                    tags: Some(workflow.tags.clone()),
                    setup_steps: Some(workflow.setup_steps.clone()),
                    verification_steps: Some(workflow.verification_steps.clone()),
                    agentic_steps: Some(workflow.agentic_steps.clone()),
                    completion_steps: Some(workflow.completion_steps.clone()),
                    max_iterations: Some(workflow.max_iterations),
                    timeout_seconds: Some(workflow.timeout_seconds),
                    provider: workflow.provider.clone(),
                    model: workflow.model.clone(),
                    skip_ai_summary: Some(workflow.skip_ai_summary),
                    log_source_selection: Some(workflow.log_source_selection.clone()),
                    context_ids: Some(workflow.context_ids.clone()),
                    disabled_context_ids: Some(workflow.disabled_context_ids.clone()),
                    auto_include_contexts: Some(workflow.auto_include_contexts),
                    prompt_template: workflow.prompt_template.clone(),
                    log_watch_enabled: Some(workflow.log_watch_enabled),
                    health_check_enabled: Some(workflow.health_check_enabled),
                    health_check_urls: Some(workflow.health_check_urls.clone()),
                    preflight_check_enabled: Some(workflow.preflight_check_enabled),
                    enable_sweep: Some(workflow.enable_sweep),
                    max_sweep_iterations: Some(workflow.max_sweep_iterations),
                    stages: Some(workflow.stages.clone()),
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
                if let Err(e) = db.update_unified_workflow(&workflow.id, &update_req) {
                    warn!("Failed to update cached workflow {}: {}", workflow.id, e);
                }
            }
            Ok(None) => {
                // Create locally
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
                    skip_ai_summary: workflow.skip_ai_summary,
                    log_source_selection: Some(workflow.log_source_selection.clone()),
                    context_ids: Some(workflow.context_ids.clone()),
                    disabled_context_ids: Some(workflow.disabled_context_ids.clone()),
                    auto_include_contexts: Some(workflow.auto_include_contexts),
                    prompt_template: workflow.prompt_template.clone(),
                    log_watch_enabled: Some(workflow.log_watch_enabled),
                    health_check_enabled: Some(workflow.health_check_enabled),
                    health_check_urls: Some(workflow.health_check_urls.clone()),
                    preflight_check_enabled: Some(workflow.preflight_check_enabled),
                    targeted_error_ids: None,
                    generated_by_task_run_id: workflow.generated_by_task_run_id.clone(),
                    enable_sweep: Some(workflow.enable_sweep),
                    max_sweep_iterations: Some(workflow.max_sweep_iterations),
                    stages: Some(workflow.stages.clone()),
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
                if let Err(e) = db.create_unified_workflow(&create_req) {
                    warn!("Failed to cache workflow {}: {}", workflow.id, e);
                }
            }
            Err(e) => {
                warn!(
                    "Error checking local cache for workflow {}: {}",
                    workflow.id, e
                );
            }
        }
    }

    // Push any locally-modified workflows (sync_pending = 1) to the backend
    push_pending_workflows(db, &client).await;

    info!("Workflow sync complete: {} workflows from backend", count);
    Ok(count)
}

/// Push locally-created/modified workflows to the web backend.
async fn push_pending_workflows(
    db: &crate::database::CheckpointDb,
    client: &WebBackendWorkflowClient,
) {
    let pending = match db.get_pending_sync_workflows() {
        Ok(w) => w,
        Err(e) => {
            warn!("Could not get pending workflows: {}", e);
            return;
        }
    };

    if pending.is_empty() {
        return;
    }

    info!("Pushing {} pending workflows to web backend", pending.len());

    for workflow in &pending {
        match client.save_workflow(workflow).await {
            Ok(_) => {
                // Clear sync_pending flag
                if let Err(e) = db.clear_sync_pending(&workflow.id) {
                    warn!("Failed to clear sync_pending for {}: {}", workflow.id, e);
                }
            }
            Err(e) => {
                warn!("Failed to push workflow {} to backend: {}", workflow.id, e);
            }
        }
    }
}
