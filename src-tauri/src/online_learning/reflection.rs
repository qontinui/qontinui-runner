//! Post-step and post-run experience reflection.
//!
//! Extracts structured lessons from workflow execution data incrementally,
//! rather than waiting for the batch reflection workflow.
//!
//! Two reflection layers:
//! 1. **Deterministic post-step extraction** — Rule-based, no LLM, immediate
//! 2. **Post-run experience summarization** — Structured from pipeline traces
//!
//! Inspired by MUSE's post-subtask reflection approach (KnowledgeXLab/MUSE).

use serde::{Deserialize, Serialize};
use tracing::debug;

// =============================================================================
// Step-level reflection
// =============================================================================

/// Data about a completed step for reflection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepReflection {
    /// Type of step (e.g., "generate_code", "verify", "implement").
    pub step_type: String,
    /// Index of the step in the execution sequence.
    pub step_index: usize,
    /// ID of the containing task run.
    pub task_run_id: String,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
    /// Whether the step succeeded.
    pub success: bool,
    /// Tools invoked by this step.
    pub tool_calls: Vec<String>,
    /// Error type if the step failed.
    pub error_type: Option<String>,
    /// Number of retry attempts.
    pub retry_count: u32,
}

/// A lesson extracted from step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepLesson {
    /// Step type this lesson applies to.
    pub step_type: String,
    /// Knowledge layer (e.g., "universal", "input_handling", "execution", "validation").
    pub layer: String,
    /// Short title for the lesson.
    pub title: String,
    /// Detailed lesson content.
    pub content: String,
    /// Priority (higher = more important, 1-10).
    pub priority: i32,
    /// Source of this lesson.
    pub provenance: String,
    /// Content hash for deduplication.
    pub content_hash: String,
}

/// Extract deterministic lessons from a completed step.
///
/// These are rule-based extractions that require no LLM. They capture
/// anti-patterns and positive patterns from step execution data.
pub fn extract_step_lessons(reflection: &StepReflection) -> Vec<StepLesson> {
    let mut lessons = Vec::new();

    // Rule 1: Repeated failures indicate a problematic step pattern
    if !reflection.success && reflection.retry_count > 3 {
        let content = format!(
            "Step '{}' failed after {} retries. Error type: {}. \
             Consider adding pre-validation or input sanitization for this step type.",
            reflection.step_type,
            reflection.retry_count,
            reflection.error_type.as_deref().unwrap_or("unknown")
        );
        let hash = compute_lesson_hash(&reflection.step_type, "repeated_failure", &content);
        lessons.push(StepLesson {
            step_type: reflection.step_type.clone(),
            layer: "execution".to_string(),
            title: format!("Repeated failure in {}", reflection.step_type),
            content,
            priority: 5,
            provenance: "online_learning:step_failure".to_string(),
            content_hash: hash,
        });
    }

    // Rule 2: Timeout errors suggest the step needs resource limits
    if !reflection.success
        && reflection
            .error_type
            .as_deref()
            .is_some_and(|e| e.contains("timeout"))
    {
        let content = format!(
            "Step '{}' timed out after {}ms. Consider reducing scope, \
             adding intermediate checkpoints, or increasing the timeout threshold.",
            reflection.step_type, reflection.duration_ms
        );
        let hash = compute_lesson_hash(&reflection.step_type, "timeout", &content);
        lessons.push(StepLesson {
            step_type: reflection.step_type.clone(),
            layer: "execution".to_string(),
            title: format!("Timeout in {}", reflection.step_type),
            content,
            priority: 4,
            provenance: "online_learning:step_timeout".to_string(),
            content_hash: hash,
        });
    }

    // Rule 3: Very fast successful steps may indicate shortcuts
    if reflection.success && reflection.duration_ms < 500 && reflection.retry_count == 0 {
        debug!(
            "Fast success for step '{}' ({}ms) — potential shortcut or trivial execution",
            reflection.step_type, reflection.duration_ms
        );
        // Don't create a lesson for this; just log it for observability
    }

    // Rule 4: Steps with many tool calls that fail may indicate tool misuse
    if !reflection.success && reflection.tool_calls.len() > 10 {
        let content = format!(
            "Step '{}' made {} tool calls before failing. Excessive tool usage may indicate \
             unclear instructions or missing context for the step.",
            reflection.step_type,
            reflection.tool_calls.len()
        );
        let hash = compute_lesson_hash(&reflection.step_type, "excessive_tools", &content);
        lessons.push(StepLesson {
            step_type: reflection.step_type.clone(),
            layer: "input_handling".to_string(),
            title: format!("Excessive tool usage in {}", reflection.step_type),
            content,
            priority: 3,
            provenance: "online_learning:excessive_tools".to_string(),
            content_hash: hash,
        });
    }

    lessons
}

