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
//!   "type": "command",
//!   "name": "Verify KB entry created",
//!   "command": "curl -s http://localhost:9876/task-runs/xxx/knowledge"
//! }
//! [/INJECT_STEP]
//! ```

use regex::Regex;
use std::sync::OnceLock;
use tracing::warn;

use crate::str_utils::truncate_str;

/// What the parser needs of the step type it deserializes into.
///
/// Declared here, in the parser, and implemented by the module that owns
/// the concrete config type — so this module names no execution type and
/// the parser is exercisable against any step shape.
pub trait InjectableStep: serde::de::DeserializeOwned {
    /// The step's declared type, checked against the parser's allow-list.
    fn step_type(&self) -> &str;
    /// Force the phase an injected step runs in.
    fn set_phase(&mut self, phase: String);
    /// Whether an explicit command mode was supplied.
    fn has_command_mode(&self) -> bool;
    fn set_command_mode(&mut self, mode: String);
    /// Whether the step carried its own id.
    fn has_id(&self) -> bool;
    fn set_id(&mut self, id: String);
}

/// Maximum number of injected steps per agentic phase.
pub(crate) const MAX_INJECTED_STEPS: usize = 20;

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
    "command",
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
    /// Returns `Some(T)` when a complete block is parsed.
    pub fn process_line<T: InjectableStep>(&mut self, line: &str) -> Option<T> {
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

    /// Try to parse the accumulated content as JSON into a step config.
    fn try_parse_step<T: InjectableStep>(&mut self, content: &str) -> Option<T> {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            warn!("INJECT_STEP: Empty block content, skipping");
            return None;
        }

        match serde_json::from_str::<T>(trimmed) {
            Ok(mut step) => {
                // Validate step_type
                if !ALLOWED_STEP_TYPES.contains(&step.step_type()) {
                    warn!(
                        "INJECT_STEP: Invalid step_type '{}', allowed: {:?}",
                        step.step_type(),
                        ALLOWED_STEP_TYPES
                    );
                    return None;
                }

                // Force phase to verification
                step.set_phase("verification".to_string());

                // Default command_mode to "shell" for command-type steps
                if step.step_type() == "command" && !step.has_command_mode() {
                    step.set_command_mode("shell".to_string());
                }

                // Generate UUID id if none provided
                if !step.has_id() {
                    step.set_id(uuid::Uuid::new_v4().to_string());
                }

                self.steps_parsed += 1;
                Some(step)
            }
            Err(e) => {
                warn!(
                    "INJECT_STEP: Failed to parse JSON: {} (content: {})",
                    e,
                    truncate_str(trimmed, 200)
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
