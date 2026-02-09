//! AI Context CRUD handlers for MCP API
//!
//! Provides HTTP handlers for managing AI contexts (user, project, builtin):
//! list, create, update, delete, duplicate, categories, tags, enable/disable,
//! and web sync approval/dismissal.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::context;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Request Types
// ============================================================================

/// Request body for creating a context
#[derive(Debug, Deserialize)]
pub struct CreateContextRequest {
    pub name: String,
    pub content: String,
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(rename = "autoInclude")]
    pub auto_include: Option<context::ContextAutoInclude>,
}

/// Request body for updating a context
#[derive(Debug, Deserialize)]
pub struct UpdateContextRequest {
    pub name: Option<String>,
    pub content: Option<String>,
    pub category: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
    #[serde(rename = "autoInclude")]
    pub auto_include: Option<Option<context::ContextAutoInclude>>,
}

/// Request body for duplicating a context
#[derive(Debug, Deserialize)]
pub struct DuplicateContextRequest {
    #[serde(rename = "targetScope")]
    pub target_scope: String,
}

// ============================================================================
// Helpers
// ============================================================================

/// Convert Context to ContextWithMetadata
fn context_to_with_metadata(
    ctx: context::Context,
    scope: context::ContextScope,
    library: &context::UserContextLibrary,
) -> context::ContextWithMetadata {
    let metadata = library.metadata.iter().find(|m| m.context_id == ctx.id);

    context::ContextWithMetadata {
        context: ctx,
        scope,
        enabled: metadata.map(|m| m.enabled).unwrap_or(true),
        use_count: metadata.map(|m| m.use_count).unwrap_or(0),
        last_used_at: metadata.and_then(|m| m.last_used_at.clone()),
        web_sync_status: metadata.and_then(|m| m.web_sync_status.clone()),
    }
}

// ============================================================================
// Handlers
// ============================================================================

/// GET /contexts - List all contexts from all scopes
pub async fn list_all_contexts(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<context::ContextWithMetadata>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let library = context::load_user_context_library();
    let mut all_contexts: Vec<context::ContextWithMetadata> = Vec::new();

    // Add project contexts from loaded config (if any)
    if let Ok(config_lock) = state.app_state.current_config.lock() {
        if let Some(ref config) = *config_lock {
            for ctx in context::get_project_contexts_from_config(&config.contexts) {
                all_contexts.push(context_to_with_metadata(
                    ctx,
                    context::ContextScope::Project,
                    &library,
                ));
            }
        }
    }

    // Add user contexts
    for ctx in context::get_all_user_contexts() {
        all_contexts.push(context_to_with_metadata(
            ctx,
            context::ContextScope::User,
            &library,
        ));
    }

    // Add builtin contexts
    for ctx in context::get_builtin_contexts() {
        all_contexts.push(context_to_with_metadata(
            ctx,
            context::ContextScope::Builtin,
            &library,
        ));
    }

    Ok(Json(ApiResponse::success(all_contexts)))
}

/// GET /contexts/categories - List all unique categories
pub async fn list_context_categories(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut categories = context::get_user_context_categories();

    // Add categories from project contexts
    if let Ok(config_lock) = state.app_state.current_config.lock() {
        if let Some(ref config) = *config_lock {
            for ctx in context::get_project_contexts_from_config(&config.contexts) {
                if let Some(cat) = ctx.category {
                    if !categories.contains(&cat) {
                        categories.push(cat);
                    }
                }
            }
        }
    }

    // Add categories from builtin contexts
    for ctx in context::get_builtin_contexts() {
        if let Some(cat) = ctx.category {
            if !categories.contains(&cat) {
                categories.push(cat);
            }
        }
    }

    categories.sort();
    categories.dedup();

    Ok(Json(ApiResponse::success(categories)))
}

