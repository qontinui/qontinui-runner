//! Dual-mode parser for agentic phase output.
//!
//! Attempts to extract structured JSON from the AI output first (a ```json block
//! with recognized keys), then falls back to the existing marker-based parsing
//! for backward compatibility. The raw output is always preserved.

use tracing::debug;

use super::agentic_output::{AgenticPhaseOutput, AgenticStatus, FindingOutput};

/// Parse raw AI output into a structured `AgenticPhaseOutput`.
///
/// Strategy:
/// 1. Try to find and parse a structured JSON summary block (```json ... ```)
/// 2. Fall back to marker-based parsing (reuses existing parsers)
/// 3. Always preserves `raw_output`
pub fn parse_agentic_output(raw_output: &str) -> AgenticPhaseOutput {
    // 1. Try structured JSON parsing
    if let Some(parsed) = try_parse_json_summary(raw_output) {
        debug!(
            "Parsed structured agentic output: status={:?}, confidence={:?}, {} files, {} findings",
            parsed.status,
            parsed.confidence,
            parsed.files_modified.len(),
            parsed.findings.len(),
        );
        return parsed;
    }

    // 2. Fall back to marker parsing
    let mut output = AgenticPhaseOutput::from_raw(raw_output.to_string());

    // Check for [UNFIXABLE_ERRORS] marker
    if raw_output.contains("[UNFIXABLE_ERRORS]") || raw_output.contains("[UNFIXABLE_ERROR]") {
        output.unfixable = true;
        output.status = AgenticStatus::Unfixable;
        // Try to extract reason from nearby text
        output.unfixable_reason = extract_unfixable_reason(raw_output);
    }

    // Parse [FINDING] markers using the existing parser
    output.findings = parse_findings_from_markers(raw_output);

    // Note: [INJECT_STEP] and [REFLECTION_FIX] markers are parsed by the
    // streaming parsers during AI session execution (claude_session/runner.rs).
    // They arrive via channels, not post-hoc parsing. We don't duplicate that here.

    if !output.findings.is_empty() || output.unfixable {
        debug!(
            "Parsed agentic output via markers: unfixable={}, {} findings",
            output.unfixable,
            output.findings.len(),
        );
    }

    output
}

/// Try to find and parse a JSON summary block in the AI output.
///
/// Looks for a fenced code block like:
/// ```json
/// { "status": "partial_success", "summary": "...", ... }
/// ```
///
/// The JSON must contain at least a "status" key to be recognized as a
/// structured agentic output (vs. any random JSON the AI might output).
fn try_parse_json_summary(raw_output: &str) -> Option<AgenticPhaseOutput> {
    // Find ```json blocks
    let mut search_start = 0;
    while let Some(block_start) = raw_output[search_start..].find("```json") {
        let abs_start = search_start + block_start + 7; // skip "```json"
        if let Some(block_end) = raw_output[abs_start..].find("```") {
            let json_str = raw_output[abs_start..abs_start + block_end].trim();

            // Try to parse as our structured output
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) {
                // Must have a "status" field to be recognized as agentic output
                if value.get("status").is_some() {
                    match serde_json::from_value::<AgenticPhaseOutput>(value) {
                        Ok(mut parsed) => {
                            parsed.raw_output = raw_output.to_string();
                            // Also check for unfixable markers in raw output as a safety net
                            if !parsed.unfixable
                                && (raw_output.contains("[UNFIXABLE_ERRORS]")
                                    || raw_output.contains("[UNFIXABLE_ERROR]"))
                            {
                                parsed.unfixable = true;
                                parsed.status = AgenticStatus::Unfixable;
                            }
                            return Some(parsed);
                        }
                        Err(e) => {
                            debug!(
                                "Found JSON block with 'status' but failed to parse as AgenticPhaseOutput: {}",
                                e
                            );
                        }
                    }
                }
            }

            search_start = abs_start + block_end + 3;
        } else {
            break;
        }
    }

    None
}

/// Extract unfixable reason from text near the [UNFIXABLE_ERRORS] marker.
fn extract_unfixable_reason(raw_output: &str) -> Option<String> {
    // Look for text on the same line or the next line after the marker
    for marker in &["[UNFIXABLE_ERRORS]", "[UNFIXABLE_ERROR]"] {
        if let Some(pos) = raw_output.find(marker) {
            let after = &raw_output[pos + marker.len()..];
            // Take text until end of line or next marker
            let reason: String = after
                .lines()
                .take(3)
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if !reason.is_empty() {
                return Some(reason);
            }
        }
    }
    None
}

