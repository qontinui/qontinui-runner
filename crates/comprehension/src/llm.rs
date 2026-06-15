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
pub fn comprehension_argv(prompt: &str, model: &str) -> Vec<String> {
    vec![
        "-p".into(),
        "--output-format".into(),
        "json".into(),
        "--json-schema".into(),
        functional_spec_schema_json(),
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
/// type. Passed to `claude --json-schema` so the model's `.structured_output`
/// parses back into a `FunctionalSpec`.
pub fn functional_spec_schema_json() -> String {
    let schema = schema_for!(FunctionalSpec);
    serde_json::to_string(&schema).expect("FunctionalSpec schema serializes")
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
