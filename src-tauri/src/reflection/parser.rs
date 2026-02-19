//! Parser for [REFLECTION_FIX:...] markers in AI output.
//!
//! Parses structured reflection fix markers from Claude's output stream during
//! reflection workflows. These markers allow the AI to record fixes without
//! needing HTTP tool access.
//!
//! ## Format:
//! ```text
//! [REFLECTION_FIX:fix_type:confidence]
//! Description: What was changed and why
//! File: path/to/file (optional)
//! Old: previous value (optional)
//! New: new value (optional)
//! Finding: finding-id (optional)
//! [/REFLECTION_FIX]
//! ```

use regex::Regex;
use std::sync::OnceLock;

/// Parsed reflection fix from AI output markers.
#[derive(Debug, Clone)]
pub struct ParsedReflectionFix {
    pub fix_type: String,
    pub confidence: String,
    pub description: String,
    pub file_changed: Option<String>,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub source_finding_id: Option<String>,
}

static REFLECTION_FIX_START: OnceLock<Regex> = OnceLock::new();
static REFLECTION_FIX_END: OnceLock<Regex> = OnceLock::new();

fn get_start_pattern() -> &'static Regex {
    REFLECTION_FIX_START
        .get_or_init(|| Regex::new(r"(?i)\[REFLECTION_FIX:([a-z_]+):([a-z]+)\]").unwrap())
}

fn get_end_pattern() -> &'static Regex {
    REFLECTION_FIX_END.get_or_init(|| Regex::new(r"(?i)\[/REFLECTION_FIX\]").unwrap())
}

/// Normalize fix type string to canonical form.
fn normalize_fix_type(s: &str) -> String {
    match s.to_lowercase().as_str() {
        "knowledge_base_update" | "kb_update" => "knowledge_base_update".to_string(),
        "workflow_step_rewrite" | "step_rewrite" => "workflow_step_rewrite".to_string(),
        "selector_fix" | "selector" => "selector_fix".to_string(),
        "tool_config_update" | "tool_config" => "tool_config_update".to_string(),
        "context_addition" | "context" => "context_addition".to_string(),
        "instruction_clarification" | "clarification" => "instruction_clarification".to_string(),
        other => other.to_string(),
    }
}

/// Normalize confidence string.
fn normalize_confidence(s: &str) -> String {
    match s.to_lowercase().as_str() {
        "high" | "h" => "high".to_string(),
        "medium" | "med" | "m" => "medium".to_string(),
        "low" | "l" => "low".to_string(),
        other => other.to_string(),
    }
}

/// State machine for parsing multi-line reflection fix blocks.
#[derive(Debug, Default)]
pub struct ReflectionFixParser {
    in_block: bool,
    current_content: String,
    current_fix_type: String,
    current_confidence: String,
}

