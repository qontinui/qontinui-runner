//! Library Sync commands for syncing library items to qontinui-web backend.
//!
//! This module syncs library items (checks, check groups, shell commands,
//! saved API requests, contexts, macros, prompt snippets) from the runner's local
//! storage (SQLite + JSON files) to the web backend's PostgreSQL database.
//!
//! Follows the same patterns as task_sync.rs for consistency.

use crate::auth::AuthManager;
use crate::context;
use crate::database::CheckpointDb;
use crate::macros as macro_lib;
use crate::prompt_snippets;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
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
// Request Types (matching backend schemas)
// ============================================================================

/// Request to create/update a check on the backend.
#[derive(Debug, Serialize)]
struct CheckSyncRequest {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    check_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    working_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_path: Option<String>,
    auto_fix: bool,
    fail_on_warning: bool,
    is_critical: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_seconds: Option<u32>,
    enabled: bool,
    tags: Vec<String>,
}

/// Request to create/update a check group on the backend.
#[derive(Debug, Serialize)]
struct CheckGroupSyncRequest {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    check_ids: Vec<String>,
    stop_on_failure: bool,
    run_in_parallel: bool,
    tags: Vec<String>,
}

/// Request to create/update a shell command on the backend.
#[derive(Debug, Serialize)]
struct ShellCommandSyncRequest {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    working_directory: Option<String>,
    timeout_seconds: i32,
    fail_on_error: bool,
    enabled: bool,
    tags: Vec<String>,
}

/// Request to create/update a saved API request on the backend.
#[derive(Debug, Serialize)]
struct SavedApiRequestSyncRequest {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    method: String,
    url: String,
    headers: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
    timeout_ms: u64,
    tags: Vec<String>,
}

/// Request to create/update a context on the backend.
#[derive(Debug, Serialize)]
struct ContextSyncRequest {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    auto_include: Option<serde_json::Value>,
    tags: Vec<String>,
}

/// Request to create/update a macro on the backend.
#[derive(Debug, Serialize)]
struct MacroSyncRequest {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    steps: Vec<serde_json::Value>,
    tags: Vec<String>,
}

/// Request to create/update a prompt snippet on the backend.
#[derive(Debug, Serialize)]
struct PromptSnippetSyncRequest {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    language: String,
    code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    tags: Vec<String>,
}

// ============================================================================
// Response Types
// ============================================================================

/// Generic response from the backend for a synced library item.
#[derive(Debug, Deserialize, Serialize)]
pub struct LibraryItemResponse {
    pub id: String,
    pub name: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Result of a library sync operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibrarySyncResult {
    pub item_type: String,
    pub synced: u32,
    pub failed: u32,
    pub errors: Vec<String>,
}

/// Combined result of syncing all library types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullLibrarySyncResult {
    pub results: Vec<LibrarySyncResult>,
    pub total_synced: u32,
    pub total_failed: u32,
}

// ============================================================================
// Library Sync Service
// ============================================================================

/// Service for syncing library items to qontinui-web backend.
///
/// This service handles:
/// - Reading library items from local storage (SQLite for checks, shell commands,
///   check groups, saved API requests; JSON files for contexts, macros, prompt snippets)
/// - Upserting them to the backend via POST endpoints
/// - Tracking sync results per item type
pub struct LibrarySyncService {
    auth_manager: AuthManager,
    client: reqwest::Client,
}

