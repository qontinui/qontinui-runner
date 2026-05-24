#![allow(dead_code)]

use super::cache_aware_builder::{
    parse_cache_tokens, CacheAwareRequestBuilder, StructuredPrompt, PROMPT_CACHING_BETA_HEADER,
};
use super::multimodal::MultimodalPrompt;
use super::retry::retry_with_backoff;
use super::types::AiResponse;
use crate::config_facade::ai_keychain;
use crate::doctor::DoctorHandle;
use crate::settings;
use reqwest::StatusCode;
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Default cooldown applied to a rate-limited account when the server
/// response omits a `Retry-After` header. Five minutes — same as
/// `config::RATE_LIMIT_COOLDOWN_SECS`.
const DEFAULT_RETRY_AFTER_SECS: u64 = 300;

/// Parse `Retry-After` from a 429 response and mark the currently-resolved
/// Claude account as rate-limited for that duration. The retry layer in
/// `super::retry` will then rotate to another account (and because
/// [`super::config::rotate_account_on_rate_limit`] won't re-mark an account
/// that's already in the cooldown map, the precise duration we set here
/// survives).
///
/// If no account is resolved (manual mode without a resolved dir), this is
/// a no-op — the generic cooldown rotation handles it on the next pass.
fn mark_current_account_rate_limited(headers: &reqwest::header::HeaderMap) {
    let Some(dir) = super::config::get_resolved_config_dir() else {
        return;
    };
    let secs = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_RETRY_AFTER_SECS);
    super::config::mark_account_rate_limited_with_duration(&dir, Duration::from_secs(secs));
}

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
///
/// TODO(Phase 4): When `MiddlewareContext` has `expected_schema_name`, pass the
/// JSON Schema via Claude's `tool_use` response format parameter for server-side
/// constrained generation. This avoids client-side retries entirely for supported
/// models. Until then, the retry-with-validation-feedback path (Phase 3) handles
/// schema compliance via the `StructuredOutputMiddleware`.
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
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        mark_current_account_rate_limited(resp.headers());
                    }
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
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        mark_current_account_rate_limited(resp.headers());
                    }
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

/// Run a multimodal prompt via Claude API with text + image content blocks.
///
/// This sends proper content arrays to Claude's Messages API, enabling vision
/// features like screenshot analysis. Falls back to text-only if no images.
///
/// Claude API multimodal format:
/// ```json
/// {
///   "messages": [{
///     "role": "user",
///     "content": [
///       {"type": "text", "text": "..."},
///       {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "..."}}
///     ]
///   }]
/// }
/// ```
pub(super) fn run_claude_api_multimodal(
    prompt: &MultimodalPrompt,
    settings: &settings::ClaudeApiSettings,
    model_override: Option<&str>,
    _doctor_handle: Option<&DoctorHandle>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
) -> AiResponse {
    let model = model_override.unwrap_or(&settings.model);
    let max_tokens = max_tokens_override.unwrap_or(settings.max_tokens);

    info!(
        "Running Claude API multimodal (model: {}, has_images: {}, blocks: {})",
        model,
        prompt.has_images(),
        prompt.content.len()
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

    // Build multimodal request body with content block array
    let mut request_body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": prompt.to_claude_messages()
    });

    // Add system prompt if present
    if let Some(ref system) = prompt.system {
        request_body["system"] = serde_json::json!(system);
    }

    // Add temperature override if present
    if let Some(temp) = temperature_override {
        request_body["temperature"] = serde_json::json!(temp);
    }

    retry_with_backoff("Claude API (multimodal)", || {
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
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        mark_current_account_rate_limited(resp.headers());
                    }
                    let body = resp.text().unwrap_or_default();
                    return AiResponse::error(format!(
                        "Claude API multimodal error ({}): {}",
                        status, body
                    ));
                }

                match resp.json::<serde_json::Value>() {
                    Ok(json) => {
                        // Extract text content from response (same format as text-only)
                        let content = json["content"]
                            .as_array()
                            .and_then(|arr| arr.first())
                            .and_then(|c| c["text"].as_str())
                            .unwrap_or("")
                            .to_string();

                        let input_tokens = json["usage"]["input_tokens"].as_u64();
                        let output_tokens = json["usage"]["output_tokens"].as_u64();

                        if let (Some(input), Some(output)) = (input_tokens, output_tokens) {
                            debug!(
                                "Claude API multimodal tokens - input: {}, output: {}, total: {}",
                                input,
                                output,
                                input + output
                            );
                            AiResponse::success_with_tokens(content, input, output)
                        } else {
                            AiResponse::success(content)
                        }
                    }
                    Err(e) => AiResponse::error(format!(
                        "Failed to parse Claude API multimodal response: {}",
                        e
                    )),
                }
            }
            Err(e) => AiResponse::error(format!("Claude API multimodal request failed: {}", e)),
        }
    })
}

