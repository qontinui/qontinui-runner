//! Structured "Already Tried" context for reflection AI.
//!
//! Builds a summary of all previously attempted fixes (including failed ones)
//! grouped by type, with explicit "DO NOT retry" warnings for failed approaches.
//! Injected into the reflection workflow's agentic prompt to prevent the AI from
//! re-attempting known-bad strategies.

use std::collections::BTreeMap;

use crate::reflection::types::ReflectionFix;
use crate::str_utils::truncate_str;

/// Status label and sort priority for display grouping.
fn effectiveness_label(fix: &ReflectionFix) -> (&'static str, u8) {
    match fix.effectiveness.as_deref() {
        Some("effective") => ("EFFECTIVE", 0),
        Some("ineffective") => ("FAILED", 1),
        Some("caused_regression") => ("REGRESSION", 2),
        Some("inconclusive") => ("INCONCLUSIVE", 3),
        _ => ("PENDING", 4),
    }
}

/// Counts of fix outcomes for a given fix type.
#[derive(Default)]
struct TypeStats {
    effective: u32,
    ineffective: u32,
    regression: u32,
    inconclusive: u32,
    pending: u32,
}

impl TypeStats {
    fn record(&mut self, fix: &ReflectionFix) {
        match fix.effectiveness.as_deref() {
            Some("effective") => self.effective += 1,
            Some("ineffective") => self.ineffective += 1,
            Some("caused_regression") => self.regression += 1,
            Some("inconclusive") => self.inconclusive += 1,
            _ => self.pending += 1,
        }
    }

    fn summary_parts(&self) -> String {
        let mut parts = Vec::new();
        if self.effective > 0 {
            parts.push(format!("{} effective", self.effective));
        }
        if self.ineffective > 0 {
            parts.push(format!("{} ineffective", self.ineffective));
        }
        if self.regression > 0 {
            parts.push(format!("{} regression", self.regression));
        }
        if self.inconclusive > 0 {
            parts.push(format!("{} inconclusive", self.inconclusive));
        }
        if self.pending > 0 {
            parts.push(format!("{} pending", self.pending));
        }
        parts.join(", ")
    }
}