impl LibrarySyncService {
    pub fn new() -> Self {
        Self {
            auth_manager: AuthManager::new(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    /// Check if authenticated and can sync.
    fn check_auth(&self) -> Result<String, String> {
        if !self.auth_manager.has_tokens() {
            return Err("Not authenticated. Please log in to sync library items.".to_string());
        }

        self.auth_manager.get_access_token().map_err(|e| {
            error!("Failed to get access token: {}", e);
            format!("Failed to get access token: {}", e)
        })
    }

    /// POST a single item to the backend, returning success/failure.
    async fn post_item<T: Serialize>(
        &self,
        access_token: &str,
        endpoint: &str,
        payload: &T,
    ) -> Result<(), String> {
        let url = format!("{}/api/v1/library/{}", get_api_base_url(), endpoint);

        let response = self
            .client
            .post(&url)
            .bearer_auth(access_token)
            .json(payload)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    "Request timed out".to_string()
                } else if e.is_connect() {
                    "Failed to connect to server".to_string()
                } else {
                    format!("Network error: {}", e)
                }
            })?;

        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            Err(format!("HTTP {} - {}", status, error_text))
        }
    }

    /// Sync all checks from the runner's SQLite database.
    pub async fn sync_checks(&self, db: &Arc<CheckpointDb>) -> LibrarySyncResult {
        let mut result = LibrarySyncResult {
            item_type: "checks".to_string(),
            synced: 0,
            failed: 0,
            errors: Vec::new(),
        };

        let access_token = match self.check_auth() {
            Ok(t) => t,
            Err(e) => {
                result.failed = 1;
                result.errors.push(e);
                return result;
            }
        };

        let checks = match db.list_checks(false, None, None) {
            Ok(c) => c,
            Err(e) => {
                result
                    .errors
                    .push(format!("Failed to read checks from DB: {}", e));
                result.failed = 1;
                return result;
            }
        };

        info!("Syncing {} checks to backend", checks.len());

        for check in &checks {
            let payload = CheckSyncRequest {
                name: check.name.clone(),
                description: check.description.clone(),
                check_type: check.check_type.clone(),
                tool: Some(check.tool.clone()),
                command: check.command.clone(),
                working_directory: check.working_directory.clone(),
                config_path: check.config_path.clone(),
                auto_fix: check.auto_fix,
                fail_on_warning: check.fail_on_warning,
                is_critical: check.is_critical,
                timeout_seconds: check.timeout_seconds,
                enabled: check.enabled,
                tags: check.tags.clone(),
            };

            match self.post_item(&access_token, "checks", &payload).await {
                Ok(()) => result.synced += 1,
                Err(e) => {
                    warn!("Failed to sync check '{}': {}", check.name, e);
                    result.failed += 1;
                    result.errors.push(format!("Check '{}': {}", check.name, e));
                }
            }
        }

        info!(
            "Checks sync complete: {} synced, {} failed",
            result.synced, result.failed
        );
        result
    }

    /// Sync all check groups from the runner's SQLite database.
    pub async fn sync_check_groups(&self, db: &Arc<CheckpointDb>) -> LibrarySyncResult {
        let mut result = LibrarySyncResult {
            item_type: "check_groups".to_string(),
            synced: 0,
            failed: 0,
            errors: Vec::new(),
        };

        let access_token = match self.check_auth() {
            Ok(t) => t,
            Err(e) => {
                result.failed = 1;
                result.errors.push(e);
                return result;
            }
        };

        let groups = match db.list_check_groups(false) {
            Ok(g) => g,
            Err(e) => {
                result
                    .errors
                    .push(format!("Failed to read check groups from DB: {}", e));
                result.failed = 1;
                return result;
            }
        };

        info!("Syncing {} check groups to backend", groups.len());

        for group in &groups {
            // Collect check IDs from the group's checks
            let check_ids: Vec<String> = group.checks.iter().map(|c| c.id.clone()).collect();

            let payload = CheckGroupSyncRequest {
                name: group.name.clone(),
                description: group.description.clone(),
                check_ids,
                stop_on_failure: group.stop_on_failure,
                run_in_parallel: group.run_in_parallel,
                tags: group.tags.clone(),
            };

            match self
                .post_item(&access_token, "check-groups", &payload)
                .await
            {
                Ok(()) => result.synced += 1,
                Err(e) => {
                    warn!("Failed to sync check group '{}': {}", group.name, e);
                    result.failed += 1;
                    result
                        .errors
                        .push(format!("CheckGroup '{}': {}", group.name, e));
                }
            }
        }

        info!(
            "Check groups sync complete: {} synced, {} failed",
            result.synced, result.failed
        );
        result
    }

    /// Sync all shell commands from the runner's SQLite database.
    pub async fn sync_shell_commands(&self, db: &Arc<CheckpointDb>) -> LibrarySyncResult {
        let mut result = LibrarySyncResult {
            item_type: "shell_commands".to_string(),
            synced: 0,
            failed: 0,
            errors: Vec::new(),
        };

        let access_token = match self.check_auth() {
            Ok(t) => t,
            Err(e) => {
                result.failed = 1;
                result.errors.push(e);
                return result;
            }
        };

        let commands = match db.list_shell_commands(false, None) {
            Ok(c) => c,
            Err(e) => {
                result
                    .errors
                    .push(format!("Failed to read shell commands from DB: {}", e));
                result.failed = 1;
                return result;
            }
        };

        info!("Syncing {} shell commands to backend", commands.len());

        for cmd in &commands {
            let payload = ShellCommandSyncRequest {
                name: cmd.name.clone(),
                description: cmd.description.clone(),
                command: cmd.command.clone(),
                working_directory: cmd.working_directory.clone(),
                timeout_seconds: cmd.timeout_seconds,
                fail_on_error: cmd.fail_on_error,
                enabled: cmd.enabled,
                tags: cmd.tags.clone(),
            };

            match self
                .post_item(&access_token, "shell-commands", &payload)
                .await
            {
                Ok(()) => result.synced += 1,
                Err(e) => {
                    warn!("Failed to sync shell command '{}': {}", cmd.name, e);
                    result.failed += 1;
                    result
                        .errors
                        .push(format!("ShellCommand '{}': {}", cmd.name, e));
                }
            }
        }

        info!(
            "Shell commands sync complete: {} synced, {} failed",
            result.synced, result.failed
        );
        result
    }

    /// Sync all saved API requests from the runner's SQLite database.
    pub async fn sync_saved_api_requests(&self, db: &Arc<CheckpointDb>) -> LibrarySyncResult {
        let mut result = LibrarySyncResult {
            item_type: "saved_api_requests".to_string(),
            synced: 0,
            failed: 0,
            errors: Vec::new(),
        };

        let access_token = match self.check_auth() {
            Ok(t) => t,
            Err(e) => {
                result.failed = 1;
                result.errors.push(e);
                return result;
            }
        };

        let requests = match db.list_saved_api_requests() {
            Ok(r) => r,
            Err(e) => {
                result
                    .errors
                    .push(format!("Failed to read saved API requests from DB: {}", e));
                result.failed = 1;
                return result;
            }
        };

        info!("Syncing {} saved API requests to backend", requests.len());

        for req in &requests {
            let payload = SavedApiRequestSyncRequest {
                name: req.name.clone(),
                description: if req.description.is_empty() {
                    None
                } else {
                    Some(req.description.clone())
                },
                method: format!("{:?}", req.method).to_uppercase(),
                url: req.url.clone(),
                headers: req.headers.clone(),
                body: req.body.clone(),
                timeout_ms: req.timeout_ms,
                tags: req.tags.clone(),
            };

            match self
                .post_item(&access_token, "api-requests", &payload)
                .await
            {
                Ok(()) => result.synced += 1,
                Err(e) => {
                    warn!("Failed to sync API request '{}': {}", req.name, e);
                    result.failed += 1;
                    result
                        .errors
                        .push(format!("ApiRequest '{}': {}", req.name, e));
                }
            }
        }

        info!(
            "Saved API requests sync complete: {} synced, {} failed",
            result.synced, result.failed
        );
        result
    }

    /// Sync all contexts from the runner's JSON file storage.
    pub async fn sync_contexts(&self) -> LibrarySyncResult {
        let mut result = LibrarySyncResult {
            item_type: "contexts".to_string(),
            synced: 0,
            failed: 0,
            errors: Vec::new(),
        };

        let access_token = match self.check_auth() {
            Ok(t) => t,
            Err(e) => {
                result.failed = 1;
                result.errors.push(e);
                return result;
            }
        };

        let contexts = context::get_all_user_contexts();

        info!("Syncing {} contexts to backend", contexts.len());

        for ctx in &contexts {
            // Convert auto_include to JSON value if present
            let auto_include = ctx
                .auto_include
                .as_ref()
                .and_then(|ai| serde_json::to_value(ai).ok());

            let payload = ContextSyncRequest {
                name: ctx.name.clone(),
                description: None, // Runner contexts don't have a separate description field
                content: ctx.content.clone(),
                category: ctx.category.clone(),
                enabled: true,
                auto_include,
                tags: ctx.tags.clone(),
            };

            match self.post_item(&access_token, "contexts", &payload).await {
                Ok(()) => result.synced += 1,
                Err(e) => {
                    warn!("Failed to sync context '{}': {}", ctx.name, e);
                    result.failed += 1;
                    result.errors.push(format!("Context '{}': {}", ctx.name, e));
                }
            }
        }

        info!(
            "Contexts sync complete: {} synced, {} failed",
            result.synced, result.failed
        );
        result
    }

    /// Sync all macros from the runner's JSON file storage.
    pub async fn sync_macros(&self) -> LibrarySyncResult {
        let mut result = LibrarySyncResult {
            item_type: "macros".to_string(),
            synced: 0,
            failed: 0,
            errors: Vec::new(),
        };

        let access_token = match self.check_auth() {
            Ok(t) => t,
            Err(e) => {
                result.failed = 1;
                result.errors.push(e);
                return result;
            }
        };

        let macros = macro_lib::load_macros();

        info!("Syncing {} macros to backend", macros.len());

        for m in &macros {
            // Serialize steps to JSON values
            let steps: Vec<serde_json::Value> = m
                .steps
                .iter()
                .filter_map(|s| serde_json::to_value(s).ok())
                .collect();

            let payload = MacroSyncRequest {
                name: m.name.clone(),
                description: if m.description.is_empty() {
                    None
                } else {
                    Some(m.description.clone())
                },
                category: if m.category.is_empty() {
                    None
                } else {
                    Some(m.category.clone())
                },
                steps,
                tags: m.tags.clone(),
            };

            match self.post_item(&access_token, "macros", &payload).await {
                Ok(()) => result.synced += 1,
                Err(e) => {
                    warn!("Failed to sync macro '{}': {}", m.name, e);
                    result.failed += 1;
                    result.errors.push(format!("Macro '{}': {}", m.name, e));
                }
            }
        }

        info!(
            "Macros sync complete: {} synced, {} failed",
            result.synced, result.failed
        );
        result
    }

    /// Sync all prompt snippets from the runner's JSON file storage.
    pub async fn sync_prompt_snippets(&self) -> LibrarySyncResult {
        let mut result = LibrarySyncResult {
            item_type: "prompt_snippets".to_string(),
            synced: 0,
            failed: 0,
            errors: Vec::new(),
        };

        let access_token = match self.check_auth() {
            Ok(t) => t,
            Err(e) => {
                result.failed = 1;
                result.errors.push(e);
                return result;
            }
        };

        let all_snippets = prompt_snippets::get_all_prompt_snippets();

        info!("Syncing {} prompt snippets to backend", all_snippets.len());

        for s in &all_snippets {
            let payload = PromptSnippetSyncRequest {
                name: s.name.clone(),
                description: None,
                language: "text".to_string(), // Prompt snippets in runner are text snippets
                code: s.content.clone(),
                category: if s.category.is_empty() {
                    None
                } else {
                    Some(s.category.clone())
                },
                tags: s.tags.clone(),
            };

            match self
                .post_item(&access_token, "prompt-snippets", &payload)
                .await
            {
                Ok(()) => result.synced += 1,
                Err(e) => {
                    warn!("Failed to sync prompt snippet '{}': {}", s.name, e);
                    result.failed += 1;
                    result
                        .errors
                        .push(format!("PromptSnippet '{}': {}", s.name, e));
                }
            }
        }

        info!(
            "Prompt snippets sync complete: {} synced, {} failed",
            result.synced, result.failed
        );
        result
    }

    /// Full sync: sync all library item types to the backend.
    pub async fn full_sync(&self, db: &Arc<CheckpointDb>) -> FullLibrarySyncResult {
        info!("Starting full library sync to backend");

        let mut results = Vec::new();

        // Sync items stored in SQLite
        results.push(self.sync_checks(db).await);
        results.push(self.sync_check_groups(db).await);
        results.push(self.sync_shell_commands(db).await);
        results.push(self.sync_saved_api_requests(db).await);

        // Sync items stored in JSON files
        results.push(self.sync_contexts().await);
        results.push(self.sync_macros().await);
        results.push(self.sync_prompt_snippets().await);

        let total_synced: u32 = results.iter().map(|r| r.synced).sum();
        let total_failed: u32 = results.iter().map(|r| r.failed).sum();

        info!(
            "Full library sync complete: {} synced, {} failed",
            total_synced, total_failed
        );

        FullLibrarySyncResult {
            results,
            total_synced,
            total_failed,
        }
    }
}

