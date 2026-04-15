//! API request import and test handlers for MCP API
//!
//! Provides HTTP handlers for importing cURL commands,
//! testing API requests, and saving to the library.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// API Request Import/Test Handlers
// ============================================================================

/// Request body for importing a cURL command
#[derive(Debug, Deserialize)]
pub struct ImportCurlRequest {
    curl_command: String,
}

/// Request body for testing an API request
#[derive(Debug, Deserialize)]
pub struct TestApiRequestBody {
    method: String,
    url: String,
    headers: Option<std::collections::HashMap<String, String>>,
    body: Option<String>,
    content_type: Option<String>,
    timeout_ms: Option<u64>,
    follow_redirects: Option<bool>,
    variables: Option<std::collections::HashMap<String, String>>,
}

/// Import a cURL command and return the parsed configuration
pub async fn import_curl_command(
    Json(request): Json<ImportCurlRequest>,
) -> Result<Json<ApiResponse<crate::api_request::ParsedCurl>>, (StatusCode, Json<ApiResponse<()>>)>
{
    info!(
        "Importing cURL command: {} bytes",
        request.curl_command.len()
    );

    match crate::api_request::parse_curl(&request.curl_command) {
        Ok(parsed) => {
            info!("Parsed cURL: {} {}", parsed.method, parsed.url);
            Ok(Json(ApiResponse::success(parsed)))
        }
        Err(e) => {
            error!("Failed to parse cURL command: {}", e);
            Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!("Failed to parse cURL command: {}", e))),
            ))
        }
    }
}

/// Test an API request immediately (for debugging/testing in the editor)
pub async fn test_api_request(
    Json(request): Json<TestApiRequestBody>,
) -> Result<
    Json<ApiResponse<crate::api_request::ApiRequestResult>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Testing API request: {} {}", request.method, request.url);

    // Parse method
    let method = match request.method.to_uppercase().as_str() {
        "GET" => crate::api_request::HttpMethod::Get,
        "POST" => crate::api_request::HttpMethod::Post,
        "PUT" => crate::api_request::HttpMethod::Put,
        "PATCH" => crate::api_request::HttpMethod::Patch,
        "DELETE" => crate::api_request::HttpMethod::Delete,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "Invalid HTTP method: {}",
                    request.method
                ))),
            ))
        }
    };

    // Build config
    let config = crate::api_request::ApiRequestConfig {
        step_id: None,
        step_name: None,
        method,
        url: request.url.clone(),
        resolved_url: None,
        headers: request.headers,
        body: request.body,
        content_type: request.content_type,
        timeout_ms: request.timeout_ms.or(Some(30000)),
        follow_redirects: request.follow_redirects.or(Some(true)),
        credential_id: None,
        extractions: None,
        assertions: None,
    };

    // Create executor with provided variables
    let executor = crate::api_request::ApiRequestExecutor::new();
    if let Some(vars) = request.variables {
        for (key, value) in vars {
            executor.resolver().set(&key, &value);
        }
    }

    // Execute the request (no credentials for test endpoint)
    match executor.execute(&config, None).await {
        Ok(result) => {
            info!(
                "API request completed: {} {} - {} in {}ms",
                request.method, request.url, result.status_code, result.response_time_ms
            );
            Ok(Json(ApiResponse::success(result)))
        }
        Err(e) => {
            error!("API request failed: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("API request failed: {}", e))),
            ))
        }
    }
}

// ============================================================================
// Import cURL to API Request Library
// ============================================================================

/// Request body for importing cURL to library
#[derive(Debug, Deserialize)]
pub struct ImportCurlToLibraryRequest {
    curl_command: String,
    /// Custom name for the saved request (optional, defaults to URL-based name)
    name: Option<String>,
    /// Category for organization (optional, defaults to "imported")
    category: Option<String>,
}

/// Import a cURL command and save it to the API Request Library
pub async fn import_curl_to_library(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ImportCurlToLibraryRequest>,
) -> Result<
    Json<ApiResponse<crate::saved_api_requests::SavedApiRequest>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!(
        "Importing cURL to library: {} bytes",
        request.curl_command.len()
    );

    // Parse the cURL command
    let parsed = match crate::api_request::parse_curl(&request.curl_command) {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to parse cURL command: {}", e);
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!("Failed to parse cURL command: {}", e))),
            ));
        }
    };

    // Generate a name from the URL if not provided
    let name = request.name.unwrap_or_else(|| {
        // Extract path from URL for a meaningful name
        if let Ok(url) = tauri::Url::parse(&parsed.url) {
            let path = url.path();
            if path.len() > 1 {
                // Remove leading slash and take first segment
                let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
                if let Some(first) = segments.first() {
                    return format!("{} {}", parsed.method, first);
                }
            }
        }
        format!("{} Request", parsed.method)
    });

    let create_req = crate::saved_api_requests::CreateSavedApiRequestRequest {
        name,
        description: String::new(),
        category: request.category.unwrap_or_else(|| "imported".to_string()),
        tags: vec!["imported".to_string(), "curl".to_string()],
        method: parsed.method,
        url: parsed.url,
        headers: parsed.headers,
        body: parsed.body,
        body_content_type: parsed.content_type,
        timeout_ms: 30000,
        follow_redirects: true,
        variable_extractions: Vec::new(),
        assertions: Vec::new(),
        credential_id: None,
    };

    match state
        .app_state
        .pg_db
        .create_saved_api_request(&create_req)
        .await
    {
        Ok(value) => {
            match serde_json::from_value::<crate::saved_api_requests::SavedApiRequest>(value) {
                Ok(parsed_req) => Ok(Json(ApiResponse::success(parsed_req))),
                Err(e) => {
                    error!("Failed to parse created saved API request: {}", e);
                    Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!(
                            "Failed to parse created saved API request: {}",
                            e
                        ))),
                    ))
                }
            }
        }
        Err(e) => {
            error!("Failed to save imported cURL to library: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to save imported cURL to library: {}",
                    e
                ))),
            ))
        }
    }
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::post;
    axum::Router::new()
        .route("/api-request/import-curl", post(import_curl_command))
        .route(
            "/api-request/import-to-library",
            post(import_curl_to_library),
        )
        .route("/api-request/test", post(test_api_request))
}
