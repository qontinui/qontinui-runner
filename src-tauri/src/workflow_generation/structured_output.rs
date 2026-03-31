//! Structured Output Enforcement
//!
//! Generates a JSON Schema for the UnifiedWorkflow verification steps format
//! and provides output format instructions for AI providers that support
//! constrained decoding (Claude output_format, vLLM guided decoding, etc.).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::debug;

/// Which constrained decoding backend to use.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstrainedDecodingBackend {
    /// Claude's server-side output_format
    ClaudeOutputFormat,
    /// OpenAI-compatible APIs with `response_format: { type: "json_schema", json_schema: ... }`
    OpenAiStructuredOutput,
    /// Gemini's `generationConfig.responseSchema`
    GeminiResponseSchema,
    /// vLLM's guided decoding
    VllmGuided { base_url: String },
    /// Ollama's format parameter
    OllamaFormat { base_url: String },
    /// llama.cpp / llama-server grammar-based constrained decoding
    LlamaCppGrammar { base_url: String },
    /// No constrained decoding — use prompt + parse + fixer
    None,
}

impl Default for ConstrainedDecodingBackend {
    fn default() -> Self {
        ConstrainedDecodingBackend::None
    }
}

/// Detect the best available constrained decoding backend based on provider.
///
/// Recognizes provider names commonly used in the runner's AI routing layer.
/// For local inference servers (llama.cpp, vLLM, Ollama), the base URL
/// defaults to the standard localhost port; callers should override if needed.
pub fn detect_backend(provider: Option<&str>) -> ConstrainedDecodingBackend {
    match provider {
        Some("claude_api") | Some("anthropic_api") => {
            ConstrainedDecodingBackend::ClaudeOutputFormat
        }
        Some("openai") | Some("openai_api") | Some("openai_compatible") | Some("azure_openai") => {
            ConstrainedDecodingBackend::OpenAiStructuredOutput
        }
        Some("gemini") | Some("gemini_api") | Some("google") | Some("google_ai") => {
            ConstrainedDecodingBackend::GeminiResponseSchema
        }
        Some("llamacpp") | Some("llama_cpp") | Some("llama.cpp") => {
            ConstrainedDecodingBackend::LlamaCppGrammar {
                base_url: "http://localhost:8080".to_string(),
            }
        }
        Some("vllm") | Some("vllm_api") => ConstrainedDecodingBackend::VllmGuided {
            base_url: "http://localhost:8000".to_string(),
        },
        Some("ollama") | Some("ollama_api") => ConstrainedDecodingBackend::OllamaFormat {
            base_url: "http://localhost:11434".to_string(),
        },
        _ => ConstrainedDecodingBackend::None,
    }
}

/// Valid step_type enum values (kept for backward compatibility with validate_against_schema).
const VALID_STEP_TYPES: &[&str] = &["command", "prompt", "check-group"];

/// Valid check_type enum values.
const VALID_CHECK_TYPES: &[&str] = &[
    "contains",
    "exit_code",
    "not_contains",
    "regex",
    "json_path",
    "equals",
];

/// Valid phase enum values.
const VALID_PHASES: &[&str] = &["setup", "verification", "completion"];

// ============================================================================
// Schemars-derived schema types
// ============================================================================

/// Step type for workflow steps.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum StepType {
    Command,
    Prompt,
    CheckGroup,
}

/// Check type for evaluating command output.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckType {
    Contains,
    ExitCode,
    NotContains,
    Regex,
    JsonPath,
    Equals,
}

/// Phase a step belongs to.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum StepPhase {
    Setup,
    Verification,
    Completion,
}

/// Schema type for a single workflow verification step.
///
/// This is a schema-only type used to derive JSON Schema via schemars.
/// The actual runtime representation uses `serde_json::Value` in
/// `UnifiedWorkflow`. This type mirrors the shape that
/// `build_step_json_schema()` previously hand-rolled.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowStepSchema {
    /// Unique name identifying this step.
    pub name: String,
    /// Type of step: command (shell), prompt (AI evaluation), or check-group (composite).
    pub step_type: StepType,
    /// Human-readable description of what this step verifies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Shell command to execute (for command step_type).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// How to evaluate the command output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_type: Option<CheckType>,
    /// Expected value or pattern to match against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// Names of steps that must pass before this step runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,
    /// Whether failure of this step should halt the workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// URL for HTTP-based checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Directory to run the command in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Maximum seconds to wait for this step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
    /// Number of times to retry on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,
    /// Which workflow phase this step belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<StepPhase>,
}

