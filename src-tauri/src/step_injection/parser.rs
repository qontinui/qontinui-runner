//! Parser for [INJECT_STEP]...[/INJECT_STEP] markers in AI output.
//!
//! Parses dynamic verification step definitions from Claude's output stream.
//! These markers allow the AI to inject new verification steps that will be
//! added to subsequent loop iterations.
//!
//! ## Format:
//! ```text
//! [INJECT_STEP]
//! {
//!   "type": "api_request",
//!   "name": "Verify KB entry created",
//!   "api_url": "http://localhost:9876/task-runs/xxx/knowledge",
//!   "api_method": "GET",
//!   "api_expected_status": 200
//! }
//! [/INJECT_STEP]
//! ```

use regex::Regex;
use std::sync::OnceLock;
use tracing::warn;

use crate::step_executor::ExecutionStepConfig;

/// Maximum number of injected steps per agentic phase.
const MAX_INJECTED_STEPS: usize = 20;

static INJECT_STEP_START: OnceLock<Regex> = OnceLock::new();
static INJECT_STEP_END: OnceLock<Regex> = OnceLock::new();

fn get_start_pattern() -> &'static Regex {
    INJECT_STEP_START.get_or_init(|| Regex::new(r"(?i)\[INJECT_STEP\]").unwrap())
}

fn get_end_pattern() -> &'static Regex {
    INJECT_STEP_END.get_or_init(|| Regex::new(r"(?i)\[/INJECT_STEP\]").unwrap())
}

/// Allowed step types for injected steps.
const ALLOWED_STEP_TYPES: &[&str] = &[
    "api_request",
    "check_command",
    "shell_command",
    "spec",
    "prompt",
    "test",
    "log_watch",
];

/// State machine for parsing multi-line injected step blocks.
#[derive(Debug, Default)]
pub struct InjectedStepParser {
    in_block: bool,
    current_content: String,
    steps_parsed: usize,
}

