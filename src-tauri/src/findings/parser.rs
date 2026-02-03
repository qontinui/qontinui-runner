//! Parser for [FINDING:...] markers in AI output.
//!
//! Parses structured finding markers from Claude's output stream.
//!
//! ## Supported marker formats:
//!
//! - `[FINDING:category:severity]` - New finding
//! - `[FINDING:category:severity:needs_input]` - Finding that requires user input
//! - `[FINDING:category:severity:resolved]` - Finding that has been resolved
//!
//! ## Example:
//! ```text
//! [FINDING:code_bug:high]
//! Title: Null pointer exception
//! Description: Variable x is null
//! File: src/main.rs
//! Line: 42
//! [/FINDING]
//!
//! [FINDING:code_bug:high:resolved]
//! Title: Null pointer exception
//! Resolution: Added null check before dereferencing
//! [/FINDING]
//! ```

use regex::Regex;
use std::sync::OnceLock;

use super::types::{FindingCategory, FindingSeverity, ParsedFinding};

/// Regex for finding start marker: [FINDING:category:severity] or [FINDING:category:severity:modifier]
/// where modifier can be 'needs_input' or 'resolved'
static FINDING_START_PATTERN: OnceLock<Regex> = OnceLock::new();

/// Regex for finding end marker: [/FINDING]
static FINDING_END_PATTERN: OnceLock<Regex> = OnceLock::new();

fn get_start_pattern() -> &'static Regex {
    FINDING_START_PATTERN.get_or_init(|| {
        // Matches: [FINDING:category:severity] or [FINDING:category:severity:modifier]
        // where modifier can be needs_input or resolved
        // Case-insensitive to handle mixed-case output from Claude (e.g., CodeBug, Medium)
        Regex::new(r"(?i)\[FINDING:([a-z_]+):([a-z]+)(?::(needs_input|resolved))?\]").unwrap()
    })
}

fn get_end_pattern() -> &'static Regex {
    // Case-insensitive to match start pattern
    FINDING_END_PATTERN.get_or_init(|| Regex::new(r"(?i)\[/FINDING\]").unwrap())
}

/// State machine for parsing multi-line findings
#[derive(Debug, Default)]
pub struct FindingParser {
    /// Currently parsing a finding block
    in_finding_block: bool,
    /// Accumulated content for current finding
    current_content: String,
    /// Metadata for current finding (category, severity, needs_input, is_resolved)
    current_meta: Option<FindingMeta>,
}

#[derive(Debug, Clone)]
struct FindingMeta {
    category: FindingCategory,
    severity: FindingSeverity,
    needs_input: bool,
    /// Whether this finding is marked as already resolved
    is_resolved: bool,
}

impl FindingParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a line of AI output.
    /// Returns Some(ParsedFinding) when a complete finding is parsed.
    pub fn process_line(&mut self, line: &str) -> Option<ParsedFinding> {
        let start_pattern = get_start_pattern();
        let end_pattern = get_end_pattern();

        // Check for end marker first (if we're in a block)
        if self.in_finding_block {
            if end_pattern.is_match(line) {
                // End of finding block - parse accumulated content
                let meta = self.current_meta.take()?;
                let content = std::mem::take(&mut self.current_content);
                self.in_finding_block = false;

                return Some(parse_finding_content(&content, meta));
            } else {
                // Accumulate content
                self.current_content.push_str(line);
                self.current_content.push('\n');
                return None;
            }
        }

        // Check for start marker
        if let Some(caps) = start_pattern.captures(line) {
            // Lowercase captured values since from_str expects lowercase
            let category_str = caps
                .get(1)
                .map(|m| m.as_str().to_lowercase())
                .unwrap_or_else(|| "code_bug".to_string());
            let severity_str = caps
                .get(2)
                .map(|m| m.as_str().to_lowercase())
                .unwrap_or_else(|| "medium".to_string());
            // Third capture group contains the modifier (needs_input or resolved)
            let modifier = caps.get(3).map(|m| m.as_str().to_lowercase());
            let needs_input = modifier.as_deref() == Some("needs_input");
            let is_resolved = modifier.as_deref() == Some("resolved");

            let category =
                FindingCategory::from_str(&category_str).unwrap_or(FindingCategory::CodeBug);
            let severity =
                FindingSeverity::from_str(&severity_str).unwrap_or(FindingSeverity::Medium);

            self.current_meta = Some(FindingMeta {
                category,
                severity,
                needs_input,
                is_resolved,
            });
            self.in_finding_block = true;
            self.current_content.clear();

            // Check if end marker is on the same line (single-line finding)
            let marker_end = line.find(']').map(|i| i + 1).unwrap_or(0);
            let rest = &line[marker_end..];

            if end_pattern.is_match(rest) {
                // Single-line finding
                let content = rest.replace("[/FINDING]", "").trim().to_string();
                let meta = self.current_meta.take()?;
                self.in_finding_block = false;

                return Some(parse_finding_content(&content, meta));
            }

            // Start accumulating content after the marker
            if marker_end < line.len() {
                self.current_content.push_str(&line[marker_end..]);
                self.current_content.push('\n');
            }
        }

        None
    }

    /// Reset parser state (e.g., on session end)
    pub fn reset(&mut self) {
        self.in_finding_block = false;
        self.current_content.clear();
        self.current_meta = None;
    }

    /// Check if currently parsing a finding block
    pub fn is_in_block(&self) -> bool {
        self.in_finding_block
    }
}

