#![allow(dead_code)]

use super::multimodal::MultimodalPrompt;
use super::retry::retry_with_backoff;
use super::types::AiResponse;
use crate::config_facade::ai_keychain;
use crate::doctor::DoctorHandle;
use crate::settings;
use serde_json::Value;
use tracing::{debug, info, warn};

/// Run a prompt via Gemini API (direct HTTP calls)
///
/// Gemini API responses include usage metadata:
/// ```json
/// {
///   "usageMetadata": {
///     "promptTokenCount": 123,
///     "candidatesTokenCount": 456,
///     "totalTokenCount": 579
///   }
/// }
/// ```
pub(super) fn run_gemini_api(
    prompt: &str,
    settings: &settings::GeminiApiSettings,
    model_override: Option<&str>,
    _doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    let model = model_override.unwrap_or(&settings.model);
    info!(
        "Running Gemini API (model: {}, temp: {}, override: {})",
        model,
        settings.temperature,
        model_override.is_some()
    );

    // Get API key from keychain using KeychainHelper
    let api_key = match ai_keychain().get("gemini_api") {
        Ok(Some(key)) => key,
        Ok(None) => {
            return AiResponse::error(
                "No Gemini API key configured. Please set your API key in Settings.".to_string(),
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

    // Gemini API endpoint
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        model, api_key
    );

    // Build the request body once (it's the same for every retry attempt)
    let request_body = serde_json::json!({
        "contents": [{"parts": [{"text": prompt}]}],
        "generationConfig": {
            "temperature": settings.temperature,
            "maxOutputTokens": settings.max_output_tokens
        }
    });

    retry_with_backoff("Gemini API", || {
        let response = client
            .post(&url)
            .header("content-type", "application/json")
            .json(&request_body)
            .send();

        match response {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().unwrap_or_default();
                    return AiResponse::error(format!("Gemini API error ({}): {}", status, body));
                }

                match resp.json::<serde_json::Value>() {
                    Ok(json) => {
                        // Extract text content from response
                        let content = json["candidates"]
                            .as_array()
                            .and_then(|arr| arr.first())
                            .and_then(|c| c["content"]["parts"].as_array())
                            .and_then(|parts| parts.first())
                            .and_then(|p| p["text"].as_str())
                            .unwrap_or("")
                            .to_string();

                        // Extract token usage from response
                        // Gemini API format: {"usageMetadata": {"promptTokenCount": N, "candidatesTokenCount": N}}
                        let input_tokens = json["usageMetadata"]["promptTokenCount"].as_u64();
                        let output_tokens = json["usageMetadata"]["candidatesTokenCount"].as_u64();

                        if let (Some(input), Some(output)) = (input_tokens, output_tokens) {
                            debug!(
                                "Gemini API tokens - input: {}, output: {}, total: {}",
                                input,
                                output,
                                input + output
                            );
                            AiResponse::success_with_tokens(content, input, output)
                        } else {
                            debug!(
                                "Gemini API response missing token counts: usageMetadata={:?}",
                                json["usageMetadata"]
                            );
                            AiResponse::success(content)
                        }
                    }
                    Err(e) => AiResponse::error(format!("Failed to parse API response: {}", e)),
                }
            }
            Err(e) => AiResponse::error(format!("Gemini API request failed: {}", e)),
        }
    })
}