/// Run a prompt via Claude API with prompt caching enabled.
///
/// Uses the `anthropic-beta: prompt-caching-2024-07-31` header and places
/// `cache_control: {"type": "ephemeral"}` on stable content blocks in the
/// system prompt array. Cached blocks are served at 10% of base input price
/// on subsequent reads within the 5-minute TTL window.
pub(super) fn run_claude_api_cached(
    prompt: &StructuredPrompt,
    settings: &settings::ClaudeApiSettings,
    model_override: Option<&str>,
    _doctor_handle: Option<&DoctorHandle>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
) -> AiResponse {
    let model = model_override.unwrap_or(&settings.model);
    let max_tokens = max_tokens_override.unwrap_or(settings.max_tokens);

    info!(
        "Running Claude API with prompt caching (model: {}, cached_blocks: {}, dynamic_blocks: {}, user_msg: {} chars)",
        model,
        prompt.cached_system_blocks.len(),
        prompt.dynamic_system_blocks.len(),
        prompt.user_message.len(),
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

    let mut builder = CacheAwareRequestBuilder::from_structured_prompt(prompt, model, max_tokens);
    if let Some(temp) = temperature_override {
        builder = builder.temperature(temp);
    }
    let request_body = builder.build(&prompt.user_message);

    retry_with_backoff("Claude API (cached)", || {
        let request = super::anthropic_auth::apply_blocking(
            client.post("https://api.anthropic.com/v1/messages"),
            &api_key,
        )
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", PROMPT_CACHING_BETA_HEADER)
        .header("content-type", "application/json")
        .json(&request_body);
        let response = request.send();

        match response {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        mark_current_account_rate_limited(resp.headers());
                    }
                    let body = resp.text().unwrap_or_default();
                    return AiResponse::error(format!(
                        "Claude API cached error ({}): {}",
                        status, body
                    ));
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
                        let (cache_creation, cache_read) = parse_cache_tokens(&json);

                        if cache_creation > 0 || cache_read > 0 {
                            info!(
                                "Claude API cache metrics - creation: {}, read: {} (saved ~{:.0}% on cached tokens)",
                                cache_creation,
                                cache_read,
                                if cache_read > 0 { 90.0 } else { 0.0 }
                            );
                        }

                        if let (Some(input), Some(output)) = (input_tokens, output_tokens) {
                            debug!(
                                "Claude API cached tokens - input: {}, output: {}, cache_create: {}, cache_read: {}",
                                input, output, cache_creation, cache_read
                            );
                            AiResponse::success_with_cache_tokens(
                                content,
                                input,
                                output,
                                cache_creation,
                                cache_read,
                            )
                        } else {
                            AiResponse::success(content)
                        }
                    }
                    Err(e) => AiResponse::error(format!(
                        "Failed to parse Claude API cached response: {}",
                        e
                    )),
                }
            }
            Err(e) => AiResponse::error(format!("Claude API cached request failed: {}", e)),
        }
    })
}

