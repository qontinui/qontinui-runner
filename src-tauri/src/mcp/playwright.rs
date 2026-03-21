//! Playwright test library CRUD handlers for MCP API
//!
//! Provides HTTP handlers for managing Playwright tests:
//! list, get, create, update, delete, run, search, import/export, duplicate.
//!
//! Note: The Playwright *collection* handlers (start/stop/status/results for
//! bulk collection) remain in mcp_api.rs as they have different concerns.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::playwright::{self, DisplayMode};

// ============================================================================
// Request Types
// ============================================================================

/// Request to create a new Playwright script
#[derive(Debug, Deserialize)]
pub struct CreatePlaywrightScriptRequest {
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
    #[serde(default = "default_playwright_timeout")]
    pub timeout_seconds: u32,
    #[serde(default)]
    pub display_mode: DisplayMode,
    #[serde(default = "default_playwright_browser")]
    pub browser: String,
}

fn default_playwright_timeout() -> u32 {
    60
}

fn default_playwright_browser() -> String {
    "chromium".to_string()
}

/// Request to update an existing Playwright script
#[derive(Debug, Deserialize)]
pub struct UpdatePlaywrightScriptRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub ai_instructions: Option<String>,
    #[serde(default)]
    pub target_url: Option<String>,
    #[serde(default)]
    pub script_content: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_seconds: Option<u32>,
    #[serde(default)]
    pub display_mode: Option<DisplayMode>,
    #[serde(default)]
    pub browser: Option<String>,
}

/// Request to run a Playwright script
#[derive(Debug, Deserialize)]
pub struct RunPlaywrightScriptRequest {
    /// Optional URL override for this run
    #[serde(default)]
    pub target_url_override: Option<String>,
}

/// Request to import Playwright scripts
#[derive(Debug, Deserialize)]
pub struct ImportPlaywrightScriptsRequest {
    /// JSON array of scripts to import
    pub scripts_json: String,
}

/// Request to duplicate a Playwright script
#[derive(Debug, Deserialize)]
pub struct DuplicatePlaywrightScriptRequest {
    /// Optional new name (defaults to "Original Name (Copy)")
    #[serde(default)]
    pub new_name: Option<String>,
}

// ============================================================================
// Handlers
// ============================================================================

/// List all Playwright tests
pub async fn list_playwright_tests(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<playwright::PlaywrightScript>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let scripts = playwright::get_all_scripts();
    Ok(Json(ApiResponse::success(scripts)))
}

/// Get a single Playwright test by ID
pub async fn get_playwright_test(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<playwright::PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::get_script(&id) {
        Some(script) => Ok(Json(ApiResponse::success(script))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Playwright test not found: {}", id))),
        )),
    }
}

/// Create a new Playwright test
pub async fn create_playwright_test(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreatePlaywrightScriptRequest>,
) -> Result<Json<ApiResponse<playwright::PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::create_script(
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

/// Update an existing Playwright test
pub async fn update_playwright_test(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdatePlaywrightScriptRequest>,
) -> Result<Json<ApiResponse<playwright::PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::update_script(
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
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Delete a Playwright test
pub async fn delete_playwright_test(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::delete_script(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Run a Playwright test
pub async fn run_playwright_test(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<RunPlaywrightScriptRequest>,
) -> Result<Json<ApiResponse<playwright::PlaywrightResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    let target_url_override = request.target_url_override;

    // Run in spawn_blocking since it's a blocking operation
    let result =
        tokio::task::spawn_blocking(move || playwright::run_script(&id, target_url_override))
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Task error: {}", e))),
                )
            })?;

    match result {
        Ok(play_result) => Ok(Json(ApiResponse::success(play_result))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Get Playwright test categories
pub async fn get_playwright_categories(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = playwright::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

/// Get Playwright test tags
pub async fn get_playwright_tags(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tags = playwright::get_all_tags();
    Ok(Json(ApiResponse::success(tags)))
}

/// Search Playwright tests
pub async fn search_playwright_tests(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<playwright::PlaywrightScript>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let results = playwright::search_scripts(query);
    Ok(Json(ApiResponse::success(results)))
}

/// Import Playwright tests
pub async fn import_playwright_tests(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<ImportPlaywrightScriptsRequest>,
) -> Result<Json<ApiResponse<Vec<playwright::PlaywrightScript>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    match playwright::import_scripts(&request.scripts_json) {
        Ok(scripts) => Ok(Json(ApiResponse::success(scripts))),
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(api_error(e)))),
    }
}

/// Export all Playwright tests
pub async fn export_playwright_tests(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::export_scripts() {
        Ok(json) => Ok(Json(ApiResponse::success(json))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Duplicate a Playwright test
pub async fn duplicate_playwright_test(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<DuplicatePlaywrightScriptRequest>,
) -> Result<Json<ApiResponse<playwright::PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::duplicate_script(&id, request.new_name) {
        Ok(script) => Ok(Json(ApiResponse::success(script))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{delete, get, post, put};
    axum::Router::new()
        .route("/playwright/tests", get(list_playwright_tests))
        .route("/playwright/tests", post(create_playwright_test))
        .route("/playwright/tests/search", get(search_playwright_tests))
        .route(
            "/playwright/tests/categories",
            get(get_playwright_categories),
        )
        .route("/playwright/tests/tags", get(get_playwright_tags))
        .route("/playwright/tests/import", post(import_playwright_tests))
        .route("/playwright/tests/export", get(export_playwright_tests))
        .route("/playwright/tests/{id}", get(get_playwright_test))
        .route("/playwright/tests/{id}", put(update_playwright_test))
        .route("/playwright/tests/{id}", delete(delete_playwright_test))
        .route("/playwright/tests/{id}/run", post(run_playwright_test))
        .route(
            "/playwright/tests/{id}/duplicate",
            post(duplicate_playwright_test),
        )
}
