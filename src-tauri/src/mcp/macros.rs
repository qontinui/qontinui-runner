//! Macro CRUD handlers for MCP API
//!
//! Provides HTTP handlers for managing automation macros:
//! list, get, create, update, delete, search, categories, tags, run.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use std::sync::Arc;

use crate::macros;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Request Types
// ============================================================================

/// Request to create a new macro
#[derive(Debug, Deserialize)]
pub struct CreateMacroRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub steps: Vec<macros::MacroStep>,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Request to update an existing macro
#[derive(Debug, Deserialize)]
pub struct UpdateMacroRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub steps: Option<Vec<macros::MacroStep>>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

// ============================================================================
// Handlers
// ============================================================================

/// List all macros
pub async fn list_macros(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<macros::Macro>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let category = params.get("category").map(|s| s.as_str());
    let macro_list = macros::list_macros(category);
    Ok(Json(ApiResponse::success(macro_list)))
}

/// Get a single macro by ID
pub async fn get_macro(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<macros::Macro>>, (StatusCode, Json<ApiResponse<()>>)> {
    match macros::get_macro(&id) {
        Some(macro_item) => Ok(Json(ApiResponse::success(macro_item))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Macro not found: {}", id))),
        )),
    }
}

/// Create a new macro
pub async fn create_macro(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreateMacroRequest>,
) -> Result<Json<ApiResponse<macros::Macro>>, (StatusCode, Json<ApiResponse<()>>)> {
    match macros::create_macro(
        request.name,
        request.description,
        request.steps,
        request.category,
        request.tags,
    ) {
        Ok(macro_item) => Ok(Json(ApiResponse::success(macro_item))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Update an existing macro
pub async fn update_macro(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdateMacroRequest>,
) -> Result<Json<ApiResponse<macros::Macro>>, (StatusCode, Json<ApiResponse<()>>)> {
    match macros::update_macro(
        &id,
        request.name,
        request.description,
        request.steps,
        request.category,
        request.tags,
    ) {
        Ok(macro_item) => Ok(Json(ApiResponse::success(macro_item))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Delete a macro
pub async fn delete_macro(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match macros::delete_macro(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Search macros by query
pub async fn search_macros(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<macros::Macro>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let results = macros::search_macros(query);
    Ok(Json(ApiResponse::success(results)))
}

/// Get all macro categories
pub async fn get_macro_categories(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = macros::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

/// Get all macro tags
pub async fn get_macro_tags(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tags = macros::get_tags();
    Ok(Json(ApiResponse::success(tags)))
}

/// Run a macro by ID
pub async fn run_macro(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Get the macro
    let macro_item = match macros::get_macro(&id) {
        Some(m) => m,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Macro not found: {}", id))),
            ));
        }
    };

    // Increment run count
    if let Err(e) = macros::increment_run_count(&id) {
        tracing::warn!("Failed to increment run count for macro {}: {}", id, e);
    }

    let start_time = std::time::Instant::now();
    let mut step_results: Vec<serde_json::Value> = Vec::new();
    let mut successful_steps = 0;
    let mut failed_steps = 0;

    // Execute each step using the action service
    let action_service = state.action_service.clone();

    for (idx, step) in macro_item.steps.iter().enumerate() {
        let step_start = std::time::Instant::now();
        let mut step_success = false;
        let mut step_error: Option<String> = None;

        match step.action_type.as_str() {
            "click" | "double_click" | "right_click" => {
                if let Some(ref image_ids) = step.target_image_ids {
                    if let Some(first_image_id) = image_ids.first() {
                        match action_service
                            .execute_action(
                                &step.action_type,
                                first_image_id,
                                None,
                                step.monitor_index,
                            )
                            .await
                        {
                            Ok(_result) => {
                                step_success = true;
                            }
                            Err(e) => {
                                step_error = Some(format!("{:?}", e));
                            }
                        }
                    } else {
                        step_error = Some("No target image specified".to_string());
                    }
                } else {
                    step_error = Some("No target image IDs specified".to_string());
                }
            }
            "type" => {
                if let Some(ref text) = step.text_input {
                    let config = serde_json::json!({
                        "text": text
                    });
                    match action_service
                        .execute_action("TYPE", "", Some(&config), step.monitor_index)
                        .await
                    {
                        Ok(_result) => {
                            step_success = true;
                        }
                        Err(e) => {
                            step_error = Some(format!("{:?}", e));
                        }
                    }
                } else {
                    step_error = Some("No text specified for type action".to_string());
                }
            }
            "hotkey" => {
                if let Some(ref hotkey) = step.hotkey {
                    let config = serde_json::json!({
                        "hotkey": hotkey
                    });
                    match action_service
                        .execute_action("HOTKEY", "", Some(&config), step.monitor_index)
                        .await
                    {
                        Ok(_result) => {
                            step_success = true;
                        }
                        Err(e) => {
                            step_error = Some(format!("{:?}", e));
                        }
                    }
                } else {
                    step_error = Some("No hotkey specified".to_string());
                }
            }
            "go_to_state" => {
                if let Some(ref state_ids) = step.target_state_ids {
                    if let Some(first_state_id) = state_ids.first() {
                        let timeout = step.timeout_seconds;
                        match action_service
                            .go_to_state(first_state_id, None, step.monitor_index, timeout)
                            .await
                        {
                            Ok(_result) => {
                                step_success = true;
                            }
                            Err(e) => {
                                step_error = Some(format!("{:?}", e));
                            }
                        }
                    } else {
                        step_error = Some("No target state specified".to_string());
                    }
                } else {
                    step_error = Some("No target state IDs specified".to_string());
                }
            }
            _ => {
                step_error = Some(format!("Unknown action type: {}", step.action_type));
            }
        }

        let step_duration = step_start.elapsed().as_millis() as u64;

        // Apply pause_after_ms if specified
        if let Some(pause_ms) = step.pause_after_ms {
            tokio::time::sleep(tokio::time::Duration::from_millis(pause_ms as u64)).await;
        }

        if step_success {
            successful_steps += 1;
        } else {
            failed_steps += 1;
        }

        step_results.push(serde_json::json!({
            "step_index": idx,
            "step_name": step.name,
            "action_type": step.action_type,
            "success": step_success,
            "error": step_error,
            "duration_ms": step_duration,
        }));
    }

    let total_duration = start_time.elapsed().as_millis() as u64;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "macro_id": id,
        "macro_name": macro_item.name,
        "total_steps": macro_item.steps.len(),
        "successful_steps": successful_steps,
        "failed_steps": failed_steps,
        "duration_ms": total_duration,
        "step_results": step_results,
    }))))
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/macros", get(list_macros))
        .route("/macros", post(create_macro))
        .route("/macros/search", get(search_macros))
        .route("/macros/categories", get(get_macro_categories))
        .route("/macros/tags", get(get_macro_tags))
        .route(
            "/macros/{id}",
            get(get_macro).put(update_macro).delete(delete_macro),
        )
        .route("/macros/{id}/run", post(run_macro))
}