/// Run Claude API with server-side structured output enforcement via tool_use.
///
/// Uses Claude's tool_use mechanism to enforce JSON Schema on the server side:
/// 1. Defines a tool whose `input_schema` is the desired JSON Schema
/// 2. Forces `tool_choice` to use that specific tool
/// 3. Extracts the tool input (which conforms to the schema) as the response
///
/// This avoids client-side retries for schema compliance. The response content
/// is the raw JSON matching the schema, not wrapped in tool_use metadata.
pub(super) fn run_claude_api_with_structured_output(
    prompt: &str,
    settings: &settings::ClaudeApiSettings,
    model_override: Option<&str>,
    _doctor_handle: Option<&DoctorHandle>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
    schema: &Value,
    schema_name: &str,
) -> AiResponse {
    let model = model_override.unwrap_or(&settings.model);
    let max_tokens = max_tokens_override.unwrap_or(settings.max_tokens);

    info!(
        "Running Claude API with structured output (model: {}, schema: {})",
        model, schema_name
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

    // Build tool definition with the schema as input_schema.
    // The tool name is derived from the schema name, sanitized for Claude's requirements.
    let tool_name = schema_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();

    let tool_def = serde_json::json!({
        "name": tool_name,
        "description": format!("Generate output conforming to the {} schema", schema_name),
        "input_schema": schema
    });

    let mut request_body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [{"role": "user", "content": prompt}],
        "tools": [tool_def],
        "tool_choice": {"type": "tool", "name": tool_name}
    });

    if let Some(temp) = temperature_override {
        request_body["temperature"] = serde_json::json!(temp);
    }

    retry_with_backoff("Claude API (structured output)", || {
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
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        mark_current_account_rate_limited(resp.headers());
                    }
                    let body = resp.text().unwrap_or_default();
                    return AiResponse::error(format!(
                        "Claude API structured output error ({}): {}",
                        status, body
                    ));
                }

                match resp.json::<serde_json::Value>() {
                    Ok(json) => {
                        // Extract tool_use content block — the input field contains
                        // the schema-compliant JSON.
                        let content = extract_tool_use_input(&json, &tool_name);

                        let input_tokens = json["usage"]["input_tokens"].as_u64();
                        let output_tokens = json["usage"]["output_tokens"].as_u64();

                        if let (Some(input), Some(output)) = (input_tokens, output_tokens) {
                            debug!(
                                "Claude API structured output tokens - input: {}, output: {}",
                                input, output
                            );
                            AiResponse::success_with_tokens(content, input, output)
                        } else {
                            AiResponse::success(content)
                        }
                    }
                    Err(e) => AiResponse::error(format!(
                        "Failed to parse Claude API structured output response: {}",
                        e
                    )),
                }
            }
            Err(e) => AiResponse::error(format!(
                "Claude API structured output request failed: {}",
                e
            )),
        }
    })
}

/// Extract the tool input JSON from a Claude tool_use response.
///
/// Claude responds with content blocks; when tool_choice forces a tool, the
/// response contains a `tool_use` block whose `input` field is the
/// schema-conformant JSON object. We serialize it back to a JSON string.
fn extract_tool_use_input(response_json: &Value, tool_name: &str) -> String {
    if let Some(content_array) = response_json["content"].as_array() {
        for block in content_array {
            if block["type"].as_str() == Some("tool_use")
                && block["name"].as_str() == Some(tool_name)
            {
                // The "input" field is the parsed JSON matching the schema
                if let Some(input) = block.get("input") {
                    return serde_json::to_string(input).unwrap_or_default();
                }
            }
        }
        // Fallback: try to find any tool_use block
        for block in content_array {
            if block["type"].as_str() == Some("tool_use") {
                if let Some(input) = block.get("input") {
                    debug!(
                        "Structured output: tool name mismatch (expected '{}', got '{:?}'), using first tool_use block",
                        tool_name,
                        block["name"].as_str()
                    );
                    return serde_json::to_string(input).unwrap_or_default();
                }
            }
        }
        // Final fallback: extract text content (shouldn't happen with tool_choice forced)
        for block in content_array {
            if let Some(text) = block["text"].as_str() {
                debug!("Structured output: no tool_use block found, falling back to text content");
                return text.to_string();
            }
        }
    }
    String::new()
}
