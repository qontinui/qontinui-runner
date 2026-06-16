//! LLM inference step — the deterministic-parse parts are tested here; the live
//! `claude` CLI call is `#[ignore]`d (runtime-deferred, non-deterministic, NOT
//! faked).

use qontinui_comprehension::llm::{
    comprehension_argv, functional_spec_schema_json, parse_envelope, SYSTEM_PROMPT,
};
use qontinui_types::functional_spec::SpecProvenance;

#[test]
fn argv_matches_the_command_interpreter_structured_output_shape() {
    let argv = comprehension_argv("PROMPT-CONTEXT", "opus");
    // The non-negotiable structured-output flags (command_interpreter.rs pattern).
    assert!(argv.contains(&"-p".to_string()));
    assert_eq!(
        argv.iter()
            .position(|a| a == "--output-format")
            .map(|i| &argv[i + 1]),
        Some(&"json".to_string())
    );
    assert!(argv.contains(&"--json-schema".to_string()));
    assert!(argv.contains(&"--system-prompt".to_string()));
    assert!(argv.contains(&"--disallowed-tools".to_string()));
    // Model is the comprehension model (opus/sonnet), NOT the router's haiku.
    assert_eq!(
        argv.iter()
            .position(|a| a == "--model")
            .map(|i| &argv[i + 1]),
        Some(&"opus".to_string())
    );
    // The prompt is the trailing positional.
    assert_eq!(argv.last(), Some(&"PROMPT-CONTEXT".to_string()));
    // Edit/exec tools are disallowed — comprehension only observes.
    let disallowed = &argv[argv.iter().position(|a| a == "--disallowed-tools").unwrap() + 1];
    for t in ["Edit", "Write", "Bash"] {
        assert!(disallowed.contains(t), "{t} must be disallowed");
    }
}

#[test]
fn json_schema_arg_is_the_functional_spec_schema() {
    let schema = functional_spec_schema_json();
    // schemars emits a JSON Schema object; it must reference the FunctionalSpec
    // shape so the model's structured_output parses back into one.
    let v: serde_json::Value = serde_json::from_str(&schema).expect("schema is valid JSON");
    let s = v.to_string();
    assert!(
        s.contains("uiStates"),
        "schema must carry the camelCase spec fields"
    );
    assert!(s.contains("specVersion"));
    assert!(s.contains("assumptions"));
}

#[test]
fn system_prompt_states_the_honesty_rules() {
    // The prompt is the first line of defence (the clamp is the guarantee), but
    // it must still explicitly instruct honest provenance.
    assert!(SYSTEM_PROMPT.contains("observed"));
    assert!(SYSTEM_PROMPT.contains("inferred"));
    assert!(SYSTEM_PROMPT.contains("assumed"));
    assert!(SYSTEM_PROMPT.to_lowercase().contains("effect"));
}

#[test]
fn parse_envelope_extracts_structured_output() {
    // The deterministic envelope parse (no spawn) — proves the wire-shape
    // handling without a live CLI.
    let envelope = serde_json::json!({
        "is_error": false,
        "structured_output": {
            "specVersion": "0",
            "target": { "sourceUrl": "https://x.test" },
            "operations": [{
                "name": "pairConfirm", "verb": "create",
                "effect": { "confidence": "assumed" },
                "confidence": "observed"
            }]
        }
    });
    let spec = parse_envelope(envelope.to_string().as_bytes()).expect("parses");
    assert_eq!(spec.spec_version, "0");
    assert_eq!(spec.operations[0].name, "pairConfirm");
    assert_eq!(
        spec.operations[0].effect.as_ref().unwrap().confidence,
        SpecProvenance::Assumed
    );
}

#[test]
fn parse_envelope_surfaces_is_error() {
    let envelope = serde_json::json!({ "is_error": true, "result": "rate limited" });
    let err = parse_envelope(envelope.to_string().as_bytes()).unwrap_err();
    assert!(err.contains("rate limited"));
}

/// RUNTIME-DEFERRED: the live `claude` CLI comprehension call. NON-DETERMINISTIC
/// (an LLM) and requires a logged-in subscription CLI on the host — so it is
/// NEVER part of the golden suite and is NOT faked. Run explicitly with
/// `cargo test -p qontinui-comprehension -- --ignored live_claude_cli` on a host
/// with the `claude` CLI authenticated.
#[test]
#[ignore = "runtime-deferred: spawns the live, non-deterministic claude CLI"]
fn live_claude_cli_comprehends_a_snapshot() {
    // Intentionally not implemented as an automated assertion: the output is
    // non-deterministic. The wiring (argv + parse_envelope) is covered by the
    // deterministic tests above; this hook documents the runtime entrypoint.
    // A real run would: build the prompt from a live snapshot, spawn
    // `claude` with `comprehension_argv`, then `parse_envelope` + `assemble_spec`.
    panic!("runtime-deferred entrypoint — invoke against a live claude CLI manually");
}