impl InjectedStepParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a line of AI output.
    /// Returns Some(ExecutionStepConfig) when a complete block is parsed.
    pub fn process_line(&mut self, line: &str) -> Option<ExecutionStepConfig> {
        let start_pattern = get_start_pattern();
        let end_pattern = get_end_pattern();

        if self.in_block {
            if end_pattern.is_match(line) {
                let content = std::mem::take(&mut self.current_content);
                self.in_block = false;
                return self.try_parse_step(&content);
            } else {
                self.current_content.push_str(line);
                self.current_content.push('\n');
                return None;
            }
        }

        if start_pattern.is_match(line) {
            if self.steps_parsed >= MAX_INJECTED_STEPS {
                warn!(
                    "INJECT_STEP: Cap of {} reached, ignoring further blocks",
                    MAX_INJECTED_STEPS
                );
                return None;
            }

            self.in_block = true;
            self.current_content.clear();

            // Check if there's content after the marker on the same line
            if let Some(pos) = line.find(']') {
                let rest = &line[pos + 1..];
                let trimmed = rest.trim();

                // Check for single-line block: [INJECT_STEP]{...}[/INJECT_STEP]
                if end_pattern.is_match(rest) {
                    let content = rest
                        .replace("[/INJECT_STEP]", "")
                        .replace("[/inject_step]", "");
                    self.in_block = false;
                    return self.try_parse_step(content.trim());
                }

                if !trimmed.is_empty() {
                    self.current_content.push_str(trimmed);
                    self.current_content.push('\n');
                }
            }
        }

        None
    }

    /// Try to parse the accumulated content as JSON into an ExecutionStepConfig.
    fn try_parse_step(&mut self, content: &str) -> Option<ExecutionStepConfig> {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            warn!("INJECT_STEP: Empty block content, skipping");
            return None;
        }

        match serde_json::from_str::<ExecutionStepConfig>(trimmed) {
            Ok(mut step) => {
                // Validate step_type
                if !ALLOWED_STEP_TYPES.contains(&step.step_type.as_str()) {
                    warn!(
                        "INJECT_STEP: Invalid step_type '{}', allowed: {:?}",
                        step.step_type, ALLOWED_STEP_TYPES
                    );
                    return None;
                }

                // Force phase to verification
                step.phase = Some("verification".to_string());

                // Generate UUID id if none provided
                if step.id.is_none() {
                    step.id = Some(uuid::Uuid::new_v4().to_string());
                }

                self.steps_parsed += 1;
                Some(step)
            }
            Err(e) => {
                warn!(
                    "INJECT_STEP: Failed to parse JSON: {} (content: {})",
                    e,
                    &trimmed[..trimmed.len().min(200)]
                );
                None
            }
        }
    }

    /// Reset the parser state.
    pub fn reset(&mut self) {
        self.in_block = false;
        self.current_content.clear();
        // Note: steps_parsed is NOT reset — cap is per agentic phase
    }

    /// Get the number of steps parsed so far.
    pub fn steps_parsed(&self) -> usize {
        self.steps_parsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_api_request_step() {
        let mut parser = InjectedStepParser::new();

        assert!(parser.process_line("[INJECT_STEP]").is_none());
        assert!(parser
            .process_line(r#"{"type": "api_request", "name": "Verify KB entry", "api_url": "http://localhost:9876/knowledge", "api_method": "GET"}"#)
            .is_none());

        let step = parser
            .process_line("[/INJECT_STEP]")
            .expect("Should parse step");

        assert_eq!(step.step_type, "api_request");
        assert_eq!(step.name, Some("Verify KB entry".to_string()));
        assert_eq!(step.phase, Some("verification".to_string()));
        assert!(step.id.is_some()); // UUID generated
    }

    #[test]
    fn test_parse_multiline_json() {
        let mut parser = InjectedStepParser::new();

        parser.process_line("[INJECT_STEP]");
        parser.process_line("{");
        parser.process_line(r#"  "type": "check_command","#);
        parser.process_line(r#"  "name": "Check file exists","#);
        parser.process_line(r#"  "promptContent": "ls -la /tmp/test""#);
        parser.process_line("}");

        let step = parser
            .process_line("[/INJECT_STEP]")
            .expect("Should parse multiline step");

        assert_eq!(step.step_type, "check_command");
        assert_eq!(step.name, Some("Check file exists".to_string()));
        assert_eq!(step.phase, Some("verification".to_string()));
    }

    #[test]
    fn test_parse_prompt_step() {
        let mut parser = InjectedStepParser::new();

        parser.process_line("[INJECT_STEP]");
        parser.process_line(
            r#"{"type": "prompt", "name": "Verify fix applied", "promptContent": "Check that the fix was correctly applied to the codebase."}"#,
        );

        let step = parser
            .process_line("[/INJECT_STEP]")
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

        parser.process_line("[INJECT_STEP]");
        parser.process_line(r#"{"type": "dangerous_action", "name": "Bad step"}"#);

        let result = parser.process_line("[/INJECT_STEP]");
        assert!(result.is_none(), "Invalid step_type should be rejected");
    }

    #[test]
    fn test_malformed_json_discarded() {
        let mut parser = InjectedStepParser::new();

        parser.process_line("[INJECT_STEP]");
        parser.process_line("this is not json at all");

        let result = parser.process_line("[/INJECT_STEP]");
        assert!(result.is_none(), "Malformed JSON should be discarded");
        assert_eq!(parser.steps_parsed(), 0);
    }

    #[test]
    fn test_empty_block_discarded() {
        let mut parser = InjectedStepParser::new();

        parser.process_line("[INJECT_STEP]");
        let result = parser.process_line("[/INJECT_STEP]");
        assert!(result.is_none(), "Empty block should be discarded");
    }

    #[test]
    fn test_multiple_blocks() {
        let mut parser = InjectedStepParser::new();

        // First block
        parser.process_line("[INJECT_STEP]");
        parser.process_line(
            r#"{"type": "api_request", "name": "Check 1", "api_url": "http://localhost:9876/check1", "api_method": "GET"}"#,
        );
        let step1 = parser
            .process_line("[/INJECT_STEP]")
            .expect("Should parse first step");
        assert_eq!(step1.name, Some("Check 1".to_string()));

        // Second block
        parser.process_line("[INJECT_STEP]");
        parser.process_line(
            r#"{"type": "prompt", "name": "Check 2", "promptContent": "Verify something"}"#,
        );
        let step2 = parser
            .process_line("[/INJECT_STEP]")
            .expect("Should parse second step");
        assert_eq!(step2.name, Some("Check 2".to_string()));

        assert_eq!(parser.steps_parsed(), 2);
    }

    #[test]
    fn test_cap_enforcement() {
        let mut parser = InjectedStepParser::new();

        // Parse MAX_INJECTED_STEPS steps
        for i in 0..MAX_INJECTED_STEPS {
            parser.process_line("[INJECT_STEP]");
            parser.process_line(&format!(
                r#"{{"type": "prompt", "name": "Step {}", "promptContent": "Check {}"}}"#,
                i, i
            ));
            let result = parser.process_line("[/INJECT_STEP]");
            assert!(result.is_some(), "Step {} should parse", i);
        }

        assert_eq!(parser.steps_parsed(), MAX_INJECTED_STEPS);

        // Next step should be rejected
        parser.process_line("[INJECT_STEP]");
        parser.process_line(
            r#"{"type": "prompt", "name": "Over limit", "promptContent": "Should fail"}"#,
        );
        let result = parser.process_line("[/INJECT_STEP]");
        assert!(result.is_none(), "Over-cap step should be rejected");
    }

    #[test]
    fn test_case_insensitive_markers() {
        let mut parser = InjectedStepParser::new();

        parser.process_line("[inject_step]");
        parser.process_line(r#"{"type": "prompt", "name": "Test", "promptContent": "hi"}"#);

        let step = parser
            .process_line("[/inject_step]")
            .expect("Should parse case-insensitive");

        assert_eq!(step.step_type, "prompt");
    }

    #[test]
    fn test_phase_always_forced_to_verification() {
        let mut parser = InjectedStepParser::new();

        parser.process_line("[INJECT_STEP]");
        // JSON explicitly sets phase to "agentic" — should be overridden
        parser.process_line(
            r#"{"type": "prompt", "name": "Test", "phase": "agentic", "promptContent": "hi"}"#,
        );

        let step = parser.process_line("[/INJECT_STEP]").expect("Should parse");

        assert_eq!(
            step.phase,
            Some("verification".to_string()),
            "Phase must be forced to verification"
        );
    }

    #[test]
    fn test_existing_id_preserved() {
        let mut parser = InjectedStepParser::new();

        parser.process_line("[INJECT_STEP]");
        parser.process_line(
            r#"{"type": "prompt", "id": "my-custom-id", "name": "Test", "promptContent": "hi"}"#,
        );

        let step = parser.process_line("[/INJECT_STEP]").expect("Should parse");

        assert_eq!(step.id, Some("my-custom-id".to_string()));
    }
}