/// Run Gemini API with optional temperature and max_tokens overrides.
pub(super) fn run_gemini_api_with_overrides(
    prompt: &str,
    settings: &settings::GeminiApiSettings,
    model_override: Option<&str>,
    _doctor_handle: Option<&DoctorHandle>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
) -> AiResponse {
    if temperature_override.is_none() && max_tokens_override.is_none() {
        return run_gemini_api(prompt, settings, model_override, _doctor_handle);
    }

    let model = model_override.unwrap_or(&settings.model);
    let temperature = temperature_override.unwrap_or(settings.temperature);
    let max_output_tokens = max_tokens_override.unwrap_or(settings.max_output_tokens);

    info!(
        "Running Gemini API with overrides (model: {}, temp: {}, max_tokens: {})",
        model, temperature, max_output_tokens
    );

    let api_key = match ai_keychain().get("gemini_api") {
        Ok(Some(key)) => key,
        Ok(None) => {
            return AiResponse::error(
                "No Gemini API key configured. Please set your API key in Settings.".to_string(),
            )
        }
        Err(e) => return AiResponse::error(format!("Failed to retrieve API key: {}", e)),
    };

    let client = match reqwest::blocking::Client::builder().build() {
        Ok(c) => c,
        Err(e) => return AiResponse::error(format!("Failed to create HTTP client: {}", e)),
    };

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        model, api_key
    );

    let request_body = serde_json::json!({
        "contents": [{"parts": [{"text": prompt}]}],
        "generationConfig": {
            "temperature": temperature,
            "maxOutputTokens": max_output_tokens
        }
    });

    retry_with_backoff("Gemini API (overrides)", || {
        let response = client
            .post(&url)
            .header("content-type", "application/json")
            .json(&request_body)
            .send();

        match response {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().unwrap_or_default();
                    return AiResponse::error(format!("Gemini API error ({}): {}", status, body));
                }

                match resp.json::<serde_json::Value>() {
                    Ok(json) => {
                        let content = json["candidates"]
                            .as_array()
                            .and_then(|arr| arr.first())
                            .and_then(|c| c["content"]["parts"].as_array())
                            .and_then(|parts| parts.first())
                            .and_then(|p| p["text"].as_str())
                            .unwrap_or("")
                            .to_string();

                        let input_tokens = json["usageMetadata"]["promptTokenCount"].as_u64();
                        let output_tokens = json["usageMetadata"]["candidatesTokenCount"].as_u64();

                        if let (Some(input), Some(output)) = (input_tokens, output_tokens) {
                            AiResponse::success_with_tokens(content, input, output)
                        } else {
                            AiResponse::success(content)
                        }
                    }
                    Err(e) => AiResponse::error(format!("Failed to parse API response: {}", e)),
                }
            }
            Err(e) => AiResponse::error(format!("Gemini API request failed: {}", e)),
        }
    })
}

