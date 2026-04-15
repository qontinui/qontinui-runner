use serde_json::Value;
use std::collections::HashMap;

/// Maximum characters for the state summary injected into a fresh context prompt.
const MAX_STATE_SUMMARY_CHARS: usize = 8000;

/// Build a fresh-context prompt that summarizes accumulated workflow state.
///
/// When `context: fresh` is set on a DAG node, the AI starts a new conversation
/// but receives a structured summary of:
/// - Current iteration number
/// - Variables from the variable store
/// - Verification failures from the last iteration
/// - Iteration diff summaries
///
/// This replaces conversation history with an explicit state snapshot,
/// preventing context pollution in long-running loops.
pub fn build_fresh_context_prompt(
    base_prompt: &str,
    iteration: u32,
    variables: &HashMap<String, Value>,
    verification_failures: &[String],
    iteration_diffs: &[IterationDiffSummary],
) -> String {
    let mut sections: Vec<String> = Vec::new();

    // Header
    sections.push(format!(
        "## Workflow Context (Iteration {})\n\nThis is a fresh context start. Previous conversation history has been cleared to prevent context pollution.",
        iteration
    ));

    // Variables
    if !variables.is_empty() {
        let mut var_section = String::from("### Current Variables\n");
        for (key, val) in variables {
            let val_str = match val {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            var_section.push_str(&format!("- **{}**: {}\n", key, truncate(&val_str, 200)));
        }
        sections.push(var_section);
    }

    // Verification failures
    if !verification_failures.is_empty() {
        let mut fail_section =
            String::from("### Verification Failures (from previous iteration)\n");
        for failure in verification_failures {
            fail_section.push_str(&format!("- {}\n", truncate(failure, 500)));
        }
        sections.push(fail_section);
    }

    // Iteration diffs
    if !iteration_diffs.is_empty() {
        let mut diff_section = String::from("### Changes from Previous Iterations\n");
        for diff in iteration_diffs {
            diff_section.push_str(&format!(
                "**Iteration {}**: {} files changed ({} insertions, {} deletions)\n{}\n\n",
                diff.iteration,
                diff.files_changed,
                diff.insertions,
                diff.deletions,
                truncate(&diff.summary, 1000),
            ));
        }
        sections.push(diff_section);
    }

    // Combine and truncate
    let state_summary = sections.join("\n\n");
    let state_summary = truncate(&state_summary, MAX_STATE_SUMMARY_CHARS);

    format!("{}\n\n---\n\n{}", state_summary, base_prompt)
}

/// Compact summary of an iteration's changes.
#[derive(Debug, Clone)]
pub struct IterationDiffSummary {
    pub iteration: u32,
    pub files_changed: usize,
    pub insertions: u32,
    pub deletions: u32,
    pub summary: String,
}

/// Truncate a string to approximately max_chars, appending "..." if truncated.
/// Uses char boundaries to avoid panicking on multi-byte UTF-8.
fn truncate(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        s.to_string()
    } else {
        let limit = max_chars.saturating_sub(3);
        // Find a valid UTF-8 char boundary at or before `limit`
        let boundary = s
            .char_indices()
            .take_while(|(i, _)| *i < limit)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        format!("{}...", &s[..boundary])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_build_fresh_context_prompt_minimal() {
        let prompt = build_fresh_context_prompt("Do the thing.", 1, &HashMap::new(), &[], &[]);
        assert!(prompt.contains("Iteration 1"));
        assert!(prompt.contains("Do the thing."));
        assert!(prompt.contains("---"));
    }

    #[test]
    fn test_build_fresh_context_prompt_with_variables() {
        let mut vars = HashMap::new();
        vars.insert("status".to_string(), json!("running"));
        vars.insert("count".to_string(), json!(42));

        let prompt = build_fresh_context_prompt("Base.", 2, &vars, &[], &[]);
        assert!(prompt.contains("Current Variables"));
        assert!(prompt.contains("status"));
        assert!(prompt.contains("running"));
        assert!(prompt.contains("count"));
    }

    #[test]
    fn test_build_fresh_context_prompt_with_failures() {
        let failures = vec![
            "Assertion failed: expected 'pass' got 'fail'".to_string(),
            "Screenshot mismatch on step 3".to_string(),
        ];

        let prompt = build_fresh_context_prompt("Base.", 3, &HashMap::new(), &failures, &[]);
        assert!(prompt.contains("Verification Failures"));
        assert!(prompt.contains("Assertion failed"));
        assert!(prompt.contains("Screenshot mismatch"));
    }

    #[test]
    fn test_build_fresh_context_prompt_with_diffs() {
        let diffs = vec![IterationDiffSummary {
            iteration: 1,
            files_changed: 3,
            insertions: 42,
            deletions: 7,
            summary: "Refactored main loop".to_string(),
        }];

        let prompt = build_fresh_context_prompt("Base.", 2, &HashMap::new(), &[], &diffs);
        assert!(prompt.contains("Changes from Previous Iterations"));
        assert!(prompt.contains("3 files changed"));
        assert!(prompt.contains("42 insertions"));
        assert!(prompt.contains("Refactored main loop"));
    }

    #[test]
    fn test_truncate_short_string() {
        let s = "hello";
        assert_eq!(truncate(s, 100), "hello");
    }

    #[test]
    fn test_truncate_long_string() {
        let s = "a".repeat(50);
        let result = truncate(&s, 20);
        assert_eq!(result.len(), 20);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_exact_length() {
        let s = "a".repeat(20);
        let result = truncate(&s, 20);
        assert_eq!(result, s);
        assert!(!result.ends_with("..."));
    }

    #[test]
    fn test_state_summary_truncated_to_max() {
        // Build a prompt with a very long variable value that should trigger truncation
        let mut vars = HashMap::new();
        vars.insert("big_var".to_string(), json!("x".repeat(10000)));

        let prompt = build_fresh_context_prompt("Base.", 1, &vars, &[], &[]);
        // The state summary portion (before "---") should not exceed MAX_STATE_SUMMARY_CHARS
        let parts: Vec<&str> = prompt.splitn(2, "\n\n---\n\n").collect();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].len() <= MAX_STATE_SUMMARY_CHARS);
    }
}