/// GET /contexts/tags - List all unique tags
pub async fn list_context_tags(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let library = context::load_user_context_library();
    let mut tags: Vec<String> = Vec::new();

    // Collect tags from user contexts
    for ctx in &library.contexts {
        for tag in &ctx.tags {
            if !tags.contains(tag) {
                tags.push(tag.clone());
            }
        }
    }

    // Collect tags from project contexts
    if let Ok(config_lock) = state.app_state.current_config.lock() {
        if let Some(ref config) = *config_lock {
            for ctx in context::get_project_contexts_from_config(&config.contexts) {
                for tag in ctx.tags {
                    if !tags.contains(&tag) {
                        tags.push(tag);
                    }
                }
            }
        }
    }

    // Collect tags from builtin contexts
    for ctx in context::get_builtin_contexts() {
        for tag in ctx.tags {
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
    }

    tags.sort();

    Ok(Json(ApiResponse::success(tags)))
}

/// POST /contexts/{scope} - Create a new context
pub async fn create_context_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(scope): axum::extract::Path<String>,
    Json(req): Json<CreateContextRequest>,
) -> Result<Json<ApiResponse<context::ContextWithMetadata>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scope.as_str() {
        "project" => {
            // Project contexts are stored in the loaded config
            let ctx = {
                let mut config_lock = state.app_state.current_config.lock().map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!("Failed to lock config: {}", e))),
                    )
                })?;

                let config = config_lock.as_mut().ok_or_else(|| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(api_error(
                            "No project loaded. Please load a project configuration first.",
                        )),
                    )
                })?;

                // Create the context
                let ctx = context::create_project_context(
                    req.name,
                    req.content,
                    req.category,
                    req.tags,
                    req.auto_include,
                );

                // Add to config
                context::add_project_context_to_config(&mut config.contexts, ctx.clone()).map_err(
                    |e| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(api_error(format!("Failed to add context to config: {}", e))),
                        )
                    },
                )?;

                ctx
            }; // config_lock dropped here

            // Save the config to the file
            if let Err(e) = crate::mcp::shared::save_current_config_to_file(&state.app_state) {
                warn!(
                    "Failed to save config after creating project context: {}",
                    e
                );
            }

            // Mark as pending sync to qontinui-web
            if let Err(e) =
                context::set_web_sync_status(&ctx.id, Some(context::WebSyncStatus::Pending))
            {
                warn!("Failed to set pending sync status for context: {}", e);
            }

            let library = context::load_user_context_library();
            Ok(Json(ApiResponse::success(context_to_with_metadata(
                ctx,
                context::ContextScope::Project,
                &library,
            ))))
        }
        "user" => {
            // User contexts are stored in the user library
            match context::create_user_context(
                req.name,
                req.content,
                req.category,
                req.tags,
                req.auto_include,
            ) {
                Ok(ctx) => {
                    let library = context::load_user_context_library();
                    Ok(Json(ApiResponse::success(context_to_with_metadata(
                        ctx,
                        context::ContextScope::User,
                        &library,
                    ))))
                }
                Err(e) => Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to create context: {}", e))),
                )),
            }
        }
        "builtin" => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("Cannot create builtin contexts")),
        )),
        _ => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Invalid scope: {}", scope))),
        )),
    }
}

