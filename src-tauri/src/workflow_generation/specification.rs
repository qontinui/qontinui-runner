//! Specification Agent — Pre-Generation Acceptance Criteria
//!
//! Runs before the Builder Agent to define structured acceptance criteria that
//! guide verification step generation. Forces the AI to think about observable
//! success conditions before any implementation steps are created.
//!
//! The pipeline becomes:
//!   Discovery → Investigation → **Specification** → Builder → Autofix → [Verify↔Fix] → Hardener → Validate

use crate::ai_provider::AiResponse;
use crate::ai_router::TaskContext;
use crate::doctor::DoctorHandle;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tracing::{debug, info, warn};

// ============================================================================
// Types
// ============================================================================

/// How a criterion should be verified.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMethod {
    /// Shell command (exit code, stdout check)
    Command,
    /// UI Bridge SDK assertion
    UiBridge,
    /// Test runner (Playwright, pytest, etc.)
    Test,
    /// Cannot be automated — human review needed
    Manual,
}

/// Priority level for an acceptance criterion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CriterionPriority {
    /// Must pass for the workflow to succeed
    Critical,
    /// Should pass, but workflow can proceed without it
    Important,
    /// Nice-to-have verification
    Optional,
}

/// A single acceptance criterion describing an observable success condition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceCriterion {
    /// Unique kebab-case identifier, e.g. "typecheck-passes"
    pub id: String,
    /// Human-readable description, e.g. "TypeScript compilation succeeds with no errors"
    pub description: String,
    /// How this criterion should be verified
    pub method: VerificationMethod,
    /// Priority level
    pub priority: CriterionPriority,
    /// Concrete hint for the builder, e.g. "Run `npx tsc --noEmit` in frontend/"
    pub verification_hint: String,
    /// Category grouping, e.g. "compilation", "ui-content", "behavior"
    pub category: String,
}

/// Collection of acceptance criteria produced by the specification agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceCriteria {
    /// One-line summary of what success looks like
    pub goal_summary: String,
    /// Ordered list of acceptance criteria
    pub criteria: Vec<AcceptanceCriterion>,
    /// Assumptions made by the specification agent
    pub assumptions: Vec<String>,
}

/// Result of the specification phase.
#[derive(Debug, Clone)]
pub struct SpecificationResult {
    /// The parsed acceptance criteria (may be empty on failure)
    pub criteria: AcceptanceCriteria,
    /// Whether the specification phase succeeded
    pub success: bool,
    /// Duration in milliseconds
    pub duration_ms: u64,
    /// Raw AI output for debugging
    pub raw_output: String,
    /// The full prompt sent to the AI (for training data capture)
    pub prompt: String,
}

// ============================================================================
// Specification Agent
// ============================================================================

/// Run the specification agent to produce acceptance criteria for a task.
///
/// Follows the same pattern as `investigator::run_investigation()`:
/// - Builds a prompt from task context
/// - Makes a single AI call
/// - Parses structured output
/// - Returns gracefully on any failure (empty criteria, `success: false`)
pub fn run_specification_agent(
    description: &str,
    discovery_context: &str,
    resolved_contexts: &str,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
    insights_section: Option<&str>,
) -> SpecificationResult {
    let start = Instant::now();

    let prompt = build_specification_prompt(
        description,
        discovery_context,
        resolved_contexts,
        insights_section,
    );
    let task_context = TaskContext::from_prompt(&prompt);
    let ai_result: AiResponse = crate::ai_provider::run_prompt_with_model_override(
        &prompt,
        &task_context,
        doctor_handle,
        model_override,
        provider_override,
        None,
        None,
        None,
        None,
    );

    let duration_ms = start.elapsed().as_millis() as u64;

    if !ai_result.success {
        warn!(
            "Specification agent failed: {}",
            ai_result.error.as_deref().unwrap_or("unknown error")
        );
        return SpecificationResult {
            criteria: empty_criteria(),
            success: false,
            duration_ms,
            raw_output: ai_result.error.unwrap_or_default(),
            prompt,
        };
    }

    let output = ai_result.output.trim().to_string();

    if output.is_empty() || output.len() < 20 {
        warn!(
            "Specification agent produced too-short output ({} chars), falling back",
            output.len()
        );
        return SpecificationResult {
            criteria: empty_criteria(),
            success: false,
            duration_ms,
            raw_output: output,
            prompt,
        };
    }

    // Extract JSON from the response (may be wrapped in markdown code blocks)
    let json_text = crate::workflow_generation::generator::extract_json_from_response(&output);

    match serde_json::from_str::<AcceptanceCriteria>(&json_text) {
        Ok(criteria) => {
            info!(
                "Specification agent produced {} criteria in {}ms (goal: {})",
                criteria.criteria.len(),
                duration_ms,
                &criteria.goal_summary[..criteria.goal_summary.len().min(80)]
            );
            debug!(
                "Criteria IDs: {:?}",
                criteria.criteria.iter().map(|c| &c.id).collect::<Vec<_>>()
            );

            if criteria.criteria.is_empty() {
                warn!("Specification agent returned zero criteria");
                return SpecificationResult {
                    criteria,
                    success: false,
                    duration_ms,
                    raw_output: output,
                    prompt,
                };
            }

            SpecificationResult {
                criteria,
                success: true,
                duration_ms,
                raw_output: output,
                prompt,
            }
        }
        Err(e) => {
            warn!("Failed to parse specification output as JSON: {}", e);
            debug!(
                "Specification raw output: {}",
                &output[..output.len().min(500)]
            );
            SpecificationResult {
                criteria: empty_criteria(),
                success: false,
                duration_ms,
                raw_output: output,
                prompt,
            }
        }
    }
}

