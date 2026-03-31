//! Retry-with-Validation-Feedback (Instructor Pattern)
//!
//! Core logic for structured output validation with automatic retry.
//! When LLM output fails schema validation, builds feedback prompts
//! with validation errors to guide the LLM toward correct output.

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::debug;

use crate::ai_provider::AiResponse;
use crate::schema_registry;
use crate::validation::coercion::{apply_coercions, CoercionPolicy, CoercionRecord};
use crate::validation::validator::SchemaValidator;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for structured output with validation retry.
#[derive(Debug, Clone)]
pub struct StructuredOutputConfig {
    /// Maximum number of validation retries (default 2).
    pub max_validation_retries: u32,
    /// Coercion policy for minor type mismatches.
    pub coercion_policy: CoercionPolicy,
    /// Whether to include the full schema in retry prompts (1st retry only).
    pub include_schema_in_retry: bool,
    /// Whether to include an example in retry prompts (1st retry only).
    pub include_example_in_retry: bool,
}

impl Default for StructuredOutputConfig {
    fn default() -> Self {
        Self {
            max_validation_retries: 2,
            coercion_policy: CoercionPolicy::Lenient,
            include_schema_in_retry: true,
            include_example_in_retry: true,
        }
    }
}

// ============================================================================
// Result types
// ============================================================================

/// Result of a structured output call with validation.
/// Used by the full retry orchestration loop (Phase 6 meta-optimizer integration).
#[derive(Debug)]
#[allow(dead_code)]
pub struct StructuredOutputResult<T> {
    /// The validated and deserialized value.
    pub value: T,
    /// The raw JSON value.
    pub raw_json: Value,
    /// Number of validation attempts (1 = valid first try).
    pub validation_attempts: u32,
    /// Coercions that were applied.
    pub coercions_applied: Vec<CoercionRecord>,
    /// Total input tokens used across all attempts.
    pub total_input_tokens: u64,
    /// Total output tokens used across all attempts.
    pub total_output_tokens: u64,
}

/// Error from structured output call.
#[derive(Debug)]
pub struct StructuredOutputError {
    /// Error message.
    pub message: String,
    /// Number of attempts made.
    pub attempts: u32,
    /// Last validation errors (if validation failed).
    pub last_validation_errors: Vec<String>,
    /// Last raw output (if any).
    pub last_raw_output: Option<String>,
}

impl std::fmt::Display for StructuredOutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Structured output failed after {} attempts: {}",
            self.attempts, self.message
        )
    }
}

// ============================================================================
// JSON extraction
// ============================================================================

/// Extract a JSON block from LLM output text.
///
/// Looks for ```json ... ``` blocks first, then falls back to bare JSON objects.
pub fn extract_json_from_output(output: &str) -> Option<String> {
    // Try ```json block first
    if let Some(start) = output.find("```json") {
        let json_start = start + 7;
        if let Some(end) = output[json_start..].find("```") {
            return Some(output[json_start..json_start + end].trim().to_string());
        }
    }

    // Try bare JSON object
    if let Some(start) = output.find('{') {
        let mut depth = 0;
        let mut in_string = false;
        let mut escape = false;
        for (i, ch) in output[start..].char_indices() {
            if escape {
                escape = false;
                continue;
            }
            match ch {
                '\\' if in_string => escape = true,
                '"' => in_string = !in_string,
                '{' if !in_string => depth += 1,
                '}' if !in_string => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(output[start..start + i + 1].to_string());
                    }
                }
                _ => {}
            }
        }
    }

    None
}

// ============================================================================
// Validation and parsing
// ============================================================================

/// Validate and parse a JSON string into a typed value.
///
/// Attempts validation, then coercion if needed. Returns the parsed value
/// with coercion records on success, or validation error strings on failure.
pub fn validate_and_parse<T: JsonSchema + DeserializeOwned>(
    json_str: &str,
    config: &StructuredOutputConfig,
) -> Result<(T, Value, Vec<CoercionRecord>), Vec<String>> {
    let raw_value: Value =
        serde_json::from_str(json_str).map_err(|e| vec![format!("JSON parse error: {}", e)])?;

    let validator = SchemaValidator::for_type::<T>()
        .map_err(|e| vec![format!("Schema compilation error: {}", e)])?;

    // Validate raw value first
    let result = validator.validate(&raw_value);
    if result.valid {
        let value: T = serde_json::from_value(raw_value.clone())
            .map_err(|e| vec![format!("Deserialization error: {}", e)])?;
        return Ok((value, raw_value, Vec::new()));
    }

    // Try coercion before declaring failure
    let schema = schema_registry::schema_as_json::<T>();
    let (coerced_value, coercion_records) =
        apply_coercions(&raw_value, &schema, config.coercion_policy);

    if !coercion_records.is_empty() {
        debug!(
            "Applied {} coercions, re-validating",
            coercion_records.len()
        );
        let coerced_result = validator.validate(&coerced_value);
        if coerced_result.valid {
            let value: T = serde_json::from_value(coerced_value.clone())
                .map_err(|e| vec![format!("Deserialization error after coercion: {}", e)])?;
            return Ok((value, coerced_value, coercion_records));
        }
        return Err(coerced_result
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect());
    }

    Err(result.errors.iter().map(|e| e.to_string()).collect())
}

