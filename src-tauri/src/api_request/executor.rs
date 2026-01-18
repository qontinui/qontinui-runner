//! API Request Executor
//!
//! Executes HTTP requests with variable substitution, response handling,
//! JSON path extraction, and assertion evaluation.

use super::types::{
    ApiAssertion, ApiRequestConfig, ApiRequestResult, AssertionResult, CredentialType,
    CredentialValue, ExtractionResult, VariableExtraction,
};
use super::variable_resolver::VariableResolver;
use anyhow::{Context, Result};
use jsonpath_rust::JsonPathFinder;
use reqwest::Client;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::{debug, error, info};
use uuid::Uuid;

/// Directory for storing API response images
fn get_api_images_dir() -> PathBuf {
    crate::paths::get_api_images_dir()
}

/// API Request Executor
pub struct ApiRequestExecutor {
    client: Client,
    variable_resolver: VariableResolver,
}

impl Default for ApiRequestExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiRequestExecutor {
    /// Create a new API request executor
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            variable_resolver: VariableResolver::new(),
        }
    }

    /// Create executor with a shared variable resolver
    pub fn with_resolver(resolver: VariableResolver) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            variable_resolver: resolver,
        }
    }

    /// Get mutable access to the variable resolver
    pub fn resolver(&self) -> &VariableResolver {
        &self.variable_resolver
    }

    /// Execute an API request
    pub async fn execute(
        &self,
        config: &ApiRequestConfig,
        credential: Option<(&CredentialType, &CredentialValue)>,
    ) -> Result<ApiRequestResult> {
        let start = Instant::now();

        // 1. Resolve variables in URL, headers, body
        let resolved_url = self.variable_resolver.resolve(&config.url);
        let resolved_headers = config
            .headers
            .as_ref()
            .map(|h| self.variable_resolver.resolve_map(h))
            .unwrap_or_default();
        let resolved_body = config.body.as_ref().map(|b| self.variable_resolver.resolve(b));

        debug!(
            "Executing API request: {} {}",
            config.method, resolved_url
        );

        // 2. Build request
        let timeout = Duration::from_millis(config.timeout_ms.unwrap_or(30000));
        let mut request = self
            .client
            .request(config.method.into(), &resolved_url)
            .timeout(timeout);

        // Apply headers (excluding Content-Type which we handle separately)
        for (key, value) in &resolved_headers {
            // Skip Content-Type from headers - we set it based on config.content_type
            if key.to_lowercase() != "content-type" {
                request = request.header(key, value);
            }
        }

        // Apply credential if provided
        if let Some((cred_type, cred_value)) = credential {
            if let Some((header_name, header_value)) = cred_value.get_auth_header(*cred_type) {
                request = request.header(&header_name, &header_value);
            }
        }

        // Apply content type and body
        if let Some(ref body) = resolved_body {
            // Use content_type from config, fall back to headers, then default to json
            let content_type = config
                .content_type
                .as_deref()
                .or_else(|| resolved_headers.get("Content-Type").map(|s| s.as_str()))
                .or_else(|| resolved_headers.get("content-type").map(|s| s.as_str()))
                .unwrap_or("application/json");
            request = request.header("Content-Type", content_type);
            request = request.body(body.clone());
        }

        // 3. Execute request
        let response = match request.send().await {
            Ok(resp) => resp,
            Err(e) => {
                error!("API request failed: {}", e);
                return Ok(ApiRequestResult {
                    success: false,
                    status_code: 0,
                    status_text: "Request Failed".to_string(),
                    response_headers: HashMap::new(),
                    response_time_ms: start.elapsed().as_millis() as u64,
                    response_body_type: "error".to_string(),
                    response_body: None,
                    response_file_path: None,
                    response_size_bytes: 0,
                    extractions: vec![],
                    assertions: vec![],
                    error: Some(e.to_string()),
                });
            }
        };

        let response_time_ms = start.elapsed().as_millis() as u64;

        // 4. Process response
        let status_code = response.status().as_u16();
        let status_text = response
            .status()
            .canonical_reason()
            .unwrap_or("Unknown")
            .to_string();

        let response_headers: HashMap<String, String> = response
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();

        let content_type = response_headers
            .get("content-type")
            .cloned()
            .unwrap_or_default();

        // Determine response type and handle accordingly
        let (response_body_type, response_body, response_file_path, response_size_bytes) =
            if is_binary_content(&content_type) {
                let bytes = response.bytes().await.context("Failed to read response body")?;
                let size = bytes.len();

                // Save binary to file
                let file_path = save_binary_response(&bytes, &content_type)?;

                ("binary".to_string(), None, Some(file_path), size)
            } else {
                let text = response.text().await.context("Failed to read response body")?;
                let size = text.len();
                let body_type = if content_type.contains("json") {
                    "json"
                } else {
                    "text"
                };
                (body_type.to_string(), Some(text), None, size)
            };

        // 5. Run extractions
        let extractions = self.run_extractions(&response_body, &config.extractions);

        // Store extracted variables
        for ext in &extractions {
            if ext.success {
                if let Some(ref value) = ext.extracted_value {
                    self.variable_resolver.set(&ext.variable_name, value);
                }
            }
        }

        // 6. Run assertions
        let assertions = self.run_assertions(
            status_code,
            &response_headers,
            &response_body,
            response_time_ms,
            &config.assertions,
        );

        // Determine overall success
        let assertion_failures = assertions.iter().filter(|a| !a.passed).count();
        let success = assertion_failures == 0;

        info!(
            "API request completed: {} {} -> {} ({} ms)",
            config.method, resolved_url, status_code, response_time_ms
        );

        Ok(ApiRequestResult {
            success,
            status_code,
            status_text,
            response_headers,
            response_time_ms,
            response_body_type,
            response_body,
            response_file_path,
            response_size_bytes,
            extractions,
            assertions,
            error: if !success {
                Some(format!("{} assertion(s) failed", assertion_failures))
            } else {
                None
            },
        })
    }

    /// Run variable extractions from response body
    fn run_extractions(
        &self,
        response_body: &Option<String>,
        extractions: &Option<Vec<VariableExtraction>>,
    ) -> Vec<ExtractionResult> {
        let Some(extractions) = extractions else {
            return vec![];
        };

        let Some(body) = response_body else {
            return extractions
                .iter()
                .map(|ext| ExtractionResult {
                    variable_name: ext.variable_name.clone(),
                    json_path: ext.json_path.clone(),
                    extracted_value: ext.default_value.clone(),
                    success: ext.default_value.is_some(),
                    error: if ext.default_value.is_none() {
                        Some("No response body to extract from".to_string())
                    } else {
                        None
                    },
                })
                .collect();
        };

        extractions
            .iter()
            .map(|ext| {
                // Create finder and execute JSON path query
                let finder = match JsonPathFinder::from_str(body, &ext.json_path) {
                    Ok(f) => f,
                    Err(e) => {
                        return ExtractionResult {
                            variable_name: ext.variable_name.clone(),
                            json_path: ext.json_path.clone(),
                            extracted_value: ext.default_value.clone(),
                            success: ext.default_value.is_some(),
                            error: Some(format!("Invalid JSON path: {}", e)),
                        };
                    }
                };

                // Execute query
                let results = finder.find_slice();

                if results.is_empty() {
                    ExtractionResult {
                        variable_name: ext.variable_name.clone(),
                        json_path: ext.json_path.clone(),
                        extracted_value: ext.default_value.clone(),
                        success: ext.default_value.is_some(),
                        error: if ext.default_value.is_none() {
                            Some("No matches found".to_string())
                        } else {
                            None
                        },
                    }
                } else {
                    // Get first result - JsonPathValue contains a reference to the data
                    // Clone the result before calling to_data() since it takes ownership
                    let value = match results[0].clone().to_data() {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        serde_json::Value::Null => "null".to_string(),
                        other => other.to_string(),
                    };

                    ExtractionResult {
                        variable_name: ext.variable_name.clone(),
                        json_path: ext.json_path.clone(),
                        extracted_value: Some(value),
                        success: true,
                        error: None,
                    }
                }
            })
            .collect()
    }

    /// Run assertions on response
    fn run_assertions(
        &self,
        status_code: u16,
        headers: &HashMap<String, String>,
        body: &Option<String>,
        response_time_ms: u64,
        assertions: &Option<Vec<ApiAssertion>>,
    ) -> Vec<AssertionResult> {
        let Some(assertions) = assertions else {
            return vec![];
        };

        assertions
            .iter()
            .map(|assertion| {
                self.evaluate_assertion(assertion, status_code, headers, body, response_time_ms)
            })
            .collect()
    }

    /// Evaluate a single assertion
    fn evaluate_assertion(
        &self,
        assertion: &ApiAssertion,
        status_code: u16,
        headers: &HashMap<String, String>,
        body: &Option<String>,
        response_time_ms: u64,
    ) -> AssertionResult {
        let operator = assertion.operator.as_deref().unwrap_or("equals");
        let expected = value_to_string(&assertion.expected);

        match assertion.assertion_type.as_str() {
            "status_code" => {
                let actual = status_code.to_string();
                let passed = compare_values(&actual, &expected, operator);
                AssertionResult {
                    assertion_type: "status_code".to_string(),
                    expected,
                    actual,
                    passed,
                    error: if !passed {
                        Some("Status code mismatch".to_string())
                    } else {
                        None
                    },
                }
            }
            "header" => {
                let header_name = assertion
                    .header_name
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase();
                let actual = headers
                    .iter()
                    .find(|(k, _)| k.to_lowercase() == header_name)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                let passed = compare_values(&actual, &expected, operator);
                AssertionResult {
                    assertion_type: "header".to_string(),
                    expected,
                    actual,
                    passed,
                    error: if !passed {
                        Some(format!("Header '{}' mismatch", header_name))
                    } else {
                        None
                    },
                }
            }
            "json_path" => {
                let json_path = assertion.json_path.as_deref().unwrap_or("$");
                let actual = extract_json_value(body, json_path);
                let passed = compare_values(&actual, &expected, operator);
                AssertionResult {
                    assertion_type: "json_path".to_string(),
                    expected,
                    actual,
                    passed,
                    error: if !passed {
                        Some(format!("JSON path '{}' mismatch", json_path))
                    } else {
                        None
                    },
                }
            }
            "body_contains" => {
                let actual = body.as_deref().unwrap_or("");
                let passed = actual.contains(&expected);
                AssertionResult {
                    assertion_type: "body_contains".to_string(),
                    expected,
                    actual: if actual.len() > 100 {
                        format!("{}...", &actual[..100])
                    } else {
                        actual.to_string()
                    },
                    passed,
                    error: if !passed {
                        Some("Body does not contain expected string".to_string())
                    } else {
                        None
                    },
                }
            }
            "response_time" => {
                let actual = response_time_ms.to_string();
                let passed = compare_values(&actual, &expected, operator);
                AssertionResult {
                    assertion_type: "response_time".to_string(),
                    expected: format!("{} ms", expected),
                    actual: format!("{} ms", actual),
                    passed,
                    error: if !passed {
                        Some("Response time exceeded limit".to_string())
                    } else {
                        None
                    },
                }
            }
            other => AssertionResult {
                assertion_type: other.to_string(),
                expected,
                actual: "N/A".to_string(),
                passed: false,
                error: Some(format!("Unknown assertion type: {}", other)),
            },
        }
    }
}

