//! The comprehension **LLM inference** step — runtime-deferred.
//!
//! This wires the `claude` CLI structured-output subprocess exactly like the
//! exemplar `commands::command_interpreter::command_interpret`
//! (`claude -p --output-format json --json-schema <schema> --system-prompt …
//! --disallowed-tools … --model …`, parsed from `.structured_output`). The
//! `--json-schema` argument is the **`FunctionalSpec`'s own schema** emitted by
//! `schemars` from the frozen Rust type, so the model is constrained to emit a
//! parseable `FunctionalSpec`.
//!
//! ## Why it is NOT in the golden test
//!
//! The CLI call is non-deterministic (an LLM). Its output therefore CANNOT be
//! golden-tested; the deterministic mapping + clamp are what the golden test
//! locks down. The end-to-end call against a real `claude` CLI is exercised only
//! by `#[ignore]`d runtime tests (see `tests/llm_runtime_deferred.rs`). Per
//! `feedback_no_anthropic_api`, this MUST always be the subscription-billed
//! `claude` CLI, NEVER `api.anthropic.com`.

use qontinui_types::functional_spec::FunctionalSpec;
use schemars::schema_for;

/// The system prompt instructing the model to comprehend observation context
/// into a `FunctionalSpec` with **honest** provenance. The deterministic clamp
/// (`clamp::clamp_provenance`) is the machine-checked backstop; this prompt is
/// the first line of defence, not the guarantee.
pub const SYSTEM_PROMPT: &str = "\
You are a frontend-comprehension worker. Given an observation context (a UI \
Bridge snapshot, a discovery StateDiscoveryResult, and optionally an AWAS \
manifest) of ONE web page, emit a FunctionalSpec describing the domain \
entities, operations, ui states, navigation, and auth model the page reveals.\n\
\n\
HONESTY RULES (load-bearing):\n\
- Mark a node `observed` ONLY if the frontend directly evidences it (a rendered \
element, an observed validation firing, an AWAS-declared action/auth).\n\
- Mark a node `inferred` (with a credibility in [0,1]) when it is deduced from \
multiple observations (a relationship, a token semantic, an auth model behind a \
shell).\n\
- Mark a node `assumed` when the frontend is silent. A server-side operation \
`effect` is ALWAYS `assumed` — the frontend cannot reveal what persists.\n\
- NEVER label a guess `observed`. A downstream deterministic clamp will \
downgrade over-confident nodes, so over-claiming only loses you credibility.";

/// The argv for the comprehension `claude` CLI call. Returned (not executed) so
/// it is inspectable in tests without spawning a process. Mirrors
/// `command_interpreter.rs:136-152`.
///
/// The caller assembles the prompt (snapshot + discovery + manifest context)
/// and selects the model (`opus` / `sonnet` for comprehension, vs the exemplar's
/// `haiku` router).
///
/// ## Why the curated schema, not `schema_for!(FunctionalSpec)`
///
/// The `claude` CLI's `--json-schema` structured-output path (CLI v2.1.x) only
/// engages — i.e. populates `.structured_output` — for **flat, `$ref`-free**
/// schemas. The full `schema_for!(FunctionalSpec)` schema is ~17 KB with 37
/// `$ref`s into `$defs`; against it the CLI silently degrades to free-form prose
/// in `.result` and leaves `.structured_output` absent (verified live, runner
/// repo PR — the runtime-verification finding that drove this change). Even a
/// fully ref-inlined 30 KB variant fails the same way. A hand-curated, flat
/// schema covering exactly the **entities / operations / auth** subset the LLM
/// is asked to infer (the IR + ledger are owned by the deterministic substrate,
/// not the model) DOES engage structured output, and its result deserializes
/// cleanly into a `FunctionalSpec` because the frozen type is `#[serde(default)]`
/// throughout (no `deny_unknown_fields`). See [`comprehension_schema_json`].
pub fn comprehension_argv(prompt: &str, model: &str) -> Vec<String> {
    vec![
        "-p".into(),
        "--output-format".into(),
        "json".into(),
        "--json-schema".into(),
        comprehension_schema_json(),
        "--system-prompt".into(),
        SYSTEM_PROMPT.into(),
        // A comprehension call observes; it must not edit/exec.
        "--disallowed-tools".into(),
        "Edit,Write,Bash,NotebookEdit,Task,Agent".into(),
        "--model".into(),
        model.into(),
        prompt.into(),
    ]
}

