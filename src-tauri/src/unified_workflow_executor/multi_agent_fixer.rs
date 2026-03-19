//! Multi-Agent Fixer for Verification Failures
//!
//! Instead of running a single monolithic AI session to fix all verification
//! failures at once, this module decomposes the work into specialized agents:
//!
//! 1. **Triage Agent** (fast, response-mode) — Classifies failures by type,
//!    determines priority order, and identifies dependencies between failures.
//!
//! 2. **Quick Fix Agents** — Handle trivial failures (lint, format, type errors)
//!    with focused, narrow prompts.
//!
//! 3. **Feature Fix Agents** — Handle missing-feature or logic failures that
//!    require deeper context and code understanding.
//!
//! 4. **Targeted Verification** — Runs just the relevant verification steps
//!    after each fix agent, providing fast feedback.
//!
//! 5. **Regression Check** — Runs all verification steps after all fixes to
//!    catch regressions.
//!
//! The coordinator orchestrates these agents in dependency order, running quick
//! fixes first (they're fast and often unblock other checks).

use serde::{Deserialize, Serialize};

use crate::step_executor::{StepExecutionConfig, StepExecutionResult, VerificationPhaseResult};

// =============================================================================
// Types
// =============================================================================

/// Classification of a verification failure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FailureType {
    /// Lint/format errors — trivially fixable with `eslint --fix`, `prettier`, etc.
    LintFormat,
    /// TypeScript/compilation errors — fix imports, types, syntax.
    Compilation,
    /// Missing feature — code doesn't implement what verification checks for.
    MissingFeature,
    /// UI assertion failure — element not found, wrong state, etc.
    UiAssertion,
    /// Cascading — this failure will auto-resolve when its dependency is fixed.
    Cascading,
    /// Test failure — a test case fails.
    TestFailure,
    /// Unknown — couldn't classify.
    Unknown,
}

impl std::fmt::Display for FailureType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LintFormat => write!(f, "lint_format"),
            Self::Compilation => write!(f, "compilation"),
            Self::MissingFeature => write!(f, "missing_feature"),
            Self::UiAssertion => write!(f, "ui_assertion"),
            Self::Cascading => write!(f, "cascading"),
            Self::TestFailure => write!(f, "test_failure"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// A group of related verification failures to be fixed together.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureGroup {
    /// Unique ID for this group (e.g., "lint", "view_modes_code").
    pub id: String,
    /// Classification of the failure type.
    pub failure_type: FailureType,
    /// Names of the verification steps in this group.
    pub step_names: Vec<String>,
    /// Indices of the verification steps in the original step list.
    pub step_indices: Vec<usize>,
    /// Priority (lower = fix first). Quick fixes get priority 1, features get 2+.
    pub priority: u32,
    /// Suggested fix strategy for the AI agent.
    pub strategy: String,
    /// IDs of groups this group depends on (fix those first).
    pub blocked_by: Vec<String>,
}

/// Result of the triage agent's classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageResult {
    /// Classified failure groups.
    pub groups: Vec<FailureGroup>,
    /// Execution order (group IDs in the order they should be fixed).
    pub execution_order: Vec<String>,
    /// Summary of the triage analysis.
    pub summary: String,
}

/// Result of a single fix agent's attempt.
#[derive(Debug, Clone)]
pub struct FixAgentResult {
    /// Which group was being fixed.
    pub group_id: String,
    /// Whether the AI session succeeded.
    pub session_success: bool,
    /// Whether targeted verification passed after the fix.
    pub verification_passed: bool,
    /// Number of steps that now pass.
    pub steps_passed: usize,
    /// Number of steps that still fail.
    pub steps_failed: usize,
    /// AI session output (for logging).
    pub output: String,
    /// Duration of the fix attempt in milliseconds.
    pub duration_ms: u64,
}

/// Overall result of the multi-agent fix cycle.
#[derive(Debug, Clone)]
pub struct MultiAgentFixResult {
    /// Individual fix agent results.
    pub fix_results: Vec<FixAgentResult>,
    /// Whether all verification steps pass after all fixes.
    pub all_passed: bool,
    /// Final verification result (full regression check).
    pub final_verification: Option<VerificationPhaseResult>,
    /// Total duration of the multi-agent cycle.
    pub total_duration_ms: u64,
    /// Summary of what was fixed.
    pub summary: String,
}

// =============================================================================
// Triage
// =============================================================================

