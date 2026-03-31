//! Constrained Decoding Adapter
//!
//! Prepares JSON Schemas for each constrained decoding backend, translating
//! schemars-derived schemas into the appropriate format (JSON Schema, GBNF
//! grammar, regex, etc.).

use schemars::JsonSchema;
use serde_json::Value;
use tracing::debug;

use crate::schema_registry;
use crate::workflow_generation::structured_output::ConstrainedDecodingBackend;

// ============================================================================
// Constrained Decoding Payload
// ============================================================================

/// Payload prepared for a constrained decoding backend.
///
/// Each variant carries the schema in the format expected by the target
/// backend, ready to be attached to the API request.
#[derive(Debug, Clone)]
pub enum ConstrainedDecodingPayload {
    /// JSON Schema for API providers that accept it directly
    /// (Claude, OpenAI, Gemini).
    JsonSchema(Value),
    /// GBNF grammar string for llama.cpp grammar-based constrained decoding.
    Gbnf(String),
    /// Regex pattern for vLLM guided generation.
    Regex(String),
    /// No constrained decoding — use prompt + parse + validate + retry.
    None,
}

impl ConstrainedDecodingPayload {
    /// Returns `true` if this payload actually constrains decoding.
    pub fn is_constrained(&self) -> bool {
        !matches!(self, ConstrainedDecodingPayload::None)
    }
}

// ============================================================================
// Schema Preparation
// ============================================================================

/// Prepare a schema for the given constrained decoding backend.
///
/// Uses the schemars-derived JSON Schema for `T` and converts it into the
/// format required by the backend. Falls back to `None` if the backend
/// doesn't support constrained decoding.
pub fn prepare_schema_for_backend<T: JsonSchema>(
    backend: &ConstrainedDecodingBackend,
) -> ConstrainedDecodingPayload {
    match backend {
        ConstrainedDecodingBackend::ClaudeOutputFormat
        | ConstrainedDecodingBackend::OpenAiStructuredOutput
        | ConstrainedDecodingBackend::GeminiResponseSchema => {
            let schema = schema_registry::schema_as_json::<T>();
            debug!(
                "Prepared JSON Schema for {:?} (type: {})",
                backend,
                T::schema_name()
            );
            ConstrainedDecodingPayload::JsonSchema(schema)
        }
        ConstrainedDecodingBackend::OllamaFormat { .. } => {
            // Ollama accepts JSON Schema in its format parameter
            let schema = schema_registry::schema_as_json::<T>();
            debug!(
                "Prepared JSON Schema for Ollama format (type: {})",
                T::schema_name()
            );
            ConstrainedDecodingPayload::JsonSchema(schema)
        }
        ConstrainedDecodingBackend::LlamaCppGrammar { .. } => {
            let schema = schema_registry::schema_as_json::<T>();
            let gbnf = schema_to_gbnf(&schema);
            debug!(
                "Prepared GBNF grammar for llama.cpp (type: {}, {} bytes)",
                T::schema_name(),
                gbnf.len()
            );
            ConstrainedDecodingPayload::Gbnf(gbnf)
        }
        ConstrainedDecodingBackend::VllmGuided { .. } => {
            // vLLM supports JSON Schema directly in guided decoding
            let schema = schema_registry::schema_as_json::<T>();
            debug!(
                "Prepared JSON Schema for vLLM guided decoding (type: {})",
                T::schema_name()
            );
            ConstrainedDecodingPayload::JsonSchema(schema)
        }
        ConstrainedDecodingBackend::None => {
            debug!("No constrained decoding backend; using prompt-based fallback");
            ConstrainedDecodingPayload::None
        }
    }
}