/// PUT /contexts/{scope}/{id} - Update a context
pub async fn update_context_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path((scope, id)): axum::extract::Path<(String, String)>,
    Json(req): Json<UpdateContextRequest>,
) -> Result<Json<ApiResponse<context::ContextWithMetadata>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scope.as_str() {
        "project" => {
            // Project contexts are stored in the loaded config
            let updated_ctx = {
                let mut config_lock = state.app_state.current_config.lock().map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!("Failed to lock config: {}", e))),
                    )
                })?;

                let config = config_lock.as_mut().ok_or_else(|| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(api_error(
                            "No project loaded. Please load a project configuration first.",
                        )),
                    )
                })?;

                // Get the existing context
                let existing = context::get_project_context_from_config(&config.contexts, &id)
                    .ok_or_else(|| {
                        (
                            StatusCode::NOT_FOUND,
                            Json(api_error(format!("Context not found: {}", id))),
                        )
                    })?;

                // Create updated context
                let updated_ctx = context::Context {
                    id: existing.id,
                    name: req.name.unwrap_or(existing.name),
                    content: req.content.unwrap_or(existing.content),
                    category: req.category.unwrap_or(existing.category),
                    tags: req.tags.unwrap_or(existing.tags),
                    auto_include: req.auto_include.unwrap_or(existing.auto_include),
                    created_at: existing.created_at,
                    modified_at: chrono::Utc::now().to_rfc3339(),
                };

                // Update in config
                context::update_project_context_in_config(
                    &mut config.contexts,
                    updated_ctx.clone(),
                )
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!("Failed to update context: {}", e))),
                    )
                })?;

                updated_ctx
            }; // config_lock dropped here

            // Save the config to the file
            if let Err(e) = crate::mcp::shared::save_current_config_to_file(&state.app_state) {
                warn!(
                    "Failed to save config after updating project context: {}",
                    e
                );
            }

            let library = context::load_user_context_library();
            Ok(Json(ApiResponse::success(context_to_with_metadata(
                updated_ctx,
                context::ContextScope::Project,
                &library,
            ))))
        }
        "user" => {
            // User contexts are stored in the user library
            match context::update_user_context(
                &id,
                req.name,
                req.content,
                req.category,
                req.tags,
                req.auto_include,
            ) {
                Ok(ctx) => {
                    let library = context::load_user_context_library();
                    Ok(Json(ApiResponse::success(context_to_with_metadata(
                        ctx,
                        context::ContextScope::User,
                        &library,
                    ))))
                }
                Err(e) => Err((
                    StatusCode::NOT_FOUND,
                    Json(api_error(format!("Context not found: {}", e))),
                )),
            }
        }
        "builtin" => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("Cannot update builtin contexts")),
        )),
        _ => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Invalid scope: {}", scope))),
        )),
    }
}

/// DELETE /contexts/{scope}/{id} - Delete a context
pub async fn delete_context_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path((scope, id)): axum::extract::Path<(String, String)>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scope.as_str() {
        "project" => {
            // Project contexts are stored in the loaded config
            {
                let mut config_lock = state.app_state.current_config.lock().map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!("Failed to lock config: {}", e))),
                    )
                })?;

                let config = config_lock.as_mut().ok_or_else(|| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(api_error(
                            "No project loaded. Please load a project configuration first.",
                        )),
                    )
                })?;

                context::delete_project_context_from_config(&mut config.contexts, &id).map_err(
                    |e| {
                        (
                            StatusCode::NOT_FOUND,
                            Json(api_error(format!("Context not found: {}", e))),
                        )
                    },
                )?;
            } // config_lock dropped here

            // Save the config to the file
            if let Err(e) = crate::mcp::shared::save_current_config_to_file(&state.app_state) {
                warn!(
                    "Failed to save config after deleting project context: {}",
                    e
                );
            }

            Ok(Json(ApiResponse::success(())))
        }
        "user" => match context::delete_user_context(&id) {
            Ok(()) => Ok(Json(ApiResponse::success(()))),
            Err(e) => Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Context not found: {}", e))),
            )),
        },
        "builtin" => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("Cannot delete builtin contexts")),
        )),
        _ => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Invalid scope: {}", scope))),
        )),
    }
}