/// Build the triage prompt for classifying verification failures.
///
/// The triage agent receives the failure context and returns structured JSON
/// classifying each failure into groups with priorities and dependencies.
pub fn build_triage_prompt(failure_context: &str, all_step_results: &[StepExecutionResult]) -> String {
    // Build a structured summary of all steps (passed and failed)
    let mut steps_summary = String::new();
    for (i, result) in all_step_results.iter().enumerate() {
        let status = if result.success { "PASS" } else { "FAIL" };
        let check_type = result.config.check_type.as_deref().unwrap_or("");
        let test_type = result.config.test_type.as_deref().unwrap_or("");
        let type_info = if !check_type.is_empty() {
            format!(" [{}]", check_type)
        } else if !test_type.is_empty() {
            format!(" [test:{}]", test_type)
        } else {
            String::new()
        };

        steps_summary.push_str(&format!(
            "{}. [{}]{} {} — {}\n",
            i,
            status,
            type_info,
            result.step_name,
            if result.success {
                "OK".to_string()
            } else {
                result.error.as_deref().unwrap_or("unknown error").chars().take(200).collect::<String>()
            }
        ));
    }

    format!(
        r#"You are a triage agent. Analyze the following verification step results and classify the failures into groups.

## All Verification Steps

{steps_summary}

## Failure Details

{failure_context}

## Your Task

Classify the FAILED steps into groups. Each group should contain related failures that should be fixed together.

For each group, determine:
1. **failure_type**: One of: `lint_format`, `compilation`, `missing_feature`, `ui_assertion`, `cascading`, `test_failure`, `unknown`
2. **priority**: Lower number = fix first. Use: 1 for lint/format/compilation (trivial), 2 for missing features, 3 for UI assertions, 4 for unknown
3. **strategy**: A specific, actionable instruction for the fix agent. Not generic — reference the actual error and what to do.
4. **blocked_by**: If this group can only pass after another group is fixed first (cascading failures), list the dependency group IDs.

Rules:
- If a step checks for something in the UI that depends on code-level changes in another failing step, mark the UI step as `cascading` with `blocked_by` pointing to the code step.
- Lint/format failures should always be their own group — they're trivially fixable and independent.
- Compilation/typecheck failures should be their own group.
- Group related feature checks together (e.g., "verify X exists in code" and "verify X works in UI" are the same feature).

Respond with ONLY valid JSON, no markdown fences, no explanation:

{{"groups":[{{"id":"lint","failure_type":"lint_format","step_names":["ESLint check"],"step_indices":[4],"priority":1,"strategy":"Run eslint --fix on the affected files, then manually fix any remaining warnings.","blocked_by":[]}}],"execution_order":["lint"],"summary":"Brief description"}}"#,
        steps_summary = steps_summary,
        failure_context = failure_context,
    )
}

/// Parse the triage agent's response into a TriageResult.
pub fn parse_triage_response(response: &str) -> Result<TriageResult, String> {
    // Try to find JSON in the response (may have markdown fences or surrounding text)
    let json_str = extract_json_from_response(response);

    serde_json::from_str::<TriageResult>(&json_str).map_err(|e| {
        format!(
            "Failed to parse triage response as JSON: {}. Response: {}",
            e,
            &response[..response.len().min(500)]
        )
    })
}