impl Default for LibrarySyncService {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Sync all library items to the backend.
///
/// Reads checks, check groups, shell commands, saved API requests, contexts,
/// macros, and prompt snippets from local storage and POSTs them to the backend.
#[tauri::command]
pub async fn sync_library_to_backend(
    app_state: tauri::State<'_, Arc<crate::commands::AppState>>,
) -> Result<FullLibrarySyncResult, String> {
    let service = LibrarySyncService::new();

    if !service.auth_manager.has_tokens() {
        return Err("Not authenticated. Please log in to sync library items.".to_string());
    }

    Ok(service.full_sync(&app_state.checkpoint_db).await)
}

/// Sync only checks to the backend.
#[tauri::command]
pub async fn sync_checks_to_backend(
    app_state: tauri::State<'_, Arc<crate::commands::AppState>>,
) -> Result<LibrarySyncResult, String> {
    let service = LibrarySyncService::new();
    Ok(service.sync_checks(&app_state.checkpoint_db).await)
}

/// Sync only check groups to the backend.
#[tauri::command]
pub async fn sync_check_groups_to_backend(
    app_state: tauri::State<'_, Arc<crate::commands::AppState>>,
) -> Result<LibrarySyncResult, String> {
    let service = LibrarySyncService::new();
    Ok(service.sync_check_groups(&app_state.checkpoint_db).await)
}

/// Sync only shell commands to the backend.
#[tauri::command]
pub async fn sync_shell_commands_to_backend(
    app_state: tauri::State<'_, Arc<crate::commands::AppState>>,
) -> Result<LibrarySyncResult, String> {
    let service = LibrarySyncService::new();
    Ok(service.sync_shell_commands(&app_state.checkpoint_db).await)
}

/// Sync only saved API requests to the backend.
#[tauri::command]
pub async fn sync_api_requests_to_backend(
    app_state: tauri::State<'_, Arc<crate::commands::AppState>>,
) -> Result<LibrarySyncResult, String> {
    let service = LibrarySyncService::new();
    Ok(service
        .sync_saved_api_requests(&app_state.checkpoint_db)
        .await)
}

/// Sync only contexts to the backend.
#[tauri::command]
pub async fn sync_contexts_to_backend() -> Result<LibrarySyncResult, String> {
    let service = LibrarySyncService::new();
    Ok(service.sync_contexts().await)
}

/// Sync only macros to the backend.
#[tauri::command]
pub async fn sync_macros_to_backend() -> Result<LibrarySyncResult, String> {
    let service = LibrarySyncService::new();
    Ok(service.sync_macros().await)
}

/// Sync only prompt snippets to the backend.
#[tauri::command]
pub async fn sync_prompt_snippets_to_backend() -> Result<LibrarySyncResult, String> {
    let service = LibrarySyncService::new();
    Ok(service.sync_prompt_snippets().await)
}
