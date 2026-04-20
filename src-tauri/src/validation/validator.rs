//! Core Schema Validator
//!
//! Wraps the `jsonschema` crate to provide structured validation results
//! with human-readable and LLM-consumable error messages.

use jsonschema::Validator;
use schemars::schema_for;
use schemars::JsonSchema;
use serde_json::Value;
use tracing::debug;

// ============================================================================
// Validation Result
// ============================================================================

/// Result of validating a JSON value against a schema.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// Whether the value is valid against the schema.
    pub valid: bool,
    /// Structured validation errors (empty if valid).
    pub errors: Vec<ValidationError>,
    /// The original value that was validated.
    pub raw_value: Value,
}

impl ValidationResult {
    /// Create a successful validation result.
    pub fn valid(value: Value) -> Self {
        Self {
            valid: true,
            errors: Vec::new(),
            raw_value: value,
        }
    }

    /// Create a failed validation result.
    pub fn invalid(value: Value, errors: Vec<ValidationError>) -> Self {
        Self {
            valid: false,
            errors,
            raw_value: value,
        }
    }

    /// Get a human-readable error summary.
    pub fn error_summary(&self) -> String {
        if self.valid {
            return "Valid".to_string();
        }
        self.errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ============================================================================
// Validation Error
// ============================================================================

/// A structured validation error with path, kind, and contextual information.
#[derive(Debug, Clone)]
pub struct ValidationError {
    /// JSON Pointer path to the invalid value (RFC 6901).
    /// e.g., "/signals/0/type" or "/confidence"
    pub path: String,
    /// The kind of validation error.
    pub error_kind: ErrorKind,
    /// Human-readable error message.
    pub message: String,
    /// What the schema expected.
    pub expected: String,
    /// What was actually found.
    pub actual: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} (expected {}, got {})",
            self.path, self.message, self.expected, self.actual
        )
    }
}

/// Types of validation errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// A required field is missing.
    MissingField,
    /// The value has the wrong type.
    WrongType,
    /// The value is not a valid enum variant.
    InvalidEnum,
    /// A string doesn't match the expected pattern.
    PatternMismatch,
    /// A numeric value is out of range.
    OutOfRange,
    /// An array has too many or too few items.
    ArrayLength,
    /// Additional properties not allowed.
    AdditionalProperty,
    /// Other validation error.
    Other,
}

// ============================================================================
// Schema Validator
// ============================================================================

/// A compiled schema validator that can be reused across multiple validations.
///
/// Wraps `jsonschema::Validator` which compiles the schema once for efficient
/// repeated validation. Thread-safe and immutable after construction.
pub struct SchemaValidator {
    compiled: Validator,
    schema_name: String,
}

impl SchemaValidator {
    /// Create a validator for a Rust type that implements `JsonSchema`.
    ///
    /// The schema is generated from the type's `JsonSchema` implementation
    /// and compiled once.
    pub fn for_type<T: JsonSchema>() -> Result<Self, String> {
        let schema = schema_for!(T);
        let schema_value = serde_json::to_value(&schema)
            .map_err(|e| format!("Schema serialization error: {}", e))?;
        let compiled = Validator::new(&schema_value)
            .map_err(|e| format!("Schema compilation error: {}", e))?;

        Ok(Self {
            compiled,
            schema_name: T::schema_name().to_string(),
        })
    }

    /// Create a validator from a raw JSON Schema value.
    pub fn from_schema(schema: &Value, name: impl Into<String>) -> Result<Self, String> {
        let compiled =
            Validator::new(schema).map_err(|e| format!("Schema compilation error: {}", e))?;
        Ok(Self {
            compiled,
            schema_name: name.into(),
        })
    }

    /// Get the schema name.
    pub fn schema_name(&self) -> &str {
        &self.schema_name
    }

