//! Prompt library handlers for MCP API
//!
//! Provides handlers for managing prompts, AI workflows, and scriptlets.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use std::sync::Arc;

use super::types::{api_error, ApiResponse, ApiState};
use crate::ai_workflows::{self, AiWorkflow};
use crate::prompts::{self, SavedPrompt};
use crate::scriptlets::{self, Scriptlet};

// ============================================================================
// Prompt Library Handlers
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub category: Option<String>,
    pub tag: Option<String>,
}

pub async fn list_prompts(
) -> Result<Json<ApiResponse<Vec<SavedPrompt>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let list = prompts::get_all_prompts();
    Ok(Json(ApiResponse::success(list)))
}

pub async fn get_prompt(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::get_prompt(&id) {
        Some(prompt) => Ok(Json(ApiResponse::success(prompt))),
        None => Err((StatusCode::NOT_FOUND, Json(api_error("Prompt not found")))),
    }
}

#[derive(Debug, Deserialize)]
pub struct CreatePromptRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub content: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub max_sessions: Option<u32>,
}

pub async fn create_prompt(
    Json(request): Json<CreatePromptRequest>,
) -> Result<Json<ApiResponse<SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::create_prompt(
        request.name,
        request.description,
        request.content,
        request.category,
        request.tags,
        request.max_sessions,
    ) {
        Ok(prompt) => Ok(Json(ApiResponse::success(prompt))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdatePromptRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub content: Option<String>,
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    pub max_sessions: Option<Option<u32>>,
}

pub async fn update_prompt(
    Path(id): Path<String>,
    Json(request): Json<UpdatePromptRequest>,
) -> Result<Json<ApiResponse<SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::update_prompt(
        &id,
        request.name,
        request.description,
        request.content,
        request.category,
        request.tags,
        request.max_sessions,
    ) {
        Ok(prompt) => Ok(Json(ApiResponse::success(prompt))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

pub async fn delete_prompt(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::delete_prompt(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

pub async fn duplicate_prompt(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::duplicate_prompt(&id, None) {
        Ok(prompt) => Ok(Json(ApiResponse::success(prompt))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

pub async fn search_prompts(
    Query(query): Query<SearchQuery>,
) -> Result<Json<ApiResponse<Vec<SavedPrompt>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let prompts_list = if let Some(q) = query.q {
        prompts::search_prompts(&q)
    } else {
        prompts::get_all_prompts()
    };

    // Filter by category if provided
    let prompts_list = if let Some(category) = query.category {
        prompts_list
            .into_iter()
            .filter(|p| p.category == category)
            .collect()
    } else {
        prompts_list
    };

    // Filter by tag if provided
    let prompts_list = if let Some(tag) = query.tag {
        prompts_list
            .into_iter()
            .filter(|p| p.tags.contains(&tag))
            .collect()
    } else {
        prompts_list
    };

    Ok(Json(ApiResponse::success(prompts_list)))
}

pub async fn get_prompt_categories(
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = prompts::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

pub async fn get_prompt_tags(
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tags = prompts::get_all_tags();
    Ok(Json(ApiResponse::success(tags)))
}

#[derive(Debug, Deserialize)]
pub struct ImportPromptsRequest {
    pub prompts_json: String,
}

pub async fn import_prompts(
    Json(request): Json<ImportPromptsRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::import_prompts(&request.prompts_json) {
        Ok(imported) => Ok(Json(ApiResponse::success(serde_json::json!({
            "imported": imported.len()
        })))),
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(api_error(e)))),
    }
}

pub async fn export_prompts(
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::export_prompts() {
        Ok(json_str) => {
            // Parse the JSON string to return as structured data
            match serde_json::from_str::<serde_json::Value>(&json_str) {
                Ok(value) => Ok(Json(ApiResponse::success(value))),
                Err(e) => Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to parse export: {}", e))),
                )),
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

#[derive(Debug, Deserialize)]
pub struct RunPromptRequest {
    pub id: String,
    pub variables: Option<std::collections::HashMap<String, String>>,
}

pub async fn run_prompt(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<RunPromptRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Get the prompt
    let prompt = match prompts::get_prompt(&request.id) {
        Some(p) => p,
        None => return Err((StatusCode::NOT_FOUND, Json(api_error("Prompt not found")))),
    };

    // Apply variable substitution if provided
    let mut final_content = prompt.content.clone();
    if let Some(vars) = request.variables {
        for (key, value) in vars {
            final_content = final_content.replace(&format!("{{{{{}}}}}", key), &value);
        }
    }

    Ok(Json(ApiResponse::success(serde_json::json!({
        "prompt_id": request.id,
        "content": final_content,
        "started": true
    }))))
}

// ============================================================================
// AI Workflow Library Handlers
// ============================================================================

pub async fn list_ai_workflows(
) -> Result<Json<ApiResponse<Vec<AiWorkflow>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let list = ai_workflows::list_workflows(None);
    Ok(Json(ApiResponse::success(list)))
}

pub async fn get_ai_workflow(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<AiWorkflow>>, (StatusCode, Json<ApiResponse<()>>)> {
    match ai_workflows::get_workflow(&id) {
        Some(workflow) => Ok(Json(ApiResponse::success(workflow))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error("Workflow not found")),
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkflowRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub prompt: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub max_sessions: Option<u32>,
    #[serde(default)]
    pub execution_steps: Vec<ai_workflows::ExecutionStep>,
}

pub async fn create_ai_workflow(
    Json(request): Json<CreateWorkflowRequest>,
) -> Result<Json<ApiResponse<AiWorkflow>>, (StatusCode, Json<ApiResponse<()>>)> {
    match ai_workflows::create_workflow(
        request.name,
        request.description,
        request.prompt,
        request.category,
        request.tags,
        request.max_sessions,
        request.execution_steps,
    ) {
        Ok(workflow) => Ok(Json(ApiResponse::success(workflow))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkflowRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub prompt: Option<String>,
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    pub max_sessions: Option<Option<u32>>,
    pub execution_steps: Option<Vec<ai_workflows::ExecutionStep>>,
}

pub async fn update_ai_workflow(
    Path(id): Path<String>,
    Json(request): Json<UpdateWorkflowRequest>,
) -> Result<Json<ApiResponse<AiWorkflow>>, (StatusCode, Json<ApiResponse<()>>)> {
    match ai_workflows::update_workflow(
        &id,
        request.name,
        request.description,
        request.prompt,
        request.category,
        request.tags,
        request.max_sessions,
        request.execution_steps,
    ) {
        Ok(workflow) => Ok(Json(ApiResponse::success(workflow))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

pub async fn delete_ai_workflow(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match ai_workflows::delete_workflow(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

pub async fn search_ai_workflows(
    Query(query): Query<SearchQuery>,
) -> Result<Json<ApiResponse<Vec<AiWorkflow>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let workflows = if let Some(q) = query.q {
        ai_workflows::search_workflows(&q)
    } else {
        ai_workflows::list_workflows(None)
    };

    // Filter by category if provided
    let workflows = if let Some(category) = query.category {
        workflows
            .into_iter()
            .filter(|w| w.category == category)
            .collect()
    } else {
        workflows
    };

    // Filter by tag if provided
    let workflows = if let Some(tag) = query.tag {
        workflows
            .into_iter()
            .filter(|w| w.tags.contains(&tag))
            .collect()
    } else {
        workflows
    };

    Ok(Json(ApiResponse::success(workflows)))
}

pub async fn get_ai_workflow_categories(
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = ai_workflows::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

pub async fn get_ai_workflow_tags(
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tags = ai_workflows::get_tags();
    Ok(Json(ApiResponse::success(tags)))
}

// ============================================================================
// Scriptlet Handlers
// ============================================================================

pub async fn list_scriptlets(
) -> Result<Json<ApiResponse<Vec<Scriptlet>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let list = scriptlets::get_all_scriptlets();
    Ok(Json(ApiResponse::success(list)))
}

pub async fn get_scriptlet(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Scriptlet>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::get_scriptlet(&id) {
        Some(scriptlet) => Ok(Json(ApiResponse::success(scriptlet))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error("Scriptlet not found")),
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateScriptletRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub content: String,
    #[serde(default)]
    pub category: String,
}

pub async fn create_scriptlet(
    Json(request): Json<CreateScriptletRequest>,
) -> Result<Json<ApiResponse<Scriptlet>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::create_scriptlet(
        request.name,
        request.description,
        request.content,
        request.category,
    ) {
        Ok(scriptlet) => Ok(Json(ApiResponse::success(scriptlet))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateScriptletRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub content: Option<String>,
    pub category: Option<String>,
}

pub async fn update_scriptlet(
    Path(id): Path<String>,
    Json(request): Json<UpdateScriptletRequest>,
) -> Result<Json<ApiResponse<Scriptlet>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::update_scriptlet(
        &id,
        request.name,
        request.description,
        request.content,
        request.category,
    ) {
        Ok(scriptlet) => Ok(Json(ApiResponse::success(scriptlet))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

pub async fn delete_scriptlet(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::delete_scriptlet(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

pub async fn search_scriptlets(
    Query(query): Query<SearchQuery>,
) -> Result<Json<ApiResponse<Vec<Scriptlet>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let scriptlets_list = if let Some(q) = query.q {
        scriptlets::search_scriptlets(&q)
    } else {
        scriptlets::get_all_scriptlets()
    };

    // Filter by category if provided
    let scriptlets_list = if let Some(category) = query.category {
        scriptlets_list
            .into_iter()
            .filter(|s| s.category == category)
            .collect()
    } else {
        scriptlets_list
    };

    Ok(Json(ApiResponse::success(scriptlets_list)))
}

pub async fn get_scriptlet_categories(
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = scriptlets::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}