/// The `FunctionalSpec` JSON Schema, emitted by `schemars` from the frozen Rust
/// type. This is the *canonical* schema (the contract reference) — but it is NOT
/// what the CLI `--json-schema` flag receives, because its `$ref`/`$defs` shape
/// defeats the CLI structured-output path (see [`comprehension_argv`]). Retained
/// for documentation / downstream validation use.
pub fn functional_spec_schema_json() -> String {
    let schema = schema_for!(FunctionalSpec);
    serde_json::to_string(&schema).expect("FunctionalSpec schema serializes")
}

/// The provenance enum values, shared by every confidence node in the curated
/// schema.
const PROVENANCE_ENUM: &str = r#"{"type":"string","enum":["observed","inferred","assumed"]}"#;

/// A **flat, `$ref`-free** JSON Schema covering the `entities` / `operations` /
/// `auth` subset of `FunctionalSpec` that the LLM infers. This is the schema
/// actually passed to `claude --json-schema` — it engages the CLI's
/// structured-output path (which the full `schema_for!` schema does not; see
/// [`comprehension_argv`]) and its output deserializes into a `FunctionalSpec`
/// (the absent `uiStates`/`navigation`/`assumptions` are filled by the
/// deterministic substrate downstream).
///
/// The field names are camelCase to match the frozen type's serde renames
/// (`sourceUrl`, `type`, etc.), so `serde_json::from_value::<FunctionalSpec>`
/// over the structured output succeeds directly.
pub fn comprehension_schema_json() -> String {
    let p = PROVENANCE_ENUM;
    format!(
        r#"{{
  "type": "object",
  "properties": {{
    "specVersion": {{ "type": "string" }},
    "target": {{
      "type": "object",
      "properties": {{
        "sourceUrl": {{ "type": "string" }},
        "observedAt": {{ "type": "string" }}
      }},
      "required": ["sourceUrl"]
    }},
    "entities": {{
      "type": "array",
      "items": {{
        "type": "object",
        "properties": {{
          "name": {{ "type": "string" }},
          "confidence": {p},
          "provenance": {{ "type": "string" }},
          "credibility": {{ "type": "number" }},
          "fields": {{
            "type": "array",
            "items": {{
              "type": "object",
              "properties": {{
                "name": {{ "type": "string" }},
                "type": {{ "type": "string" }},
                "confidence": {p},
                "provenance": {{ "type": "string" }},
                "credibility": {{ "type": "number" }}
              }},
              "required": ["name", "type", "confidence"]
            }}
          }}
        }},
        "required": ["name", "confidence"]
      }}
    }},
    "operations": {{
      "type": "array",
      "items": {{
        "type": "object",
        "properties": {{
          "name": {{ "type": "string" }},
          "verb": {{ "type": "string" }},
          "entity": {{ "type": "string" }},
          "confidence": {p},
          "provenance": {{ "type": "string" }},
          "inputs": {{
            "type": "array",
            "items": {{
              "type": "object",
              "properties": {{
                "field": {{ "type": "string" }},
                "required": {{ "type": "boolean" }},
                "validation": {{
                  "type": "object",
                  "properties": {{
                    "rule": {{ "type": "string" }},
                    "confidence": {p},
                    "provenance": {{ "type": "string" }}
                  }},
                  "required": ["rule", "confidence"]
                }}
              }},
              "required": ["field"]
            }}
          }},
          "effect": {{
            "type": "object",
            "properties": {{
              "confidence": {p},
              "assumption": {{ "type": "string" }},
              "provenance": {{ "type": "string" }}
            }},
            "required": ["confidence"]
          }}
        }},
        "required": ["name", "verb", "confidence"]
      }}
    }},
    "auth": {{
      "type": "object",
      "properties": {{
        "model": {{ "type": "string" }},
        "confidence": {p},
        "provenance": {{ "type": "string" }},
        "credibility": {{ "type": "number" }}
      }},
      "required": ["model", "confidence"]
    }}
  }},
  "required": ["specVersion", "target", "entities", "operations"]
}}"#
    )
}

/// Parse the `claude` CLI envelope (`{ is_error, structured_output, … }`) into a
/// `FunctionalSpec`. Extracted so the (deterministic) parse logic is unit-
/// testable without a live CLI — only the spawn is runtime-deferred.
pub fn parse_envelope(stdout: &[u8]) -> Result<FunctionalSpec, String> {
    let envelope: serde_json::Value =
        serde_json::from_slice(stdout).map_err(|e| format!("envelope JSON parse: {e}"))?;
    if envelope
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let msg = envelope
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or("(no error message)");
        return Err(format!("claude returned is_error=true: {msg}"));
    }
    let structured = envelope
        .get("structured_output")
        .ok_or_else(|| "envelope missing .structured_output".to_string())?;
    serde_json::from_value(structured.clone())
        .map_err(|e| format!("structured_output parse: {e} — raw: {structured}"))
}
