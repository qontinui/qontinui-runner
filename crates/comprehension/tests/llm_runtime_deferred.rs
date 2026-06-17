//! LLM inference step — the deterministic-parse parts are tested here; the live
//! `claude` CLI call is `#[ignore]`d (runtime-deferred, non-deterministic, NOT
//! faked).

use qontinui_comprehension::llm::{
    comprehension_argv, comprehension_schema_json, functional_spec_schema_json, parse_envelope,
    SYSTEM_PROMPT,
};
use qontinui_types::functional_spec::{FunctionalSpec, SpecProvenance};

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
fn functional_spec_schema_is_the_canonical_schemars_schema() {
    // The canonical (contract-reference) schema from schemars. It carries the
    // FULL camelCase spec — but it is NOT what the CLI receives (its $ref/$defs
    // shape defeats the CLI structured-output path; see the curated schema test).
    let schema = functional_spec_schema_json();
    let v: serde_json::Value = serde_json::from_str(&schema).expect("schema is valid JSON");
    let s = v.to_string();
    assert!(s.contains("uiStates"), "canonical schema carries uiStates");
    assert!(s.contains("specVersion"));
    assert!(s.contains("assumptions"));
}

#[test]
fn cli_json_schema_arg_is_the_flat_ref_free_curated_schema() {
    // The schema actually passed to `claude --json-schema`. Runtime verification
    // (runner PR) found the full schemars schema's $ref/$defs shape silently
    // disables the CLI's structured-output path, so comprehension uses a flat,
    // $ref-free curated schema over the entities/operations/auth subset.
    let argv = comprehension_argv("PROMPT", "sonnet");
    let schema_idx = argv.iter().position(|a| a == "--json-schema").unwrap() + 1;
    let cli_schema = &argv[schema_idx];
    assert_eq!(
        cli_schema,
        &comprehension_schema_json(),
        "the CLI receives the curated schema, not schema_for!(FunctionalSpec)"
    );

    let v: serde_json::Value = serde_json::from_str(cli_schema).expect("curated schema valid JSON");
    let s = v.to_string();
    // Flat + $ref-free is the load-bearing property (engages CLI structured output).
    assert!(
        !s.contains("$ref") && !s.contains("$defs"),
        "curated schema must be $ref-free so the CLI structured-output path engages"
    );
    // It must still carry the camelCase subset the LLM infers.
    assert!(s.contains("specVersion") && s.contains("sourceUrl"));
    assert!(s.contains("entities") && s.contains("operations") && s.contains("auth"));
    // The IR + ledger are owned downstream — the LLM is NOT asked for them.
    assert!(!s.contains("uiStates") && !s.contains("navigation"));
    // Its output deserializes into a FunctionalSpec (subset → defaults fill rest).
    let sample = serde_json::json!({
        "specVersion": "0",
        "target": { "sourceUrl": "https://x.test" },
        "entities": [],
        "operations": []
    });
    let _spec: FunctionalSpec =
        serde_json::from_value(sample).expect("curated-schema output parses into FunctionalSpec");
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
/// NEVER part of the golden suite. Run explicitly with
/// `cargo test -p qontinui-comprehension --test llm_runtime_deferred -- --ignored`
/// on a host with the `claude` CLI authenticated.
///
/// This is NOT a no-op stub: it runs the genuine end-to-end leg (real CLI →
/// `parse_envelope` → `assemble_spec`) and asserts the **honesty invariants that
/// must hold regardless of the LLM's naming choices** — the load-bearing
/// properties the deterministic core guarantees over any model output:
///   - the comprehended spec round-trips through `evaluate_completeness` to
///     coverage 1.0 with no gaps (a well-formed downstream input);
///   - every operation `effect` is `Assumed` (clamped by construction);
///   - the auth model, if present, is `Inferred` with credibility ≤ 0.5;
///   - the IR (`uiStates`/`navigation`) is the deterministic discovery overlay,
///     NOT whatever the model emitted.
///
/// It deliberately does NOT assert node-set equality with the oracle: a real LLM
/// names the domain its own way (e.g. `Runner`/`PairingRequest` vs the oracle's
/// `Device`), so byte/ref equality is the wrong contract — the deterministic
/// substrate is what is pinned, not the model's content (plan §4 Phase 1.3:
/// "match the provenance distribution and node set, not byte-identity").
#[test]
#[ignore = "runtime-deferred: spawns the live, non-deterministic claude CLI"]
fn live_claude_cli_comprehends_a_snapshot() {
    use std::collections::{BTreeMap, BTreeSet};

    use qontinui_comprehension::clamp::EvidenceClass;
    use qontinui_comprehension::input::{DiscoveryResult, Snapshot};
    use qontinui_comprehension::mapping::degraded_evidence_classes;
    use qontinui_comprehension::worker::assemble_spec;
    use qontinui_types::completeness_eval::{
        enumerate_nodes, evaluate_completeness, CoverageEvidence,
    };

    const SNAPSHOT: &str = include_str!("fixtures/comprehension/connect-runner.snapshot.json");
    const DISCOVERY: &str = include_str!("fixtures/comprehension/connect-runner.discovery.json");
    const SOURCE_URL: &str = "https://app.qontinui.io/connect-runner";

    let snapshot: Snapshot = serde_json::from_str(SNAPSHOT).unwrap();
    let discovery: DiscoveryResult = serde_json::from_str(DISCOVERY).unwrap();

    let prompt = format!(
        "Comprehend the following ONE web page ({SOURCE_URL}) into a FunctionalSpec. \
         No AWAS manifest is served (DEGRADED explorer path). Emit entities, \
         operations (with inputs/validation/effect), and the auth model. Do NOT \
         emit uiStates/navigation.\n\n=== SNAPSHOT ===\n{}\n\n=== DISCOVERY ===\n{}\n",
        serde_json::to_string_pretty(&snapshot).unwrap(),
        serde_json::to_string_pretty(&discovery).unwrap(),
    );

    let argv = comprehension_argv(&prompt, "sonnet");
    let output = std::process::Command::new("claude")
        .args(&argv)
        .output()
        .expect("spawn claude CLI (authenticate it first)");
    assert!(
        output.status.success(),
        "claude exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let inferred: FunctionalSpec = parse_envelope(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "parse_envelope failed: {e}\n--- raw stdout ---\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });

    let classes: BTreeMap<String, EvidenceClass> = degraded_evidence_classes(
        &["deviceName".into(), "deviceId".into()],
        &["Device".into()],
        &["pairConfirm".into()],
        &[
            ("Device".into(), "state".into()),
            ("Device".into(), "callback".into()),
        ],
    );
    let spec = assemble_spec(
        inferred,
        &discovery,
        &classes,
        SOURCE_URL,
        Some("2026-06-14T00:00:00Z"),
    );

    // Invariant 1: round-trips to full coverage (well-formed downstream input).
    let mut covered = BTreeSet::new();
    let mut filled_assumed = BTreeSet::new();
    for node in enumerate_nodes(&spec) {
        match node.provenance {
            SpecProvenance::Assumed => {
                filled_assumed.insert(node.r#ref);
            }
            _ => {
                covered.insert(node.r#ref);
            }
        }
    }
    let verdict = evaluate_completeness(
        &spec,
        &CoverageEvidence {
            covered,
            gaps: BTreeMap::new(),
            filled_assumed,
        },
        "2026-06-14T00:00:00Z",
    );
    assert!(
        (verdict.coverage - 1.0).abs() < 1e-9 && verdict.gaps.is_empty(),
        "live-comprehended spec must round-trip to coverage 1.0; got {} gaps {:?}",
        verdict.coverage,
        verdict.gaps
    );

    // Invariant 2: every operation effect is Assumed (clamped by construction).
    for op in &spec.operations {
        if let Some(eff) = &op.effect {
            assert_eq!(
                eff.confidence,
                SpecProvenance::Assumed,
                "op {} effect must be Assumed regardless of what the LLM claimed",
                op.name
            );
        }
    }

    // Invariant 3: auth (if present) is Inferred with credibility ≤ 0.5.
    if let Some(auth) = &spec.auth {
        assert_eq!(auth.confidence, SpecProvenance::Inferred);
        assert!(auth.credibility.unwrap_or(1.0) <= 0.5 + 1e-9);
    }

    // Invariant 4: the IR is the deterministic discovery overlay, not the LLM's.
    assert_eq!(spec.ui_states.len(), 2);
    assert_eq!(spec.navigation.len(), 1);
    for st in &spec.ui_states {
        assert!(!st.assertions.is_empty());
        assert_eq!(
            st.provenance.as_ref().unwrap().file.as_deref(),
            Some("discovery/state_discovery/strategies/fingerprint.py")
        );
    }
}
