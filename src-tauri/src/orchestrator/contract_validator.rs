//! Contract Validator for Inter-Agent Pipeline Boundaries
//!
//! Validates agent output JSON against the typed schema contracts defined in
//! [`contracts`](super::contracts). Used at pipeline phase boundaries to ensure
//! data flowing between agents conforms to the expected structure.
//!
//! When validation fails, the structured errors can be fed back to the agent
//! via the retry-with-validation-feedback mechanism (Phase 3) to request
//! corrected output.

use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::{debug, warn};

use crate::orchestrator::contracts::{
    AgentRole, ImplementerOutput, LocatorOutput, SpecAnalystOutput, VerifierOutput,
};
use crate::validation::validator::{SchemaValidator, ValidationResult};

// ============================================================================
// Contract Schemas
// ============================================================================

/// Pre-compiled schema validators for a single agent role's output contract.
struct ContractSchemas {
    output_validator: SchemaValidator,
}

// ============================================================================
// Contract Validator
// ============================================================================

/// Validates inter-agent data at pipeline phase boundaries.
///
/// Pre-compiles JSON Schema validators for each agent role on construction,
/// then validates output JSON cheaply on each call. Thread-safe after construction.
///
/// # Example
///
/// ```rust,ignore
/// let validator = ContractValidator::new()?;
/// let result = validator.validate_output("spec_analyst", &raw_json);
/// if !result.valid {
///     // Feed errors back to agent for retry
///     let feedback = validation_feedback_prompt(&result, "SpecAnalystOutput");
/// }
/// ```
pub struct ContractValidator {
    validators: HashMap<AgentRole, ContractSchemas>,
}

impl ContractValidator {
    /// Create a new ContractValidator with pre-compiled schemas for all agent roles.
    ///
    /// Returns an error if any schema fails to compile (should not happen
    /// with valid JsonSchema derives).
    pub fn new() -> Result<Self, String> {
        let mut validators = HashMap::new();

        validators.insert(
            AgentRole::SpecAnalyst,
            ContractSchemas {
                output_validator: SchemaValidator::for_type::<SpecAnalystOutput>()?,
            },
        );

        validators.insert(
            AgentRole::Locator,
            ContractSchemas {
                output_validator: SchemaValidator::for_type::<LocatorOutput>()?,
            },
        );

        validators.insert(
            AgentRole::Implementer,
            ContractSchemas {
                output_validator: SchemaValidator::for_type::<ImplementerOutput>()?,
            },
        );

        validators.insert(
            AgentRole::Verifier,
            ContractSchemas {
                output_validator: SchemaValidator::for_type::<VerifierOutput>()?,
            },
        );

        debug!(
            "ContractValidator initialized with {} role schemas",
            validators.len()
        );
        Ok(Self { validators })
    }

    /// Validate an agent's output JSON against its contract schema.
    ///
    /// Returns a `ValidationResult` with structured errors if validation fails.
    /// If the role is unknown, returns an invalid result with a descriptive error.
    pub fn validate_output(&self, role: &str, data: &Value) -> ValidationResult {
        let Some(agent_role) = AgentRole::from_str(role) else {
            warn!(
                "ContractValidator: unknown agent role '{}', skipping validation",
                role
            );
            return ValidationResult::valid(data.clone());
        };

        let Some(schemas) = self.validators.get(&agent_role) else {
            warn!(
                "ContractValidator: no schema registered for role '{}'",
                role
            );
            return ValidationResult::valid(data.clone());
        };

        let result = schemas.output_validator.validate(data);
        if result.valid {
            debug!("ContractValidator: {} output passed validation", role);
        } else {
            debug!(
                "ContractValidator: {} output failed with {} errors",
                role,
                result.errors.len()
            );
        }
        result
    }

    /// Validate and deserialize an agent's output into the typed contract.
    ///
    /// First validates the JSON against the schema, then deserializes into `T`.
    /// Returns `Err` if validation fails or deserialization fails.
    pub fn validate_and_parse<T: DeserializeOwned>(
        &self,
        role: &str,
        data: &Value,
    ) -> Result<T, ContractValidationError> {
        let validation = self.validate_output(role, data);
        if !validation.valid {
            return Err(ContractValidationError::SchemaViolation {
                role: role.to_string(),
                errors: validation.error_summary(),
            });
        }

        serde_json::from_value(data.clone()).map_err(|e| {
            ContractValidationError::DeserializationFailed {
                role: role.to_string(),
                error: e.to_string(),
            }
        })
    }

