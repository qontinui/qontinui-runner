//! Generation Rules — Externalized workflow generation rules stored in PostgreSQL.
//!
//! Rules that govern workflow generation (schema context, hardener, verification)
//! are stored in the `generation_rules` table (see `database/pg/generation.rs`).
//! This allows the reflection system to create/modify rules at runtime without
//! Rust recompilation. This module exposes the shared types and prompt-formatting
//! helpers consumed by both the generator and the HTTP API layer.

use serde::{Deserialize, Serialize};

use crate::str_utils::truncate_str;

/// Severity tiers for progressive rule loading.
///
/// Controls how many rules are loaded based on the generation phase:
/// - `Critical` — only critical rules (always-on, high-signal, ~5 rules)
/// - `Important` — critical + important rules (default for initial generation)
/// - `Full` — all rules including normal and hint (used in fixer iterations)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleTier {
    Critical,
    Important,
    Full,
}

/// A single generation rule stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationRule {
    pub id: String,
    pub agent: String,
    pub section: String,
    pub rule_number: i32,
    pub title: String,
    pub content: String,
    pub condition: Option<String>,
    pub status: String,
    pub provenance: String,
    pub source_fix_id: Option<String>,
    pub severity: String,
    pub failure_count: i32,
    pub examples_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Input for inserting a new rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsertRuleInput {
    pub agent: String,
    pub section: String,
    pub rule_number: i32,
    pub title: String,
    pub content: String,
    pub condition: Option<String>,
    pub provenance: String,
    pub source_fix_id: Option<String>,
    pub severity: Option<String>,
    pub examples_json: Option<String>,
}

/// Input for updating an existing rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRuleInput {
    pub title: Option<String>,
    pub content: Option<String>,
    pub condition: Option<String>,
    pub status: Option<String>,
    pub rule_number: Option<i32>,
    /// Severity level: 'critical', 'important', 'normal', 'hint'
    pub severity: Option<String>,
    /// JSON array of positive/negative examples
    pub examples_json: Option<String>,
}

/// Query parameters for listing rules.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListRulesQuery {
    pub agent: Option<String>,
    pub section: Option<String>,
    pub status: Option<String>,
    pub provenance: Option<String>,
    /// Filter by severity level: 'critical', 'important', 'normal', 'hint'
    pub severity: Option<String>,
}

// ============================================================================
// Formatting Functions
// ============================================================================

/// A deserialized rule example from `examples_json`.
#[derive(Debug, Deserialize)]
struct RuleExample {
    #[serde(rename = "type")]
    example_type: String,
    output: String,
    #[serde(default)]
    explanation: String,
}

/// Format rules into numbered markdown for prompt injection.
///
/// When a rule has `examples_json`, up to 2 examples are appended as
/// GOOD/BAD pairs to provide concrete demonstrations (PromptWizard-inspired).
pub fn format_rules_as_markdown(rules: &[GenerationRule]) -> String {
    format_rules_as_markdown_with_examples(rules, false)
}

