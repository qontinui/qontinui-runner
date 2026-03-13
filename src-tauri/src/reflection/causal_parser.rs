//! Parser for [CAUSAL_CHAIN:...] markers in AI output.
//!
//! Streaming parser that follows the same pattern as `ReflectionFixParser`.
//! Extracts causal chain relationships from the AI's reflection output.
//!
//! ## Format:
//! ```text
//! [CAUSAL_CHAIN:relationship]
//! Cause: event_type:reference_or_description
//! Effect: event_type:reference_or_description
//! Description: Human-readable explanation
//! [/CAUSAL_CHAIN]
//! ```

use regex::Regex;
use std::sync::OnceLock;

/// A parsed causal link from AI output markers.
#[derive(Debug, Clone)]
pub struct ParsedCausalLink {
    pub cause_type: String,
    pub cause_ref: String,
    pub effect_type: String,
    pub effect_ref: String,
    pub relationship: String,
    pub description: String,
}

static CAUSAL_CHAIN_START: OnceLock<Regex> = OnceLock::new();
static CAUSAL_CHAIN_END: OnceLock<Regex> = OnceLock::new();

fn get_start_pattern() -> &'static Regex {
    CAUSAL_CHAIN_START.get_or_init(|| Regex::new(r"(?i)\[CAUSAL_CHAIN:([a-z_]+)\]").unwrap())
}

fn get_end_pattern() -> &'static Regex {
    CAUSAL_CHAIN_END.get_or_init(|| Regex::new(r"(?i)\[/CAUSAL_CHAIN\]").unwrap())
}

/// Normalize relationship type to canonical form.
fn normalize_relationship(s: &str) -> String {
    match s.to_lowercase().as_str() {
        "caused" | "cause" => "caused".to_string(),
        "triggered" | "trigger" => "triggered".to_string(),
        "resolved" | "resolve" | "fixed" => "resolved".to_string(),
        "prevented" | "prevent" => "prevented".to_string(),
        other => other.to_string(),
    }
}

/// Validate event type is one of the known types.
fn normalize_event_type(s: &str) -> String {
    match s.to_lowercase().as_str() {
        "code_change" | "change" => "code_change".to_string(),
        "finding_detected" | "finding" => "finding_detected".to_string(),
        "error_occurred" | "error" => "error_occurred".to_string(),
        "fix_applied" | "fix" => "fix_applied".to_string(),
        "fix_effective" => "fix_effective".to_string(),
        "fix_ineffective" => "fix_ineffective".to_string(),
        "verification_passed" | "verify_pass" => "verification_passed".to_string(),
        "verification_failed" | "verify_fail" => "verification_failed".to_string(),
        "knowledge_created" | "knowledge" => "knowledge_created".to_string(),
        other => other.to_string(),
    }
}

/// State machine for parsing multi-line causal chain blocks.
#[derive(Debug, Default)]
pub struct CausalChainParser {
    in_block: bool,
    current_relationship: String,
    current_content: String,
}

impl CausalChainParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a line of AI output.
    /// Returns `Some(ParsedCausalLink)` when a complete block is parsed.
    pub fn process_line(&mut self, line: &str) -> Option<ParsedCausalLink> {
        let start_pattern = get_start_pattern();
        let end_pattern = get_end_pattern();

        if self.in_block {
            if end_pattern.is_match(line) {
                let content = std::mem::take(&mut self.current_content);
                let relationship = std::mem::take(&mut self.current_relationship);
                self.in_block = false;
                return parse_causal_content(&content, &relationship);
            } else {
                self.current_content.push_str(line);
                self.current_content.push('\n');
                return None;
            }
        }

        if let Some(caps) = start_pattern.captures(line) {
            let relationship_raw = caps.get(1).map(|m| m.as_str()).unwrap_or("caused");
            self.current_relationship = normalize_relationship(relationship_raw);
            self.in_block = true;
            self.current_content.clear();

            // Check for single-line block (unlikely for this format but handle it)
            let marker_end = line.find(']').map(|i| i + 1).unwrap_or(0);
            let rest = &line[marker_end..];
            if end_pattern.is_match(rest) {
                let content = rest.replace("[/CAUSAL_CHAIN]", "").trim().to_string();
                let relationship = std::mem::take(&mut self.current_relationship);
                self.in_block = false;
                return parse_causal_content(&content, &relationship);
            }

            if marker_end < line.len() {
                self.current_content.push_str(&line[marker_end..]);
                self.current_content.push('\n');
            }
        }

        None
    }

    pub fn reset(&mut self) {
        self.in_block = false;
        self.current_content.clear();
        self.current_relationship.clear();
    }
}