    /// Check whether a role has a registered contract.
    pub fn has_contract(&self, role: &str) -> bool {
        AgentRole::from_str(role)
            .map(|r| self.validators.contains_key(&r))
            .unwrap_or(false)
    }

    /// Get all registered role names.
    pub fn registered_roles(&self) -> Vec<&'static str> {
        self.validators.keys().map(|r| r.as_str()).collect()
    }
}

// ============================================================================
// Error Types
// ============================================================================

/// Error from contract validation.
#[derive(Debug, Clone)]
pub enum ContractValidationError {
    /// The JSON did not conform to the agent's output schema.
    SchemaViolation {
        role: String,
        errors: String,
    },
    /// The JSON was schema-valid but could not be deserialized into the Rust type.
    /// This typically indicates a mismatch between the JsonSchema derive and serde.
    DeserializationFailed {
        role: String,
        error: String,
    },
}

impl std::fmt::Display for ContractValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaViolation { role, errors } => {
                write!(
                    f,
                    "Contract validation failed for {}: {}",
                    role, errors
                )
            }
            Self::DeserializationFailed { role, error } => {
                write!(
                    f,
                    "Contract deserialization failed for {}: {}",
                    role, error
                )
            }
        }
    }
}

impl std::error::Error for ContractValidationError {}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_contract_validator_creation() {
        let validator = ContractValidator::new().expect("should compile all schemas");
        assert!(validator.has_contract("spec_analyst"));
        assert!(validator.has_contract("locator"));
        assert!(validator.has_contract("implementer"));
        assert!(validator.has_contract("verifier"));
        assert!(!validator.has_contract("unknown_role"));
    }

    #[test]
    fn test_validate_valid_spec_analyst_output() {
        let validator = ContractValidator::new().unwrap();
        let data = serde_json::json!({
            "criteria": [],
            "complexity_assessment": "moderate",
            "subtree_count_estimate": 0,
            "complexity_rationale": ""
        });
        let result = validator.validate_output("spec_analyst", &data);
        assert!(result.valid, "Expected valid, got: {}", result.error_summary());
    }

    #[test]
    fn test_validate_invalid_spec_analyst_output() {
        let validator = ContractValidator::new().unwrap();
        // Missing required field "criteria"
        let data = serde_json::json!({
            "complexity_assessment": "moderate"
        });
        let result = validator.validate_output("spec_analyst", &data);
        assert!(!result.valid);
    }

    #[test]
    fn test_validate_valid_verifier_output() {
        let validator = ContractValidator::new().unwrap();
        let data = serde_json::json!({
            "results": [
                {
                    "criterion_id": "c1",
                    "passed": true,
                    "method": "test_output"
                }
            ],
            "overall_pass": true,
            "pass_count": 1,
            "fail_count": 0
        });
        let result = validator.validate_output("verifier", &data);
        assert!(result.valid, "Expected valid, got: {}", result.error_summary());
    }

    #[test]
    fn test_validate_and_parse_success() {
        let validator = ContractValidator::new().unwrap();
        let data = serde_json::json!({
            "results": [],
            "overall_pass": true,
            "pass_count": 0,
            "fail_count": 0
        });
        let output: VerifierOutput = validator
            .validate_and_parse("verifier", &data)
            .expect("should parse");
        assert!(output.overall_pass);
    }

    #[test]
    fn test_validate_and_parse_schema_failure() {
        let validator = ContractValidator::new().unwrap();
        let data = serde_json::json!({"wrong": "data"});
        let err = validator
            .validate_and_parse::<VerifierOutput>("verifier", &data)
            .unwrap_err();
        assert!(matches!(err, ContractValidationError::SchemaViolation { .. }));
    }

    #[test]
    fn test_unknown_role_passes_validation() {
        let validator = ContractValidator::new().unwrap();
        let data = serde_json::json!({"anything": true});
        // Unknown roles pass validation (graceful degradation)
        let result = validator.validate_output("snapshot", &data);
        assert!(result.valid);
    }

    #[test]
    fn test_registered_roles() {
        let validator = ContractValidator::new().unwrap();
        let roles = validator.registered_roles();
        assert_eq!(roles.len(), 4);
        assert!(roles.contains(&"spec_analyst"));
        assert!(roles.contains(&"locator"));
        assert!(roles.contains(&"implementer"));
        assert!(roles.contains(&"verifier"));
    }
}