/// Check if content type indicates binary data
fn is_binary_content(content_type: &str) -> bool {
    let ct = content_type.to_lowercase();
    ct.starts_with("image/")
        || ct.starts_with("video/")
        || ct.starts_with("audio/")
        || ct.starts_with("application/octet-stream")
        || ct.starts_with("application/pdf")
        || ct.starts_with("application/zip")
}

/// Save binary response to file
fn save_binary_response(bytes: &[u8], content_type: &str) -> Result<String> {
    let api_images_dir = get_api_images_dir();
    fs::create_dir_all(&api_images_dir).context("Failed to create api-images directory")?;

    // Determine file extension from content type
    let extension = match content_type.to_lowercase().as_str() {
        ct if ct.contains("image/png") => "png",
        ct if ct.contains("image/jpeg") || ct.contains("image/jpg") => "jpg",
        ct if ct.contains("image/gif") => "gif",
        ct if ct.contains("image/webp") => "webp",
        ct if ct.contains("image/svg") => "svg",
        ct if ct.contains("application/pdf") => "pdf",
        ct if ct.contains("application/zip") => "zip",
        _ => "bin",
    };

    let filename = format!("api-{}.{}", Uuid::new_v4(), extension);
    let file_path = api_images_dir.join(&filename);

    fs::write(&file_path, bytes).context("Failed to write response to file")?;

    Ok(file_path.to_string_lossy().to_string())
}

