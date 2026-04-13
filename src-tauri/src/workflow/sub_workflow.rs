use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Maximum sub-workflow nesting depth.
pub const MAX_SUB_WORKFLOW_DEPTH: u32 = 8;

/// Error returned when sub-workflow execution fails validation.
#[derive(Debug)]
pub enum SubWorkflowError {
    MaxDepthExceeded { current_depth: u32, max_depth: u32 },
    WorkflowNotFound { workflow_ref: String },
    InputMappingError { message: String },
}

impl std::fmt::Display for SubWorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MaxDepthExceeded {
                current_depth,
                max_depth,
            } => {
                write!(
                    f,
                    "Sub-workflow depth {} exceeds maximum of {}",
                    current_depth, max_depth
                )
            }
            Self::WorkflowNotFound { workflow_ref } => {
                write!(f, "Referenced workflow '{}' not found", workflow_ref)
            }
            Self::InputMappingError { message } => {
                write!(f, "Input mapping error: {}", message)
            }
        }
    }
}

impl std::error::Error for SubWorkflowError {}

/// Configuration for a sub-workflow invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubWorkflowConfig {
    /// The workflow ID or YAML path to execute.
    pub workflow_ref: String,
    /// Input mappings: child_input_name -> value (already resolved from parent variables).
    pub resolved_inputs: HashMap<String, Value>,
    /// Parent execution ID for linking.
    pub parent_execution_id: String,
    /// Current nesting depth (parent's depth + 1).
    pub depth: u32,
}

/// Validate that a sub-workflow invocation is allowed.
pub fn validate_sub_workflow(
    workflow_ref: &str,
    current_depth: u32,
    max_depth: Option<u32>,
) -> Result<(), SubWorkflowError> {
    let max = max_depth.unwrap_or(MAX_SUB_WORKFLOW_DEPTH);
    if current_depth >= max {
        return Err(SubWorkflowError::MaxDepthExceeded {
            current_depth,
            max_depth: max,
        });
    }
    if workflow_ref.is_empty() {
        return Err(SubWorkflowError::WorkflowNotFound {
            workflow_ref: workflow_ref.to_string(),
        });
    }
    Ok(())
}

/// Prepare a sub-workflow configuration from a parent's step config.
///
/// Resolves input mappings using the parent's variable store and validates
/// the nesting depth. The actual workflow loading and execution is handled
/// by the step executor's workflow_ref handler.
pub fn prepare_sub_workflow(
    workflow_ref: &str,
    workflow_inputs: Option<&HashMap<String, String>>,
    parent_variables: &crate::workflow::variable_engine::VariableStore,
    parent_execution_id: &str,
    parent_depth: u32,
) -> Result<SubWorkflowConfig, SubWorkflowError> {
    validate_sub_workflow(workflow_ref, parent_depth, None)?;

    let resolved_inputs = match workflow_inputs {
        Some(inputs) => {
            let mut resolved = HashMap::new();
            for (child_key, parent_ref) in inputs {
                let value = parent_variables
                    .resolve_reference(parent_ref)
                    .unwrap_or(Value::Null);
                resolved.insert(child_key.clone(), value);
            }
            resolved
        }
        None => HashMap::new(),
    };

    Ok(SubWorkflowConfig {
        workflow_ref: workflow_ref.to_string(),
        resolved_inputs,
        parent_execution_id: parent_execution_id.to_string(),
        depth: parent_depth + 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::variable_engine::VariableStore;
    use serde_json::json;

    #[test]
    fn test_validate_sub_workflow_ok() {
        let result = validate_sub_workflow("my-workflow.yaml", 3, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_sub_workflow_max_depth_exceeded() {
        let result = validate_sub_workflow("my-workflow.yaml", MAX_SUB_WORKFLOW_DEPTH, None);
        assert!(matches!(
            result,
            Err(SubWorkflowError::MaxDepthExceeded { .. })
        ));
    }

    #[test]
    fn test_validate_sub_workflow_custom_max_depth() {
        // depth 2 with max 3 should pass
        assert!(validate_sub_workflow("wf.yaml", 2, Some(3)).is_ok());
        // depth 3 with max 3 should fail
        assert!(matches!(
            validate_sub_workflow("wf.yaml", 3, Some(3)),
            Err(SubWorkflowError::MaxDepthExceeded { .. })
        ));
    }

    #[test]
    fn test_validate_sub_workflow_empty_ref() {
        let result = validate_sub_workflow("", 0, None);
        assert!(matches!(
            result,
            Err(SubWorkflowError::WorkflowNotFound { .. })
        ));
    }

    #[test]
    fn test_prepare_sub_workflow_no_inputs() {
        let store = VariableStore::new();
        let config = prepare_sub_workflow("child.yaml", None, &store, "exec-123", 0).unwrap();
        assert_eq!(config.workflow_ref, "child.yaml");
        assert_eq!(config.depth, 1);
        assert_eq!(config.parent_execution_id, "exec-123");
        assert!(config.resolved_inputs.is_empty());
    }

    #[test]
    fn test_prepare_sub_workflow_with_inputs() {
        let mut store = VariableStore::new();
        store.set_output("nodeA", json!({"score": 99}));

        let mut inputs = HashMap::new();
        inputs.insert("child_score".to_string(), "$nodeA.score".to_string());
        inputs.insert("missing_ref".to_string(), "$nodeX.nothing".to_string());

        let config =
            prepare_sub_workflow("child.yaml", Some(&inputs), &store, "exec-456", 1).unwrap();

        assert_eq!(config.depth, 2);
        assert_eq!(config.resolved_inputs.get("child_score"), Some(&json!(99)));
        // Missing reference resolves to null
        assert_eq!(
            config.resolved_inputs.get("missing_ref"),
            Some(&Value::Null)
        );
    }

    #[test]
    fn test_prepare_sub_workflow_depth_exceeded() {
        let store = VariableStore::new();
        let result = prepare_sub_workflow(
            "child.yaml",
            None,
            &store,
            "exec-789",
            MAX_SUB_WORKFLOW_DEPTH,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_sub_workflow_config_serializes() {
        let config = SubWorkflowConfig {
            workflow_ref: "wf.yaml".to_string(),
            resolved_inputs: {
                let mut m = HashMap::new();
                m.insert("key".to_string(), json!("value"));
                m
            },
            parent_execution_id: "abc".to_string(),
            depth: 2,
        };
        let serialized = serde_json::to_string(&config).unwrap();
        let deserialized: SubWorkflowConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.workflow_ref, "wf.yaml");
        assert_eq!(deserialized.depth, 2);
        assert_eq!(deserialized.resolved_inputs.get("key"), Some(&json!("value")));
    }

    #[test]
    fn test_error_display_max_depth() {
        let e = SubWorkflowError::MaxDepthExceeded {
            current_depth: 8,
            max_depth: 8,
        };
        assert!(e.to_string().contains("exceeds maximum"));
    }

    #[test]
    fn test_error_display_not_found() {
        let e = SubWorkflowError::WorkflowNotFound {
            workflow_ref: "ghost.yaml".to_string(),
        };
        assert!(e.to_string().contains("ghost.yaml"));
    }

    #[test]
    fn test_error_display_input_mapping() {
        let e = SubWorkflowError::InputMappingError {
            message: "bad ref".to_string(),
        };
        assert!(e.to_string().contains("bad ref"));
    }
}
