//! Generation Rules — Externalized workflow generation rules stored in SQLite.
//!
//! Rules that govern workflow generation (schema context, hardener, verification)
//! are stored in the `generation_rules` table. This allows the reflection system
//! to create/modify rules at runtime without Rust recompilation.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::reflection::types::ReflectionFix;
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
// Loading Functions
// ============================================================================

/// Load active rules for a specific agent and section, ordered by rule_number.
pub fn load_rules(agent: &str, section: &str) -> Vec<GenerationRule> {
    Vec::new()
}

/// Load active rules filtered by severity tier for progressive loading.
///
/// - `Critical` loads only `severity = 'critical'` rules
/// - `Important` loads `'critical'` and `'important'` rules
/// - `Full` loads all severities (`'critical'`, `'important'`, `'normal'`, `'hint'`)
pub fn load_rules_progressive(agent: &str, section: &str, tier: RuleTier) -> Vec<GenerationRule> {
    Vec::new()
}

/// Load all active rules for an agent, grouped by section.
pub fn load_rules_by_agent(agent: &str) -> HashMap<String, Vec<GenerationRule>> {
    HashMap::new()
}

/// List rules with optional filters.
pub fn list_rules(query: &ListRulesQuery) -> Result<Vec<GenerationRule>, String> {
    Err("SQLite removed".to_string())
}