/// Build a structured summary of all previously attempted fixes.
///
/// Groups fixes by `fix_type`, shows effectiveness status for each, and
/// includes a prominent "DO NOT Re-Attempt" section for failed/regression fixes.
pub fn build_already_tried_summary(fixes: &[ReflectionFix]) -> String {
    if fixes.is_empty() {
        return "No prior reflection fixes have been recorded for this workflow.".to_string();
    }

    // Group fixes by fix_type using BTreeMap for stable ordering
    let mut by_type: BTreeMap<String, (TypeStats, Vec<&ReflectionFix>)> = BTreeMap::new();
    for fix in fixes {
        let entry = by_type
            .entry(fix.fix_type.clone())
            .or_insert_with(|| (TypeStats::default(), Vec::new()));
        entry.0.record(fix);
        entry.1.push(fix);
    }

    let mut output = String::with_capacity(4096);
    output.push_str("## Previously Attempted Fixes\n\n");

    // Per-type sections
    for (fix_type, (stats, type_fixes)) in &by_type {
        output.push_str(&format!("### {} ({})\n", fix_type, stats.summary_parts()));

        // Sort fixes within type by effectiveness priority (effective first, then failed, etc.)
        let mut sorted_fixes: Vec<&&ReflectionFix> = type_fixes.iter().collect();
        sorted_fixes.sort_by_key(|f| effectiveness_label(f).1);

        for fix in sorted_fixes {
            let (label, _) = effectiveness_label(fix);
            let desc = truncate_str(&fix.fix_description, 200);

            match fix.effectiveness.as_deref() {
                Some("ineffective") => {
                    let evidence = fix
                        .effectiveness_evidence
                        .as_deref()
                        .map(|e| format!(" -- Reason: {}", truncate_str(e, 150)))
                        .unwrap_or_default();
                    output.push_str(&format!("- [{}] {}{}\n", label, desc, evidence));
                }
                Some("caused_regression") => {
                    let evidence = fix
                        .effectiveness_evidence
                        .as_deref()
                        .map(|e| format!(" -- Caused: {}", truncate_str(e, 150)))
                        .unwrap_or_default();
                    output.push_str(&format!("- [{}] {}{}\n", label, desc, evidence));
                }
                _ => {
                    output.push_str(&format!("- [{}] {}\n", label, desc));
                }
            }
        }
        output.push('\n');
    }

    // Collect all failed and regression fixes for the DO NOT section
    let do_not_retry: Vec<&ReflectionFix> = fixes
        .iter()
        .filter(|f| {
            matches!(
                f.effectiveness.as_deref(),
                Some("ineffective") | Some("caused_regression")
            )
        })
        .collect();

    if !do_not_retry.is_empty() {
        output.push_str("## DO NOT Re-Attempt\n\n");
        output.push_str(
            "The following approaches have been tried and FAILED. Do NOT retry them:\n\n",
        );

        for fix in &do_not_retry {
            let desc = truncate_str(&fix.fix_description, 200);
            let status = if fix.effectiveness.as_deref() == Some("caused_regression") {
                "CAUSED REGRESSION"
            } else {
                "FAILED"
            };
            output.push_str(&format!("- [{}] ({}): {}\n", fix.fix_type, status, desc));
        }

        output.push_str(
            "\nInstead, try approaches that have not been attempted yet, \
             or significantly modify successful approaches.\n",
        );
    } else {
        // All fixes were effective, inconclusive, or pending -- no warnings needed
        output.push_str(
            "No failed or regressive fixes recorded. All prior approaches were effective \
             or still pending evaluation.\n",
        );
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_fix(
        fix_type: &str,
        description: &str,
        effectiveness: Option<&str>,
        evidence: Option<&str>,
    ) -> ReflectionFix {
        ReflectionFix {
            id: uuid::Uuid::new_v4().to_string(),
            source_task_run_id: "src-1".to_string(),
            reflection_task_run_id: "ref-1".to_string(),
            source_finding_id: None,
            source_knowledge_id: None,
            fix_type: fix_type.to_string(),
            fix_description: description.to_string(),
            file_changed: None,
            old_value: None,
            new_value: None,
            confidence: "high".to_string(),
            content_hash: None,
            status: "applied".to_string(),
            effectiveness: effectiveness.map(|s| s.to_string()),
            effectiveness_evidence: evidence.map(|s| s.to_string()),
            applied_at: "2025-01-01T00:00:00Z".to_string(),
            evaluated_at: None,
            created_at: "2025-01-01T00:00:00Z".to_string(),
            source_agent: None,
            reasoning: None,
            alternatives_considered: None,
            reflection_scope: None,
            project_path: None,
            applicability_context: None,
        }
    }

    #[test]
    fn test_empty_fixes() {
        let result = build_already_tried_summary(&[]);
        assert!(result.contains("No prior reflection fixes"));
    }

    #[test]
    fn test_all_effective() {
        let fixes = vec![
            make_fix(
                "selector_fix",
                "Updated login selector",
                Some("effective"),
                None,
            ),
            make_fix(
                "context_addition",
                "Added workspace path",
                Some("effective"),
                None,
            ),
        ];
        let result = build_already_tried_summary(&fixes);
        assert!(result.contains("## Previously Attempted Fixes"));
        assert!(result.contains("[EFFECTIVE]"));
        assert!(!result.contains("## DO NOT Re-Attempt"));
        assert!(result.contains("No failed or regressive fixes"));
    }

    #[test]
    fn test_mixed_effectiveness() {
        let fixes = vec![
            make_fix(
                "selector_fix",
                "Updated login selector to data-testid",
                Some("effective"),
                None,
            ),
            make_fix(
                "selector_fix",
                "Changed to CSS class selector",
                Some("ineffective"),
                Some("Selector broke after CSS refactor"),
            ),
            make_fix(
                "context_addition",
                "Added workspace root",
                Some("caused_regression"),
                Some("Caused path conflicts in CI"),
            ),
        ];
        let result = build_already_tried_summary(&fixes);

        // Verify structure
        assert!(result.contains("## Previously Attempted Fixes"));
        assert!(result.contains("### selector_fix"));
        assert!(result.contains("1 effective, 1 ineffective"));
        assert!(result.contains("[EFFECTIVE]"));
        assert!(result.contains("[FAILED]"));
        assert!(result.contains("[REGRESSION]"));

        // Verify DO NOT section
        assert!(result.contains("## DO NOT Re-Attempt"));
        assert!(result.contains("Changed to CSS class selector"));
        assert!(result.contains("Added workspace root"));
        assert!(result.contains("CAUSED REGRESSION"));
    }

    #[test]
    fn test_pending_fixes() {
        let fixes = vec![make_fix(
            "tool_config_update",
            "Increased timeout",
            None,
            None,
        )];
        let result = build_already_tried_summary(&fixes);
        assert!(result.contains("[PENDING]"));
        assert!(result.contains("1 pending"));
    }

    #[test]
    fn test_truncation() {
        let long_desc = "A".repeat(300);
        let fixes = vec![make_fix(
            "selector_fix",
            &long_desc,
            Some("ineffective"),
            None,
        )];
        let result = build_already_tried_summary(&fixes);
        // The description should be truncated to 200 chars
        assert!(!result.contains(&long_desc));
        assert!(result.contains(&"A".repeat(200)));
    }
}