/// Run a multimodal prompt via Gemini API with text + image content.
///
/// Gemini multimodal format:
/// ```json
/// {
///   "contents": [{
///     "parts": [
///       {"text": "..."},
///       {"inline_data": {"mime_type": "image/png", "data": "base64..."}}
///     ]
///   }]
/// }
/// ```
pub(super) fn run_gemini_api_multimodal(
    prompt: &MultimodalPrompt,
    settings: &settings::GeminiApiSettings,
    model_override: Option<&str>,
    _doctor_handle: Option<&DoctorHandle>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
) -> AiResponse {
    let model = model_override.unwrap_or(&settings.model);
    let temperature = temperature_override.unwrap_or(settings.temperature);
    let max_output_tokens = max_tokens_override.unwrap_or(settings.max_output_tokens);

    info!(
        "Running Gemini API multimodal (model: {}, has_images: {}, blocks: {})",
        model,
        prompt.has_images(),
        prompt.content.len()
    );

    let api_key = match ai_keychain().get("gemini_api") {
        Ok(Some(key)) => key,
        Ok(None) => {
            return AiResponse::error(
                "No Gemini API key configured. Please set your API key in Settings.".to_string(),
            )
        }
        Err(e) => return AiResponse::error(format!("Failed to retrieve API key: {}", e)),
    };

    let client = match reqwest::blocking::Client::builder().build() {
        Ok(c) => c,
        Err(e) => return AiResponse::error(format!("Failed to create HTTP client: {}", e)),
    };

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        model, api_key
    );

    // Build request body with optional system instruction
    let mut request_body = serde_json::json!({
        "contents": prompt.to_gemini_contents(),
        "generationConfig": {
            "temperature": temperature,
            "maxOutputTokens": max_output_tokens
        }
    });

    // Include system prompt via Gemini's systemInstruction field
    if let Some(system_instruction) = prompt.to_gemini_system_instruction() {
        request_body["systemInstruction"] = system_instruction;
    }

    retry_with_backoff("Gemini API (multimodal)", || {
        let response = client
            .post(&url)
            .header("content-type", "application/json")
            .json(&request_body)
            .send();

        match response {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().unwrap_or_default();
                    return AiResponse::error(format!(
                        "Gemini API multimodal error ({}): {}",
                        status, body
                    ));
                }

                match resp.json::<serde_json::Value>() {
                    Ok(json) => {
                        let content = json["candidates"]
                            .as_array()
                            .and_then(|arr| arr.first())
                            .and_then(|c| c["content"]["parts"].as_array())
                            .and_then(|parts| parts.first())
                            .and_then(|p| p["text"].as_str())
                            .unwrap_or("")
                            .to_string();

                        let input_tokens = json["usageMetadata"]["promptTokenCount"].as_u64();
                        let output_tokens = json["usageMetadata"]["candidatesTokenCount"].as_u64();

                        if let (Some(input), Some(output)) = (input_tokens, output_tokens) {
                            debug!(
                                "Gemini API multimodal tokens - input: {}, output: {}, total: {}",
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
                        "Failed to parse Gemini API multimodal response: {}",
                        e
                    )),
                }
            }
            Err(e) => AiResponse::error(format!("Gemini API multimodal request failed: {}", e)),
        }
    })
}

/// Run Gemini API with server-side structured output enforcement.
///
/// Uses Gemini's native structured output support:
/// - `generationConfig.responseMimeType: "application/json"` to force JSON output
/// - `generationConfig.responseSchema: <schema>` to enforce the JSON Schema
///
/// This is simpler than Claude's tool_use approach since Gemini supports
/// JSON Schema directly in the generation config.
pub(super) fn run_gemini_api_with_structured_output(
    prompt: &str,
    settings: &settings::GeminiApiSettings,
    model_override: Option<&str>,
    _doctor_handle: Option<&DoctorHandle>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
    schema: &Value,
    schema_name: &str,
) -> AiResponse {
    let model = model_override.unwrap_or(&settings.model);
    let temperature = temperature_override.unwrap_or(settings.temperature);
    let max_output_tokens = max_tokens_override.unwrap_or(settings.max_output_tokens);

    info!(
        "Running Gemini API with structured output (model: {}, schema: {})",
        model, schema_name
    );

    let api_key = match ai_keychain().get("gemini_api") {
        Ok(Some(key)) => key,
        Ok(None) => {
            return AiResponse::error(
                "No Gemini API key configured. Please set your API key in Settings.".to_string(),
            )
        }
        Err(e) => return AiResponse::error(format!("Failed to retrieve API key: {}", e)),
    };

    let client = match reqwest::blocking::Client::builder().build() {
        Ok(c) => c,
        Err(e) => return AiResponse::error(format!("Failed to create HTTP client: {}", e)),
    };

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        model, api_key
    );

    // Build request with responseSchema in generationConfig.
    // Gemini expects the schema without $defs/$ref — strip schemars metadata
    // that Gemini doesn't understand.
    let clean_schema = strip_schemars_metadata(schema);

    let request_body = serde_json::json!({
        "contents": [{"parts": [{"text": prompt}]}],
        "generationConfig": {
            "temperature": temperature,
            "maxOutputTokens": max_output_tokens,
            "responseMimeType": "application/json",
            "responseSchema": clean_schema
        }
    });

    retry_with_backoff("Gemini API (structured output)", || {
        let response = client
            .post(&url)
            .header("content-type", "application/json")
            .json(&request_body)
            .send();

        match response {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().unwrap_or_default();
                    return AiResponse::error(format!(
                        "Gemini API structured output error ({}): {}",
                        status, body
                    ));
                }

                match resp.json::<serde_json::Value>() {
                    Ok(json) => {
                        let content = json["candidates"]
                            .as_array()
                            .and_then(|arr| arr.first())
                            .and_then(|c| c["content"]["parts"].as_array())
                            .and_then(|parts| parts.first())
                            .and_then(|p| p["text"].as_str())
                            .unwrap_or("")
                            .to_string();

                        let input_tokens = json["usageMetadata"]["promptTokenCount"].as_u64();
                        let output_tokens = json["usageMetadata"]["candidatesTokenCount"].as_u64();

                        if let (Some(input), Some(output)) = (input_tokens, output_tokens) {
                            debug!(
                                "Gemini API structured output tokens - input: {}, output: {}",
                                input, output
                            );
                            AiResponse::success_with_tokens(content, input, output)
                        } else {
                            AiResponse::success(content)
                        }
                    }
                    Err(e) => AiResponse::error(format!(
                        "Failed to parse Gemini API structured output response: {}",
                        e
                    )),
                }
            }
            Err(e) => AiResponse::error(format!(
                "Gemini API structured output request failed: {}",
                e
            )),
        }
    })
}