/// POST /contexts/{scope}/{id}/duplicate - Duplicate a context
pub async fn duplicate_context_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path((scope, id)): axum::extract::Path<(String, String)>,
    Json(req): Json<DuplicateContextRequest>,
) -> Result<Json<ApiResponse<context::ContextWithMetadata>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Find the source context from the appropriate scope
    let source_ctx = match scope.as_str() {
        "builtin" => context::get_builtin_contexts()
            .into_iter()
            .find(|c| c.id == id),
        "project" => {
            // Try to find in project contexts
            let config_lock = state.app_state.current_config.lock().map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to lock config: {}", e))),
                )
            })?;

            if let Some(ref config) = *config_lock {
                context::get_project_context_from_config(&config.contexts, &id)
            } else {
                None
            }
        }
        _ => context::get_user_context(&id),
    };

    let Some(source) = source_ctx else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Context not found: {}", id))),
        ));
    };

    // Create copy based on target scope
    let library = context::load_user_context_library();

    if req.target_scope == "project" {
        // Create copy in project config
        let ctx = {
            let mut config_lock = state.app_state.current_config.lock().map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to lock config: {}", e))),
                )
            })?;

            let config = config_lock.as_mut().ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(api_error(
                        "No project loaded. Please load a project configuration first.",
                    )),
                )
            })?;

            let ctx = context::create_project_context(
                format!("{} (Copy)", source.name),
                source.content.clone(),
                source.category.clone(),
                source.tags.clone(),
                source.auto_include.clone(),
            );

            context::add_project_context_to_config(&mut config.contexts, ctx.clone()).map_err(
                |e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!("Failed to add context to config: {}", e))),
                    )
                },
            )?;

            ctx
        }; // config_lock dropped here

        // Save the config to the file
        if let Err(e) = crate::mcp::shared::save_current_config_to_file(&state.app_state) {
            warn!(
                "Failed to save config after duplicating to project context: {}",
                e
            );
        }

        Ok(Json(ApiResponse::success(context_to_with_metadata(
            ctx,
            context::ContextScope::Project,
            &library,
        ))))
    } else {
        // Create copy in user library
        match context::create_user_context(
            format!("{} (Copy)", source.name),
            source.content.clone(),
            source.category.clone(),
            source.tags.clone(),
            source.auto_include.clone(),
        ) {
            Ok(ctx) => Ok(Json(ApiResponse::success(context_to_with_metadata(
                ctx,
                context::ContextScope::User,
                &library,
            )))),
            Err(e) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to duplicate context: {}", e))),
            )),
        }
    }
}

/// POST /contexts/metadata/{id}/enable - Enable a context
pub async fn enable_context_handler(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match context::set_context_enabled(&id, true) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to enable context: {}", e))),
        )),
    }
}

/// POST /contexts/metadata/{id}/disable - Disable a context
pub async fn disable_context_handler(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match context::set_context_enabled(&id, false) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to disable context: {}", e))),
        )),
    }
}

/// POST /contexts/:id/approve-sync - Approve syncing a project context to qontinui-web
pub async fn approve_context_sync(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Approving context sync for: {}", id);

    // Get the context from the loaded config
    let ctx = {
        let config_lock = state.app_state.current_config.lock().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to lock config: {}", e))),
            )
        })?;

        let config = config_lock.as_ref().ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "No project loaded. Please load a project configuration first.",
                )),
            )
        })?;

        context::get_project_context_from_config(&config.contexts, &id).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Context not found: {}", id))),
            )
        })?
    };

    // Get the project ID from the loaded config
    let project_id = {
        let config_lock = state.app_state.current_config.lock().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to lock config: {}", e))),
            )
        })?;

        let config = config_lock.as_ref().ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(api_error("No project loaded")),
            )
        })?;

        config.metadata.project_id.clone().ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "No project ID found in configuration. Cannot sync to qontinui-web.",
                )),
            )
        })?
    };

    // Sync to qontinui-web
    match sync_context_to_web(&project_id, &ctx).await {
        Ok(_) => {
            // Mark as synced
            if let Err(e) = context::set_web_sync_status(&id, Some(context::WebSyncStatus::Synced))
            {
                warn!("Failed to update sync status: {}", e);
            }

            info!("Successfully synced context {} to qontinui-web", id);
            Ok(Json(ApiResponse::success(serde_json::json!({
                "synced": true,
                "contextId": id,
                "projectId": project_id
            }))))
        }
        Err(e) => {
            error!("Failed to sync context to qontinui-web: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to sync to qontinui-web: {}", e))),
            ))
        }
    }
}

/// POST /contexts/:id/dismiss-sync - Dismiss syncing a project context
pub async fn dismiss_context_sync(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Dismissing context sync for: {}", id);

    match context::set_web_sync_status(&id, Some(context::WebSyncStatus::Dismissed)) {
        Ok(()) => {
            info!("Dismissed sync for context: {}", id);
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to dismiss sync: {}", e))),
        )),
    }
}