/// Build a deterministic triage as fallback when the AI triage fails.
///
/// Classifies failures by inspecting the step configuration and error messages.
pub fn deterministic_triage(step_results: &[StepExecutionResult]) -> TriageResult {
    let mut groups: Vec<FailureGroup> = Vec::new();
    let mut lint_steps = Vec::new();
    let mut lint_indices = Vec::new();
    let mut compile_steps = Vec::new();
    let mut compile_indices = Vec::new();
    let mut other_steps = Vec::new();
    let mut other_indices = Vec::new();

    for (i, result) in step_results.iter().enumerate() {
        if result.success {
            continue;
        }

        let check_type = result.config.check_type.as_deref().unwrap_or("");
        let error_msg = result.error.as_deref().unwrap_or("").to_lowercase();
        let step_name_lower = result.step_name.to_lowercase();

        if check_type == "lint"
            || check_type == "format"
            || step_name_lower.contains("eslint")
            || step_name_lower.contains("prettier")
            || error_msg.contains("eslint")
            || error_msg.contains("too many warnings")
        {
            lint_steps.push(result.step_name.clone());
            lint_indices.push(i);
        } else if check_type == "typecheck"
            || step_name_lower.contains("typecheck")
            || step_name_lower.contains("type check")
            || step_name_lower.contains("tsc")
            || error_msg.contains("ts2")
            || error_msg.contains("type error")
        {
            compile_steps.push(result.step_name.clone());
            compile_indices.push(i);
        } else {
            other_steps.push(result.step_name.clone());
            other_indices.push(i);
        }
    }

    let mut execution_order = Vec::new();

    if !lint_steps.is_empty() {
        let id = "lint_format".to_string();
        execution_order.push(id.clone());
        groups.push(FailureGroup {
            id,
            failure_type: FailureType::LintFormat,
            step_names: lint_steps,
            step_indices: lint_indices,
            priority: 1,
            strategy: "Run the linter with --fix flag first, then manually fix any remaining issues. Focus only on the specific warnings — don't refactor other code.".to_string(),
            blocked_by: vec![],
        });
    }

    if !compile_steps.is_empty() {
        let id = "compilation".to_string();
        execution_order.push(id.clone());
        groups.push(FailureGroup {
            id,
            failure_type: FailureType::Compilation,
            step_names: compile_steps,
            step_indices: compile_indices,
            priority: 1,
            strategy: "Fix type errors and compilation issues. Read the error output carefully — fix the specific errors listed.".to_string(),
            blocked_by: vec![],
        });
    }

    if !other_steps.is_empty() {
        let id = "other_failures".to_string();
        execution_order.push(id.clone());
        groups.push(FailureGroup {
            id,
            failure_type: FailureType::MissingFeature,
            step_names: other_steps,
            step_indices: other_indices,
            priority: 2,
            strategy: "Read the verification step command and error output to understand what is being checked. Then read the relevant source code and implement the missing functionality.".to_string(),
            blocked_by: vec![],
        });
    }

    TriageResult {
        groups,
        execution_order,
        summary: "Deterministic triage: classified by step type and error patterns.".to_string(),
    }
}

// =============================================================================
// Focused Prompts
// =============================================================================

/// Build a focused prompt for a quick-fix agent (lint, format, compilation).
pub fn build_quick_fix_prompt(
    group: &FailureGroup,
    failure_context: &str,
    working_directory: Option<&str>,
) -> String {
    let wd_instruction = if let Some(wd) = working_directory {
        format!("Working directory: {}\n\n", wd)
    } else {
        String::new()
    };

    format!(
        r#"You are a focused fix agent. Your ONLY job is to fix the following verification failure. Do not do anything else.

{wd_instruction}## Failure Type: {failure_type}

## Failed Steps
{step_list}

## Error Details
{failure_context}

## Strategy
{strategy}

## Rules
- Fix ONLY the specific issues listed above. Do not refactor, improve, or change anything else.
- If the failure is a lint/format issue, run the auto-fix tool first (e.g., `eslint --fix`), then fix remaining issues manually.
- If the failure is a compilation error, read the error messages and fix the specific type/import/syntax errors.
- After fixing, run the verification command to confirm it passes.
- If you use agents, have them work on independent files in parallel.
- Be fast and focused. This should take minutes, not hours."#,
        wd_instruction = wd_instruction,
        failure_type = group.failure_type,
        step_list = group
            .step_names
            .iter()
            .enumerate()
            .map(|(i, name)| format!("{}. {}", i + 1, name))
            .collect::<Vec<_>>()
            .join("\n"),
        failure_context = failure_context,
        strategy = group.strategy,
    )
}

/// Build a focused prompt for a feature-fix agent.
pub fn build_feature_fix_prompt(
    group: &FailureGroup,
    failure_context: &str,
    base_task_description: &str,
    working_directory: Option<&str>,
) -> String {
    let wd_instruction = if let Some(wd) = working_directory {
        format!("Working directory: {}\n\n", wd)
    } else {
        String::new()
    };

    format!(
        r#"You are a focused fix agent. Your job is to fix a specific verification failure from a larger workflow.

{wd_instruction}## Original Task Context
{base_task_description}

## What Needs Fixing
The following verification steps are failing:

{step_list}

## Error Details
{failure_context}

## Strategy
{strategy}

## Rules
- Focus ONLY on making the listed verification steps pass. Do not work on other parts of the task.
- Read the verification command to understand exactly what it checks for.
- Read the relevant source files to understand the current state of the code.
- Implement the minimum changes needed to make the checks pass.
- After making changes, run the verification command to confirm it passes.
- If a check verifies that specific functionality exists in the UI, make sure the code implements that functionality AND that it renders correctly.
- Run typecheck (`npx tsc --noEmit`) after your changes to ensure you haven't introduced compilation errors."#,
        wd_instruction = wd_instruction,
        base_task_description = base_task_description,
        step_list = group
            .step_names
            .iter()
            .enumerate()
            .map(|(i, name)| format!("{}. {}", i + 1, name))
            .collect::<Vec<_>>()
            .join("\n"),
        failure_context = failure_context,
        strategy = group.strategy,
    )
}