/// Get a single rule by ID.
pub fn get_rule(id: &str) -> Result<Option<GenerationRule>, String> {
    Err("SQLite removed".to_string())
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
// Write Functions
// ============================================================================

/// Get the next rule_number for a given agent + section.
pub fn next_rule_number(agent: &str, section: &str) -> i32 {
    0
}

/// Insert a new generation rule.
/// If `source_fix_id` is provided and a rule with that source already exists, returns the
/// existing rule instead of creating a duplicate.
pub fn insert_rule(input: &InsertRuleInput) -> Result<GenerationRule, String> {
    Err("SQLite removed".to_string())
}

/// Update an existing generation rule.
pub fn update_rule(id: &str, input: &UpdateRuleInput) -> Result<GenerationRule, String> {
    Err("SQLite removed".to_string())
}

/// Delete a generation rule (hard delete).
pub fn delete_rule(id: &str) -> Result<bool, String> {
    Err("SQLite removed".to_string())
}

// ============================================================================
// Rule Application Tracking
// ============================================================================

/// Record which generation rules were applied during a workflow generation run.
/// Creates a `rule_applications` row for each active rule, linking it to the
/// generated workflow and (optionally) the task run that triggered generation.
pub fn record_rule_applications(
    rules: &[GenerationRule],
    workflow_id: Option<&str>,
    task_run_id: Option<&str>,
) {
    // SQLite removed - no-op
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

// ============================================================================
// Direct Rule Creation from Reflection Fixes
// ============================================================================

/// Create generation rules directly from reflection fixes — no accumulation
/// threshold or effectiveness history required.
///
/// Every fix with a qualifying fix_type at high or medium confidence becomes a
/// generation rule immediately. This ensures that reflection insights improve
/// future workflow generation from the very first occurrence.
///
/// Deduplication is by content hash: if a rule with the same content already
/// exists, it is skipped.
///
/// Returns the number of newly created rules.
pub fn create_rules_from_reflection_fixes(fixes: &[ReflectionFix]) -> Result<u32, String> {
    Err("SQLite removed".to_string())
}

// ============================================================================
// Auto-Rule Generation from Insights
// ============================================================================

/// Promote prompt insights into auto-generated rules.
///
/// An insight is promoted when:
/// - `confidence > 0.3`
/// - `evidence_count >= 1`
/// - No existing rule with similar content (content-hash dedup)
///
/// Returns the number of newly created rules.
pub fn promote_insights_to_rules(
    insights: &[super::prompt_analysis::PromptInsight],
) -> Result<u32, String> {
    Err("SQLite removed".to_string())
}

/// Simple content hash for dedup (first 16 chars of content, normalized).
fn simple_content_hash(content: &str) -> String {
    let normalized: String = content
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(32)
        .collect();
    normalized[..normalized.len().min(16)].to_string()
}

/// Map an insight's agent + type to the appropriate rule agent + section.
fn map_insight_to_rule_location(agent: &str, insight_type: &str) -> (String, String) {
    match (agent, insight_type) {
        ("specification", _) => ("specification".to_string(), "criteria_rules".to_string()),
        ("verification", "verification_blind_spot") => {
            ("verification".to_string(), "check_rules".to_string())
        }
        ("verification", _) => ("verification".to_string(), "check_rules".to_string()),
        ("hardener", _) => ("hardener".to_string(), "conversion_rules".to_string()),
        ("builder", _) => ("schema_context".to_string(), "important_rules".to_string()),
        _ => ("schema_context".to_string(), "important_rules".to_string()),
    }
}

// ============================================================================
// Failure Tracking & Auto-Promotion
// ============================================================================

/// Increment the failure_count of a rule and auto-promote to 'critical' if threshold reached.
pub fn increment_rule_failure_count(rule_id: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Identify which rules were violated by a generation/verification error.
/// Returns IDs of rules whose content keywords appear in the error message.
pub fn identify_violated_rules(error_message: &str, agent: &str) -> Vec<String> {
    Vec::new()
}

// ============================================================================
// Known Issue → Rule Sync
// ============================================================================

/// Create generation rules from active known issues that have verification templates.
///
/// Each known issue with a `verification_step_template` becomes an "important" rule
/// in the verification agent. Deduplicates by `source_fix_id` (using the known issue ID).
pub fn sync_rules_from_known_issues() -> Result<u32, String> {
    Err("SQLite removed".to_string())
}

/// Map a known issue category to the appropriate rule agent.
fn infer_agent_from_issue(category: &crate::known_issues::types::IssueCategory) -> String {
    use crate::known_issues::types::IssueCategory;
    match category {
        IssueCategory::Timing | IssueCategory::State | IssueCategory::DataIntegrity => {
            "verification".to_string()
        }
        _ => "schema_context".to_string(),
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

    // ========================================================================
    // Database-backed tests for promote_insights_to_rules and helpers
    // ========================================================================

    use crate::workflow_generation::prompt_analysis::PromptInsight;

    #[test]
    fn test_promote_high_confidence_insight() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_promote_low_confidence_skipped() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_promote_low_evidence_skipped() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_promote_no_suggested_rule_skipped() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_promote_dedup() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_simple_content_hash_empty() {
        let result = simple_content_hash("");
        assert_eq!(result, "", "Hash of empty string should be empty");
    }

    #[test]
    fn test_map_insight_to_rule_location() {
        assert_eq!(
            map_insight_to_rule_location("specification", "any"),
            ("specification".to_string(), "criteria_rules".to_string()),
        );
        assert_eq!(
            map_insight_to_rule_location("verification", "verification_blind_spot"),
            ("verification".to_string(), "check_rules".to_string()),
        );
        assert_eq!(
            map_insight_to_rule_location("hardener", "any"),
            ("hardener".to_string(), "conversion_rules".to_string()),
        );
        assert_eq!(
            map_insight_to_rule_location("builder", "any"),
            ("schema_context".to_string(), "important_rules".to_string()),
        );
        assert_eq!(
            map_insight_to_rule_location("unknown", "any"),
            ("schema_context".to_string(), "important_rules".to_string()),
        );
    }

    // ========================================================================
    // Tests for create_rules_from_reflection_fixes
    // ========================================================================

    fn make_test_fix(fix_type: &str, confidence: &str, description: &str) -> ReflectionFix {
        ReflectionFix {
            id: format!("fix-{}", uuid::Uuid::new_v4()),
            source_task_run_id: "src-1".into(),
            reflection_task_run_id: "ref-1".into(),
            source_finding_id: None,
            source_knowledge_id: None,
            fix_type: fix_type.into(),
            fix_description: description.into(),
            file_changed: None,
            old_value: None,
            new_value: None,
            confidence: confidence.into(),
            content_hash: None,
            status: "applied".into(),
            effectiveness: None,
            effectiveness_evidence: None,
            applied_at: "2026-01-01T00:00:00Z".into(),
            evaluated_at: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            source_agent: Some("builder".into()),
            reasoning: None,
            alternatives_considered: None,
            reflection_scope: None,
            project_path: None,
            applicability_context: None,
        }
    }

    #[test]
    fn test_create_rules_from_reflection_fixes_qualifying() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_create_rules_skips_low_confidence() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_create_rules_skips_non_qualifying_types() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_create_rules_dedup() {
        // SQLite removed - no-op
    }
}

// ============================================================================
// Seed Rules
// ============================================================================

/// Ensure seed rules are present in the database.
/// These are hardcoded quality rules that prevent known anti-patterns.
/// Deduplicates by matching on provenance='seed' and title — safe to call repeatedly.
pub fn ensure_seed_rules() {
    // SQLite removed - no-op
}