/// Sync a context to qontinui-web by updating the project configuration
async fn sync_context_to_web(project_id: &str, ctx: &context::Context) -> Result<(), String> {
    use crate::auth::AuthManager;

    let auth_manager = AuthManager::new();

    // Check if authenticated
    if !auth_manager.has_tokens() {
        return Err("Not authenticated. Please log in to qontinui-web first.".to_string());
    }

    let access_token = auth_manager
        .get_access_token()
        .map_err(|e| format!("Failed to get access token: {}", e))?;

    // Get the API base URL
    let api_url = std::env::var("QONTINUI_API_URL").unwrap_or_else(|_| {
        if cfg!(debug_assertions) {
            "http://localhost:8000".to_string()
        } else {
            "https://qontinui-prod-py.eba-km2u4s23.eu-central-1.elasticbeanstalk.com".to_string()
        }
    });

    let client = reqwest::Client::new();

    // First, get the current project configuration
    let get_response = client
        .get(format!("{}/api/v1/projects/{}", api_url, project_id))
        .bearer_auth(&access_token)
        .send()
        .await
        .map_err(|e| format!("Network error fetching project: {}", e))?;

    if !get_response.status().is_success() {
        let status = get_response.status();
        let error_text = get_response.text().await.unwrap_or_default();
        return Err(format!(
            "Failed to fetch project ({}): {}",
            status, error_text
        ));
    }

    let project: serde_json::Value = get_response
        .json()
        .await
        .map_err(|e| format!("Failed to parse project response: {}", e))?;

    // Get the current configuration and contexts
    let mut configuration = project
        .get("configuration")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let mut contexts: Vec<serde_json::Value> = configuration
        .get("contexts")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();

    // Check if context already exists (by ID)
    let existing_index = contexts
        .iter()
        .position(|c| c.get("id").and_then(|id| id.as_str()) == Some(&ctx.id));

    // Convert our context to JSON
    let ctx_json =
        serde_json::to_value(ctx).map_err(|e| format!("Failed to serialize context: {}", e))?;

    if let Some(idx) = existing_index {
        // Update existing context
        contexts[idx] = ctx_json;
        info!(
            "Updated existing context {} in qontinui-web project",
            ctx.id
        );
    } else {
        // Add new context
        contexts.push(ctx_json);
        info!("Added new context {} to qontinui-web project", ctx.id);
    }

    // Update the configuration
    configuration["contexts"] = serde_json::Value::Array(contexts);

    // PUT the updated project
    let update_body = serde_json::json!({
        "configuration": configuration
    });

    let put_response = client
        .put(format!("{}/api/v1/projects/{}", api_url, project_id))
        .bearer_auth(&access_token)
        .json(&update_body)
        .send()
        .await
        .map_err(|e| format!("Network error updating project: {}", e))?;

    if !put_response.status().is_success() {
        let status = put_response.status();
        let error_text = put_response.text().await.unwrap_or_default();
        return Err(format!(
            "Failed to update project ({}): {}",
            status, error_text
        ));
    }

    Ok(())
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post, put};
    axum::Router::new()
        .route("/contexts", get(list_all_contexts))
        .route("/contexts/categories", get(list_context_categories))
        .route("/contexts/tags", get(list_context_tags))
        .route("/contexts/:scope", post(create_context_handler))
        .route(
            "/contexts/:scope/:id",
            put(update_context_handler).delete(delete_context_handler),
        )
        .route(
            "/contexts/:scope/:id/duplicate",
            post(duplicate_context_handler),
        )
        .route(
            "/contexts/metadata/:id/enable",
            post(enable_context_handler),
        )
        .route(
            "/contexts/metadata/:id/disable",
            post(disable_context_handler),
        )
        .route("/contexts/:id/approve-sync", post(approve_context_sync))
        .route("/contexts/:id/dismiss-sync", post(dismiss_context_sync))
}
