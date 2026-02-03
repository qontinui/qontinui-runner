//! Utility functions for the Recap module.
//!
//! Contains helper functions for parsing, formatting, and data transformation.

#![allow(dead_code)]

use serde::Deserialize;

/// Map step type and name to a specific icon type for frontend rendering.
///
/// This provides more granular icon selection than just step_type.
/// For example, a "check" step can be rendered with different icons
/// depending on whether it's a lint check, type check, or build check.
pub fn get_icon_type(step_type: &str, step_name: &str) -> Option<String> {
    let name_lower = step_name.to_lowercase();

    match step_type {
        "workflow" => Some("workflow".to_string()),
        "workflow_ref" => Some("workflow_ref".to_string()),
        "action" => Some("gui_action".to_string()),
        "ai_session" => Some("prompt".to_string()),
        "test" => {
            if name_lower.contains("playwright") {
                Some("test_playwright".to_string())
            } else if name_lower.contains("vision") {
                Some("test_vision".to_string())
            } else if name_lower.contains("python") {
                Some("test_python".to_string())
            } else {
                Some("test".to_string())
            }
        }
        "check" => {
            if name_lower.contains("lint")
                || name_lower.contains("ruff")
                || name_lower.contains("eslint")
            {
                Some("check_lint".to_string())
            } else if name_lower.contains("type")
                || name_lower.contains("mypy")
                || name_lower.contains("typescript")
            {
                Some("check_typecheck".to_string())
            } else if name_lower.contains("build") {
                Some("check_build".to_string())
            } else {
                Some("check".to_string())
            }
        }
        "screenshot" => Some("screenshot".to_string()),
        "script" => Some("script".to_string()),
        "shell_command" => Some("shell_command".to_string()),
        "api_request" => Some("api_request".to_string()),
        "prompt" => Some("prompt".to_string()),
        _ => None,
    }
}

/// Parse actions summary JSON to get counts.
pub fn parse_actions_summary(json_str: &str) -> (i32, i32, i32, i32) {
    #[derive(Deserialize)]
    struct ActionsSummary {
        total: Option<i32>,
        success: Option<i32>,
        failed: Option<i32>,
        skipped: Option<i32>,
    }

    if let Ok(summary) = serde_json::from_str::<ActionsSummary>(json_str) {
        (
            summary.total.unwrap_or(0),
            summary.success.unwrap_or(0),
            summary.failed.unwrap_or(0),
            summary.skipped.unwrap_or(0),
        )
    } else {
        (0, 0, 0, 0)
    }
}

/// Calculate duration between two timestamps (ISO 8601 format).
pub fn calculate_duration_ms(start: &str, end: &str) -> Option<i64> {
    use chrono::DateTime;

    let start_dt = DateTime::parse_from_rfc3339(start).ok()?;
    let end_dt = DateTime::parse_from_rfc3339(end).ok()?;

    Some((end_dt - start_dt).num_milliseconds())
}

/// Determine the workflow phase based on workflow name patterns.
pub fn determine_workflow_phase(workflow_name: &str) -> String {
    let name_lower = workflow_name.to_lowercase();
    if name_lower.contains("setup")
        || name_lower.contains("init")
        || name_lower.contains("prepare")
        || name_lower.contains("configure")
    {
        "setup".to_string()
    } else if name_lower.contains("verify")
        || name_lower.contains("test")
        || name_lower.contains("check")
        || name_lower.contains("validate")
    {
        "verification".to_string()
    } else if name_lower.contains("summary")
        || name_lower.contains("complete")
        || name_lower.contains("cleanup")
        || name_lower.contains("finish")
    {
        "completion".to_string()
    } else {
        "agentic".to_string()
    }
}

/// Check if a step type represents an AI task.
pub fn is_ai_step_type(step_type: &str) -> bool {
    matches!(step_type, "ai" | "prompt" | "ai_session" | "claude" | "llm")
}

/// Check if a step name suggests it's setup-related.
pub fn is_step_name_setup_related(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    name_lower.contains("setup")
        || name_lower.contains("init")
        || name_lower.contains("plan")
        || name_lower.contains("configure")
        || name_lower.contains("prepare")
}

/// Check if a step name suggests it's verification-related.
pub fn is_step_name_verification_related(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    name_lower.contains("test")
        || name_lower.contains("check")
        || name_lower.contains("verify")
        || name_lower.contains("validate")
        || name_lower.contains("assert")
}

/// Check if a step name suggests it's completion-related.
pub fn is_step_name_completion_related(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    name_lower.contains("summary")
        || name_lower.contains("complete")
        || name_lower.contains("completion")
        || name_lower.contains("finish")
        || name_lower.contains("cleanup")
        || name_lower.contains("teardown")
}

/// Get display name for a stage.
pub fn get_stage_display_name(stage: &str) -> String {
    match stage {
        "setup" => "Setup",
        "agentic" => "Agentic",
        "verification" => "Verification",
        "completion" => "Completion",
        _ => stage,
    }
    .to_string()
}