    /// Validate a JSON value against the compiled schema.
    ///
    /// Returns a `ValidationResult` with structured errors if validation fails.
    pub fn validate(&self, value: &Value) -> ValidationResult {
        // NOTE: We use `iter_errors` rather than `evaluate().iter_errors()` because
        // the annotation-based `evaluate` API in jsonschema 0.45 does not report
        // structural errors like missing `required` properties — only schema-violation
        // annotations. `iter_errors` reports all validation errors faithfully.
        let errors: Vec<ValidationError> = self
            .compiled
            .iter_errors(value)
            .map(|err| {
                let loc = err.instance_path().to_string();
                let path = if loc.is_empty() {
                    String::new()
                } else if loc.starts_with('/') {
                    loc
                } else {
                    format!("/{}", loc)
                };

                let desc = err.to_string();
                let (error_kind, expected, actual) = classify_error_desc(&desc);

                ValidationError {
                    path,
                    message: desc,
                    error_kind,
                    expected,
                    actual,
                }
            })
            .collect();

        if errors.is_empty() {
            debug!("Schema validation passed for {}", self.schema_name);
            ValidationResult::valid(value.clone())
        } else {
            debug!(
                "Schema validation failed for {} with {} errors",
                self.schema_name,
                errors.len()
            );
            ValidationResult::invalid(value.clone(), errors)
        }
    }

    /// Quick check whether a value is valid (no error details).
    pub fn is_valid(&self, value: &Value) -> bool {
        self.compiled.is_valid(value)
    }
}

// ============================================================================
// Error Classification
// ============================================================================

/// Classify a validation error description into our structured error types.
fn classify_error_desc(desc: &str) -> (ErrorKind, String, String) {
    let desc_lower = desc.to_lowercase();

    let error_kind = if desc_lower.contains("required") {
        ErrorKind::MissingField
    } else if desc_lower.contains("type") && desc_lower.contains("expected") {
        ErrorKind::WrongType
    } else if desc_lower.contains("enum") || desc_lower.contains("one of") {
        ErrorKind::InvalidEnum
    } else if desc_lower.contains("pattern") {
        ErrorKind::PatternMismatch
    } else if desc_lower.contains("minimum") || desc_lower.contains("maximum") {
        ErrorKind::OutOfRange
    } else if desc_lower.contains("items") || desc_lower.contains("length") {
        ErrorKind::ArrayLength
    } else if desc_lower.contains("additional") {
        ErrorKind::AdditionalProperty
    } else {
        ErrorKind::Other
    };

    // Extract expected/actual from the description
    let expected = extract_expected(desc);
    let actual = extract_actual(desc);

    (error_kind, expected, actual)
}

/// Extract "expected" information from an error description.
fn extract_expected(desc: &str) -> String {
    // Common patterns: "expected type X", "one of [...]", "required property 'X'"
    if desc.contains("expected") {
        desc.split("expected")
            .nth(1)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string())
    } else {
        desc.to_string()
    }
}

/// Extract "actual" information from an error description.
fn extract_actual(desc: &str) -> String {
    if desc.contains("got") {
        desc.split("got")
            .nth(1)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string())
    } else if desc.contains("found") {
        desc.split("found")
            .nth(1)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string())
    } else {
        "see error message".to_string()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    struct SimpleSchema {
        /// A required name field.
        pub name: String,
        /// An optional count.
        #[serde(default)]
        pub count: u32,
    }

    #[test]
    fn test_valid_input() {
        let validator = SchemaValidator::for_type::<SimpleSchema>().unwrap();
        let value = serde_json::json!({
            "name": "test",
            "count": 5
        });
        let result = validator.validate(&value);
        assert!(result.valid);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_missing_required_field() {
        let validator = SchemaValidator::for_type::<SimpleSchema>().unwrap();
        let value = serde_json::json!({
            "count": 5
        });
        let result = validator.validate(&value);
        assert!(!result.valid);
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn test_wrong_type() {
        let validator = SchemaValidator::for_type::<SimpleSchema>().unwrap();
        let value = serde_json::json!({
            "name": 123,
            "count": 5
        });
        let result = validator.validate(&value);
        assert!(!result.valid);
    }

    #[test]
    fn test_is_valid() {
        let validator = SchemaValidator::for_type::<SimpleSchema>().unwrap();
        assert!(validator.is_valid(&serde_json::json!({"name": "test"})));
        assert!(!validator.is_valid(&serde_json::json!({"count": 5})));
    }

    #[test]
    fn test_error_summary() {
        let validator = SchemaValidator::for_type::<SimpleSchema>().unwrap();
        let result = validator.validate(&serde_json::json!({}));
        assert!(!result.valid);
        let summary = result.error_summary();
        assert!(!summary.is_empty());
    }
}