/// Parse content between start and end markers.
fn parse_causal_content(content: &str, relationship: &str) -> Option<ParsedCausalLink> {
    let mut cause_type = String::new();
    let mut cause_ref = String::new();
    let mut effect_type = String::new();
    let mut effect_ref = String::new();
    let mut description = String::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("Cause:") {
            let val = rest.trim();
            if let Some((etype, eref)) = val.split_once(':') {
                cause_type = normalize_event_type(etype.trim());
                cause_ref = eref.trim().to_string();
            } else {
                cause_ref = val.to_string();
            }
        } else if let Some(rest) = trimmed.strip_prefix("Effect:") {
            let val = rest.trim();
            if let Some((etype, eref)) = val.split_once(':') {
                effect_type = normalize_event_type(etype.trim());
                effect_ref = eref.trim().to_string();
            } else {
                effect_ref = val.to_string();
            }
        } else if let Some(rest) = trimmed.strip_prefix("Description:") {
            description = rest.trim().to_string();
        } else if !trimmed.is_empty() && !description.is_empty() {
            // Continuation of description
            description.push(' ');
            description.push_str(trimmed);
        }
    }

    // Must have at least cause and effect
    if cause_ref.is_empty() || effect_ref.is_empty() {
        return None;
    }

    // Default types if not specified
    if cause_type.is_empty() {
        cause_type = "code_change".to_string();
    }
    if effect_type.is_empty() {
        effect_type = "finding_detected".to_string();
    }

    Some(ParsedCausalLink {
        cause_type,
        cause_ref,
        effect_type,
        effect_ref,
        relationship: relationship.to_string(),
        description,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_full_causal_chain() {
        let mut parser = CausalChainParser::new();

        assert!(parser.process_line("[CAUSAL_CHAIN:caused]").is_none());
        assert!(parser
            .process_line("Cause: code_change:src/auth/middleware.rs")
            .is_none());
        assert!(parser
            .process_line("Effect: finding_detected:Login page redirect failed")
            .is_none());
        assert!(parser
            .process_line(
                "Description: The auth middleware change removed the session token refresh"
            )
            .is_none());

        let link = parser
            .process_line("[/CAUSAL_CHAIN]")
            .expect("Should parse causal link");

        assert_eq!(link.cause_type, "code_change");
        assert_eq!(link.cause_ref, "src/auth/middleware.rs");
        assert_eq!(link.effect_type, "finding_detected");
        assert_eq!(link.effect_ref, "Login page redirect failed");
        assert_eq!(link.relationship, "caused");
        assert_eq!(
            link.description,
            "The auth middleware change removed the session token refresh"
        );
    }

    #[test]
    fn test_parse_resolved_relationship() {
        let mut parser = CausalChainParser::new();

        parser.process_line("[CAUSAL_CHAIN:resolved]");
        parser.process_line("Cause: fix_applied:Added missing import");
        parser.process_line("Effect: error_occurred:ModuleNotFoundError in auth.py");
        parser.process_line("Description: Import fix resolved the module error");

        let link = parser
            .process_line("[/CAUSAL_CHAIN]")
            .expect("Should parse");

        assert_eq!(link.relationship, "resolved");
        assert_eq!(link.cause_type, "fix_applied");
        assert_eq!(link.effect_type, "error_occurred");
    }

    #[test]
    fn test_normalize_relationship() {
        assert_eq!(normalize_relationship("cause"), "caused");
        assert_eq!(normalize_relationship("TRIGGERED"), "triggered");
        assert_eq!(normalize_relationship("fixed"), "resolved");
        assert_eq!(normalize_relationship("prevent"), "prevented");
    }

    #[test]
    fn test_shorthand_event_types() {
        let mut parser = CausalChainParser::new();

        parser.process_line("[CAUSAL_CHAIN:triggered]");
        parser.process_line("Cause: error:Runtime crash in handler");
        parser.process_line("Effect: finding:Test suite failure");
        parser.process_line("Description: Error triggered test failure");

        let link = parser
            .process_line("[/CAUSAL_CHAIN]")
            .expect("Should parse");

        assert_eq!(link.cause_type, "error_occurred");
        assert_eq!(link.effect_type, "finding_detected");
    }

    #[test]
    fn test_missing_required_fields_returns_none() {
        let mut parser = CausalChainParser::new();

        parser.process_line("[CAUSAL_CHAIN:caused]");
        parser.process_line("Description: Missing cause and effect");

        let result = parser.process_line("[/CAUSAL_CHAIN]");
        assert!(result.is_none());
    }

    #[test]
    fn test_mixed_case() {
        let mut parser = CausalChainParser::new();

        parser.process_line("[CAUSAL_CHAIN:Caused]");
        parser.process_line("Cause: Code_Change:src/main.rs");
        parser.process_line("Effect: Error:Build failed");
        parser.process_line("Description: Test");

        let link = parser
            .process_line("[/CAUSAL_CHAIN]")
            .expect("Should parse");

        assert_eq!(link.relationship, "caused");
        assert_eq!(link.cause_type, "code_change");
        assert_eq!(link.effect_type, "error_occurred");
    }

    #[test]
    fn test_multiline_description() {
        let mut parser = CausalChainParser::new();

        parser.process_line("[CAUSAL_CHAIN:caused]");
        parser.process_line("Cause: code_change:src/api.rs");
        parser.process_line("Effect: finding:API returns 500");
        parser.process_line("Description: The route handler was moved");
        parser.process_line("  but the import path wasn't updated");

        let link = parser
            .process_line("[/CAUSAL_CHAIN]")
            .expect("Should parse");

        assert_eq!(
            link.description,
            "The route handler was moved but the import path wasn't updated"
        );
    }
}
