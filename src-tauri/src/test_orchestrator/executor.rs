//! Test Orchestration Executor
//!
//! Executes orchestration plans by running HTTP requests in sequence,
//! extracting variables from responses, and substituting them into
//! subsequent requests.

use super::types::*;
// HttpMethod is imported via super::types
use crate::saved_api_requests::SavedApiRequest;
use crate::settings::{
    get_execution_variables_settings, get_resolved_auth_token, get_resolved_custom_variables,
    AuthSource,
};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use jsonpath_rust::JsonPath;
use regex::Regex;
use reqwest::Client;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

/// Test Orchestrator - executes plans with variable chaining
pub struct TestOrchestrator {
    /// HTTP client for making requests
    client: Client,
    /// Variables extracted during execution
    variables: HashMap<String, serde_json::Value>,
    /// Regex for variable substitution (matches {{variableName}})
    var_regex: Regex,
    /// Auth source configuration
    auth_source: AuthSource,
    /// Auth header name (e.g., "Authorization")
    auth_header_name: String,
    /// Resolved auth token (if auth_source is not Captured)
    auth_token: Option<String>,
}

impl Default for TestOrchestrator {
    fn default() -> Self {
        Self::new()
    }
}

impl TestOrchestrator {
    /// Create a new TestOrchestrator
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("Failed to build HTTP client");

        // Load execution variables settings
        let exec_settings = get_execution_variables_settings();
        let auth_token = get_resolved_auth_token();

        info!(
            "TestOrchestrator initialized with auth_source={:?}, auth_token_present={}",
            exec_settings.auth_source,
            auth_token.is_some()
        );

