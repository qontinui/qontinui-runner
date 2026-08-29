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
        // Render in sorted key order, NOT `HashMap` iteration order.
        //
        // `variables_as_map` builds a fresh `HashMap` per call (`dag_context.rs`),
        // and `HashMap::new()` seeds a fresh `RandomState` per instance — so the
        // order varies per constructed map, not merely per process. That makes
        // this text nondeterministic across runs with identical inputs, which
        // costs three things:
        //
        //  1. The step fingerprint hashes `prompt_content`, so a `context: fresh`
        //     prompt node would compute a DIFFERENT digest on every resume and
        //     never replay — permanently re-executing and re-billing exactly the
        //     node class that is most expensive to re-run, while logging "the
        //     definition or its inputs changed" when nothing changed.
        //  2. The prompt actually sent to the model varied run to run.
        //  3. `MAX_STATE_SUMMARY_CHARS` truncation below is order-dependent, so
        //     which variables survive the cut was effectively random.
        //
        // Sorting fixes all three. Two or more resolvable variables are needed to
        // observe any of it, which is why it stayed invisible.
        let mut keys: Vec<&String> = variables.keys().collect();
        keys.sort();
        for key in keys {
            let val = &variables[key];
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

    /// Regression guard for the `context: fresh` replay defect.
    ///
    /// `variables_as_map` (`workflow/dag_context.rs`) builds a FRESH `HashMap`
    /// per call, and `HashMap::new()` seeds a fresh `RandomState` per instance
    /// — so iteration order varies per constructed map, not merely per
    /// process. This text is written into `cfg.prompt_content` by the
    /// `prepared_config` block in `dag_driver.rs` and then hashed by
    /// `node_fingerprint`, so unsorted iteration made a `context: fresh` prompt
    /// node compute a different digest on every resume: it could never replay,
    /// re-executing and re-billing the most expensive node class while logging
    /// "the definition or its inputs changed" when nothing had.
    ///
    /// This test fails if anyone reverts to `variables.iter()`.
    #[test]
    fn test_variables_render_in_sorted_order_and_are_deterministic() {
        // Insertion order is deliberately NOT sorted order, and deliberately
        // different between the two maps — with 5 keys, a `HashMap` whose
        // ordering leaked would have to reproduce the same permutation twice
        // from two independently seeded `RandomState`s.
        let mut first = HashMap::new();
        first.insert("zulu".to_string(), json!("z-val"));
        first.insert("alpha".to_string(), json!(1));
        first.insert("mike".to_string(), json!("m-val"));
        first.insert("bravo".to_string(), json!(true));
        first.insert("yankee".to_string(), json!("y-val"));

        let mut second = HashMap::new();
        second.insert("mike".to_string(), json!("m-val"));
        second.insert("yankee".to_string(), json!("y-val"));
        second.insert("bravo".to_string(), json!(true));
        second.insert("zulu".to_string(), json!("z-val"));
        second.insert("alpha".to_string(), json!(1));

        let rendered_first = build_fresh_context_prompt("Base.", 7, &first, &[], &[]);
        let rendered_second = build_fresh_context_prompt("Base.", 7, &second, &[], &[]);

        // Byte-identical output from two independently constructed maps with
        // the same contents — this is exactly what the fingerprint hashes.
        assert_eq!(
            rendered_first, rendered_second,
            "the fresh-context prompt must be byte-identical for equal variable \
             maps, or `node_fingerprint` changes on every resume and replay is \
             permanently disabled"
        );

        // And the order is specifically SORTED, not merely stable.
        let positions: Vec<usize> = ["alpha", "bravo", "mike", "yankee", "zulu"]
            .iter()
            .map(|k| {
                rendered_first
                    .find(&format!("- **{}**:", k))
                    .unwrap_or_else(|| panic!("variable line for `{}` is missing", k))
            })
            .collect();
        let mut sorted_positions = positions.clone();
        sorted_positions.sort_unstable();
        assert_eq!(
            positions, sorted_positions,
            "variable lines must appear in sorted key order; got offsets {:?}",
            positions
        );
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