/// Convert serde_json::Value to string
fn value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// Extract value from JSON body using JSON path
fn extract_json_value(body: &Option<String>, json_path: &str) -> String {
    let Some(body) = body else {
        return String::new();
    };

    let finder = match JsonPathFinder::from_str(body, json_path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };

    let results = finder.find_slice();
    if results.is_empty() {
        String::new()
    } else {
        // Clone the result before calling to_data() since it takes ownership
        value_to_string(&results[0].clone().to_data())
    }
}

/// Compare two values using the specified operator
fn compare_values(actual: &str, expected: &str, operator: &str) -> bool {
    match operator {
        "equals" => actual == expected,
        "contains" => actual.contains(expected),
        "matches" => {
            regex::Regex::new(expected)
                .map(|re| re.is_match(actual))
                .unwrap_or(false)
        }
        "greater_than" => {
            let a: f64 = actual.parse().unwrap_or(0.0);
            let e: f64 = expected.parse().unwrap_or(0.0);
            a > e
        }
        "less_than" => {
            let a: f64 = actual.parse().unwrap_or(0.0);
            let e: f64 = expected.parse().unwrap_or(0.0);
            a < e
        }
        _ => actual == expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_binary_content() {
        assert!(is_binary_content("image/png"));
        assert!(is_binary_content("image/jpeg"));
        assert!(is_binary_content("application/pdf"));
        assert!(!is_binary_content("application/json"));
        assert!(!is_binary_content("text/html"));
    }

    #[test]
    fn test_compare_values() {
        assert!(compare_values("200", "200", "equals"));
        assert!(!compare_values("200", "201", "equals"));
        assert!(compare_values("hello world", "world", "contains"));
        assert!(compare_values("150", "100", "greater_than"));
        assert!(compare_values("50", "100", "less_than"));
    }

    #[test]
    fn test_value_to_string() {
        assert_eq!(value_to_string(&serde_json::json!("hello")), "hello");
        assert_eq!(value_to_string(&serde_json::json!(123)), "123");
        assert_eq!(value_to_string(&serde_json::json!(true)), "true");
    }
}