// ============================================================================
// Prompt Builder
// ============================================================================

/// Build the prompt for the specification agent.
fn build_specification_prompt(
    description: &str,
    discovery_context: &str,
    resolved_contexts: &str,
    insights_section: Option<&str>,
) -> String {
    let mut prompt = format!(
        r#"You are a specification agent for an automation platform. Your job is to define **acceptance criteria** — concrete, observable conditions that prove a task was completed successfully.

You do NOT implement anything. You only define what "done" looks like.

## Task Description

{description}

## Project Context

{discovery}
"#,
        description = description,
        discovery = if discovery_context.is_empty() {
            "(No project discovery data available)"
        } else {
            discovery_context
        },
    );

    if !resolved_contexts.is_empty() {
        prompt.push_str(&format!(
            "\n## Additional Context\n\n{}\n",
            resolved_contexts
        ));
    }

    if let Some(insights) = insights_section {
        if !insights.is_empty() {
            prompt.push_str("\n");
            prompt.push_str(insights);
        }
    }

    prompt.push_str(
        r#"
## Instructions

Think about what observable success looks like for this task. Focus on outcomes, not implementation.

For each criterion, consider:
- **What can be checked automatically?** Prefer command-based checks (exit codes, grep, test runners) and UI Bridge assertions over manual review.
- **What is the minimum bar?** Critical criteria are necessary conditions. Optional criteria are nice-to-have.
- **What would a reviewer check?** If a human reviewed this work, what would they verify?

Produce 3–8 structured criteria. Each criterion must have:
- `id`: kebab-case identifier (e.g., "typecheck-passes", "button-renders-correctly")
- `description`: one-sentence description of the success condition
- `method`: how to verify it — one of "command", "ui_bridge", "test", "manual"
- `priority`: one of "critical", "important", "optional"
- `verification_hint`: a concrete suggestion for how to check it (e.g., "Run `npx tsc --noEmit`", "Assert element with data-ui-id='save-btn' is visible")
- `category`: grouping label (e.g., "compilation", "ui-content", "behavior", "data-integrity", "style")

Also provide:
- `goal_summary`: one sentence summarizing what overall success looks like
- `assumptions`: list of assumptions you're making (e.g., "Project uses TypeScript", "Frontend runs on localhost:3001")

## Output Format

Return ONLY valid JSON matching this structure. No markdown code blocks, no explanations.

{
  "goal_summary": "...",
  "criteria": [
    {
      "id": "...",
      "description": "...",
      "method": "command|ui_bridge|test|manual",
      "priority": "critical|important|optional",
      "verification_hint": "...",
      "category": "..."
    }
  ],
  "assumptions": ["...", "..."]
}
"#,
    );

    prompt
}

// ============================================================================
// Criteria Formatter (for Builder Prompt Injection)
// ============================================================================

/// Format acceptance criteria as a section to inject into the builder prompt.
///
/// Produces a markdown section with:
/// - A requirements table
/// - Instructions for the builder to tag verification steps with `criterion_id`
/// - A minimum verification step count
pub fn format_criteria_for_builder(criteria: &AcceptanceCriteria) -> String {
    let automatable: Vec<&AcceptanceCriterion> = criteria
        .criteria
        .iter()
        .filter(|c| c.method != VerificationMethod::Manual)
        .collect();

    let mut section = String::from("## Acceptance Criteria\n\n");
    section.push_str(&format!("**Goal:** {}\n\n", criteria.goal_summary));

    // Requirements table
    section.push_str("| ID | Priority | Method | Description | Verification Hint |\n");
    section.push_str("|---|---|---|---|---|\n");
    for c in &criteria.criteria {
        let method_str = match c.method {
            VerificationMethod::Command => "command",
            VerificationMethod::UiBridge => "ui_bridge",
            VerificationMethod::Test => "test",
            VerificationMethod::Manual => "manual",
        };
        let priority_str = match c.priority {
            CriterionPriority::Critical => "CRITICAL",
            CriterionPriority::Important => "important",
            CriterionPriority::Optional => "optional",
        };
        section.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            c.id, priority_str, method_str, c.description, c.verification_hint
        ));
    }

    section.push_str("\n### Requirements for Verification Steps\n\n");
    section.push_str(
        "- **CRITICAL:** Your `verification_steps` MUST include a step for each automatable criterion (method ≠ manual).\n",
    );
    section.push_str(
        "- Each verification step MUST include a `\"criterion_id\"` field matching the criterion's `id`.\n",
    );
    section.push_str(
        "- The verification step's type should match the criterion's method: `command` → command step, `ui_bridge` → ui_bridge step, `test` → command step with test_type.\n",
    );

    if !automatable.is_empty() {
        section.push_str(&format!(
            "- Minimum verification step count: **{}** (one per automatable criterion).\n",
            automatable.len()
        ));
    }

    if !criteria.assumptions.is_empty() {
        section.push_str("\n### Assumptions\n\n");
        for assumption in &criteria.assumptions {
            section.push_str(&format!("- {}\n", assumption));
        }
    }

    section
}