/// Schema type for the full workflow output.
///
/// This is a schema-only type used to derive JSON Schema via schemars.
/// Mirrors the shape that `build_workflow_json_schema()` previously hand-rolled.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowOutputSchema {
    /// Workflow name.
    pub name: String,
    /// What this workflow accomplishes.
    pub description: String,
    /// Workflow category (e.g. build, test, deploy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Tags for discovery and filtering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Steps to prepare the environment before agentic work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_steps: Option<Vec<WorkflowStepSchema>>,
    /// Steps that verify the task was completed correctly.
    pub verification_steps: Vec<WorkflowStepSchema>,
    /// Steps the AI agent executes to accomplish the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agentic_steps: Option<Vec<WorkflowStepSchema>>,
    /// Cleanup or finalization steps after verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_steps: Option<Vec<WorkflowStepSchema>>,
    /// Maximum agentic loop iterations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    /// Overall workflow timeout in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

/// Build the JSON Schema for a single verification step object.
///
/// Now derived from `WorkflowStepSchema` via schemars instead of hand-rolled JSON.
pub fn build_step_json_schema() -> Value {
    debug!("Building step JSON schema (schemars-derived)");
    crate::schema_registry::schema_as_json::<WorkflowStepSchema>()
}

/// Build the JSON Schema for the full workflow output.
///
/// Now derived from `WorkflowOutputSchema` via schemars instead of hand-rolled JSON.
pub fn build_workflow_json_schema() -> Value {
    debug!("Building workflow JSON schema (schemars-derived)");
    crate::schema_registry::schema_as_json::<WorkflowOutputSchema>()
}

/// Build the output format instruction string for prompt injection.
/// This is appended to the builder prompt to guide JSON output structure
/// even when constrained decoding is not available.
pub fn build_output_format_instructions() -> String {
    let instructions = r#"
## Output Format

You MUST respond with a single JSON object (no markdown fences, no commentary outside the JSON). The schema is:

```
{
  "name": "<string, required>",
  "description": "<string, required>",
  "category": "<string, optional>",
  "tags": ["<string>", ...],
  "setup_steps": [ <step>, ... ],
  "verification_steps": [ <step>, ... ],
  "agentic_steps": [ <step>, ... ],
  "completion_steps": [ <step>, ... ],
  "max_iterations": <integer, >= 1>,
  "timeout_seconds": <integer, >= 10>
}
```

Each `<step>` object has this shape:

```
{
  "name": "<string, required — unique step identifier>",
  "step_type": "<required, one of: command | prompt | check-group>",
  "description": "<string, optional>",
  "command": "<string, shell command — required for step_type=command>",
  "check_type": "<one of: contains | exit_code | not_contains | regex | json_path | equals>",
  "expected": "<string, the value/pattern to match against>",
  "depends_on": ["<step name>", ...],
  "required": <boolean, default true>,
  "url": "<string, for HTTP checks>",
  "working_directory": "<string>",
  "timeout_seconds": <integer>,
  "retry_count": <integer>
}
```

### Rules
- `verification_steps` MUST be a non-empty array.
- Every step MUST have `name` and `step_type`.
- For `step_type: "command"`, include a `command` field with the shell command.
- For `step_type: "prompt"`, include a `description` explaining what the AI should evaluate.
- `check_type` and `expected` work together: e.g. `"check_type": "contains", "expected": "SUCCESS"`.

### Example step

```json
{
  "name": "check_build_output",
  "step_type": "command",
  "command": "cargo build --release 2>&1",
  "check_type": "exit_code",
  "expected": "0",
  "description": "Verify the project compiles without errors",
  "required": true,
  "timeout_seconds": 120
}
```
"#;
    instructions.trim().to_string()
}

/// Convert a schemars-derived JSON Schema to a GBNF grammar string for llama.cpp.
///
/// This is the preferred replacement for [`build_gbnf_grammar`] — it walks
/// the schemars-generated schema for type `T` instead of using a hand-rolled
/// grammar, keeping the grammar in sync with Rust type changes automatically.
pub fn schema_to_gbnf<T: schemars::JsonSchema>() -> String {
    let schema = crate::schema_registry::schema_as_json::<T>();
    crate::validation::schema_to_gbnf(&schema)
}