/// Extract failure context for a specific set of verification steps.
pub fn extract_group_failure_context(
    group: &FailureGroup,
    all_results: &[StepExecutionResult],
) -> String {
    let mut context = String::new();

    for &idx in &group.step_indices {
        if let Some(result) = all_results.get(idx) {
            if !result.success {
                context.push_str(&format!("### {} (step {})\n", result.step_name, idx));

                if let Some(ref cmd) = result.config.command {
                    context.push_str(&format!("**Command:** `{}`\n", cmd));
                }
                if let Some(ref wd) = result.config.working_directory {
                    context.push_str(&format!("**Working Directory:** `{}`\n", wd));
                }
                if let Some(ref error) = result.error {
                    context.push_str(&format!("**Error:** {}\n", error));
                }
                if let Some(ref details) = result.verification_details {
                    if let Some(ref stdout) = details.stdout {
                        let s: &str = stdout.as_ref();
                        if !s.is_empty() {
                            let truncated = if s.len() > 1500 {
                                format!("{}...\n[truncated]", &s[..1500])
                            } else {
                                s.to_string()
                            };
                            context.push_str(&format!("**Output:**\n```\n{}\n```\n", truncated));
                        }
                    }
                    if let Some(ref stderr) = details.stderr {
                        let s: &str = stderr.as_ref();
                        if !s.is_empty() {
                            let truncated = if s.len() > 1000 {
                                format!("{}...\n[truncated]", &s[..1000])
                            } else {
                                s.to_string()
                            };
                            context.push_str(&format!("**Stderr:**\n```\n{}\n```\n", truncated));
                        }
                    }
                }
                context.push('\n');
            }
        }
    }

    if context.is_empty() {
        "No specific failure details available.".to_string()
    } else {
        context
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Extract JSON from a response that may have markdown fences or surrounding text.
fn extract_json_from_response(response: &str) -> String {
    let trimmed = response.trim();

    // Try direct parse first
    if trimmed.starts_with('{') {
        return trimmed.to_string();
    }

    // Try to extract from markdown code fences
    if let Some(start) = trimmed.find("```json") {
        let after = &trimmed[start + 7..];
        if let Some(end) = after.find("```") {
            return after[..end].trim().to_string();
        }
    }
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        if let Some(end) = after.find("```") {
            let candidate = after[..end].trim();
            if candidate.starts_with('{') {
                return candidate.to_string();
            }
        }
    }

    // Try to find the first { and last } in the response
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                return trimmed[start..=end].to_string();
            }
        }
    }

    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deterministic_triage_lint() {
        let results = vec![
            StepExecutionResult {
                step_index: 0,
                step_type: "command".to_string(),
                step_name: "TypeScript typecheck".to_string(),
                step_id: None,
                success: true,
                error: None,
                screenshot_path: None,
                started_at: None,
                ended_at: None,
                duration_ms: 100,
                config: StepExecutionConfig {
                    check_type: Some("typecheck".to_string()),
                    ..Default::default()
                },
                verification_details: None,
                output_data: None,
                required: None,
                resolved_inputs: None,
                extracted_values: None,
                failure_category: None,
                interrupted: None,
            },
            StepExecutionResult {
                step_index: 1,
                step_type: "command".to_string(),
                step_name: "ESLint check".to_string(),
                step_id: None,
                success: false,
                error: Some("ESLint found too many warnings (maximum: 0).".to_string()),
                screenshot_path: None,
                started_at: None,
                ended_at: None,
                duration_ms: 200,
                config: StepExecutionConfig {
                    check_type: Some("lint".to_string()),
                    ..Default::default()
                },
                verification_details: None,
                output_data: None,
                required: None,
                resolved_inputs: None,
                extracted_values: None,
                failure_category: None,
                interrupted: None,
            },
        ];

        let triage = deterministic_triage(&results);
        assert_eq!(triage.groups.len(), 1);
        assert_eq!(triage.groups[0].failure_type, FailureType::LintFormat);
        assert_eq!(triage.groups[0].priority, 1);
    }

    #[test]
    fn test_extract_json() {
        assert_eq!(
            extract_json_from_response(r#"{"groups":[]}"#),
            r#"{"groups":[]}"#
        );
        assert_eq!(
            extract_json_from_response("```json\n{\"groups\":[]}\n```"),
            r#"{"groups":[]}"#
        );
        assert_eq!(
            extract_json_from_response("Here is the result:\n{\"groups\":[]}"),
            r#"{"groups":[]}"#
        );
    }
}