/// Parse the content of a finding block into structured data
fn parse_finding_content(content: &str, meta: FindingMeta) -> ParsedFinding {
    let mut title = String::new();
    let mut description = String::new();
    let mut file = None;
    let mut line = None;
    let mut resolution = None;
    let mut question = None;
    let mut options = None;

    // Track which field we're currently parsing (for multi-line values)
    let mut current_field: Option<&str> = None;
    let mut current_value = String::new();

    for line_str in content.lines() {
        let trimmed = line_str.trim();

        // Check for field markers
        if let Some(rest) = trimmed.strip_prefix("Title:") {
            // Save previous field if any
            save_field(
                &mut title,
                &mut description,
                &mut file,
                &mut line,
                &mut resolution,
                &mut question,
                &mut options,
                current_field,
                &current_value,
            );
            current_field = Some("title");
            current_value = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("Description:") {
            save_field(
                &mut title,
                &mut description,
                &mut file,
                &mut line,
                &mut resolution,
                &mut question,
                &mut options,
                current_field,
                &current_value,
            );
            current_field = Some("description");
            current_value = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("File:") {
            save_field(
                &mut title,
                &mut description,
                &mut file,
                &mut line,
                &mut resolution,
                &mut question,
                &mut options,
                current_field,
                &current_value,
            );
            current_field = Some("file");
            current_value = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("Line:") {
            save_field(
                &mut title,
                &mut description,
                &mut file,
                &mut line,
                &mut resolution,
                &mut question,
                &mut options,
                current_field,
                &current_value,
            );
            current_field = Some("line");
            current_value = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("Resolution:") {
            save_field(
                &mut title,
                &mut description,
                &mut file,
                &mut line,
                &mut resolution,
                &mut question,
                &mut options,
                current_field,
                &current_value,
            );
            current_field = Some("resolution");
            current_value = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("Question:") {
            save_field(
                &mut title,
                &mut description,
                &mut file,
                &mut line,
                &mut resolution,
                &mut question,
                &mut options,
                current_field,
                &current_value,
            );
            current_field = Some("question");
            current_value = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("Options:") {
            save_field(
                &mut title,
                &mut description,
                &mut file,
                &mut line,
                &mut resolution,
                &mut question,
                &mut options,
                current_field,
                &current_value,
            );
            current_field = Some("options");
            current_value = rest.trim().to_string();
        } else if current_field.is_some() && !trimmed.is_empty() {
            // Continue accumulating current field
            if !current_value.is_empty() {
                current_value.push(' ');
            }
            current_value.push_str(trimmed);
        }
    }

    // Save final field
    save_field(
        &mut title,
        &mut description,
        &mut file,
        &mut line,
        &mut resolution,
        &mut question,
        &mut options,
        current_field,
        &current_value,
    );

    // If no structured fields found, use entire content as description
    if title.is_empty() && description.is_empty() {
        let trimmed = content.trim();
        // Use first line as title, rest as description
        if let Some((first, rest)) = trimmed.split_once('\n') {
            title = first.trim().to_string();
            description = rest.trim().to_string();
        } else {
            title = trimmed.to_string();
        }
    }

    // Ensure we have at least a title
    if title.is_empty() {
        title = format!("{:?} finding", meta.category);
    }

    ParsedFinding {
        category: meta.category,
        severity: meta.severity,
        needs_input: meta.needs_input,
        is_resolved: meta.is_resolved,
        title,
        description,
        file,
        line,
        resolution,
        question,
        options,
    }
}

fn save_field(
    title: &mut String,
    description: &mut String,
    file: &mut Option<String>,
    line: &mut Option<u32>,
    resolution: &mut Option<String>,
    question: &mut Option<String>,
    options: &mut Option<Vec<String>>,
    field: Option<&str>,
    value: &str,
) {
    if value.is_empty() {
        return;
    }

    match field {
        Some("title") => *title = value.to_string(),
        Some("description") => *description = value.to_string(),
        Some("file") => *file = Some(value.to_string()),
        Some("line") => *line = value.parse().ok(),
        Some("resolution") => *resolution = Some(value.to_string()),
        Some("question") => *question = Some(value.to_string()),
        Some("options") => {
            *options = Some(
                value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            );
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_finding() {
        let mut parser = FindingParser::new();

        assert!(parser.process_line("[FINDING:code_bug:high]").is_none());
        assert!(parser
            .process_line("Title: Null pointer exception")
            .is_none());
        assert!(parser
            .process_line("Description: Variable x is null")
            .is_none());
        assert!(parser.process_line("File: src/main.rs").is_none());
        assert!(parser.process_line("Line: 42").is_none());

        let finding = parser
            .process_line("[/FINDING]")
            .expect("Should parse finding");

        assert_eq!(finding.category, FindingCategory::CodeBug);
        assert_eq!(finding.severity, FindingSeverity::High);
        assert_eq!(finding.title, "Null pointer exception");
        assert_eq!(finding.file, Some("src/main.rs".to_string()));
        assert_eq!(finding.line, Some(42));
    }

    #[test]
    fn test_parse_needs_input_finding() {
        let mut parser = FindingParser::new();

        parser.process_line("[FINDING:todo:medium:needs_input]");
        parser.process_line("Title: Choose implementation");
        parser.process_line("Question: Which approach?");
        parser.process_line("Options: A, B, C");

        let finding = parser
            .process_line("[/FINDING]")
            .expect("Should parse finding");

        assert!(finding.needs_input);
        assert!(!finding.is_resolved);
        assert_eq!(finding.question, Some("Which approach?".to_string()));
        assert_eq!(
            finding.options,
            Some(vec!["A".to_string(), "B".to_string(), "C".to_string()])
        );
    }

    #[test]
    fn test_parse_resolved_finding() {
        let mut parser = FindingParser::new();

        parser.process_line("[FINDING:code_bug:high:resolved]");
        parser.process_line("Title: Null pointer exception");
        parser.process_line("Resolution: Added null check before dereferencing");

        let finding = parser
            .process_line("[/FINDING]")
            .expect("Should parse finding");

        assert!(finding.is_resolved);
        assert!(!finding.needs_input);
        assert_eq!(finding.category, FindingCategory::CodeBug);
        assert_eq!(finding.severity, FindingSeverity::High);
        assert_eq!(finding.title, "Null pointer exception");
        assert_eq!(
            finding.resolution,
            Some("Added null check before dereferencing".to_string())
        );
    }

    #[test]
    fn test_parse_simple_finding_not_resolved() {
        let mut parser = FindingParser::new();

        parser.process_line("[FINDING:security:critical]");
        parser.process_line("Title: SQL injection vulnerability");

        let finding = parser
            .process_line("[/FINDING]")
            .expect("Should parse finding");

        assert!(!finding.is_resolved);
        assert!(!finding.needs_input);
    }

    #[test]
    fn test_parse_mixed_case_finding() {
        let mut parser = FindingParser::new();

        // Test with mixed case - should still parse correctly
        parser.process_line("[FINDING:Code_Bug:High]");
        parser.process_line("Title: Mixed case test");
        parser.process_line("Description: Testing case-insensitive parsing");

        let finding = parser
            .process_line("[/FINDING]")
            .expect("Should parse mixed-case finding");

        // Should normalize to lowercase enum values
        assert_eq!(finding.category, FindingCategory::CodeBug);
        assert_eq!(finding.severity, FindingSeverity::High);
        assert_eq!(finding.title, "Mixed case test");
    }

    #[test]
    fn test_parse_uppercase_finding() {
        let mut parser = FindingParser::new();

        // Test with all uppercase
        parser.process_line("[FINDING:SECURITY:CRITICAL]");
        parser.process_line("Title: Uppercase test");

        let finding = parser
            .process_line("[/FINDING]")
            .expect("Should parse uppercase finding");

        assert_eq!(finding.category, FindingCategory::Security);
        assert_eq!(finding.severity, FindingSeverity::Critical);
    }
}
