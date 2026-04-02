//! Cross-workflow pattern mining.
//!
//! Analyzes all successful workflows in the database to extract recurring
//! patterns and automatically generate rules for the rules engine.

use crate::database::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;
use tracing::{debug, info, warn};

use super::rules::{self, InsertRuleInput, ListRulesQuery};

// ============================================================================
// Types
// ============================================================================

/// A discovered pattern across multiple workflows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinedPattern {
    /// Human-readable pattern name.
    pub name: String,
    /// Description of what this pattern represents.
    pub description: String,
    /// How many workflows exhibited this pattern.
    pub occurrence_count: u32,
    /// Success rate of workflows with this pattern (0.0-1.0).
    pub success_rate: f32,
    /// The pattern category.
    pub category: PatternCategory,
    /// Example step JSON from a real workflow.
    pub example_step: Option<serde_json::Value>,
    /// Conditions under which this pattern applies (tech stack, framework, etc.).
    pub conditions: Vec<String>,
}

/// Category of a mined pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PatternCategory {
    /// Common setup step sequences for specific tech stacks.
    SetupSequence,
    /// Verification patterns that consistently catch real bugs.
    EffectiveVerification,
    /// Step orderings that minimize execution time.
    OptimalOrdering,
    /// Common failure patterns to avoid.
    AntiPattern,
}

impl std::fmt::Display for PatternCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatternCategory::SetupSequence => write!(f, "Setup Sequence"),
            PatternCategory::EffectiveVerification => write!(f, "Effective Verification"),
            PatternCategory::OptimalOrdering => write!(f, "Optimal Ordering"),
            PatternCategory::AntiPattern => write!(f, "Anti-Pattern"),
        }
    }
}

/// Result of a pattern mining run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningReport {
    pub patterns: Vec<MinedPattern>,
    pub workflows_analyzed: u32,
    pub duration_ms: u64,
}

// ============================================================================
// Internal helpers
// ============================================================================

/// A simplified representation of a workflow step for pattern comparison.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct StepSignature {
    step_type: String,
    command_pattern: String,
}