// =============================================================================
// Run-level experience summarization
// =============================================================================

/// A key decision made during the workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyDecision {
    /// Step index where the decision was made.
    pub step_index: usize,
    /// Step type.
    pub step_type: String,
    /// What was decided.
    pub decision: String,
    /// Estimated impact on the final outcome (-1.0 to 1.0).
    pub outcome_impact: f64,
}

/// A point where the workflow failed or degraded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailurePoint {
    /// Step index of the failure.
    pub step_index: usize,
    /// Step type.
    pub step_type: String,
    /// Error type.
    pub error_type: String,
    /// Whether this was recoverable.
    pub recoverable: bool,
    /// What happened after the failure.
    pub recovery_action: Option<String>,
}

/// An effective pattern observed in the workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectivePattern {
    /// Name/description of the pattern.
    pub pattern: String,
    /// Step types involved.
    pub step_types: Vec<String>,
    /// How effective it was (0.0-1.0).
    pub effectiveness: f64,
}

/// Structured summary of a complete workflow run's experience.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperienceSummary {
    /// ID of the task run.
    pub task_run_id: String,
    /// Primary domain of the task.
    pub domain: String,
    /// Complexity tier.
    pub complexity_tier: String,
    /// Final outcome status.
    pub outcome: String,
    /// Key decisions that shaped the outcome.
    pub key_decisions: Vec<KeyDecision>,
    /// Points where the workflow failed or degraded.
    pub failure_points: Vec<FailurePoint>,
    /// Effective patterns observed.
    pub effective_patterns: Vec<EffectivePattern>,
}

/// Data about a completed workflow run for experience extraction.
#[derive(Debug, Clone)]
pub struct RunReflectionInput {
    pub task_run_id: String,
    pub domain: String,
    pub complexity_tier: String,
    pub outcome_status: String,
    pub composite_score: Option<f64>,
    pub steps: Vec<StepReflection>,
    pub total_iterations: u32,
    pub total_cost_usd: Option<f64>,
}

/// Extract an experience summary from a completed workflow run.
///
/// This is a deterministic extraction (no LLM) that structures run data
/// into a queryable experience format for cross-run learning.
pub fn summarize_experience(input: &RunReflectionInput) -> ExperienceSummary {
    let mut key_decisions = Vec::new();
    let mut failure_points = Vec::new();
    let mut effective_patterns = Vec::new();

    // Extract failure points
    for step in &input.steps {
        if !step.success {
            failure_points.push(FailurePoint {
                step_index: step.step_index,
                step_type: step.step_type.clone(),
                error_type: step
                    .error_type
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                recoverable: step.step_index < input.steps.len() - 1, // Not the last step
                recovery_action: if step.step_index < input.steps.len() - 1 {
                    Some("continued to next step".to_string())
                } else {
                    None
                },
            });
        }
    }

    // Extract key decisions: steps with high retry counts or long durations
    for step in &input.steps {
        if step.retry_count > 1 || step.duration_ms > 30_000 {
            let impact = if step.success { 0.3 } else { -0.3 };
            key_decisions.push(KeyDecision {
                step_index: step.step_index,
                step_type: step.step_type.clone(),
                decision: if step.retry_count > 1 {
                    format!("Retried {} times", step.retry_count)
                } else {
                    format!("Long execution ({}ms)", step.duration_ms)
                },
                outcome_impact: impact,
            });
        }
    }

    // Extract effective patterns: sequences of successful steps
    let success_streak: Vec<&StepReflection> = input.steps.iter().filter(|s| s.success).collect();
    if success_streak.len() >= 3 {
        let step_types: Vec<String> = success_streak.iter().map(|s| s.step_type.clone()).collect();
        effective_patterns.push(EffectivePattern {
            pattern: format!(
                "Successful {}-step sequence: {}",
                step_types.len(),
                step_types.join(" → ")
            ),
            step_types,
            effectiveness: input.composite_score.unwrap_or(0.5),
        });
    }

    // Pattern: zero-retry workflow (very efficient)
    if input.steps.iter().all(|s| s.retry_count == 0 && s.success) && !input.steps.is_empty() {
        effective_patterns.push(EffectivePattern {
            pattern: "Zero-retry successful workflow".to_string(),
            step_types: input.steps.iter().map(|s| s.step_type.clone()).collect(),
            effectiveness: 1.0,
        });
    }

    ExperienceSummary {
        task_run_id: input.task_run_id.clone(),
        domain: input.domain.clone(),
        complexity_tier: input.complexity_tier.clone(),
        outcome: input.outcome_status.clone(),
        key_decisions,
        failure_points,
        effective_patterns,
    }
}