/// Validate a workflow JSON value using the [`SchemaValidator`] from the
/// validation module, returning structured errors.
///
/// Uses the schemars-derived `WorkflowOutputSchema` type for schema-exact
/// validation via `SchemaValidator::for_type`. The hand-rolled
/// `validate_against_schema()` is still available for callers who need
/// backward-compatible `Vec<String>` error messages.
pub fn validate_workflow_with_schema_validator(
    workflow_json: &Value,
) -> crate::validation::ValidationResult {
    match crate::validation::SchemaValidator::for_type::<WorkflowOutputSchema>() {
        Ok(validator) => validator.validate(workflow_json),
        Err(e) => {
            debug!("Failed to compile workflow schema validator: {}", e);
            // Fall back to a synthetic invalid result
            crate::validation::ValidationResult::invalid(
                workflow_json.clone(),
                vec![crate::validation::ValidationError {
                    path: String::new(),
                    error_kind: crate::validation::validator::ErrorKind::Other,
                    message: format!("Schema compilation error: {}", e),
                    expected: "valid schema".to_string(),
                    actual: "compilation failure".to_string(),
                }],
            )
        }
    }
}

/// Convert a JSON Schema to a GBNF grammar string for llama.cpp.
///
/// **Deprecated**: Prefer [`schema_to_gbnf`] which derives the grammar from
/// the schemars type, keeping it in sync with Rust type changes.
///
/// This produces a simplified GBNF grammar that constrains output to the
/// workflow JSON structure. Not a full JSON Schema → GBNF compiler, but
/// covers the key structural constraints.
pub fn build_gbnf_grammar() -> String {
    // Generate a GBNF grammar for the workflow output format
    // GBNF is a BNF-like format used by llama.cpp for constrained generation
    let grammar = r#"root ::= "{" ws workflow-body ws "}"

workflow-body ::= name-field "," ws description-field "," ws verification-steps-field ("," ws optional-field)*

name-field ::= "\"name\"" ws ":" ws string
description-field ::= "\"description\"" ws ":" ws string
verification-steps-field ::= "\"verification_steps\"" ws ":" ws "[" ws step ("," ws step)* ws "]"

optional-field ::= category-field | tags-field | setup-steps-field | agentic-steps-field | completion-steps-field | max-iterations-field | timeout-field

category-field ::= "\"category\"" ws ":" ws string
tags-field ::= "\"tags\"" ws ":" ws "[" ws (string ("," ws string)*)? ws "]"
setup-steps-field ::= "\"setup_steps\"" ws ":" ws "[" ws (step ("," ws step)*)? ws "]"
agentic-steps-field ::= "\"agentic_steps\"" ws ":" ws "[" ws (step ("," ws step)*)? ws "]"
completion-steps-field ::= "\"completion_steps\"" ws ":" ws "[" ws (step ("," ws step)*)? ws "]"
max-iterations-field ::= "\"max_iterations\"" ws ":" ws number
timeout-field ::= "\"timeout_seconds\"" ws ":" ws number

step ::= "{" ws step-body ws "}"
step-body ::= step-name "," ws step-type ("," ws step-field)*
step-name ::= "\"name\"" ws ":" ws string
step-type ::= "\"step_type\"" ws ":" ws ("\"command\"" | "\"prompt\"" | "\"check-group\"")

step-field ::= step-description | step-command | step-check-type | step-expected | step-depends-on | step-required | step-url | step-working-dir | step-timeout | step-retry

step-description ::= "\"description\"" ws ":" ws string
step-command ::= "\"command\"" ws ":" ws string
step-check-type ::= "\"check_type\"" ws ":" ws ("\"contains\"" | "\"exit_code\"" | "\"not_contains\"" | "\"regex\"" | "\"json_path\"" | "\"equals\"")
step-expected ::= "\"expected\"" ws ":" ws string
step-depends-on ::= "\"depends_on\"" ws ":" ws "[" ws (string ("," ws string)*)? ws "]"
step-required ::= "\"required\"" ws ":" ws ("true" | "false")
step-url ::= "\"url\"" ws ":" ws string
step-working-dir ::= "\"working_directory\"" ws ":" ws string
step-timeout ::= "\"timeout_seconds\"" ws ":" ws number
step-retry ::= "\"retry_count\"" ws ":" ws number

string ::= "\"" ([^"\\] | "\\" .)* "\""
number ::= [0-9]+
ws ::= [ \t\n\r]*"#;

    grammar.to_string()
}

/// Validate a parsed workflow JSON against the schema.
/// Returns a list of validation errors (empty if valid).
///
/// Uses `SchemaValidator` from the validation module for structural validation,
/// combined with workflow-specific semantic checks (non-empty strings, non-empty
/// arrays, clear enum error messages) for backward-compatible error messages.
pub fn validate_against_schema(workflow_json: &Value) -> Vec<String> {
    let mut errors = Vec::new();

    // 1. Check required top-level fields
    if !workflow_json.get("name").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()) {
        errors.push("Missing or empty required field: 'name'".to_string());
    }

    if !workflow_json
        .get("description")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
    {
        errors.push("Missing or empty required field: 'description'".to_string());
    }

    // 2. verification_steps must be a non-empty array
    match workflow_json.get("verification_steps") {
        Some(Value::Array(arr)) if !arr.is_empty() => {
            validate_steps_array(arr, "verification_steps", &mut errors);
        }
        Some(Value::Array(_)) => {
            errors.push("'verification_steps' must be a non-empty array".to_string());
        }
        _ => {
            errors.push("Missing required field: 'verification_steps'".to_string());
        }
    }

    // Validate optional step arrays
    for field in &["setup_steps", "agentic_steps", "completion_steps"] {
        if let Some(Value::Array(arr)) = workflow_json.get(*field) {
            validate_steps_array(arr, field, &mut errors);
        }
    }

    // Validate max_iterations if present
    if let Some(val) = workflow_json.get("max_iterations") {
        if let Some(n) = val.as_i64() {
            if n < 1 {
                errors.push("'max_iterations' must be >= 1".to_string());
            }
        } else {
            errors.push("'max_iterations' must be an integer".to_string());
        }
    }

    // Validate timeout_seconds if present
    if let Some(val) = workflow_json.get("timeout_seconds") {
        if let Some(n) = val.as_i64() {
            if n < 10 {
                errors.push("'timeout_seconds' must be >= 10".to_string());
            }
        } else {
            errors.push("'timeout_seconds' must be an integer".to_string());
        }
    }

    if errors.is_empty() {
        debug!("Workflow JSON passed schema validation");
    } else {
        debug!("Workflow JSON has {} validation errors", errors.len());
    }

    errors
}

/// Validate an array of step objects, appending errors to the list.
fn validate_steps_array(steps: &[Value], array_name: &str, errors: &mut Vec<String>) {
    for (i, step) in steps.iter().enumerate() {
        let prefix = format!("{array_name}[{i}]");

        // Each step must be an object
        let obj = match step.as_object() {
            Some(o) => o,
            None => {
                errors.push(format!("{prefix}: step must be a JSON object"));
                continue;
            }
        };

        // 3. Each step must have name and step_type
        if !obj
            .get("name")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
        {
            errors.push(format!("{prefix}: missing or empty required field 'name'"));
        }

        // 4. step_type must be a valid enum value
        match obj.get("step_type").and_then(|v| v.as_str()) {
            Some(st) if VALID_STEP_TYPES.contains(&st) => {}
            Some(st) => {
                errors.push(format!(
                    "{prefix}: invalid step_type '{st}'; expected one of: {}",
                    VALID_STEP_TYPES.join(", ")
                ));
            }
            None => {
                errors.push(format!("{prefix}: missing required field 'step_type'"));
            }
        }

        // 5. check_type must be a valid enum value if present
        if let Some(ct_val) = obj.get("check_type") {
            match ct_val.as_str() {
                Some(ct) if VALID_CHECK_TYPES.contains(&ct) => {}
                Some(ct) => {
                    errors.push(format!(
                        "{prefix}: invalid check_type '{ct}'; expected one of: {}",
                        VALID_CHECK_TYPES.join(", ")
                    ));
                }
                None => {
                    errors.push(format!("{prefix}: 'check_type' must be a string"));
                }
            }
        }

        // Validate phase enum if present
        if let Some(phase_val) = obj.get("phase") {
            match phase_val.as_str() {
                Some(p) if VALID_PHASES.contains(&p) => {}
                Some(p) => {
                    errors.push(format!(
                        "{prefix}: invalid phase '{p}'; expected one of: {}",
                        VALID_PHASES.join(", ")
                    ));
                }
                None => {
                    errors.push(format!("{prefix}: 'phase' must be a string"));
                }
            }
        }

        // Validate depends_on is array of strings if present
        if let Some(deps) = obj.get("depends_on") {
            match deps.as_array() {
                Some(arr) => {
                    for (j, dep) in arr.iter().enumerate() {
                        if dep.as_str().is_none() {
                            errors.push(format!(
                                "{prefix}.depends_on[{j}]: must be a string"
                            ));
                        }
                    }
                }
                None => {
                    errors.push(format!("{prefix}: 'depends_on' must be an array"));
                }
            }
        }

        // Validate timeout_seconds is positive integer if present
        if let Some(ts) = obj.get("timeout_seconds") {
            if let Some(n) = ts.as_i64() {
                if n < 1 {
                    errors.push(format!(
                        "{prefix}: 'timeout_seconds' must be >= 1"
                    ));
                }
            } else {
                errors.push(format!(
                    "{prefix}: 'timeout_seconds' must be an integer"
                ));
            }
        }

        // Validate retry_count is non-negative integer if present
        if let Some(rc) = obj.get("retry_count") {
            if let Some(n) = rc.as_i64() {
                if n < 0 {
                    errors.push(format!(
                        "{prefix}: 'retry_count' must be >= 0"
                    ));
                }
            } else {
                errors.push(format!("{prefix}: 'retry_count' must be an integer"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_backend_claude() {
        assert!(matches!(
            detect_backend(Some("claude_api")),
            ConstrainedDecodingBackend::ClaudeOutputFormat
        ));
        assert!(matches!(
            detect_backend(Some("anthropic_api")),
            ConstrainedDecodingBackend::ClaudeOutputFormat
        ));
    }

    #[test]
    fn test_detect_backend_openai() {
        assert!(matches!(
            detect_backend(Some("openai")),
            ConstrainedDecodingBackend::OpenAiStructuredOutput
        ));
        assert!(matches!(
            detect_backend(Some("openai_api")),
            ConstrainedDecodingBackend::OpenAiStructuredOutput
        ));
        assert!(matches!(
            detect_backend(Some("azure_openai")),
            ConstrainedDecodingBackend::OpenAiStructuredOutput
        ));
    }

    #[test]
    fn test_detect_backend_gemini() {
        assert!(matches!(
            detect_backend(Some("gemini")),
            ConstrainedDecodingBackend::GeminiResponseSchema
        ));
        assert!(matches!(
            detect_backend(Some("gemini_api")),
            ConstrainedDecodingBackend::GeminiResponseSchema
        ));
        assert!(matches!(
            detect_backend(Some("google_ai")),
            ConstrainedDecodingBackend::GeminiResponseSchema
        ));
    }

    #[test]
    fn test_detect_backend_vllm() {
        assert!(matches!(
            detect_backend(Some("vllm")),
            ConstrainedDecodingBackend::VllmGuided { .. }
        ));
    }

    #[test]
    fn test_detect_backend_ollama() {
        assert!(matches!(
            detect_backend(Some("ollama")),
            ConstrainedDecodingBackend::OllamaFormat { .. }
        ));
    }

    #[test]
    fn test_detect_backend_none() {
        assert!(matches!(
            detect_backend(None),
            ConstrainedDecodingBackend::None
        ));
        assert!(matches!(
            detect_backend(Some("unknown_provider")),
            ConstrainedDecodingBackend::None
        ));
    }

    #[test]
    fn test_default_backend_is_none() {
        assert!(matches!(
            ConstrainedDecodingBackend::default(),
            ConstrainedDecodingBackend::None
        ));
    }

    #[test]
    fn test_step_schema_has_required_fields() {
        let schema = build_step_json_schema();
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("name"));
        assert!(props.contains_key("step_type"));
        assert!(props.contains_key("command"));
        assert!(props.contains_key("check_type"));
        assert!(props.contains_key("expected"));

        let required = schema["required"].as_array().unwrap();
        let req_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(req_strs.contains(&"name"));
        assert!(req_strs.contains(&"step_type"));
    }

    #[test]
    fn test_workflow_schema_has_required_fields() {
        let schema = build_workflow_json_schema();
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("name"));
        assert!(props.contains_key("verification_steps"));
        assert!(props.contains_key("setup_steps"));
        assert!(props.contains_key("agentic_steps"));
        assert!(props.contains_key("completion_steps"));

        let required = schema["required"].as_array().unwrap();
        let req_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(req_strs.contains(&"name"));
        assert!(req_strs.contains(&"description"));
        assert!(req_strs.contains(&"verification_steps"));
    }

    #[test]
    fn test_output_format_instructions_not_empty() {
        let instructions = build_output_format_instructions();
        assert!(!instructions.is_empty());
        assert!(instructions.contains("verification_steps"));
        assert!(instructions.contains("step_type"));
        assert!(instructions.contains("check_type"));
    }

    #[test]
    fn test_validate_valid_workflow() {
        let workflow = json!({
            "name": "test-workflow",
            "description": "A test workflow",
            "verification_steps": [
                {
                    "name": "check_build",
                    "step_type": "command",
                    "command": "cargo build",
                    "check_type": "exit_code",
                    "expected": "0"
                }
            ]
        });
        let errors = validate_against_schema(&workflow);
        assert!(errors.is_empty(), "Expected no errors, got: {:?}", errors);
    }

    #[test]
    fn test_validate_missing_name() {
        let workflow = json!({
            "description": "A test workflow",
            "verification_steps": [
                { "name": "s1", "step_type": "command" }
            ]
        });
        let errors = validate_against_schema(&workflow);
        assert!(errors.iter().any(|e| e.contains("'name'")));
    }

    #[test]
    fn test_validate_empty_verification_steps() {
        let workflow = json!({
            "name": "test",
            "description": "test",
            "verification_steps": []
        });
        let errors = validate_against_schema(&workflow);
        assert!(errors.iter().any(|e| e.contains("non-empty")));
    }

    #[test]
    fn test_validate_missing_verification_steps() {
        let workflow = json!({
            "name": "test",
            "description": "test"
        });
        let errors = validate_against_schema(&workflow);
        assert!(errors.iter().any(|e| e.contains("verification_steps")));
    }

    #[test]
    fn test_validate_invalid_step_type() {
        let workflow = json!({
            "name": "test",
            "description": "test",
            "verification_steps": [
                { "name": "s1", "step_type": "invalid_type" }
            ]
        });
        let errors = validate_against_schema(&workflow);
        assert!(errors.iter().any(|e| e.contains("invalid step_type")));
    }

    #[test]
    fn test_validate_invalid_check_type() {
        let workflow = json!({
            "name": "test",
            "description": "test",
            "verification_steps": [
                { "name": "s1", "step_type": "command", "check_type": "bogus" }
            ]
        });
        let errors = validate_against_schema(&workflow);
        assert!(errors.iter().any(|e| e.contains("invalid check_type")));
    }

    #[test]
    fn test_validate_step_missing_name() {
        let workflow = json!({
            "name": "test",
            "description": "test",
            "verification_steps": [
                { "step_type": "command" }
            ]
        });
        let errors = validate_against_schema(&workflow);
        assert!(errors.iter().any(|e| e.contains("'name'")));
    }

    #[test]
    fn test_validate_step_missing_step_type() {
        let workflow = json!({
            "name": "test",
            "description": "test",
            "verification_steps": [
                { "name": "s1" }
            ]
        });
        let errors = validate_against_schema(&workflow);
        assert!(errors.iter().any(|e| e.contains("'step_type'")));
    }

    #[test]
    fn test_validate_optional_arrays() {
        let workflow = json!({
            "name": "test",
            "description": "test",
            "verification_steps": [
                { "name": "v1", "step_type": "command" }
            ],
            "setup_steps": [
                { "name": "s1", "step_type": "invalid" }
            ]
        });
        let errors = validate_against_schema(&workflow);
        assert!(errors.iter().any(|e| e.contains("setup_steps[0]")));
    }

    #[test]
    fn test_validate_max_iterations_too_low() {
        let workflow = json!({
            "name": "test",
            "description": "test",
            "verification_steps": [
                { "name": "v1", "step_type": "command" }
            ],
            "max_iterations": 0
        });
        let errors = validate_against_schema(&workflow);
        assert!(errors.iter().any(|e| e.contains("max_iterations")));
    }

    #[test]
    fn test_validate_timeout_too_low() {
        let workflow = json!({
            "name": "test",
            "description": "test",
            "verification_steps": [
                { "name": "v1", "step_type": "command" }
            ],
            "timeout_seconds": 5
        });
        let errors = validate_against_schema(&workflow);
        assert!(errors.iter().any(|e| e.contains("timeout_seconds")));
    }

    #[test]
    fn test_validate_invalid_phase() {
        let workflow = json!({
            "name": "test",
            "description": "test",
            "verification_steps": [
                { "name": "v1", "step_type": "command", "phase": "bogus_phase" }
            ]
        });
        let errors = validate_against_schema(&workflow);
        assert!(errors.iter().any(|e| e.contains("invalid phase")));
    }

    #[test]
    fn test_validate_depends_on_non_string() {
        let workflow = json!({
            "name": "test",
            "description": "test",
            "verification_steps": [
                { "name": "v1", "step_type": "command", "depends_on": [123] }
            ]
        });
        let errors = validate_against_schema(&workflow);
        assert!(errors.iter().any(|e| e.contains("depends_on")));
    }

    #[test]
    fn test_detect_llamacpp_backend() {
        assert!(matches!(
            detect_backend(Some("llamacpp")),
            ConstrainedDecodingBackend::LlamaCppGrammar { .. }
        ));
        assert!(matches!(
            detect_backend(Some("llama_cpp")),
            ConstrainedDecodingBackend::LlamaCppGrammar { .. }
        ));
    }

    #[test]
    fn test_gbnf_grammar_structure() {
        let grammar = build_gbnf_grammar();
        assert!(grammar.contains("root ::="));
        assert!(grammar.contains("verification-steps-field"));
        assert!(grammar.contains("step-type"));
        assert!(grammar.contains("\"command\""));
        assert!(grammar.contains("\"prompt\""));
    }

    #[test]
    fn test_backend_serialization_roundtrip() {
        let backend = ConstrainedDecodingBackend::VllmGuided {
            base_url: "http://localhost:8000".to_string(),
        };
        let serialized = serde_json::to_string(&backend).unwrap();
        let deserialized: ConstrainedDecodingBackend =
            serde_json::from_str(&serialized).unwrap();
        assert!(matches!(
            deserialized,
            ConstrainedDecodingBackend::VllmGuided { .. }
        ));
    }

    #[test]
    fn test_backend_serialization_openai() {
        let backend = ConstrainedDecodingBackend::OpenAiStructuredOutput;
        let serialized = serde_json::to_string(&backend).unwrap();
        let deserialized: ConstrainedDecodingBackend =
            serde_json::from_str(&serialized).unwrap();
        assert!(matches!(
            deserialized,
            ConstrainedDecodingBackend::OpenAiStructuredOutput
        ));
    }

    #[test]
    fn test_backend_serialization_gemini() {
        let backend = ConstrainedDecodingBackend::GeminiResponseSchema;
        let serialized = serde_json::to_string(&backend).unwrap();
        let deserialized: ConstrainedDecodingBackend =
            serde_json::from_str(&serialized).unwrap();
        assert!(matches!(
            deserialized,
            ConstrainedDecodingBackend::GeminiResponseSchema
        ));
    }

    #[test]
    fn test_validate_workflow_with_schema_validator() {
        let workflow = json!({
            "name": "test-workflow",
            "description": "A test workflow",
            "verification_steps": [
                {
                    "name": "check_build",
                    "step_type": "command",
                    "command": "cargo build",
                    "check_type": "exit_code",
                    "expected": "0"
                }
            ]
        });
        let result = validate_workflow_with_schema_validator(&workflow);
        assert!(result.valid, "Expected valid, got: {}", result.error_summary());
    }

    #[test]
    fn test_validate_workflow_with_schema_validator_invalid() {
        let workflow = json!({
            "name": 123
        });
        let result = validate_workflow_with_schema_validator(&workflow);
        assert!(!result.valid);
        assert!(!result.errors.is_empty());
    }
}
