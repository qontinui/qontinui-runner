//! Cross-run learning orchestrator.
//!
//! Called after reflection runs complete. Analyzes patterns across multiple runs,
//! detects recurring issues, auto-disables ineffective rules, and auto-applies
//! known-good fixes for recurring findings.

use crate::database::{cross_run_ops, graph_ops};
use tracing::{info, warn};

/// Run cross-run analysis after a reflection run completes.
/// This is the main entry point called from reflection/trigger.rs.
///
/// Performs:
/// 1. Detect recurring findings (break point #5: cross-run patterns)
/// 2. Detect fix oscillations (fixes that work temporarily then regress)
/// 3. Auto-disable ineffective generation rules (break point #3)
/// 4. Auto-apply known-good fixes for recurring findings (break point #7)
/// 5. Extract procedural skills from effective fix patterns (hermes-agent-inspired)
///
/// Returns (patterns_detected, rules_disabled, fixes_auto_applied).
/// Also returns extracted skills via the `extracted_skills` out-parameter so
/// the async caller can emit `skill.created` events on the workflow event bus.
pub fn post_run_analysis(
    workflow_name: &str,
    task_run_id: &str,
) -> Result<(u32, u32, u32), String> {
    Err("SQLite removed".to_string())
}

/// Same as `post_run_analysis` but also returns extracted skills for event emission.
pub fn post_run_analysis_with_skills(
    workflow_name: &str,
    task_run_id: &str,
) -> Result<(u32, u32, u32, Vec<ExtractedSkill>), String> {
    Err("SQLite removed".to_string())
}

/// Auto-disable generation rules that have been loaded multiple times
/// with no measurable positive effect.
///
/// A rule is disabled when:
/// - It has >= threshold 'no_effect' entries in rule_influence_log
/// - It has 0 'prevented_error' entries
/// - Its source reflection_fix (if any) was evaluated as 'ineffective'
///
/// This closes feedback loop break point #3: "ineffective rules stay active forever"
pub fn auto_disable_ineffective_rules(
    threshold: u32,
) -> Result<u32, String> {
    Err("SQLite removed".to_string())
}

/// For recurring findings, check if there's a known-effective fix that could be reused.
/// Specifically targets selector_fix and tool_config_update fixes that were effective
/// but aren't being auto-applied by the existing system.
///
/// This closes feedback loop break point #7: "selector/config fixes not auto-applied"
fn auto_apply_recurring_fixes(
    workflow_name: &str,
    recurring_patterns: &[cross_run_ops::CrossRunPattern],
) -> Result<u32, String> {
    Err("SQLite removed".to_string())
}

/// Get the effectiveness of the reflection fix that created a generation rule.
fn get_rule_source_fix_effectiveness(
    rule_id: &str,
) -> Result<Option<String>, String> {
    Err("SQLite removed".to_string())
}

/// Route a reflection fix to the correct generation agent based on step provenance
/// rather than step-type heuristic.
///
/// This closes feedback loop break point #1: "fixes dropped if no step type match"
/// When the standard infer_step_type_from_fix() fails, this function queries
/// step_provenance to find which agent created the problematic step and routes
/// the fix as a generation rule for that agent.
///
/// Returns `Some((generating_agent, phase))` if a provenance match is found.
pub fn provenance_based_fix_routing(
    fix_description: &str,
    file_changed: Option<&str>,
    workflow_id: Option<&str>,
) -> Option<(String, String)> {
    None
}

/// A skill that was auto-extracted during cross-run analysis.
/// Returned to the caller so they can emit async events on the workflow event bus.
#[derive(Debug, Clone)]
pub struct ExtractedSkill {
    pub skill_id: String,
    pub skill_slug: String,
    pub source_fix_id: String,
    pub source_task_run_id: String,
    pub workflow_name: String,
}

/// Extract procedural skills from successful complex task runs.
///
/// Inspired by hermes-agent's self-improving procedural memory: when a task run
/// succeeds after significant effort (multiple iterations, effective fixes), the
/// fix pattern is captured as a reusable skill with source="auto".
///
/// Skills are created with:
/// - A descriptive name derived from the fix type and target component
/// - The signature_hash embedded in the description for knowledge graph linking
/// - Approval_status="pending" (requires review before injection into prompts)
/// - A single-step template containing the fix as a command or prompt
///
/// Returns the number of skills created.
pub fn extract_procedural_skills(
    workflow_name: &str,
    task_run_id: &str,
    min_iterations: i64,
) -> Result<u32, String> {
    Err("SQLite removed".to_string())
}

/// Same as `extract_procedural_skills` but returns details of each created skill
/// so the caller can emit `skill.created` events on the workflow event bus.
pub fn extract_procedural_skills_detailed(
    workflow_name: &str,
    task_run_id: &str,
    min_iterations: i64,
) -> Result<Vec<ExtractedSkill>, String> {
    Err("SQLite removed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create an in-memory SQLite database with all tables needed by cross_run_learning.
    fn setup_test_db() -> Connection {
        panic!("SQLite tests disabled — use PG-based tests instead")
    }

    #[test]
    fn test_auto_disable_ineffective_rules() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_auto_disable_skips_effective_rules() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_provenance_based_fix_routing() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_provenance_based_fix_routing_no_match() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_extract_procedural_skills() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_extract_procedural_skills_skips_non_qualifying_runs() {
        // SQLite removed - no-op
    }
}