/// Format acceptance criteria as a cross-validation section for the verification prompt.
///
/// Instructs the verification agent to check that each criterion has a matching step.
pub fn format_criteria_for_verifier(criteria: &AcceptanceCriteria) -> String {
    let automatable: Vec<&AcceptanceCriterion> = criteria
        .criteria
        .iter()
        .filter(|c| c.method != VerificationMethod::Manual)
        .collect();

    if automatable.is_empty() {
        return String::new();
    }

    let mut section = String::from("\n## Acceptance Criteria Cross-Validation\n\n");
    section
        .push_str("The workflow was generated from these acceptance criteria. Check coverage:\n\n");

    for c in &automatable {
        let priority_str = match c.priority {
            CriterionPriority::Critical => "CRITICAL",
            CriterionPriority::Important => "important",
            CriterionPriority::Optional => "optional",
        };
        section.push_str(&format!(
            "- `{}` ({}) — {}: {}\n",
            c.id, priority_str, c.category, c.description
        ));
    }

    section.push_str("\n### Cross-validation checks:\n");
    section.push_str(
        "- Flag if any **CRITICAL** criterion lacks a verification step with a matching `criterion_id`.\n",
    );
    section.push_str(
        "- Flag verification steps with `criterion_id` values that don't match any criterion above.\n",
    );
    section.push_str(
        "- Flag if an important criterion has no verification step (warning, not blocking).\n",
    );

    section
}

// ============================================================================
// Helpers
// ============================================================================

fn empty_criteria() -> AcceptanceCriteria {
    AcceptanceCriteria {
        goal_summary: String::new(),
        criteria: vec![],
        assumptions: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_criteria() -> AcceptanceCriteria {
        AcceptanceCriteria {
            goal_summary: "Dark mode toggle works correctly".to_string(),
            criteria: vec![
                AcceptanceCriterion {
                    id: "typecheck-passes".to_string(),
                    description: "TypeScript compilation succeeds with no errors".to_string(),
                    method: VerificationMethod::Command,
                    priority: CriterionPriority::Critical,
                    verification_hint: "Run `npx tsc --noEmit` in frontend/".to_string(),
                    category: "compilation".to_string(),
                },
                AcceptanceCriterion {
                    id: "toggle-renders".to_string(),
                    description: "Dark mode toggle button is visible on settings page".to_string(),
                    method: VerificationMethod::UiBridge,
                    priority: CriterionPriority::Critical,
                    verification_hint:
                        "Assert element with data-ui-id='settings-dark-mode-toggle' exists"
                            .to_string(),
                    category: "ui-content".to_string(),
                },
                AcceptanceCriterion {
                    id: "manual-visual-review".to_string(),
                    description: "Colors look correct in dark mode".to_string(),
                    method: VerificationMethod::Manual,
                    priority: CriterionPriority::Optional,
                    verification_hint: "Visual inspection".to_string(),
                    category: "style".to_string(),
                },
            ],
            assumptions: vec![
                "Project uses TypeScript".to_string(),
                "Frontend runs on localhost:3001".to_string(),
            ],
        }
    }

    #[test]
    fn test_format_criteria_for_builder() {
        let criteria = sample_criteria();
        let output = format_criteria_for_builder(&criteria);

        assert!(output.contains("## Acceptance Criteria"));
        assert!(output.contains("typecheck-passes"));
        assert!(output.contains("toggle-renders"));
        assert!(output.contains("manual-visual-review"));
        assert!(output.contains("criterion_id"));
        // 2 automatable criteria (not manual)
        assert!(output.contains("Minimum verification step count: **2**"));
    }

    #[test]
    fn test_format_criteria_for_verifier() {
        let criteria = sample_criteria();
        let output = format_criteria_for_verifier(&criteria);

        assert!(output.contains("Cross-Validation"));
        assert!(output.contains("typecheck-passes"));
        assert!(output.contains("toggle-renders"));
        // Manual criterion should NOT appear in verifier section
        assert!(!output.contains("manual-visual-review"));
    }

    #[test]
    fn test_empty_criteria() {
        let criteria = empty_criteria();
        assert!(criteria.criteria.is_empty());
        assert!(criteria.goal_summary.is_empty());
    }

    #[test]
    fn test_serde_roundtrip() {
        let criteria = sample_criteria();
        let json = serde_json::to_string(&criteria).unwrap();
        let parsed: AcceptanceCriteria = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.criteria.len(), 3);
        assert_eq!(parsed.criteria[0].id, "typecheck-passes");
        assert_eq!(parsed.criteria[0].method, VerificationMethod::Command);
        assert_eq!(parsed.criteria[1].priority, CriterionPriority::Critical);
    }
}