// ============================================================================
// Retry prompt building
// ============================================================================

/// Build a retry prompt with validation feedback.
///
/// On the first retry, includes the full schema and error details.
/// On subsequent retries, includes only error details to avoid prompt bloat.
pub fn build_validation_retry_prompt<T: JsonSchema>(
    original_prompt: &str,
    validation_errors: &[String],
    attempt: u32,
    config: &StructuredOutputConfig,
) -> String {
    let mut retry_prompt = original_prompt.to_string();

    // Add schema on first retry only
    if attempt == 1 && config.include_schema_in_retry {
        retry_prompt.push_str("\n\n");
        retry_prompt.push_str(&schema_registry::prompt_instructions_for::<T>());
    }

    // Add validation error feedback
    let schema_name = T::schema_name();
    retry_prompt.push_str(&format!(
        "\n\n## Schema Validation Errors ({} error{})\n\n",
        validation_errors.len(),
        if validation_errors.len() == 1 { "" } else { "s" }
    ));
    retry_prompt.push_str(&format!(
        "Your previous JSON output did not match the required schema \"{}\".\n\n",
        schema_name
    ));
    for (i, error) in validation_errors.iter().enumerate() {
        retry_prompt.push_str(&format!("{}. {}\n", i + 1, error));
    }
    retry_prompt.push_str(
        "\nPlease fix these errors and output valid JSON matching the schema. \
         Do not include any text outside the JSON block.\n",
    );

    retry_prompt
}

// ============================================================================
// Response processing
// ============================================================================

