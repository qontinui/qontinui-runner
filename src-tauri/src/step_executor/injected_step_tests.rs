//! Parser-against-the-real-config tests for
//! [`crate::step_injection::parser::InjectedStepParser`].
//!
//! They live here, beside the `InjectableStep` impl, because they exercise
//! the parser at `ExecutionStepConfig` — the type this module owns. The
//! parser itself names no execution type.

use crate::step_injection::parser::{InjectedStepParser, MAX_INJECTED_STEPS};

use super::executor_types::ExecutionStepConfig;

#[test]
fn test_parse_api_request_step() {
    let mut parser = InjectedStepParser::new();

    assert!(parser
        .process_line::<ExecutionStepConfig>("[INJECT_STEP]")
        .is_none());
    assert!(parser
        .process_line::<ExecutionStepConfig>(r#"{"type": "api_request", "name": "Verify KB entry", "api_url": "http://localhost:9876/knowledge", "api_method": "GET"}"#)
        .is_none());

    let step = parser
        .process_line::<ExecutionStepConfig>("[/INJECT_STEP]")
        .expect("Should parse step");

    assert_eq!(step.step_type, "api_request");
    assert_eq!(step.name, Some("Verify KB entry".to_string()));
    assert_eq!(step.phase, Some("verification".to_string()));
    assert!(step.id.is_some()); // UUID generated
}

#[test]
fn test_parse_multiline_json() {
    let mut parser = InjectedStepParser::new();

    parser.process_line::<ExecutionStepConfig>("[INJECT_STEP]");
    parser.process_line::<ExecutionStepConfig>("{");
    parser.process_line::<ExecutionStepConfig>(r#"  "type": "check_command","#);
    parser.process_line::<ExecutionStepConfig>(r#"  "name": "Check file exists","#);
    parser.process_line::<ExecutionStepConfig>(r#"  "promptContent": "ls -la /tmp/test""#);
    parser.process_line::<ExecutionStepConfig>("}");

    let step = parser
        .process_line::<ExecutionStepConfig>("[/INJECT_STEP]")
        .expect("Should parse multiline step");

    assert_eq!(step.step_type, "check_command");
    assert_eq!(step.name, Some("Check file exists".to_string()));
    assert_eq!(step.phase, Some("verification".to_string()));
}

#[test]
fn test_parse_prompt_step() {
    let mut parser = InjectedStepParser::new();

    parser.process_line::<ExecutionStepConfig>("[INJECT_STEP]");
    parser.process_line::<ExecutionStepConfig>(
        r#"{"type": "prompt", "name": "Verify fix applied", "promptContent": "Check that the fix was correctly applied to the codebase."}"#,
    );

    let step = parser
        .process_line::<ExecutionStepConfig>("[/INJECT_STEP]")
        .expect("Should parse prompt step");

    assert_eq!(step.step_type, "prompt");
    assert_eq!(
        step.prompt_content,
        Some("Check that the fix was correctly applied to the codebase.".to_string())
    );
}

#[test]
fn test_invalid_step_type_rejected() {
    let mut parser = InjectedStepParser::new();

    parser.process_line::<ExecutionStepConfig>("[INJECT_STEP]");
    parser
        .process_line::<ExecutionStepConfig>(r#"{"type": "dangerous_action", "name": "Bad step"}"#);

    let result = parser.process_line::<ExecutionStepConfig>("[/INJECT_STEP]");
    assert!(result.is_none(), "Invalid step_type should be rejected");
}

#[test]
fn test_malformed_json_discarded() {
    let mut parser = InjectedStepParser::new();

    parser.process_line::<ExecutionStepConfig>("[INJECT_STEP]");
    parser.process_line::<ExecutionStepConfig>("this is not json at all");

    let result = parser.process_line::<ExecutionStepConfig>("[/INJECT_STEP]");
    assert!(result.is_none(), "Malformed JSON should be discarded");
    assert_eq!(parser.steps_parsed(), 0);
}

#[test]
fn test_empty_block_discarded() {
    let mut parser = InjectedStepParser::new();

    parser.process_line::<ExecutionStepConfig>("[INJECT_STEP]");
    let result = parser.process_line::<ExecutionStepConfig>("[/INJECT_STEP]");
    assert!(result.is_none(), "Empty block should be discarded");
}

#[test]
fn test_multiple_blocks() {
    let mut parser = InjectedStepParser::new();

    // First block
    parser.process_line::<ExecutionStepConfig>("[INJECT_STEP]");
    parser.process_line::<ExecutionStepConfig>(
        r#"{"type": "api_request", "name": "Check 1", "api_url": "http://localhost:9876/check1", "api_method": "GET"}"#,
    );
    let step1 = parser
        .process_line::<ExecutionStepConfig>("[/INJECT_STEP]")
        .expect("Should parse first step");
    assert_eq!(step1.name, Some("Check 1".to_string()));

    // Second block
    parser.process_line::<ExecutionStepConfig>("[INJECT_STEP]");
    parser.process_line::<ExecutionStepConfig>(
        r#"{"type": "prompt", "name": "Check 2", "promptContent": "Verify something"}"#,
    );
    let step2 = parser
        .process_line::<ExecutionStepConfig>("[/INJECT_STEP]")
        .expect("Should parse second step");
    assert_eq!(step2.name, Some("Check 2".to_string()));

    assert_eq!(parser.steps_parsed(), 2);
}

#[test]
fn test_cap_enforcement() {
    let mut parser = InjectedStepParser::new();

    // Parse MAX_INJECTED_STEPS steps
    for i in 0..MAX_INJECTED_STEPS {
        parser.process_line::<ExecutionStepConfig>("[INJECT_STEP]");
        parser.process_line::<ExecutionStepConfig>(&format!(
            r#"{{"type": "prompt", "name": "Step {}", "promptContent": "Check {}"}}"#,
            i, i
        ));
        let result = parser.process_line::<ExecutionStepConfig>("[/INJECT_STEP]");
        assert!(result.is_some(), "Step {} should parse", i);
    }

    assert_eq!(parser.steps_parsed(), MAX_INJECTED_STEPS);

    // Next step should be rejected
    parser.process_line::<ExecutionStepConfig>("[INJECT_STEP]");
    parser.process_line::<ExecutionStepConfig>(
        r#"{"type": "prompt", "name": "Over limit", "promptContent": "Should fail"}"#,
    );
    let result = parser.process_line::<ExecutionStepConfig>("[/INJECT_STEP]");
    assert!(result.is_none(), "Over-cap step should be rejected");
}

#[test]
fn test_case_insensitive_markers() {
    let mut parser = InjectedStepParser::new();

    parser.process_line::<ExecutionStepConfig>("[inject_step]");
    parser.process_line::<ExecutionStepConfig>(
        r#"{"type": "prompt", "name": "Test", "promptContent": "hi"}"#,
    );

    let step = parser
        .process_line::<ExecutionStepConfig>("[/inject_step]")
        .expect("Should parse case-insensitive");

    assert_eq!(step.step_type, "prompt");
}

#[test]
fn test_phase_always_forced_to_verification() {
    let mut parser = InjectedStepParser::new();

    parser.process_line::<ExecutionStepConfig>("[INJECT_STEP]");
    // JSON explicitly sets phase to "agentic" — should be overridden
    parser.process_line::<ExecutionStepConfig>(
        r#"{"type": "prompt", "name": "Test", "phase": "agentic", "promptContent": "hi"}"#,
    );

    let step = parser
        .process_line::<ExecutionStepConfig>("[/INJECT_STEP]")
        .expect("Should parse");

    assert_eq!(
        step.phase,
        Some("verification".to_string()),
        "Phase must be forced to verification"
    );
}

#[test]
fn test_existing_id_preserved() {
    let mut parser = InjectedStepParser::new();

    parser.process_line::<ExecutionStepConfig>("[INJECT_STEP]");
    parser.process_line::<ExecutionStepConfig>(
        r#"{"type": "prompt", "id": "my-custom-id", "name": "Test", "promptContent": "hi"}"#,
    );

    let step = parser
        .process_line::<ExecutionStepConfig>("[/INJECT_STEP]")
        .expect("Should parse");

    assert_eq!(step.id, Some("my-custom-id".to_string()));
}