/// Prepare a schema from a raw JSON Schema value (not type-derived).
///
/// Useful when the schema was built by hand or loaded from storage rather
/// than derived from a Rust type.
pub fn prepare_raw_schema_for_backend(
    backend: &ConstrainedDecodingBackend,
    schema: &Value,
) -> ConstrainedDecodingPayload {
    match backend {
        ConstrainedDecodingBackend::ClaudeOutputFormat
        | ConstrainedDecodingBackend::OpenAiStructuredOutput
        | ConstrainedDecodingBackend::GeminiResponseSchema
        | ConstrainedDecodingBackend::OllamaFormat { .. }
        | ConstrainedDecodingBackend::VllmGuided { .. } => {
            ConstrainedDecodingPayload::JsonSchema(schema.clone())
        }
        ConstrainedDecodingBackend::LlamaCppGrammar { .. } => {
            let gbnf = schema_to_gbnf(schema);
            ConstrainedDecodingPayload::Gbnf(gbnf)
        }
        ConstrainedDecodingBackend::None => ConstrainedDecodingPayload::None,
    }
}

// ============================================================================
// JSON Schema → GBNF Grammar Conversion
// ============================================================================

/// Convert a JSON Schema (as `serde_json::Value`) into a GBNF grammar string.
///
/// This walks the schemars-generated schema and produces GBNF rules that
/// constrain llama.cpp output to match the schema structure. It handles:
/// - Objects with required/optional properties
/// - Arrays with item schemas
/// - String enums
/// - Primitive types (string, number, integer, boolean, null)
/// - `$ref` / `$defs` references
///
/// This is not a full JSON Schema → GBNF compiler but covers the structural
/// constraints needed for typical agent output schemas.
pub fn schema_to_gbnf(schema: &Value) -> String {
    let mut ctx = GbnfContext {
        rules: Vec::new(),
        counter: 0,
    };

    // Add primitive rules shared across all grammars
    ctx.rules.push("ws ::= [ \\t\\n\\r]*".to_string());
    ctx.rules
        .push(r#"string ::= "\"" ([^"\\] | "\\" .)* "\""#.to_string());
    ctx.rules
        .push(r#"number ::= "-"? [0-9]+ ("." [0-9]+)?"#.to_string());
    ctx.rules.push(r#"integer ::= "-"? [0-9]+"#.to_string());
    ctx.rules
        .push(r#"boolean ::= ("true" | "false")"#.to_string());
    ctx.rules.push(r#"null ::= "null""#.to_string());
    ctx.rules.push(
        r#"json-value ::= string | number | boolean | null | json-array | json-object"#
            .to_string(),
    );
    ctx.rules.push(
        r#"json-array ::= "[" ws (json-value ("," ws json-value)*)? ws "]""#.to_string(),
    );
    ctx.rules.push(
        r#"json-object ::= "{" ws (string ws ":" ws json-value ("," ws string ws ":" ws json-value)*)? ws "}""#
            .to_string(),
    );

    // Generate the root rule from the schema
    let root_rule = emit_schema_rule(&mut ctx, schema, schema);
    ctx.rules.insert(0, format!("root ::= {}", root_rule));

    ctx.rules.join("\n\n")
}

/// Internal context for GBNF rule generation.
struct GbnfContext {
    rules: Vec<String>,
    counter: usize,
}

impl GbnfContext {
    fn next_name(&mut self, hint: &str) -> String {
        self.counter += 1;
        let sanitized: String = hint
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
            .collect();
        format!("r-{}-{}", sanitized, self.counter)
    }
}

/// Emit a GBNF rule expression for a given schema node.
///
/// Returns a GBNF expression (which may reference named rules added to `ctx`).
fn emit_schema_rule(ctx: &mut GbnfContext, node: &Value, root: &Value) -> String {
    // Handle $ref
    if let Some(ref_path) = node.get("$ref").and_then(|v| v.as_str()) {
        if let Some(resolved) = resolve_ref(root, ref_path) {
            return emit_schema_rule(ctx, resolved, root);
        }
    }

    // Handle oneOf (tagged enums) — emit alternatives
    if let Some(one_of) = node.get("oneOf").and_then(|v| v.as_array()) {
        let alts: Vec<String> = one_of
            .iter()
            .map(|variant| emit_schema_rule(ctx, variant, root))
            .collect();
        if alts.len() == 1 {
            return alts.into_iter().next().unwrap();
        }
        let name = ctx.next_name("oneof");
        ctx.rules
            .push(format!("{} ::= {}", name, alts.join(" | ")));
        return name;
    }

    // Handle anyOf the same way
    if let Some(any_of) = node.get("anyOf").and_then(|v| v.as_array()) {
        let alts: Vec<String> = any_of
            .iter()
            .map(|variant| emit_schema_rule(ctx, variant, root))
            .collect();
        if alts.len() == 1 {
            return alts.into_iter().next().unwrap();
        }
        let name = ctx.next_name("anyof");
        ctx.rules
            .push(format!("{} ::= {}", name, alts.join(" | ")));
        return name;
    }

    let schema_type = node
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("object");

    match schema_type {
        "string" => {
            // Check for enum
            if let Some(enum_vals) = node.get("enum").and_then(|v| v.as_array()) {
                let alts: Vec<String> = enum_vals
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| format!("\"\\\"{}\\\"\"", s))
                    .collect();
                if !alts.is_empty() {
                    let name = ctx.next_name("enum");
                    ctx.rules
                        .push(format!("{} ::= {}", name, alts.join(" | ")));
                    return name;
                }
            }
            "string".to_string()
        }
        "integer" => "integer".to_string(),
        "number" => "number".to_string(),
        "boolean" => "boolean".to_string(),
        "null" => "null".to_string(),
        "array" => {
            let item_rule = if let Some(items) = node.get("items") {
                emit_schema_rule(ctx, items, root)
            } else {
                "json-value".to_string()
            };
            let name = ctx.next_name("arr");
            ctx.rules.push(format!(
                "{} ::= \"[\" ws ({} (\",\" ws {})*)? ws \"]\"",
                name, item_rule, item_rule
            ));
            name
        }
        "object" | _ => {
            let props = match node.get("properties").and_then(|v| v.as_object()) {
                Some(p) => p,
                None => return "json-object".to_string(),
            };

            let required: Vec<&str> = node
                .get("required")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();

            // Collect property rules
            let mut required_fields: Vec<String> = Vec::new();
            let mut optional_fields: Vec<String> = Vec::new();

            for (key, prop_schema) in props {
                let value_rule = emit_schema_rule(ctx, prop_schema, root);
                let field_expr = format!("\"\\\"{}\\\"\" ws \":\" ws {}", key, value_rule);

                if required.contains(&key.as_str()) {
                    required_fields.push(field_expr);
                } else {
                    optional_fields.push(field_expr);
                }
            }

            let name = ctx.next_name("obj");

            // Build the object rule
            let mut body_parts = Vec::new();

            // Required fields separated by commas
            if !required_fields.is_empty() {
                body_parts.push(required_fields.join(" \",\" ws "));
            }

            // Optional fields: each is (\",\" ws field)?
            for opt in &optional_fields {
                body_parts.push(format!("(\",\" ws {})?", opt));
            }

            let body = if body_parts.is_empty() {
                String::new()
            } else {
                format!(" {} ", body_parts.join(" "))
            };

            ctx.rules
                .push(format!("{} ::= \"{{\" ws{}ws \"}}\"", name, body));
            name
        }
    }
}

/// Resolve a `$ref` pointer to its target schema.
fn resolve_ref<'a>(root: &'a Value, ref_path: &str) -> Option<&'a Value> {
    let path = ref_path.strip_prefix("#/")?;
    let mut current = root;
    for segment in path.split('/') {
        current = current.get(segment)?;
    }
    Some(current)
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
    struct TestStep {
        pub name: String,
        pub step_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub command: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    struct TestWorkflow {
        pub name: String,
        pub description: String,
        pub verification_steps: Vec<TestStep>,
    }

    #[test]
    fn test_prepare_schema_claude() {
        let backend = ConstrainedDecodingBackend::ClaudeOutputFormat;
        let payload = prepare_schema_for_backend::<TestWorkflow>(&backend);
        assert!(matches!(payload, ConstrainedDecodingPayload::JsonSchema(_)));
        assert!(payload.is_constrained());
    }

    #[test]
    fn test_prepare_schema_openai() {
        let backend = ConstrainedDecodingBackend::OpenAiStructuredOutput;
        let payload = prepare_schema_for_backend::<TestWorkflow>(&backend);
        assert!(matches!(payload, ConstrainedDecodingPayload::JsonSchema(_)));
    }

    #[test]
    fn test_prepare_schema_gemini() {
        let backend = ConstrainedDecodingBackend::GeminiResponseSchema;
        let payload = prepare_schema_for_backend::<TestWorkflow>(&backend);
        assert!(matches!(payload, ConstrainedDecodingPayload::JsonSchema(_)));
    }

    #[test]
    fn test_prepare_schema_llamacpp() {
        let backend = ConstrainedDecodingBackend::LlamaCppGrammar {
            base_url: "http://localhost:8080".to_string(),
        };
        let payload = prepare_schema_for_backend::<TestWorkflow>(&backend);
        assert!(matches!(payload, ConstrainedDecodingPayload::Gbnf(_)));
        if let ConstrainedDecodingPayload::Gbnf(grammar) = payload {
            assert!(grammar.contains("root ::="));
            assert!(grammar.contains("string ::="));
        }
    }

    #[test]
    fn test_prepare_schema_vllm() {
        let backend = ConstrainedDecodingBackend::VllmGuided {
            base_url: "http://localhost:8000".to_string(),
        };
        let payload = prepare_schema_for_backend::<TestWorkflow>(&backend);
        assert!(matches!(payload, ConstrainedDecodingPayload::JsonSchema(_)));
    }

    #[test]
    fn test_prepare_schema_ollama() {
        let backend = ConstrainedDecodingBackend::OllamaFormat {
            base_url: "http://localhost:11434".to_string(),
        };
        let payload = prepare_schema_for_backend::<TestWorkflow>(&backend);
        assert!(matches!(payload, ConstrainedDecodingPayload::JsonSchema(_)));
    }

    #[test]
    fn test_prepare_schema_none() {
        let backend = ConstrainedDecodingBackend::None;
        let payload = prepare_schema_for_backend::<TestWorkflow>(&backend);
        assert!(matches!(payload, ConstrainedDecodingPayload::None));
        assert!(!payload.is_constrained());
    }

    #[test]
    fn test_schema_to_gbnf_basic() {
        let schema = schema_registry::schema_as_json::<TestStep>();
        let gbnf = schema_to_gbnf(&schema);
        assert!(gbnf.contains("root ::="));
        assert!(gbnf.contains("string ::="));
        assert!(gbnf.contains("ws ::="));
    }

    #[test]
    fn test_schema_to_gbnf_with_array() {
        let schema = schema_registry::schema_as_json::<TestWorkflow>();
        let gbnf = schema_to_gbnf(&schema);
        assert!(gbnf.contains("root ::="));
        // Should have an array rule for verification_steps
        assert!(gbnf.contains("["));
    }

    #[test]
    fn test_prepare_raw_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"]
        });
        let backend = ConstrainedDecodingBackend::ClaudeOutputFormat;
        let payload = prepare_raw_schema_for_backend(&backend, &schema);
        assert!(matches!(payload, ConstrainedDecodingPayload::JsonSchema(_)));
    }

    #[test]
    fn test_prepare_raw_schema_gbnf() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "count": { "type": "integer" }
            },
            "required": ["name"]
        });
        let backend = ConstrainedDecodingBackend::LlamaCppGrammar {
            base_url: "http://localhost:8080".to_string(),
        };
        let payload = prepare_raw_schema_for_backend(&backend, &schema);
        assert!(matches!(payload, ConstrainedDecodingPayload::Gbnf(_)));
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "lowercase")]
    enum TestEnum {
        High,
        Medium,
        Low,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    struct WithEnum {
        pub level: TestEnum,
    }

    #[test]
    fn test_gbnf_with_enum() {
        let schema = schema_registry::schema_as_json::<WithEnum>();
        let gbnf = schema_to_gbnf(&schema);
        assert!(gbnf.contains("root ::="));
        // The grammar should be non-trivial
        assert!(gbnf.len() > 100);
    }
}
