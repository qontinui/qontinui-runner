//! API-level spec verification — verifies API endpoint responses match spec assertions
//! without requiring any browser or DOM rendering.
//!
//! Reads `apiAssertions` from spec JSON metadata and executes them by making HTTP
//! requests to the runner's own API, then evaluating field assertions against the
//! JSON response.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};

use super::types::{ApiResponse, ApiState};

// =============================================================================
// Request / Response Types
// =============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyApiSpecRequest {
    /// Registered app id whose specs/ root holds this spec
    /// (spec-multi-app Stream C). Required.
    pub app_id: String,
    /// Spec ID (filename without extension, e.g., "event-history")
    pub spec_id: String,
    /// Override base URL (default: http://localhost:{self_port})
    pub base_url: Option<String>,
    /// Only run assertions with these IDs (if empty, run all)
    pub assertion_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiAssertion {
    pub id: String,
    pub description: String,
    /// e.g., "GET /inngest/queue"
    pub endpoint: String,
    pub expected_status: Option<u16>,
    pub severity: String,
    pub expected_fields: Option<Vec<FieldAssertion>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldAssertion {
    /// JSON dot-path, e.g., "data.pending"
    pub path: String,
    /// Assertion type: "equals", "type_is", "exists", "gte", "lte", "contains"
    pub assertion: String,
    /// Expected value (interpretation depends on assertion type)
    pub expected: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiAssertionResult {
    pub id: String,
    pub passed: bool,
    pub endpoint: String,
    pub actual_status: u16,
    pub detail: String,
    pub response_time_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiSpecVerificationResult {
    pub spec_id: String,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub results: Vec<ApiAssertionResult>,
    pub duration_ms: u64,
}

// =============================================================================
// JSON Path Traversal
// =============================================================================

/// Navigate a JSON value by dot-separated path (e.g., "data.pending").
fn json_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for key in path.split('.') {
        // Try object key first
        if let Some(v) = current.get(key) {
            current = v;
        } else if let Ok(idx) = key.parse::<usize>() {
            // Try array index
            current = current.get(idx)?;
        } else {
            return None;
        }
    }
    Some(current)
}

/// Get the JSON type name for display.
fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

// =============================================================================
// Assertion Evaluation
// =============================================================================

/// Evaluate a single field assertion against a JSON response body.
fn evaluate_field_assertion(body: &serde_json::Value, field: &FieldAssertion) -> (bool, String) {
    let value = match json_path(body, &field.path) {
        Some(v) => v,
        None => {
            if field.assertion == "exists" {
                // "exists" with no value at path = fail
                return (false, format!("Path '{}' does not exist", field.path));
            }
            return (
                false,
                format!("Path '{}' not found in response", field.path),
            );
        }
    };

    match field.assertion.as_str() {
        "exists" => (true, format!("Path '{}' exists", field.path)),

        "equals" => {
            let expected = match &field.expected {
                Some(e) => e,
                None => {
                    return (
                        false,
                        "No expected value for 'equals' assertion".to_string(),
                    )
                }
            };
            let passed = value == expected;
            if passed {
                (true, format!("'{}' equals {:?}", field.path, expected))
            } else {
                (
                    false,
                    format!("'{}': expected {:?}, got {:?}", field.path, expected, value),
                )
            }
        }

        "type_is" => {
            let expected_type = match &field.expected {
                Some(serde_json::Value::String(s)) => s.as_str(),
                _ => {
                    return (
                        false,
                        "Expected a type name string for 'type_is'".to_string(),
                    )
                }
            };
            let actual_type = json_type_name(value);
            let passed = actual_type == expected_type;
            if passed {
                (true, format!("'{}' is type {}", field.path, actual_type))
            } else {
                (
                    false,
                    format!(
                        "'{}': expected type {}, got {}",
                        field.path, expected_type, actual_type
                    ),
                )
            }
        }

        "gte" => {
            let expected_num = match &field.expected {
                Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
                _ => return (false, "'gte' requires a numeric expected value".to_string()),
            };
            let actual_num = match value.as_f64() {
                Some(n) => n,
                None => return (false, format!("'{}' is not numeric", field.path)),
            };
            let passed = actual_num >= expected_num;
            if passed {
                (
                    true,
                    format!("'{}': {} >= {}", field.path, actual_num, expected_num),
                )
            } else {
                (
                    false,
                    format!(
                        "'{}': {} < {} (expected >=)",
                        field.path, actual_num, expected_num
                    ),
                )
            }
        }

        "lte" => {
            let expected_num = match &field.expected {
                Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
                _ => return (false, "'lte' requires a numeric expected value".to_string()),
            };
            let actual_num = match value.as_f64() {
                Some(n) => n,
                None => return (false, format!("'{}' is not numeric", field.path)),
            };
            let passed = actual_num <= expected_num;
            if passed {
                (
                    true,
                    format!("'{}': {} <= {}", field.path, actual_num, expected_num),
                )
            } else {
                (
                    false,
                    format!(
                        "'{}': {} > {} (expected <=)",
                        field.path, actual_num, expected_num
                    ),
                )
            }
        }

        "contains" => {
            let needle = match &field.expected {
                Some(serde_json::Value::String(s)) => s.as_str(),
                _ => {
                    return (
                        false,
                        "'contains' requires a string expected value".to_string(),
                    )
                }
            };
            let haystack = match value.as_str() {
                Some(s) => s,
                None => return (false, format!("'{}' is not a string", field.path)),
            };
            let passed = haystack.contains(needle);
            if passed {
                (true, format!("'{}' contains '{}'", field.path, needle))
            } else {
                (
                    false,
                    format!(
                        "'{}': '{}' does not contain '{}'",
                        field.path, haystack, needle
                    ),
                )
            }
        }

        other => (false, format!("Unknown assertion type: '{}'", other)),
    }
}

// =============================================================================
// Spec Loading
// =============================================================================

/// Load a spec's apiAssertions from the Spec API's projection storage.
async fn load_api_assertions(app_id: &str, spec_id: &str) -> Result<Vec<ApiAssertion>, String> {
    let pg = crate::database::pg::PgDb::global();
    let specs_root = crate::spec_api::storage::resolve_specs_root(&pg, app_id)
        .await
        .map_err(|e| format!("resolve_specs_root({}) failed: {:?}", app_id, e))?;
    let spec = match crate::spec_api::storage::read_projection(&specs_root, app_id, spec_id)? {
        Some(v) => v,
        None => {
            return Err(format!(
                "Page '{}' not found under {}/pages/",
                spec_id,
                specs_root.display()
            ));
        }
    };

    let assertions = spec
        .get("metadata")
        .and_then(|m| m.get("apiAssertions"))
        .and_then(|a| serde_json::from_value::<Vec<ApiAssertion>>(a.clone()).ok())
        .unwrap_or_default();

    if assertions.is_empty() {
        return Err(format!(
            "Spec '{}' has no apiAssertions in metadata",
            spec_id
        ));
    }

    Ok(assertions)
}

// =============================================================================
// Handlers
// =============================================================================

/// Verify a spec's API assertions by calling the endpoints and checking responses.
pub async fn verify_api_spec(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<VerifyApiSpecRequest>,
) -> Result<Json<ApiResponse<ApiSpecVerificationResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    let start = Instant::now();
    info!("API spec verification: spec_id={}", request.spec_id);

    // Load assertions
    let assertions = match load_api_assertions(&request.app_id, &request.spec_id).await {
        Ok(a) => a,
        Err(e) => {
            return Err((StatusCode::NOT_FOUND, Json(ApiResponse::error(e))));
        }
    };

    // Filter if specific assertion IDs requested
    let assertions: Vec<ApiAssertion> = match &request.assertion_ids {
        Some(ids) if !ids.is_empty() => assertions
            .into_iter()
            .filter(|a| ids.contains(&a.id))
            .collect(),
        _ => assertions,
    };

    // Determine base URL
    let port = super::types::get_mcp_api_port();
    let base_url = request
        .base_url
        .unwrap_or_else(|| format!("http://localhost:{}", port));

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::error(format!(
                    "Failed to create HTTP client: {}",
                    e
                ))),
            )
        })?;

    let mut results = Vec::new();

    for assertion in &assertions {
        // Parse endpoint string: "GET /inngest/queue" → (method, path)
        let parts: Vec<&str> = assertion.endpoint.splitn(2, ' ').collect();
        let (method, path) = match parts.as_slice() {
            [m, p] => (*m, *p),
            _ => {
                results.push(ApiAssertionResult {
                    id: assertion.id.clone(),
                    passed: false,
                    endpoint: assertion.endpoint.clone(),
                    actual_status: 0,
                    detail: format!(
                        "Invalid endpoint format: '{}' (expected 'METHOD /path')",
                        assertion.endpoint
                    ),
                    response_time_ms: 0,
                });
                continue;
            }
        };

        let url = format!("{}{}", base_url, path);
        let req_start = Instant::now();

        // Make the request
        let response = match method.to_uppercase().as_str() {
            "GET" => client.get(&url).send().await,
            "POST" => client.post(&url).send().await,
            "PUT" => client.put(&url).send().await,
            "DELETE" => client.delete(&url).send().await,
            _ => {
                results.push(ApiAssertionResult {
                    id: assertion.id.clone(),
                    passed: false,
                    endpoint: assertion.endpoint.clone(),
                    actual_status: 0,
                    detail: format!("Unsupported HTTP method: '{}'", method),
                    response_time_ms: 0,
                });
                continue;
            }
        };

        let response_time_ms = req_start.elapsed().as_millis() as u64;

        let response = match response {
            Ok(r) => r,
            Err(e) => {
                results.push(ApiAssertionResult {
                    id: assertion.id.clone(),
                    passed: false,
                    endpoint: assertion.endpoint.clone(),
                    actual_status: 0,
                    detail: format!("Request failed: {}", e),
                    response_time_ms,
                });
                continue;
            }
        };

        let status = response.status().as_u16();

        // Check status code
        if let Some(expected_status) = assertion.expected_status {
            if status != expected_status {
                results.push(ApiAssertionResult {
                    id: assertion.id.clone(),
                    passed: false,
                    endpoint: assertion.endpoint.clone(),
                    actual_status: status,
                    detail: format!("Expected status {}, got {}", expected_status, status),
                    response_time_ms,
                });
                continue;
            }
        }

        // Parse response body
        let body: serde_json::Value = match response.json().await {
            Ok(b) => b,
            Err(e) => {
                results.push(ApiAssertionResult {
                    id: assertion.id.clone(),
                    passed: false,
                    endpoint: assertion.endpoint.clone(),
                    actual_status: status,
                    detail: format!("Failed to parse response JSON: {}", e),
                    response_time_ms,
                });
                continue;
            }
        };

        // Evaluate field assertions
        let mut all_passed = true;
        let mut details = Vec::new();

        if let Some(fields) = &assertion.expected_fields {
            for field in fields {
                let (passed, detail) = evaluate_field_assertion(&body, field);
                if !passed {
                    all_passed = false;
                }
                details.push(detail);
            }
        }

        let detail = if details.is_empty() {
            format!("Status {} OK", status)
        } else {
            details.join("; ")
        };

        debug!(
            "API assertion '{}': {} ({}ms)",
            assertion.id,
            if all_passed { "PASS" } else { "FAIL" },
            response_time_ms
        );

        results.push(ApiAssertionResult {
            id: assertion.id.clone(),
            passed: all_passed,
            endpoint: assertion.endpoint.clone(),
            actual_status: status,
            detail,
            response_time_ms,
        });
    }

    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.iter().filter(|r| !r.passed).count();
    let duration_ms = start.elapsed().as_millis() as u64;

    info!(
        "API spec verification complete: {}/{} passed ({}ms)",
        passed,
        results.len(),
        duration_ms
    );

    Ok(Json(ApiResponse::success(ApiSpecVerificationResult {
        spec_id: request.spec_id,
        total: results.len(),
        passed,
        failed,
        results,
        duration_ms,
    })))
}

/// List all specs that have apiAssertions.
pub async fn list_api_specs(State(_state): State<Arc<ApiState>>) -> Json<ApiResponse<Vec<String>>> {
    let spec_dirs = [
        std::path::PathBuf::from("src/specs"),
        std::path::PathBuf::from("../src/specs"),
    ];

    let mut specs_with_api = Vec::new();

    for dir in &spec_dirs {
        if !dir.exists() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json")
                    && path.to_string_lossy().contains("spec.uibridge")
                {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(spec) = serde_json::from_str::<serde_json::Value>(&content) {
                            if spec
                                .get("metadata")
                                .and_then(|m| m.get("apiAssertions"))
                                .and_then(|a| a.as_array())
                                .is_some_and(|a| !a.is_empty())
                            {
                                let name = path
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("unknown")
                                    .replace(".spec.uibridge", "");
                                specs_with_api.push(name);
                            }
                        }
                    }
                }
            }
        }
    }

    Json(ApiResponse::success(specs_with_api))
}

// =============================================================================
// Routes
// =============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};

    axum::Router::new()
        .route("/ui-bridge/specs/verify-api", post(verify_api_spec))
        .route("/ui-bridge/specs/api-specs", get(list_api_specs))
}