/// Format rules as markdown, optionally including examples for all rules.
///
/// By default (and via `format_rules_as_markdown`), examples are only rendered
/// for **critical-severity** rules to keep prompt context tight. Pass
/// `all_examples = true` to render examples for every rule that has them
/// (useful in fixer iterations where the extra context is worth the tokens).
pub fn format_rules_as_markdown_with_examples(
    rules: &[GenerationRule],
    all_examples: bool,
) -> String {
    rules
        .iter()
        .map(|r| {
            let mut s = format!("{}. **{}**: {}", r.rule_number, r.title, r.content);

            // Append up to 2 synthetic examples if present.
            // Only render examples for critical-severity rules by default,
            // to avoid inflating prompt context with examples on every rule.
            let show_examples = all_examples || r.severity == "critical";
            if show_examples {
                if let Some(ref json) = r.examples_json {
                    if let Ok(examples) = serde_json::from_str::<Vec<RuleExample>>(json) {
                        for ex in examples.iter().take(2) {
                            let label = if ex.example_type == "positive" {
                                "GOOD"
                            } else {
                                "BAD"
                            };
                            s.push_str(&format!("\n   - {}: `{}`", label, ex.output));
                            if !ex.explanation.is_empty() {
                                s.push_str(&format!(" — {}", ex.explanation));
                            }
                        }
                    }
                }
            }

            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ============================================================================
// Helper Functions for Auto-Apply
// ============================================================================

/// Infer which agent a reflection fix targets based on keywords in the description.
pub fn infer_agent_from_fix(description: &str) -> String {
    let lower = description.to_lowercase();
    if lower.contains("hardener") || lower.contains("convert") || lower.contains("sdk replacement")
    {
        "hardener".to_string()
    } else if lower.contains("validation")
        || lower.contains("verify command")
        || lower.contains("check step")
        || lower.contains("url validation")
    {
        "verification".to_string()
    } else {
        // Default: schema_context handles generation rules
        "schema_context".to_string()
    }
}

/// Infer which section a reflection fix targets.
pub fn infer_section_from_fix(description: &str, agent: &str) -> String {
    match agent {
        "hardener" => {
            let lower = description.to_lowercase();
            if lower.contains("critical") || lower.contains("preserve") || lower.contains("do not")
            {
                "critical_rules".to_string()
            } else {
                "conversion_rules".to_string()
            }
        }
        "verification" => "check_rules".to_string(),
        _ => {
            let lower = description.to_lowercase();
            if lower.contains("uuid")
                || lower.contains("phase")
                || lower.contains("json")
                || lower.contains("timestamp")
            {
                "important_rules".to_string()
            } else {
                "verification_quality".to_string()
            }
        }
    }
}

/// Truncate a description to a short title (max ~80 chars).
pub fn truncate_to_title(description: &str) -> String {
    let first_sentence = description.split(". ").next().unwrap_or(description);
    if first_sentence.len() <= 80 {
        first_sentence.to_string()
    } else {
        format!("{}...", truncate_str(first_sentence, 77))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_agent_from_fix() {
        assert_eq!(
            infer_agent_from_fix("hardener should convert prompts"),
            "hardener"
        );
        assert_eq!(
            infer_agent_from_fix("URL validation for check steps"),
            "verification"
        );
        assert_eq!(infer_agent_from_fix("gate step required"), "schema_context");
    }

    #[test]
    fn test_infer_section_from_fix() {
        assert_eq!(
            infer_section_from_fix("preserve step IDs", "hardener"),
            "critical_rules"
        );
        assert_eq!(
            infer_section_from_fix("convert prompts", "hardener"),
            "conversion_rules"
        );
        assert_eq!(
            infer_section_from_fix("anything", "verification"),
            "check_rules"
        );
        assert_eq!(
            infer_section_from_fix("UUID format rule", "schema_context"),
            "important_rules"
        );
        assert_eq!(
            infer_section_from_fix("gate step quality", "schema_context"),
            "verification_quality"
        );
    }

    #[test]
    fn test_truncate_to_title() {
        assert_eq!(truncate_to_title("Short title"), "Short title");
        let long = "This is a very long description that goes on and on and on and should be truncated at some point to keep it reasonable";
        assert!(truncate_to_title(long).len() <= 80);
    }

    #[test]
    fn test_format_rules_as_markdown() {
        let rules = vec![
            GenerationRule {
                id: "r1".into(),
                agent: "test".into(),
                section: "test".into(),
                rule_number: 1,
                title: "Rule One".into(),
                content: "Do this thing".into(),
                condition: None,
                status: "active".into(),
                provenance: "seed".into(),
                source_fix_id: None,
                severity: "normal".into(),
                failure_count: 0,
                examples_json: None,
                created_at: "now".into(),
                updated_at: "now".into(),
            },
            GenerationRule {
                id: "r2".into(),
                agent: "test".into(),
                section: "test".into(),
                rule_number: 2,
                title: "Rule Two".into(),
                content: "Do that thing".into(),
                condition: None,
                status: "active".into(),
                provenance: "seed".into(),
                source_fix_id: None,
                severity: "normal".into(),
                failure_count: 0,
                examples_json: None,
                created_at: "now".into(),
                updated_at: "now".into(),
            },
        ];
        let md = format_rules_as_markdown(&rules);
        assert!(md.contains("1. **Rule One**: Do this thing"));
        assert!(md.contains("2. **Rule Two**: Do that thing"));
    }

}
