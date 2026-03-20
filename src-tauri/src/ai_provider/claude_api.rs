#![allow(dead_code)]

use super::retry::retry_with_backoff;
use super::types::AiResponse;
use crate::config_facade::ai_keychain;
use crate::doctor::DoctorHandle;
use crate::settings;
use tracing::{debug, info};

/// Run a prompt via Claude API (direct HTTP calls)
///
/// Claude API responses include usage information:
/// ```json
/// {
///   "usage": {
///     "input_tokens": 123,
///     "output_tokens": 456
///   }
/// }
/// ```
pub(super) fn run_claude_api(
    prompt: &str,
    settings: &settings::ClaudeApiSettings,
    model_override: Option<&str>,
    _doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    let model = model_override.unwrap_or(&settings.model);
    info!(
        "Running Claude API (model: {}, override: {})",
        model,
        model_override.is_some()
    );

    // Get API key from keychain using KeychainHelper
    let api_key = match ai_keychain().get("claude_api") {
        Ok(Some(key)) => key,
        Ok(None) => {
            return AiResponse::error(
                "No Claude API key configured. Please set your API key in Settings.".to_string(),
            )
        }
        Err(e) => return AiResponse::error(format!("Failed to retrieve API key: {}", e)),
    };

    // Use blocking reqwest client for synchronous HTTP request.
    // No timeout — the Doctor service monitors process health externally.
    let client = match reqwest::blocking::Client::builder().build() {
        Ok(c) => c,
        Err(e) => return AiResponse::error(format!("Failed to create HTTP client: {}", e)),
    };

    // Build the request body once (it's the same for every retry attempt)
    let request_body = serde_json::json!({
        "model": model,
        "max_tokens": settings.max_tokens,
        "messages": [{"role": "user", "content": prompt}]
    });

    retry_with_backoff("Claude API", || {
        let response = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request_body)
            .send();

        match response {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().unwrap_or_default();
                    return AiResponse::error(format!("Claude API error ({}): {}", status, body));
                }

                match resp.json::<serde_json::Value>() {
                    Ok(json) => {
                        // Extract text content from response
                        let content = json["content"]
                            .as_array()
                            .and_then(|arr| arr.first())
                            .and_then(|c| c["text"].as_str())
                            .unwrap_or("")
                            .to_string();

                        // Extract token usage from response
                        // Claude API format: {"usage": {"input_tokens": N, "output_tokens": N}}
                        let input_tokens = json["usage"]["input_tokens"].as_u64();
                        let output_tokens = json["usage"]["output_tokens"].as_u64();

                        if let (Some(input), Some(output)) = (input_tokens, output_tokens) {
                            debug!(
                                "Claude API tokens - input: {}, output: {}, total: {}",
                                input,
                                output,
                                input + output
                            );
                            AiResponse::success_with_tokens(content, input, output)
                        } else {
                            debug!(
                                "Claude API response missing token counts: usage={:?}",
                                json["usage"]
                            );
                            AiResponse::success(content)
                        }
                    }
                    Err(e) => AiResponse::error(format!("Failed to parse API response: {}", e)),
                }
            }
            Err(e) => AiResponse::error(format!("Claude API request failed: {}", e)),
        }
    })
}

/// Run Claude API with optional temperature and max_tokens overrides.
pub(super) fn run_claude_api_with_overrides(
    prompt: &str,
    settings: &settings::ClaudeApiSettings,
    model_override: Option<&str>,
    doctor_handle: Option<&DoctorHandle>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
) -> AiResponse {
    // If no overrides specified, use the standard path
    if temperature_override.is_none() && max_tokens_override.is_none() {
        return run_claude_api(prompt, settings, model_override, doctor_handle);
    }

    let model = model_override.unwrap_or(&settings.model);
    let max_tokens = max_tokens_override.unwrap_or(settings.max_tokens);

    info!(
        "Running Claude API with overrides (model: {}, temp: {:?}, max_tokens: {})",
        model, temperature_override, max_tokens
    );

    let api_key = match ai_keychain().get("claude_api") {
        Ok(Some(key)) => key,
        Ok(None) => {
            return AiResponse::error(
                "No Claude API key configured. Please set your API key in Settings.".to_string(),
            )
        }
        Err(e) => return AiResponse::error(format!("Failed to retrieve API key: {}", e)),
    };

    let client = match reqwest::blocking::Client::builder().build() {
        Ok(c) => c,
        Err(e) => return AiResponse::error(format!("Failed to create HTTP client: {}", e)),
    };

    let mut request_body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [{"role": "user", "content": prompt}]
    });

    if let Some(temp) = temperature_override {
        request_body["temperature"] = serde_json::json!(temp);
    }

    retry_with_backoff("Claude API (overrides)", || {
        let response = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request_body)
            .send();

        match response {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().unwrap_or_default();
                    return AiResponse::error(format!("Claude API error ({}): {}", status, body));
                }

                match resp.json::<serde_json::Value>() {
                    Ok(json) => {
                        let content = json["content"]
                            .as_array()
                            .and_then(|arr| arr.first())
                            .and_then(|c| c["text"].as_str())
                            .unwrap_or("")
                            .to_string();

                        let input_tokens = json["usage"]["input_tokens"].as_u64();
                        let output_tokens = json["usage"]["output_tokens"].as_u64();

                        if let (Some(input), Some(output)) = (input_tokens, output_tokens) {
                            AiResponse::success_with_tokens(content, input, output)
                        } else {
                            AiResponse::success(content)
                        }
                    }
                    Err(e) => {
                        AiResponse::error(format!("Failed to parse Claude API response: {}", e))
                    }
                }
            }
            Err(e) => AiResponse::error(format!("Claude API request failed: {}", e)),
        }
    })
}
