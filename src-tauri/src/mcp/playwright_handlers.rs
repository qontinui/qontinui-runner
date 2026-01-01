//! Playwright script library handlers for MCP API
//!
//! Provides handlers for managing Playwright test scripts.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::info;

use super::playwright::execute_playwright_internal;
use super::types::{api_error, ApiResponse, ApiState};
use crate::playwright::{
    create_script, delete_script, duplicate_script, get_all_scripts, get_all_tags, get_categories,
    get_script, search_scripts, update_script, DisplayMode, PlaywrightScript,
};

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub category: Option<String>,
    pub tag: Option<String>,
}

pub async fn list_playwright_scripts(
) -> Result<Json<ApiResponse<Vec<PlaywrightScript>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let scripts = get_all_scripts();
    Ok(Json(ApiResponse::success(scripts)))
}

pub async fn get_playwright_script(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match get_script(&id) {
        Some(script) => Ok(Json(ApiResponse::success(script))),
        None => Err((StatusCode::NOT_FOUND, Json(api_error("Script not found")))),
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateScriptRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub ai_instructions: Option<String>,
    #[serde(default)]
    pub target_url: String,
    pub script_content: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u32,
    #[serde(default)]
    pub display_mode: DisplayMode,
    #[serde(default = "default_browser")]
    pub browser: String,
}

fn default_timeout() -> u32 {
    60
}

fn default_browser() -> String {
    "chromium".to_string()
}

pub async fn create_playwright_script(
    Json(request): Json<CreateScriptRequest>,
) -> Result<Json<ApiResponse<PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match create_script(
        request.name,
        request.description,
        request.ai_instructions,
        request.target_url,
        request.script_content,
        request.category,
        request.tags,
        request.timeout_seconds,
        request.display_mode,
        request.browser,
    ) {
        Ok(script) => Ok(Json(ApiResponse::success(script))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateScriptRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub ai_instructions: Option<String>,
    pub target_url: Option<String>,
    pub script_content: Option<String>,
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    pub timeout_seconds: Option<u32>,
    pub display_mode: Option<DisplayMode>,
    pub browser: Option<String>,
}

pub async fn update_playwright_script(
    Path(id): Path<String>,
    Json(request): Json<UpdateScriptRequest>,
) -> Result<Json<ApiResponse<PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match update_script(
        &id,
        request.name,
        request.description,
        request.ai_instructions,
        request.target_url,
        request.script_content,
        request.category,
        request.tags,
        request.timeout_seconds,
        request.display_mode,
        request.browser,
    ) {
        Ok(script) => Ok(Json(ApiResponse::success(script))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

pub async fn delete_playwright_script(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match delete_script(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

pub async fn duplicate_playwright_script(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match duplicate_script(&id, None) {
        Ok(script) => Ok(Json(ApiResponse::success(script))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

pub async fn search_playwright_scripts(
    Query(query): Query<SearchQuery>,
) -> Result<Json<ApiResponse<Vec<PlaywrightScript>>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Use the query string if provided, otherwise list all
    let scripts = if let Some(q) = query.q {
        search_scripts(&q)
    } else {
        get_all_scripts()
    };

    // Filter by category if provided
    let scripts = if let Some(category) = query.category {
        scripts
            .into_iter()
            .filter(|s| s.category == category)
            .collect()
    } else {
        scripts
    };

    // Filter by tag if provided
    let scripts = if let Some(tag) = query.tag {
        scripts
            .into_iter()
            .filter(|s| s.tags.contains(&tag))
            .collect()
    } else {
        scripts
    };

    Ok(Json(ApiResponse::success(scripts)))
}

pub async fn get_playwright_categories(
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

pub async fn get_playwright_tags(
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tags = get_all_tags();
    Ok(Json(ApiResponse::success(tags)))
}

#[derive(Debug, Deserialize)]
pub struct ImportScriptsRequest {
    pub scripts_json: String,
}

pub async fn import_playwright_scripts(
    Json(request): Json<ImportScriptsRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    match crate::playwright::import_scripts(&request.scripts_json) {
        Ok(scripts) => Ok(Json(ApiResponse::success(serde_json::json!({
            "imported": scripts.len()
        })))),
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(api_error(e)))),
    }
}

pub async fn export_playwright_scripts(
) -> Result<Json<ApiResponse<Vec<PlaywrightScript>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let scripts = get_all_scripts();
    Ok(Json(ApiResponse::success(scripts)))
}

pub async fn run_playwright_script(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Running Playwright script: {}", id);

    let (success, error) = execute_playwright_internal(state, &id).await;

    if success {
        Ok(Json(ApiResponse::success(serde_json::json!({
            "script_id": id,
            "success": true
        }))))
    } else {
        Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(error.unwrap_or_else(|| "Unknown error".to_string()))),
        ))
    }
}