        Self {
            client,
            variables: HashMap::new(),
            var_regex: Regex::new(r"\{\{(\w+)\}\}").expect("Invalid regex"),
            auth_source: exec_settings.auth_source,
            auth_header_name: exec_settings.auth_header_name,
            auth_token,
        }
    }

    /// Create with custom timeout
    pub fn with_timeout(timeout_seconds: u64) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_seconds))
            .build()
            .expect("Failed to build HTTP client");

        // Load execution variables settings
        let exec_settings = get_execution_variables_settings();
        let auth_token = get_resolved_auth_token();

        info!(
            "TestOrchestrator initialized with auth_source={:?}, auth_token_present={}",
            exec_settings.auth_source,
            auth_token.is_some()
        );

        Self {
            client,
            variables: HashMap::new(),
            var_regex: Regex::new(r"\{\{(\w+)\}\}").expect("Invalid regex"),
            auth_source: exec_settings.auth_source,
            auth_header_name: exec_settings.auth_header_name,
            auth_token,
        }
    }

    /// Execute an orchestration plan
    ///
    /// Runs each step in order, extracting variables and substituting them
    /// into subsequent requests.
    pub async fn execute_plan(
        &mut self,
        plan: &TestOrchestrationPlan,
        saved_requests: &[SavedApiRequest],
    ) -> Result<OrchestrationExecutionResult> {
        info!("Executing orchestration plan: {}", plan.id);

        let start_time = Instant::now();
        let started_at = Utc::now().to_rfc3339();

        // Clear any previous variables
        self.variables.clear();

        // Inject custom variables from execution settings
        let custom_vars = get_resolved_custom_variables();
        for (name, value) in custom_vars {
            info!("Injecting custom variable: {}", name);
            self.variables
                .insert(name, serde_json::Value::String(value));
        }

        let mut step_results = Vec::new();
        let mut failed_at_step: Option<usize> = None;

        // Create a map of request_id -> SavedApiRequest for quick lookup
        let request_map: HashMap<&str, &SavedApiRequest> =
            saved_requests.iter().map(|r| (r.id.as_str(), r)).collect();

        for step in &plan.steps {
            info!("Executing step {}: {}", step.step_index, step.name);

            // Check dependencies
            for dep in &step.depends_on {
                if let Some(dep_result) = step_results.get(*dep) {
                    let dep_result: &StepExecutionResult = dep_result;
                    if !dep_result.success {
                        warn!("Step {} depends on failed step {}", step.step_index, dep);
                    }
                }
            }

            // Find the saved request
            let saved_request = request_map
                .get(step.request_id.as_str())
                .ok_or_else(|| anyhow!("Request not found: {}", step.request_id))?;

            // Execute the step
            match self.execute_step(step, saved_request).await {
                Ok(result) => {
                    let success = result.success;
                    step_results.push(result);
                    if !success && failed_at_step.is_none() {
                        failed_at_step = Some(step.step_index);
                        // Continue execution to gather more data, but mark as failed
                    }
                }
                Err(e) => {
                    error!("Step {} failed: {}", step.step_index, e);
                    step_results.push(StepExecutionResult {
                        step_index: step.step_index,
                        step_name: step.name.clone(),
                        request: ExecutedRequest {
                            method: saved_request.method,
                            url: self.substitute_variables(&saved_request.url),
                            headers: HashMap::new(),
                            body: None,
                        },
                        response: ExecutedResponse {
                            status_code: 0,
                            headers: HashMap::new(),
                            body: serde_json::Value::Null,
                            content_type: None,
                            size_bytes: 0,
                        },
                        extracted_variables: HashMap::new(),
                        success: false,
                        error: Some(e.to_string()),
                        duration_ms: 0,
                    });
                    if failed_at_step.is_none() {
                        failed_at_step = Some(step.step_index);
                    }
                    // Continue to try remaining steps for maximum data collection
                }
            }
        }

        let total_duration = start_time.elapsed().as_millis() as u64;
        let completed_at = Utc::now().to_rfc3339();

        Ok(OrchestrationExecutionResult {
            plan_id: plan.id.clone(),
            step_results,
            all_variables: self.variables.clone(),
            success: failed_at_step.is_none(),
            failed_at_step,
            total_duration_ms: total_duration,
            started_at,
            completed_at,
        })
    }

    /// Execute a single step
    async fn execute_step(
        &mut self,
        step: &OrchestrationStep,
        saved_request: &SavedApiRequest,
    ) -> Result<StepExecutionResult> {
        let start_time = Instant::now();

        // Substitute variables in URL and body
        let url = self.substitute_variables(&saved_request.url);
        let body = saved_request
            .body
            .as_ref()
            .map(|b| self.substitute_variables(b));

        // Substitute variables in headers
        let mut headers: HashMap<String, String> = saved_request
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), self.substitute_variables(v)))
            .collect();

        // Override auth header if auth_source is not Captured
        if self.auth_source != AuthSource::Captured {
            if let Some(ref token) = self.auth_token {
                info!(
                    "Overriding {} header with configured auth token (source: {:?})",
                    self.auth_header_name, self.auth_source
                );
                headers.insert(self.auth_header_name.clone(), token.clone());
            } else {
                warn!(
                    "Auth source is {:?} but no auth token available - using captured headers",
                    self.auth_source
                );
            }
        }

        info!("Orchestrator executing: {} {}", saved_request.method, url);
        // Log headers but redact auth values for security
        let redacted_headers: HashMap<&str, &str> = headers
            .iter()
            .map(|(k, v)| {
                let display_value = if k.to_lowercase() == "authorization"
                    || k.to_lowercase().contains("token")
                    || k.to_lowercase().contains("api-key")
                {
                    "[REDACTED]"
                } else {
                    v.as_str()
                };
                (k.as_str(), display_value)
            })
            .collect();
        info!("Orchestrator headers: {:?}", redacted_headers);
        if let Some(ref b) = body {
            info!("Orchestrator body: {}", b);
        } else {
            info!("Orchestrator body: <none>");
        }

        // Build request
        let mut request_builder = self.client.request(saved_request.method.into(), &url);

        // Apply headers
        for (key, value) in &headers {
            request_builder = request_builder.header(key, value);
        }

        // Apply body
        if let Some(ref body_str) = body {
            let content_type = saved_request
                .body_content_type
                .as_deref()
                .unwrap_or("application/json");
            request_builder = request_builder
                .header("Content-Type", content_type)
                .body(body_str.clone());
        }

        // Execute request
        let response = request_builder
            .send()
            .await
            .context("Failed to send HTTP request")?;

        let status_code = response.status().as_u16();
        let response_headers: HashMap<String, String> = response
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();

        let content_type = response_headers.get("content-type").cloned();

        // Read response body
        let body_bytes = response.bytes().await?;
        let body_size = body_bytes.len();
        let body_str = String::from_utf8_lossy(&body_bytes);

        // Try to parse as JSON
        let response_body: serde_json::Value = serde_json::from_str(&body_str)
            .unwrap_or_else(|_| serde_json::Value::String(body_str.to_string()));

        info!(
            "Orchestrator response: status={}, size={} bytes",
            status_code, body_size
        );
        if status_code >= 400 {
            warn!(
                "Orchestrator request failed with status {}: {}",
                status_code, body_str
            );
        }

        // Extract variables from response
        let mut extracted = HashMap::new();
        for extraction in &step.extractions {
            if let Some(value) = self.extract_json_path(&response_body, &extraction.json_path) {
                debug!(
                    "Extracted {} = {:?} from {}",
                    extraction.variable_name, value, extraction.json_path
                );
                self.variables
                    .insert(extraction.variable_name.clone(), value.clone());
                extracted.insert(extraction.variable_name.clone(), value);
            } else if let Some(default) = &extraction.default_value {
                let default_value = serde_json::Value::String(default.clone());
                debug!(
                    "Using default for {}: {}",
                    extraction.variable_name, default
                );
                self.variables
                    .insert(extraction.variable_name.clone(), default_value.clone());
                extracted.insert(extraction.variable_name.clone(), default_value);
            } else {
                warn!(
                    "Failed to extract {} from path {}",
                    extraction.variable_name, extraction.json_path
                );
            }
        }

        let duration_ms = start_time.elapsed().as_millis() as u64;
        let success = (200..300).contains(&(status_code as i32));

        // Build detailed error message for non-success responses
        let error_message = if success {
            None
        } else {
            // Try to extract error details from response body
            let error_detail = match &response_body {
                serde_json::Value::Object(obj) => {
                    // Look for common error fields
                    obj.get("error")
                        .or_else(|| obj.get("message"))
                        .or_else(|| obj.get("detail"))
                        .map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            _ => v.to_string(),
                        })
                }
                serde_json::Value::String(s) => Some(s.clone()),
                _ => None,
            };

            Some(match error_detail {
                Some(detail) => format!("HTTP {} - {}", status_code, detail),
                None => format!("HTTP {} response", status_code),
            })
        };

        Ok(StepExecutionResult {
            step_index: step.step_index,
            step_name: step.name.clone(),
            request: ExecutedRequest {
                method: saved_request.method,
                url,
                headers,
                body,
            },
            response: ExecutedResponse {
                status_code,
                headers: response_headers,
                body: response_body,
                content_type,
                size_bytes: body_size,
            },
            extracted_variables: extracted,
            success,
            error: error_message,
            duration_ms,
        })
    }

    /// Substitute {{variable}} placeholders in a string
    fn substitute_variables(&self, template: &str) -> String {
        self.var_regex
            .replace_all(template, |caps: &regex::Captures| {
                let var_name = &caps[1];
                if let Some(value) = self.variables.get(var_name) {
                    // Convert to string representation
                    match value {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        serde_json::Value::Null => "null".to_string(),
                        _ => serde_json::to_string(value).unwrap_or_default(),
                    }
                } else {
                    // Keep the original placeholder if variable not found
                    caps[0].to_string()
                }
            })
            .to_string()
    }

    /// Extract a value from JSON using JSONPath
    fn extract_json_path(&self, json: &serde_json::Value, path: &str) -> Option<serde_json::Value> {
        // Handle simple paths without JSONPath
        if !path.starts_with('$') {
            // Simple dot notation: data.id
            let parts: Vec<&str> = path.split('.').collect();
            let mut current = json;
            for part in parts {
                current = current.get(part)?;
            }
            return Some(current.clone());
        }

        // Use JSONPath library
        let results = match json.query(path) {
            Ok(r) => r,
            Err(_) => return None,
        };

        match results.len() {
            0 => None,
            1 => {
                let value = results[0].clone();
                // JSONPath library returns Null for non-existent paths; treat as None
                if value.is_null() {
                    None
                } else {
                    Some(value)
                }
            }
            _ => {
                let values: Vec<serde_json::Value> =
                    results.into_iter().cloned().collect();
                Some(serde_json::Value::Array(values))
            }
        }
    }

    /// Get the current value of a variable
    pub fn get_variable(&self, name: &str) -> Option<&serde_json::Value> {
        self.variables.get(name)
    }

    /// Get all current variables
    pub fn get_all_variables(&self) -> &HashMap<String, serde_json::Value> {
        &self.variables
    }

    /// Set a variable value (useful for initial/external values)
    pub fn set_variable(&mut self, name: String, value: serde_json::Value) {
        self.variables.insert(name, value);
    }

    /// Clear all variables
    pub fn clear_variables(&mut self) {
        self.variables.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_substitute_variables() {
        let mut orchestrator = TestOrchestrator::new();
        orchestrator.set_variable(
            "id".to_string(),
            serde_json::Value::String("123".to_string()),
        );
        orchestrator.set_variable("count".to_string(), serde_json::Value::Number(42.into()));

        let result =
            orchestrator.substitute_variables("http://example.com/items/{{id}}?count={{count}}");
        assert_eq!(result, "http://example.com/items/123?count=42");

        // Unknown variables should remain
        let result = orchestrator.substitute_variables("{{unknown}}");
        assert_eq!(result, "{{unknown}}");
    }

    #[test]
    fn test_extract_json_path() {
        let orchestrator = TestOrchestrator::new();
        let json: serde_json::Value = serde_json::json!({
            "data": {
                "id": "abc-123",
                "count": 5,
                "items": [1, 2, 3]
            }
        });

        // Test simple path
        let result = orchestrator.extract_json_path(&json, "data.id");
        assert_eq!(result, Some(serde_json::json!("abc-123")));

        // Test JSONPath
        let result = orchestrator.extract_json_path(&json, "$.data.count");
        assert_eq!(result, Some(serde_json::json!(5)));

        // Test array
        let result = orchestrator.extract_json_path(&json, "$.data.items");
        assert_eq!(result, Some(serde_json::json!([1, 2, 3])));

        // Test non-existent path
        let result = orchestrator.extract_json_path(&json, "$.data.missing");
        assert_eq!(result, None);
    }
}