/// Process an AI response to extract and validate structured output.
///
/// This is the core function that other code should call. It:
/// 1. Extracts JSON from the AI response
/// 2. Validates against the schema
/// 3. Applies coercion if needed
/// 4. Returns the validated value or a structured error for retry
pub fn process_ai_response_for_structured_output<T: JsonSchema + DeserializeOwned>(
    response: &AiResponse,
    config: &StructuredOutputConfig,
) -> Result<(T, Value, Vec<CoercionRecord>), StructuredOutputError> {
    if !response.success {
        return Err(StructuredOutputError {
            message: response
                .error
                .clone()
                .unwrap_or_else(|| "AI call failed".to_string()),
            attempts: 0,
            last_validation_errors: Vec::new(),
            last_raw_output: None,
        });
    }

    let json_str = extract_json_from_output(&response.output).ok_or_else(|| {
        StructuredOutputError {
            message: "No JSON block found in AI response".to_string(),
            attempts: 1,
            last_validation_errors: Vec::new(),
            last_raw_output: Some(response.output.clone()),
        }
    })?;

    validate_and_parse::<T>(&json_str, config).map_err(|errors| StructuredOutputError {
        message: format!("Schema validation failed with {} errors", errors.len()),
        attempts: 1,
        last_validation_errors: errors,
        last_raw_output: Some(response.output.clone()),
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    struct TestSchema {
        pub name: String,
        #[serde(default)]
        pub count: u32,
    }

    #[test]
    fn test_extract_json_from_code_block() {
        let output = "Here is the result:\n```json\n{\"name\": \"test\", \"count\": 5}\n```\nDone.";
        let json = extract_json_from_output(output).unwrap();
        assert_eq!(json, "{\"name\": \"test\", \"count\": 5}");
    }

    #[test]
    fn test_extract_json_bare_object() {
        let output = "The answer is {\"name\": \"hello\", \"count\": 1} end.";
        let json = extract_json_from_output(output).unwrap();
        assert_eq!(json, "{\"name\": \"hello\", \"count\": 1}");
    }

    #[test]
    fn test_extract_json_nested_braces() {
        let output = "{\"name\": \"test\", \"nested\": {\"a\": 1}}";
        let json = extract_json_from_output(output).unwrap();
        assert_eq!(json, "{\"name\": \"test\", \"nested\": {\"a\": 1}}");
    }

    #[test]
    fn test_extract_json_no_json() {
        let output = "No JSON here, just text.";
        assert!(extract_json_from_output(output).is_none());
    }

    #[test]
    fn test_extract_json_braces_in_strings() {
        let output = "{\"name\": \"hello {world}\", \"count\": 1}";
        let json = extract_json_from_output(output).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["name"], "hello {world}");
    }

    #[test]
    fn test_validate_and_parse_valid() {
        let config = StructuredOutputConfig::default();
        let json = r#"{"name": "test", "count": 5}"#;
        let result = validate_and_parse::<TestSchema>(json, &config);
        assert!(result.is_ok());
        let (value, _, coercions) = result.unwrap();
        assert_eq!(value.name, "test");
        assert_eq!(value.count, 5);
        assert!(coercions.is_empty());
    }

    #[test]
    fn test_validate_and_parse_with_coercion() {
        let config = StructuredOutputConfig::default();
        let json = r#"{"name": "test", "count": "5"}"#;
        let result = validate_and_parse::<TestSchema>(json, &config);
        assert!(result.is_ok());
        let (value, _, coercions) = result.unwrap();
        assert_eq!(value.name, "test");
        assert_eq!(value.count, 5);
        assert_eq!(coercions.len(), 1);
    }

    #[test]
    fn test_validate_and_parse_invalid() {
        let config = StructuredOutputConfig::default();
        let json = r#"{"count": 5}"#; // missing required "name"
        let result = validate_and_parse::<TestSchema>(json, &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_and_parse_bad_json() {
        let config = StructuredOutputConfig::default();
        let json = "not json at all";
        let result = validate_and_parse::<TestSchema>(json, &config);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors[0].contains("JSON parse error"));
    }

    #[test]
    fn test_build_validation_retry_prompt_first_attempt() {
        let config = StructuredOutputConfig::default();
        let errors = vec!["Missing required field: name".to_string()];
        let prompt =
            build_validation_retry_prompt::<TestSchema>("Do the thing", &errors, 1, &config);

        assert!(prompt.contains("Do the thing"));
        assert!(prompt.contains("Output Format: TestSchema"));
        assert!(prompt.contains("Missing required field: name"));
        assert!(prompt.contains("Schema Validation Errors (1 error)"));
    }

    #[test]
    fn test_build_validation_retry_prompt_second_attempt() {
        let config = StructuredOutputConfig::default();
        let errors = vec!["Wrong type at /count".to_string()];
        let prompt =
            build_validation_retry_prompt::<TestSchema>("Do the thing", &errors, 2, &config);

        // Second attempt should NOT include the schema (to avoid prompt bloat)
        assert!(prompt.contains("Do the thing"));
        assert!(!prompt.contains("Output Format: TestSchema"));
        assert!(prompt.contains("Wrong type at /count"));
    }

    #[test]
    fn test_process_ai_response_success() {
        let config = StructuredOutputConfig::default();
        let response = AiResponse::success(
            "```json\n{\"name\": \"test\", \"count\": 5}\n```".to_string(),
        );
        let result = process_ai_response_for_structured_output::<TestSchema>(&response, &config);
        assert!(result.is_ok());
        let (value, _, _) = result.unwrap();
        assert_eq!(value.name, "test");
    }

    #[test]
    fn test_process_ai_response_no_json() {
        let config = StructuredOutputConfig::default();
        let response = AiResponse::success("Just some text, no JSON".to_string());
        let result = process_ai_response_for_structured_output::<TestSchema>(&response, &config);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("No JSON block"));
    }

    #[test]
    fn test_process_ai_response_failed_call() {
        let config = StructuredOutputConfig::default();
        let response = AiResponse::error("API timeout".to_string());
        let result = process_ai_response_for_structured_output::<TestSchema>(&response, &config);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("API timeout"));
        assert_eq!(err.attempts, 0);
    }

    #[test]
    fn test_process_ai_response_invalid_schema() {
        let config = StructuredOutputConfig::default();
        let response = AiResponse::success("{\"count\": 5}".to_string());
        let result = process_ai_response_for_structured_output::<TestSchema>(&response, &config);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(!err.last_validation_errors.is_empty());
    }

    #[test]
    fn test_structured_output_error_display() {
        let err = StructuredOutputError {
            message: "bad schema".to_string(),
            attempts: 3,
            last_validation_errors: vec![],
            last_raw_output: None,
        };
        let display = format!("{}", err);
        assert!(display.contains("3 attempts"));
        assert!(display.contains("bad schema"));
    }
}