/// Parse [FINDING] markers from raw output.
///
/// This is a simplified version that extracts findings without requiring
/// the full streaming parser infrastructure. For full fidelity parsing,
/// the streaming `FindingParser` in `claude_session/runner.rs` is used.
fn parse_findings_from_markers(raw_output: &str) -> Vec<FindingOutput> {
    let mut findings = Vec::new();

    // Simple regex-free line-by-line parsing for [FINDING:category:severity]
    let mut in_finding = false;
    let mut current_category = String::new();
    let mut current_severity = String::new();
    let mut current_needs_input = false;
    let mut current_resolved = false;
    let mut content_lines: Vec<String> = Vec::new();

    for line in raw_output.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("[FINDING:") {
            // Parse the opening tag
            let tag_content = trimmed
                .trim_start_matches("[FINDING:")
                .trim_end_matches(']');
            let parts: Vec<&str> = tag_content.split(':').collect();
            if parts.len() >= 2 {
                in_finding = true;
                current_category = parts[0].to_string();
                current_severity = parts[1].to_string();
                current_needs_input = parts.get(2).is_some_and(|p| *p == "needs_input");
                current_resolved = parts.get(2).is_some_and(|p| *p == "resolved");
                content_lines.clear();
            }
        } else if trimmed == "[/FINDING]" && in_finding {
            // Extract title and description from accumulated content
            let mut title = String::new();
            let mut description = String::new();

            for content_line in &content_lines {
                let cl = content_line.trim();
                if cl.starts_with("Title:") {
                    title = cl.trim_start_matches("Title:").trim().to_string();
                } else if cl.starts_with("Description:") {
                    description = cl.trim_start_matches("Description:").trim().to_string();
                } else if title.is_empty() && !cl.is_empty() {
                    title = cl.to_string();
                } else if !title.is_empty() && !cl.is_empty() {
                    if !description.is_empty() {
                        description.push(' ');
                    }
                    description.push_str(cl);
                }
            }

            if !title.is_empty() {
                findings.push(FindingOutput {
                    category: current_category.clone(),
                    severity: current_severity.clone(),
                    title,
                    description,
                    needs_input: current_needs_input,
                    resolved: current_resolved,
                });
            }

            in_finding = false;
            content_lines.clear();
        } else if in_finding {
            content_lines.push(trimmed.to_string());
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_structured_json() {
        let output = r#"I've fixed the type errors in the auth module.

```json
{
  "status": "success",
  "summary": "Fixed type annotation errors in auth/login.ts and auth/session.ts",
  "confidence": 0.85,
  "unfixable": false,
  "files_modified": [
    {"path": "src/auth/login.ts", "action": "modified"},
    {"path": "src/auth/session.ts", "action": "modified"}
  ],
  "findings": [
    {"category": "code_bug", "severity": "high", "title": "Missing return type on login()"}
  ]
}
```

The changes should pass verification now."#;

        let parsed = parse_agentic_output(output);
        assert_eq!(parsed.status, AgenticStatus::Success);
        assert_eq!(parsed.confidence, Some(0.85));
        assert_eq!(parsed.files_modified.len(), 2);
        assert_eq!(parsed.findings.len(), 1);
        assert!(!parsed.unfixable);
        assert!(!parsed.raw_output.is_empty());
    }

    #[test]
    fn test_fallback_to_markers() {
        let output = "I tried fixing the issue but the database is down.\n\n[UNFIXABLE_ERRORS]\nThe PostgreSQL server is not running.\n\n[FINDING:architecture_issue:high]\nTitle: Database connection failure\nDescription: PostgreSQL is unreachable on port 5432\n[/FINDING]";

        let parsed = parse_agentic_output(output);
        assert!(parsed.unfixable);
        assert_eq!(parsed.status, AgenticStatus::Unfixable);
        assert_eq!(parsed.findings.len(), 1);
        assert_eq!(parsed.findings[0].title, "Database connection failure");
    }

    #[test]
    fn test_no_markers_no_json() {
        let output =
            "I made some changes to fix the linting errors. The imports are now sorted correctly.";

        let parsed = parse_agentic_output(output);
        assert_eq!(parsed.status, AgenticStatus::PartialSuccess);
        assert!(!parsed.unfixable);
        assert!(parsed.findings.is_empty());
    }

    #[test]
    fn test_json_block_without_status_ignored() {
        let output = r#"Here's the config:
```json
{"key": "value", "count": 42}
```
Done."#;

        let parsed = parse_agentic_output(output);
        // Should NOT parse the random JSON as agentic output
        assert_eq!(parsed.status, AgenticStatus::PartialSuccess);
    }

    #[test]
    fn test_unfixable_reason_extraction() {
        let reason = extract_unfixable_reason(
            "Cannot proceed.\n[UNFIXABLE_ERRORS]\nThe CI server is offline and cannot run tests.",
        );
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("CI server is offline"));
    }
}