impl ReflectionFixParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a line of AI output.
    /// Returns Some(ParsedReflectionFix) when a complete block is parsed.
    pub fn process_line(&mut self, line: &str) -> Option<ParsedReflectionFix> {
        let start_pattern = get_start_pattern();
        let end_pattern = get_end_pattern();

        if self.in_block {
            if end_pattern.is_match(line) {
                let content = std::mem::take(&mut self.current_content);
                let fix_type = std::mem::take(&mut self.current_fix_type);
                let confidence = std::mem::take(&mut self.current_confidence);
                self.in_block = false;
                return Some(parse_content(&content, &fix_type, &confidence));
            } else {
                self.current_content.push_str(line);
                self.current_content.push('\n');
                return None;
            }
        }

        if let Some(caps) = start_pattern.captures(line) {
            let fix_type_raw = caps
                .get(1)
                .map(|m| m.as_str())
                .unwrap_or("context_addition");
            let confidence_raw = caps.get(2).map(|m| m.as_str()).unwrap_or("medium");

            self.current_fix_type = normalize_fix_type(fix_type_raw);
            self.current_confidence = normalize_confidence(confidence_raw);
            self.in_block = true;
            self.current_content.clear();

            // Check for single-line block
            let marker_end = line.find(']').map(|i| i + 1).unwrap_or(0);
            let rest = &line[marker_end..];

            if end_pattern.is_match(rest) {
                let content = rest.replace("[/REFLECTION_FIX]", "").trim().to_string();
                let fix_type = std::mem::take(&mut self.current_fix_type);
                let confidence = std::mem::take(&mut self.current_confidence);
                self.in_block = false;
                return Some(parse_content(&content, &fix_type, &confidence));
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
        self.current_fix_type.clear();
        self.current_confidence.clear();
    }
}

/// Parse the content between start and end markers into structured data.
fn parse_content(content: &str, fix_type: &str, confidence: &str) -> ParsedReflectionFix {
    let mut description = String::new();
    let mut file_changed = None;
    let mut old_value = None;
    let mut new_value = None;
    let mut source_finding_id = None;

    let mut current_field: Option<&str> = None;
    let mut current_value = String::new();

    for line_str in content.lines() {
        let trimmed = line_str.trim();

        if let Some(rest) = trimmed.strip_prefix("Description:") {
            save_field(
                &mut description,
                &mut file_changed,
                &mut old_value,
                &mut new_value,
                &mut source_finding_id,
                current_field,
                &current_value,
            );
            current_field = Some("description");
            current_value = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("File:") {
            save_field(
                &mut description,
                &mut file_changed,
                &mut old_value,
                &mut new_value,
                &mut source_finding_id,
                current_field,
                &current_value,
            );
            current_field = Some("file");
            current_value = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("Old:") {
            save_field(
                &mut description,
                &mut file_changed,
                &mut old_value,
                &mut new_value,
                &mut source_finding_id,
                current_field,
                &current_value,
            );
            current_field = Some("old");
            current_value = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("New:") {
            save_field(
                &mut description,
                &mut file_changed,
                &mut old_value,
                &mut new_value,
                &mut source_finding_id,
                current_field,
                &current_value,
            );
            current_field = Some("new");
            current_value = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("Finding:") {
            save_field(
                &mut description,
                &mut file_changed,
                &mut old_value,
                &mut new_value,
                &mut source_finding_id,
                current_field,
                &current_value,
            );
            current_field = Some("finding");
            current_value = rest.trim().to_string();
        } else if current_field.is_some() && !trimmed.is_empty() {
            if !current_value.is_empty() {
                current_value.push(' ');
            }
            current_value.push_str(trimmed);
        }
    }

    // Save final field
    save_field(
        &mut description,
        &mut file_changed,
        &mut old_value,
        &mut new_value,
        &mut source_finding_id,
        current_field,
        &current_value,
    );

    // Fallback: if no structured fields, use entire content as description
    if description.is_empty() {
        description = content.trim().to_string();
    }

    ParsedReflectionFix {
        fix_type: fix_type.to_string(),
        confidence: confidence.to_string(),
        description,
        file_changed,
        old_value,
        new_value,
        source_finding_id,
    }
}

fn save_field(
    description: &mut String,
    file_changed: &mut Option<String>,
    old_value: &mut Option<String>,
    new_value: &mut Option<String>,
    source_finding_id: &mut Option<String>,
    field: Option<&str>,
    value: &str,
) {
    if value.is_empty() {
        return;
    }
    match field {
        Some("description") => *description = value.to_string(),
        Some("file") => *file_changed = Some(value.to_string()),
        Some("old") => *old_value = Some(value.to_string()),
        Some("new") => *new_value = Some(value.to_string()),
        Some("finding") => *source_finding_id = Some(value.to_string()),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_full_fix() {
        let mut parser = ReflectionFixParser::new();

        assert!(parser
            .process_line("[REFLECTION_FIX:context_addition:high]")
            .is_none());
        assert!(parser
            .process_line("Description: Added missing workspace path to context")
            .is_none());
        assert!(parser.process_line("File: .claude/settings.json").is_none());
        assert!(parser.process_line("Old: no workspace path").is_none());
        assert!(parser
            .process_line("New: workspace_path = /home/user/project")
            .is_none());

        let fix = parser
            .process_line("[/REFLECTION_FIX]")
            .expect("Should parse fix");

        assert_eq!(fix.fix_type, "context_addition");
        assert_eq!(fix.confidence, "high");
        assert_eq!(fix.description, "Added missing workspace path to context");
        assert_eq!(fix.file_changed, Some(".claude/settings.json".to_string()));
        assert_eq!(fix.old_value, Some("no workspace path".to_string()));
        assert_eq!(
            fix.new_value,
            Some("workspace_path = /home/user/project".to_string())
        );
    }

    #[test]
    fn test_parse_minimal_fix() {
        let mut parser = ReflectionFixParser::new();

        parser.process_line("[REFLECTION_FIX:kb_update:medium]");
        parser.process_line("Description: Updated knowledge base with selector pattern");

        let fix = parser
            .process_line("[/REFLECTION_FIX]")
            .expect("Should parse fix");

        assert_eq!(fix.fix_type, "knowledge_base_update");
        assert_eq!(fix.confidence, "medium");
        assert_eq!(
            fix.description,
            "Updated knowledge base with selector pattern"
        );
        assert!(fix.file_changed.is_none());
    }

    #[test]
    fn test_parse_with_finding_id() {
        let mut parser = ReflectionFixParser::new();

        parser.process_line("[REFLECTION_FIX:selector_fix:high]");
        parser.process_line("Description: Fixed CSS selector for login button");
        parser.process_line("Finding: abc-123-def");
        parser.process_line("File: workflows/login.json");

        let fix = parser
            .process_line("[/REFLECTION_FIX]")
            .expect("Should parse fix");

        assert_eq!(fix.fix_type, "selector_fix");
        assert_eq!(fix.source_finding_id, Some("abc-123-def".to_string()));
    }

    #[test]
    fn test_mixed_case() {
        let mut parser = ReflectionFixParser::new();

        parser.process_line("[REFLECTION_FIX:Context_Addition:High]");
        parser.process_line("Description: Test mixed case");

        let fix = parser
            .process_line("[/REFLECTION_FIX]")
            .expect("Should parse");

        assert_eq!(fix.fix_type, "context_addition");
        assert_eq!(fix.confidence, "high");
    }

    #[test]
    fn test_unstructured_content() {
        let mut parser = ReflectionFixParser::new();

        parser.process_line("[REFLECTION_FIX:tool_config:low]");
        parser.process_line("This is just a plain description without field markers");

        let fix = parser
            .process_line("[/REFLECTION_FIX]")
            .expect("Should parse");

        assert_eq!(fix.fix_type, "tool_config_update");
        assert_eq!(fix.confidence, "low");
        assert_eq!(
            fix.description,
            "This is just a plain description without field markers"
        );
    }
}
