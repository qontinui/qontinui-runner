//! API Request Step Handler
//!
//! Handles api_request steps that make HTTP requests with variable interpolation,
//! response extraction, and chaining support.

use async_trait::async_trait;
use std::collections::HashMap;
use tracing::{info, warn};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::api_request::{ApiRequestConfig, ApiRequestSession, HttpMethod, VariableExtraction};
use crate::orchestrator::context_propagation::ExpressionEvaluator;
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for API request steps.
pub struct ApiRequestHandler;

#[async_trait]
impl StepHandler for ApiRequestHandler {
    fn step_type(&self) -> &'static str {
        "api_request"
    }

    fn display_name(&self) -> &'static str {
        "API Request"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // Validate required fields
        let method_str = match &step.api_method {
            Some(m) => m.to_uppercase(),
            None => {
                return StepHandlerResult::failure("No HTTP method specified for API request");
            }
        };

        let url = match &step.api_url {
            Some(u) => u.clone(),
            None => {
                return StepHandlerResult::failure("No URL specified for API request");
            }
        };

        // Parse HTTP method
        let method = match method_str.as_str() {
            "GET" => HttpMethod::Get,
            "POST" => HttpMethod::Post,
            "PUT" => HttpMethod::Put,
            "PATCH" => HttpMethod::Patch,
            "DELETE" => HttpMethod::Delete,
            _ => {
                return StepHandlerResult::failure(format!(
                    "Unsupported HTTP method: {}",
                    method_str
                ));
            }
        };

        let step_name = step.name.as_deref().unwrap_or("API Request");

        // First, expand variables in URL using RuntimeContext (for step outputs, etc.)
        let evaluator = ExpressionEvaluator::new();
        let url_with_context = evaluator.evaluate(&url, &context.runtime_context);

        // Build headers HashMap from serde_json::Value
        let headers: Option<HashMap<String, String>> = step.api_headers.as_ref().and_then(|h| {
            h.as_object().map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| {
                        v.as_str().map(|s| {
                            // Expand variables in header values
                            let expanded = evaluator.evaluate(s, &context.runtime_context);
                            (k.clone(), expanded)
                        })
                    })
                    .collect()
            })
        });

        // Expand variables in body
        let body = step
            .api_body
            .as_ref()
            .map(|b| evaluator.evaluate(b, &context.runtime_context));

        // Build extractions list - combine api_extractions with legacy api_output_variable
        let mut extractions = step.api_extractions.clone().unwrap_or_default();

        // Support legacy api_output_variable format: "var_name.path.to.field" extracts $.path.to.field
        if let Some(ref var_spec) = step.api_output_variable {
            let (var_name, json_path) = Self::parse_output_variable_spec(var_spec);
            let json_path_str = json_path.unwrap_or_else(|| "$".to_string());
            extractions.push(VariableExtraction {
                variable_name: var_name,
                json_path: json_path_str,
                default_value: None,
            });
        }

        // Determine timeout: use step-specific, then default
        let timeout_ms = step.api_timeout_ms.unwrap_or(30_000);

        // Build ApiRequestConfig
        let config = ApiRequestConfig {
            step_id: None,
            step_name: Some(step_name.to_string()),
            method,
            url: url_with_context.clone(),
            resolved_url: None,
            headers,
            body,
            content_type: step.api_content_type.clone(),
            timeout_ms: Some(timeout_ms),
            follow_redirects: Some(true),
            credential_id: None,
            extractions: if extractions.is_empty() {
                None
            } else {
                Some(extractions)
            },
            assertions: step.api_assertions.clone(),
        };

        info!(
            "Executing API request '{}': {} {} (timeout: {}ms)",
            step_name, method_str, url_with_context, timeout_ms
        );

        // Create session from shared variable store for proper variable chaining
        let mut session = ApiRequestSession::from_shared_store(&context.shared_variables);

        // Execute the request
        match session.execute(&config, None).await {
            Ok(result) => {
                // Sync extracted variables back to the shared store
                session.sync_to_shared_store(&context.shared_variables);

                // Log extraction results
                for ext in &result.extractions {
                    if ext.success {
                        if let Some(ref value) = ext.extracted_value {
                            let preview = if value.len() > 50 {
                                format!("{}...", &value[..50])
                            } else {
                                value.clone()
                            };
                            info!(
                                "Extracted '{}' using JSON path '{}' -> '{}' ({} chars)",
                                ext.variable_name,
                                ext.json_path,
                                preview,
                                value.len()
                            );
                        }
                    } else if let Some(ref error) = ext.error {
                        warn!(
                            "Extraction failed for '{}' ({}): {}",
                            ext.variable_name, ext.json_path, error
                        );
                    }
                }

                info!(
                    "API request '{}' completed: status={}, duration={}ms, extractions={}",
                    step_name,
                    result.status_code,
                    result.response_time_ms,
                    result.extractions.len()
                );

                if result.success {
                    let output_data = result.response_body.map(|body| {
                        serde_json::json!({
                            "status_code": result.status_code,
                            "response_time_ms": result.response_time_ms,
                            "response_body": body,
                        })
                    });
                    match output_data {
                        Some(data) => StepHandlerResult::success_with_data(data),
                        None => StepHandlerResult::success(),
                    }
                } else {
                    let error_msg = result.error.unwrap_or_else(|| {
                        format!(
                            "HTTP {}: {}",
                            result.status_code,
                            result
                                .response_body
                                .as_ref()
                                .map(|b| b.chars().take(500).collect::<String>())
                                .unwrap_or_default()
                        )
                    });
                    StepHandlerResult::failure(error_msg)
                }
            }
            Err(e) => StepHandlerResult::failure(format!("API request failed: {}", e)),
        }
    }
}

impl ApiRequestHandler {
    /// Parse legacy output variable spec: "var_name.path.to.field" -> ("var_name", Some("$.path.to.field"))
    fn parse_output_variable_spec(spec: &str) -> (String, Option<String>) {
        if let Some(dot_pos) = spec.find('.') {
            let var_name = spec[..dot_pos].to_string();
            let json_path = format!("$.{}", &spec[dot_pos + 1..]);
            (var_name, Some(json_path))
        } else {
            // No path specified, extract entire response
            (spec.to_string(), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_request_handler_step_type() {
        let handler = ApiRequestHandler;
        assert_eq!(handler.step_type(), "api_request");
        assert_eq!(handler.display_name(), "API Request");
    }

    #[test]
    fn test_parse_output_variable_spec_with_path() {
        let (name, path) = ApiRequestHandler::parse_output_variable_spec("token.data.access_token");
        assert_eq!(name, "token");
        assert_eq!(path, Some("$.data.access_token".to_string()));
    }

    #[test]
    fn test_parse_output_variable_spec_without_path() {
        let (name, path) = ApiRequestHandler::parse_output_variable_spec("response");
        assert_eq!(name, "response");
        assert_eq!(path, None);
    }
}
