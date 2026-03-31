//! Validation Feedback Generator
//!
//! Converts `ValidationResult` into LLM-consumable feedback prompts that
//! help the model self-correct its structured output on retry.

use super::validator::{ErrorKind, ValidationResult};

/// Generate an LLM-consumable feedback prompt from a validation result.
///
/// The generated prompt is designed to be appended to a retry conversation,
/// giving the LLM clear, actionable information about what went wrong and
/// how to fix it.
///
/// # Example Output
///
/// ```text
/// ## Schema Validation Errors (2 errors)
///
/// Your JSON output did not match the required schema "WorkerOutput".
///
/// 1. `/signals/0/type`: Invalid enum value. Expected one of: work_complete, need_replan, continue, request_context, record_store
/// 2. `/confidence`: Wrong type. Expected string, got integer
///
/// Please fix these errors and output valid JSON matching the schema.
/// ```
pub fn validation_feedback_prompt(result: &ValidationResult, schema_name: &str) -> String {
    if result.valid {
        return String::new();
    }

    let error_count = result.errors.len();
    let mut output = String::new();

    output.push_str(&format!(
        "## Schema Validation Errors ({} error{})\n\n",
        error_count,
        if error_count == 1 { "" } else { "s" }
    ));

    output.push_str(&format!(
        "Your JSON output did not match the required schema \"{}\".\n\n",
        schema_name
    ));

    for (i, error) in result.errors.iter().enumerate() {
        let path_display = if error.path.is_empty() {
            "(root)".to_string()
        } else {
            format!("`{}`", error.path)
        };

        let kind_label = match &error.error_kind {
            ErrorKind::MissingField => "Missing required field",
            ErrorKind::WrongType => "Wrong type",
            ErrorKind::InvalidEnum => "Invalid enum value",
            ErrorKind::PatternMismatch => "Pattern mismatch",
            ErrorKind::OutOfRange => "Value out of range",
            ErrorKind::ArrayLength => "Array length violation",
            ErrorKind::AdditionalProperty => "Unexpected property",
            ErrorKind::Other => "Validation error",
        };

        output.push_str(&format!(
            "{}. {}: **{}**. {}\n",
            i + 1,
            path_display,
            kind_label,
            error.message
        ));
    }

    output.push_str(
        "\nPlease fix these errors and output valid JSON matching the schema. \
         Do not include any text outside the JSON block.\n",
    );

    output
}

/// Generate a concise one-line error summary for logging.
pub fn validation_error_summary(result: &ValidationResult) -> String {
    if result.valid {
        return "valid".to_string();
    }

    let kinds: Vec<&str> = result
        .errors
        .iter()
        .map(|e| match &e.error_kind {
            ErrorKind::MissingField => "missing_field",
            ErrorKind::WrongType => "wrong_type",
            ErrorKind::InvalidEnum => "invalid_enum",
            ErrorKind::PatternMismatch => "pattern",
            ErrorKind::OutOfRange => "range",
            ErrorKind::ArrayLength => "array_len",
            ErrorKind::AdditionalProperty => "extra_prop",
            ErrorKind::Other => "other",
        })
        .collect();

    format!("{} errors: [{}]", result.errors.len(), kinds.join(", "))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::validator::ValidationError;

    fn make_error(path: &str, kind: ErrorKind, message: &str) -> ValidationError {
        ValidationError {
            path: path.to_string(),
            error_kind: kind,
            message: message.to_string(),
            expected: "string".to_string(),
            actual: "integer".to_string(),
        }
    }

    #[test]
    fn test_feedback_prompt_valid() {
        let result = ValidationResult::valid(serde_json::json!({}));
        let feedback = validation_feedback_prompt(&result, "Test");
        assert!(feedback.is_empty());
    }

    #[test]
    fn test_feedback_prompt_with_errors() {
        let result = ValidationResult::invalid(
            serde_json::json!({}),
            vec![
                make_error("/name", ErrorKind::MissingField, "Required property 'name' is missing"),
                make_error(
                    "/confidence",
                    ErrorKind::WrongType,
                    "Expected string, got integer",
                ),
            ],
        );

        let feedback = validation_feedback_prompt(&result, "WorkerOutput");
        assert!(feedback.contains("2 errors"));
        assert!(feedback.contains("WorkerOutput"));
        assert!(feedback.contains("`/name`"));
        assert!(feedback.contains("`/confidence`"));
        assert!(feedback.contains("Missing required field"));
        assert!(feedback.contains("Wrong type"));
    }

    #[test]
    fn test_error_summary() {
        let result = ValidationResult::invalid(
            serde_json::json!({}),
            vec![
                make_error("/a", ErrorKind::MissingField, "missing"),
                make_error("/b", ErrorKind::WrongType, "wrong"),
            ],
        );
        let summary = validation_error_summary(&result);
        assert!(summary.contains("2 errors"));
        assert!(summary.contains("missing_field"));
        assert!(summary.contains("wrong_type"));
    }
}