/// Derive step-type knowledge entries from an experience summary.
pub fn derive_step_lessons(summary: &ExperienceSummary) -> Vec<StepLesson> {
    let mut lessons = Vec::new();

    // Failure patterns become anti-pattern knowledge
    for fp in &summary.failure_points {
        if !fp.recoverable {
            let content = format!(
                "Unrecoverable failure in {} step during {} {} workflow. Error: {}. \
                 Consider adding guardrails or pre-checks for this step type in {} contexts.",
                fp.step_type,
                summary.complexity_tier,
                summary.domain,
                fp.error_type,
                summary.domain,
            );
            let hash = compute_lesson_hash(&fp.step_type, "unrecoverable_failure", &content);
            lessons.push(StepLesson {
                step_type: fp.step_type.clone(),
                layer: "validation".to_string(),
                title: format!(
                    "Unrecoverable {} failure in {} context",
                    fp.step_type, summary.domain
                ),
                content,
                priority: 6,
                provenance: format!("online_learning:experience:{}", summary.task_run_id),
                content_hash: hash,
            });
        }
    }

    lessons
}

// =============================================================================
// Utilities
// =============================================================================

/// Compute a content hash for deduplication.
fn compute_lesson_hash(step_type: &str, pattern: &str, _content: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    step_type.hash(&mut hasher);
    pattern.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_step_reflection(
        index: usize,
        step_type: &str,
        success: bool,
        retry_count: u32,
        duration_ms: u64,
    ) -> StepReflection {
        StepReflection {
            step_type: step_type.to_string(),
            step_index: index,
            task_run_id: "test-run".to_string(),
            duration_ms,
            success,
            tool_calls: vec!["tool1".to_string()],
            error_type: if success {
                None
            } else {
                Some("runtime_error".to_string())
            },
            retry_count,
        }
    }

    #[test]
    fn test_extract_no_lessons_from_clean_step() {
        let step = make_step_reflection(0, "generate", true, 0, 5000);
        let lessons = extract_step_lessons(&step);
        assert!(lessons.is_empty());
    }

    #[test]
    fn test_extract_repeated_failure_lesson() {
        let step = make_step_reflection(0, "generate", false, 5, 10000);
        let lessons = extract_step_lessons(&step);
        assert_eq!(lessons.len(), 1);
        assert!(lessons[0].title.contains("Repeated failure"));
        assert_eq!(lessons[0].provenance, "online_learning:step_failure");
    }

    #[test]
    fn test_extract_timeout_lesson() {
        let mut step = make_step_reflection(0, "verify", false, 1, 60000);
        step.error_type = Some("timeout".to_string());
        let lessons = extract_step_lessons(&step);
        assert!(lessons.iter().any(|l| l.title.contains("Timeout")));
    }

    #[test]
    fn test_summarize_experience() {
        let input = RunReflectionInput {
            task_run_id: "run-1".to_string(),
            domain: "backend".to_string(),
            complexity_tier: "moderate".to_string(),
            outcome_status: "success".to_string(),
            composite_score: Some(0.85),
            steps: vec![
                make_step_reflection(0, "plan", true, 0, 2000),
                make_step_reflection(1, "generate", true, 0, 5000),
                make_step_reflection(2, "verify", true, 0, 3000),
            ],
            total_iterations: 1,
            total_cost_usd: Some(0.50),
        };

        let summary = summarize_experience(&input);
        assert_eq!(summary.task_run_id, "run-1");
        assert!(summary.failure_points.is_empty());
        assert!(!summary.effective_patterns.is_empty());
        // Should detect zero-retry pattern
        assert!(summary
            .effective_patterns
            .iter()
            .any(|p| p.pattern.contains("Zero-retry")));
    }

    #[test]
    fn test_summarize_with_failures() {
        let input = RunReflectionInput {
            task_run_id: "run-2".to_string(),
            domain: "frontend".to_string(),
            complexity_tier: "complex".to_string(),
            outcome_status: "failure".to_string(),
            composite_score: Some(0.30),
            steps: vec![
                make_step_reflection(0, "plan", true, 0, 2000),
                make_step_reflection(1, "generate", false, 3, 15000),
                make_step_reflection(2, "verify", false, 0, 1000),
            ],
            total_iterations: 3,
            total_cost_usd: Some(1.20),
        };

        let summary = summarize_experience(&input);
        assert_eq!(summary.failure_points.len(), 2);
        assert!(summary.failure_points[0].recoverable); // Not last step
        assert!(!summary.failure_points[1].recoverable); // Last step
    }

    #[test]
    fn test_dedup_hash_consistency() {
        let hash1 = compute_lesson_hash("generate", "failure", "some content");
        let hash2 = compute_lesson_hash("generate", "failure", "different content");
        // Same step_type + pattern should produce same hash (content is not hashed)
        assert_eq!(hash1, hash2);
    }
}