/// Strip schemars-specific metadata that Gemini's responseSchema doesn't support.
///
/// Gemini requires a clean JSON Schema without `$defs`, `$ref`, `$schema`,
/// `title`, or `description` at the property level. This function inlines
/// `$ref` references and removes unsupported fields.
fn strip_schemars_metadata(schema: &Value) -> Value {
    let defs = schema
        .get("$defs")
        .cloned()
        .or_else(|| schema.get("definitions").cloned());
    clean_schema_node(schema, &defs)
}

/// Recursively clean a schema node for Gemini compatibility.
fn clean_schema_node(node: &Value, defs: &Option<Value>) -> Value {
    // Resolve $ref first
    if let Some(ref_path) = node.get("$ref").and_then(|v| v.as_str()) {
        if let Some(resolved) = resolve_ref_from_defs(ref_path, defs) {
            return clean_schema_node(&resolved, defs);
        }
    }

    match node {
        Value::Object(map) => {
            let mut cleaned = serde_json::Map::new();
            for (key, value) in map {
                // Skip schemars-specific keys that Gemini doesn't support
                match key.as_str() {
                    "$defs" | "definitions" | "$schema" | "$ref" | "title" => continue,
                    "properties" => {
                        if let Value::Object(props) = value {
                            let mut clean_props = serde_json::Map::new();
                            for (pk, pv) in props {
                                clean_props.insert(pk.clone(), clean_schema_node(pv, defs));
                            }
                            cleaned.insert(key.clone(), Value::Object(clean_props));
                        }
                    }
                    "items" => {
                        cleaned.insert(key.clone(), clean_schema_node(value, defs));
                    }
                    "oneOf" | "anyOf" | "allOf" => {
                        if let Value::Array(arr) = value {
                            let clean_arr: Vec<Value> =
                                arr.iter().map(|v| clean_schema_node(v, defs)).collect();
                            cleaned.insert(key.clone(), Value::Array(clean_arr));
                        }
                    }
                    _ => {
                        cleaned.insert(key.clone(), value.clone());
                    }
                }
            }
            Value::Object(cleaned)
        }
        other => other.clone(),
    }
}

/// Resolve a `$ref` path against `$defs`/`definitions`.
fn resolve_ref_from_defs(ref_path: &str, defs: &Option<Value>) -> Option<Value> {
    let defs = defs.as_ref()?;
    // Handle "#/$defs/TypeName" or "#/definitions/TypeName"
    let type_name = ref_path
        .strip_prefix("#/$defs/")
        .or_else(|| ref_path.strip_prefix("#/definitions/"))?;
    defs.get(type_name).cloned()
}