/// Map TaskState names (from orchestrator) to UI stage names.
///
/// Handles both:
/// - TaskState enum names (e.g., "execution", "deterministic_verification")
/// - Direct stage names from transitions (e.g., "setup", "verification", "agentic")
pub fn map_state_to_stage(state: &str) -> Option<&'static str> {
    match state {
        // Direct stage names (from stage transitions)
        "setup" => Some("setup"),
        "verification" => Some("verification"),
        "agentic" => Some("agentic"),
        "completion" => Some("completion"),
        // TaskState enum mappings (legacy)
        "planning" => Some("setup"),
        "execution" => Some("agentic"),
        "deterministic_verification" => Some("verification"),
        "ai_verification" => Some("verification"),
        "ai_summary" => Some("completion"),
        // Non-stage states
        "initialize" | "init" | "complete" | "stopped" | "paused" => None,
        _ => None,
    }
}

/// Extract a brief summary from session content.
pub fn extract_session_summary(content: &str) -> Option<String> {
    let lines: Vec<&str> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with("---") && !l.starts_with("==="))
        .take(2)
        .collect();

    if lines.is_empty() {
        return None;
    }

    let summary = lines.join(" ");

    if summary.len() > 100 {
        Some(format!("{}...", &summary[..100]))
    } else {
        Some(summary)
    }
}

/// Extract a brief summary from the output log.
///
/// This function looks for a `## Summary` markdown section first (typically at the end
/// of AI output), and falls back to extracting the first few lines if no summary section
/// is found.
pub fn extract_output_summary(output_log: &str) -> Option<String> {
    // First, try to find a ## Summary section (case-insensitive)
    if let Some(summary) = extract_markdown_summary_section(output_log) {
        return Some(summary);
    }

    // Fallback: extract first few meaningful lines
    let lines: Vec<&str> = output_log
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty()
                && !trimmed.starts_with("[SESSION_START:")
                && !trimmed.starts_with("---")
                && !trimmed.starts_with("===")
        })
        .collect();

    if lines.is_empty() {
        return None;
    }

    let summary: String = lines.into_iter().take(3).collect::<Vec<_>>().join(" ");

    let truncated = if summary.len() > 200 {
        format!("{}...", &summary[..200])
    } else {
        summary
    };

    Some(truncated)
}

/// Extract content from a `## Summary` markdown section.
///
/// Looks for headings like "## Summary", "## summary", "### Summary", etc.
/// Returns the content between the summary heading and the next heading (or end of text).
fn extract_markdown_summary_section(text: &str) -> Option<String> {
    // Find the last occurrence of a summary heading (AI typically writes it at the end)
    let text_lower = text.to_lowercase();

    // Look for "## Summary" or "### Summary" (with various capitalizations)
    let summary_patterns = ["## summary", "### summary", "## Summary", "### Summary"];

    let mut best_pos: Option<usize> = None;
    for pattern in summary_patterns {
        // Find last occurrence of this pattern
        if let Some(pos) = text_lower.rfind(&pattern.to_lowercase()) {
            // Check that it's at the start of a line
            let is_line_start = pos == 0 || text.as_bytes().get(pos - 1) == Some(&b'\n');
            if is_line_start
                && (best_pos.is_none() || pos > best_pos.unwrap()) {
                    best_pos = Some(pos);
                }
        }
    }

    let start_pos = best_pos?;

    // Find the end of the heading line
    let after_heading = &text[start_pos..];
    let heading_end = after_heading.find('\n').unwrap_or(after_heading.len());
    let content_start = start_pos + heading_end + 1;

    if content_start >= text.len() {
        return None;
    }

    let content = &text[content_start..];

    // Find where the summary section ends (next heading or end of text)
    let section_end = content
        .find("\n## ")
        .or_else(|| content.find("\n### "))
        .or_else(|| content.find("\n# "))
        .unwrap_or(content.len());

    let summary_content = &content[..section_end];

    // Clean up the content: join lines, remove excessive whitespace
    let lines: Vec<&str> = summary_content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with("---") && !l.starts_with("==="))
        .collect();

    if lines.is_empty() {
        return None;
    }

    let summary = lines.join(" ");

    // Don't truncate - let the frontend handle display with expand/collapse
    // This allows users to see the full summary when they expand it
    Some(summary)
}

/// Extract work_summary from AI session output for a specific iteration.
pub fn extract_work_summary_for_iteration(output_log: &str, iteration: u32) -> Option<String> {
    let marker = format!("[SESSION_START:{}]", iteration);
    let start = output_log.find(&marker)?;
    let content = &output_log[start..];

    let end = content[1..]
        .find("[SESSION_START:")
        .map(|i| i + 1)
        .unwrap_or(content.len());
    let session_content = &content[..end];

    if let Some(json_start) = session_content.find("\"work_summary\"") {
        let after = &session_content[json_start..];
        if let Some(colon_pos) = after.find(':') {
            let after_colon = &after[colon_pos + 1..];
            let trimmed = after_colon.trim_start();
            if trimmed.starts_with('"') {
                let value_start = 1;
                let mut i = value_start;
                let chars: Vec<char> = trimmed.chars().collect();
                while i < chars.len() {
                    if chars[i] == '"' && (i == 0 || chars[i - 1] != '\\') {
                        let summary: String = chars[value_start..i].iter().collect();
                        if !summary.is_empty() && summary.len() < 500 {
                            let unescaped = summary
                                .replace("\\\"", "\"")
                                .replace("\\n", " ")
                                .replace("\\t", " ");
                            return Some(unescaped);
                        }
                        break;
                    }
                    i += 1;
                }
            }
        }
    }

    None
}