/// Extract a command pattern from a step JSON value.
///
/// Normalizes the command by stripping project-specific paths and arguments
/// so that structurally identical commands match across workflows.
fn extract_step_signature(step: &serde_json::Value) -> Option<StepSignature> {
    let step_type = step
        .get("type")
        .or_else(|| step.get("step_type"))
        .or_else(|| step.get("check_type"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let command_pattern = step
        .get("command")
        .or_else(|| step.get("tool"))
        .or_else(|| step.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if step_type == "unknown" && command_pattern.is_empty() {
        return None;
    }

    Some(StepSignature {
        step_type,
        command_pattern,
    })
}

/// Parse step arrays from a workflow data JSON blob.
fn parse_steps(data: &serde_json::Value, phase: &str) -> Vec<serde_json::Value> {
    data.get(phase)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

// ============================================================================
// Public API
// ============================================================================

/// Analyze all workflows in the database to extract patterns.
pub fn mine_patterns(conn: &Connection) -> Result<MiningReport, String> {
    Err("SQLite removed".to_string())
}

/// Convert mined patterns into generation rules for the rules engine.
///
/// Only patterns with `occurrence_count >= min_occurrences` are promoted to rules.
/// Returns the count of newly created rules.
pub fn patterns_to_rules(
    conn: &Connection,
    patterns: &[MinedPattern],
    min_occurrences: u32,
) -> Result<u32, String> {
    Err("SQLite removed".to_string())
}

/// Format mined patterns as context for the builder prompt.
pub fn format_patterns_for_prompt(patterns: &[MinedPattern]) -> String {
    if patterns.is_empty() {
        return String::new();
    }

    let top_n = 10;
    let top_patterns: Vec<&MinedPattern> = patterns.iter().take(top_n).collect();

    // Group by category
    let mut by_category: HashMap<String, Vec<&MinedPattern>> = HashMap::new();
    for p in &top_patterns {
        by_category
            .entry(p.category.to_string())
            .or_default()
            .push(p);
    }

    let mut output = String::from("## Mined Workflow Patterns\n\n");
    output.push_str(
        "The following patterns were automatically discovered from successful workflows. \
         Apply them where relevant.\n\n",
    );

    for (category, cat_patterns) in &by_category {
        output.push_str(&format!("### {}\n\n", category));
        for p in cat_patterns {
            output.push_str(&format!(
                "- **{}** (seen in {} workflows, {:.0}% success rate): {}\n",
                p.name,
                p.occurrence_count,
                p.success_rate * 100.0,
                p.description,
            ));
            if let Some(ref example) = p.example_step {
                if let Ok(json_str) = serde_json::to_string(example) {
                    if json_str.len() < 300 {
                        output.push_str(&format!("  Example: `{}`\n", json_str));
                    }
                }
            }
        }
        output.push('\n');
    }

    output
}

// ============================================================================
// Mining sub-routines
// ============================================================================

/// Mine common setup step sequences that appear in 3+ workflows.
fn mine_setup_sequences(
    workflows: &[(String, serde_json::Value)],
    successful_ids: &std::collections::HashSet<String>,
) -> Vec<MinedPattern> {
    // Count (step_type, command_pattern) pairs across all setup phases
    let mut sig_counts: HashMap<StepSignature, (u32, u32, Option<serde_json::Value>)> =
        HashMap::new();

    for (id, data) in workflows {
        let is_success = successful_ids.contains(id);
        let steps = parse_steps(data, "setup_steps");
        for step in &steps {
            if let Some(sig) = extract_step_signature(step) {
                let entry = sig_counts.entry(sig).or_insert((0, 0, None));
                entry.0 += 1; // total
                if is_success {
                    entry.1 += 1; // success count
                }
                if entry.2.is_none() {
                    entry.2 = Some(step.clone());
                }
            }
        }
    }

    sig_counts
        .into_iter()
        .filter(|(_, (total, _, _))| *total >= 3)
        .map(|(sig, (total, successes, example))| {
            let success_rate = if total > 0 {
                successes as f32 / total as f32
            } else {
                0.0
            };
            MinedPattern {
                name: format!("Setup: {} ({})", sig.step_type, sig.command_pattern),
                description: format!(
                    "Setup step using '{}' with command pattern '{}' appears across multiple workflows.",
                    sig.step_type, sig.command_pattern,
                ),
                occurrence_count: total,
                success_rate,
                category: PatternCategory::SetupSequence,
                example_step: example,
                conditions: vec![],
            }
        })
        .collect()
}

/// Mine verification steps from successful workflows.
fn mine_effective_verifications(
    workflows: &[(String, serde_json::Value)],
    successful_ids: &std::collections::HashSet<String>,
) -> Vec<MinedPattern> {
    let mut sig_counts: HashMap<StepSignature, (u32, u32, Option<serde_json::Value>)> =
        HashMap::new();

    for (id, data) in workflows {
        let is_success = successful_ids.contains(id);
        let steps = parse_steps(data, "verification_steps");
        for step in &steps {
            if let Some(sig) = extract_step_signature(step) {
                let entry = sig_counts.entry(sig).or_insert((0, 0, None));
                entry.0 += 1;
                if is_success {
                    entry.1 += 1;
                }
                if entry.2.is_none() {
                    entry.2 = Some(step.clone());
                }
            }
        }
    }

    sig_counts
        .into_iter()
        .filter(|(_, (total, successes, _))| *total >= 3 && *successes > 0)
        .map(|(sig, (total, successes, example))| {
            let success_rate = successes as f32 / total as f32;
            MinedPattern {
                name: format!("Verification: {} ({})", sig.step_type, sig.command_pattern),
                description: format!(
                    "Verification step '{}' with '{}' is associated with successful workflow executions.",
                    sig.step_type, sig.command_pattern,
                ),
                occurrence_count: total,
                success_rate,
                category: PatternCategory::EffectiveVerification,
                example_step: example,
                conditions: vec![],
            }
        })
        .collect()
}

/// Mine step orderings that appear in fast, successful workflows.
fn mine_optimal_orderings(
    workflows: &[(String, serde_json::Value)],
    successful_ids: &std::collections::HashSet<String>,
) -> Vec<MinedPattern> {
    // Look at pairs of consecutive step types across all phases
    let mut pair_counts: HashMap<(String, String), (u32, u32)> = HashMap::new();

    for (id, data) in workflows {
        let is_success = successful_ids.contains(id);
        // Combine all phases in order
        let mut all_steps: Vec<serde_json::Value> = Vec::new();
        for phase in &[
            "setup_steps",
            "verification_steps",
            "agentic_steps",
            "completion_steps",
        ] {
            all_steps.extend(parse_steps(data, phase));
        }

        let sigs: Vec<String> = all_steps
            .iter()
            .filter_map(|s| extract_step_signature(s).map(|sig| sig.step_type))
            .collect();

        for window in sigs.windows(2) {
            let pair = (window[0].clone(), window[1].clone());
            let entry = pair_counts.entry(pair).or_insert((0, 0));
            entry.0 += 1;
            if is_success {
                entry.1 += 1;
            }
        }
    }

    pair_counts
        .into_iter()
        .filter(|(_, (total, successes))| *total >= 3 && *successes > 0)
        .map(|((first, second), (total, successes))| {
            let success_rate = successes as f32 / total as f32;
            MinedPattern {
                name: format!("Order: {} -> {}", first, second),
                description: format!(
                    "Step ordering '{}' followed by '{}' is a common pattern in successful workflows.",
                    first, second,
                ),
                occurrence_count: total,
                success_rate,
                category: PatternCategory::OptimalOrdering,
                example_step: None,
                conditions: vec![],
            }
        })
        .collect()
}

/// Mine step patterns that appear disproportionately in failed workflows.
fn mine_anti_patterns(
    workflows: &[(String, serde_json::Value)],
    failed_ids: &std::collections::HashSet<String>,
    successful_ids: &std::collections::HashSet<String>,
) -> Vec<MinedPattern> {
    let mut sig_counts: HashMap<StepSignature, (u32, u32, u32, Option<serde_json::Value>)> =
        HashMap::new(); // (total, fail_count, success_count, example)

    for (id, data) in workflows {
        let is_failure = failed_ids.contains(id);
        let is_success = successful_ids.contains(id);

        let mut all_steps: Vec<serde_json::Value> = Vec::new();
        for phase in &[
            "setup_steps",
            "verification_steps",
            "agentic_steps",
            "completion_steps",
        ] {
            all_steps.extend(parse_steps(data, phase));
        }

        for step in &all_steps {
            if let Some(sig) = extract_step_signature(step) {
                let entry = sig_counts.entry(sig).or_insert((0, 0, 0, None));
                entry.0 += 1;
                if is_failure {
                    entry.1 += 1;
                }
                if is_success {
                    entry.2 += 1;
                }
                if entry.3.is_none() {
                    entry.3 = Some(step.clone());
                }
            }
        }
    }

    sig_counts
        .into_iter()
        .filter(|(_, (total, fail_count, success_count, _))| {
            // Must appear in 3+ workflows and have a higher failure rate than success rate
            *total >= 3 && *fail_count > *success_count
        })
        .map(|(sig, (total, fail_count, _success_count, example))| {
            let failure_rate = fail_count as f32 / total as f32;
            MinedPattern {
                name: format!("Anti: {} ({})", sig.step_type, sig.command_pattern),
                description: format!(
                    "Step '{}' with '{}' appears more often in failing workflows. Consider alternatives.",
                    sig.step_type, sig.command_pattern,
                ),
                occurrence_count: total,
                success_rate: 1.0 - failure_rate, // Invert: low success rate = bad
                category: PatternCategory::AntiPattern,
                example_step: example,
                conditions: vec![],
            }
        })
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
use crate::database::Connection;

    #[test]
    fn test_extract_step_signature_basic() {
        let step = serde_json::json!({
            "type": "check",
            "command": "cargo test"
        });
        let sig = extract_step_signature(&step).unwrap();
        assert_eq!(sig.step_type, "check");
        assert_eq!(sig.command_pattern, "cargo test");
    }

    #[test]
    fn test_extract_step_signature_empty() {
        let step = serde_json::json!({});
        assert!(extract_step_signature(&step).is_none());
    }

    #[test]
    fn test_format_patterns_for_prompt_empty() {
        let result = format_patterns_for_prompt(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_format_patterns_for_prompt_with_data() {
        let patterns = vec![MinedPattern {
            name: "Setup: check (cargo test)".to_string(),
            description: "Common cargo test setup".to_string(),
            occurrence_count: 5,
            success_rate: 0.8,
            category: PatternCategory::SetupSequence,
            example_step: None,
            conditions: vec![],
        }];
        let result = format_patterns_for_prompt(&patterns);
        assert!(result.contains("Mined Workflow Patterns"));
        assert!(result.contains("Setup: check (cargo test)"));
        assert!(result.contains("80%"));
    }

    #[test]
    fn test_pattern_category_display() {
        assert_eq!(PatternCategory::SetupSequence.to_string(), "Setup Sequence");
        assert_eq!(
            PatternCategory::EffectiveVerification.to_string(),
            "Effective Verification"
        );
        assert_eq!(
            PatternCategory::OptimalOrdering.to_string(),
            "Optimal Ordering"
        );
        assert_eq!(PatternCategory::AntiPattern.to_string(), "Anti-Pattern");
    }
}
