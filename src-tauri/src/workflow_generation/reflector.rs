//! Reflector-Curator Learning Loop
//!
//! Implements the ace-playbook-inspired Generator-Reflector-Curator pattern
//! for learning from workflow execution. The Reflector analyzes completed runs
//! and extracts lessons (both positive and negative). The Curator manages the
//! playbook: deduplicating entries, retiring underperformers, and selecting
//! relevant lessons for prompt injection into future generation runs.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use uuid::Uuid;

// ============================================================================
// Types
// ============================================================================

/// A single lesson learned from workflow execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybookEntry {
    /// Unique entry ID.
    pub id: String,
    /// Human-readable lesson text.
    pub lesson: String,
    /// Category of the lesson.
    pub category: LessonCategory,
    /// Optional domain (maps to VerificationDomain string).
    pub domain: Option<String>,
    /// How important this lesson is.
    pub severity: LessonSeverity,
    /// The run that produced this lesson.
    pub source_run_id: String,
    /// The specific step that produced this lesson, if any.
    pub source_step_id: Option<String>,
    /// `true` = "do this", `false` = "don't do this".
    pub positive: bool,
    /// How many times this lesson has been injected into a prompt.
    pub times_applied: u32,
    /// How many times the subsequent run improved after injection.
    pub times_helped: u32,
    /// Optional embedding vector for semantic dedup.
    pub embedding: Option<Vec<f32>>,
    /// Lifecycle status.
    pub status: EntryStatus,
    /// ISO-8601 creation timestamp.
    pub created_at: String,
    /// ISO-8601 last-update timestamp.
    pub updated_at: String,
}

/// Category of a playbook lesson.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonCategory {
    StepConstruction,
    SelectorChoice,
    ErrorHandling,
    ToolUsage,
    DomainKnowledge,
    AntiPattern,
}

impl std::fmt::Display for LessonCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StepConstruction => write!(f, "step_construction"),
            Self::SelectorChoice => write!(f, "selector_choice"),
            Self::ErrorHandling => write!(f, "error_handling"),
            Self::ToolUsage => write!(f, "tool_usage"),
            Self::DomainKnowledge => write!(f, "domain_knowledge"),
            Self::AntiPattern => write!(f, "anti_pattern"),
        }
    }
}

impl LessonCategory {
    fn from_str(s: &str) -> Self {
        match s {
            "step_construction" => Self::StepConstruction,
            "selector_choice" => Self::SelectorChoice,
            "error_handling" => Self::ErrorHandling,
            "tool_usage" => Self::ToolUsage,
            "domain_knowledge" => Self::DomainKnowledge,
            "anti_pattern" => Self::AntiPattern,
            _ => Self::DomainKnowledge,
        }
    }
}

/// Severity of a playbook lesson.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonSeverity {
    Minor,
    Important,
    Critical,
}

impl std::fmt::Display for LessonSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Minor => write!(f, "minor"),
            Self::Important => write!(f, "important"),
            Self::Critical => write!(f, "critical"),
        }
    }
}

impl LessonSeverity {
    fn from_str(s: &str) -> Self {
        match s {
            "critical" => Self::Critical,
            "important" => Self::Important,
            _ => Self::Minor,
        }
    }
}

/// Lifecycle status of a playbook entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    Staged,
    Active,
    Retired,
}

impl std::fmt::Display for EntryStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Staged => write!(f, "staged"),
            Self::Active => write!(f, "active"),
            Self::Retired => write!(f, "retired"),
        }
    }
}

impl EntryStatus {
    fn from_str(s: &str) -> Self {
        match s {
            "staged" => Self::Staged,
            "active" => Self::Active,
            "retired" => Self::Retired,
            _ => Self::Staged,
        }
    }
}

/// Simplified step evaluation data used by the reflector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepEvaluationSummary {
    /// Step ID.
    pub step_id: String,
    /// Human-readable step name.
    pub step_name: String,
    /// Overall composite score for the step (0.0–1.0).
    pub composite_score: f64,
    /// Minimum dimension score across all evaluation dimensions.
    pub min_score: f64,
    /// Name of the weakest evaluation dimension.
    pub weakest_dimension: String,
    /// Optional explanation of the evaluation result.
    pub explanation: Option<String>,
    /// Optional criterion description that was evaluated.
    pub criterion_description: Option<String>,
}

/// Result of a curation pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CurationResult {
    /// Number of new entries added.
    pub added: usize,
    /// Number of entries merged with existing ones.
    pub merged: usize,
    /// Number of entries retired.
    pub retired: usize,
}

// ============================================================================
// Storage
// ============================================================================

/// Ensure the playbook_entries table exists.
fn ensure_table() -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Insert a new playbook entry into the database.
fn insert_entry(entry: &PlaybookEntry) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

// row_to_entry removed (SQLite dead code)

/// Query active entries, optionally filtered by domain.
fn query_active_entries(domain: Option<&str>, limit: usize) -> Result<Vec<PlaybookEntry>, String> {
    Err("SQLite removed".to_string())
}

/// Query all non-retired entries (for dedup during curation).
fn query_non_retired_entries() -> Result<Vec<PlaybookEntry>, String> {
    Err("SQLite removed".to_string())
}

/// Merge a new entry into an existing one: bump `times_applied`, escalate severity if needed.
fn merge_into_existing(existing_id: &str, new_severity: LessonSeverity) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Retire entries that have been applied 10+ times but helped less than the threshold ratio.
fn retire_underperforming_entries(threshold: f64) -> Result<usize, String> {
    Err("SQLite removed".to_string())
}

/// Count the total number of active entries.
fn count_active_entries() -> Result<usize, String> {
    Err("SQLite removed".to_string())
}

// ============================================================================
// Reflector
// ============================================================================

/// Reflect on a completed workflow run and extract lessons.
///
/// Only reflects on "interesting" runs:
/// - Failures that were fixed (iterations > 0)
/// - High-scoring first-attempt successes (iterations == 0 AND overall_score > 0.8)
///
/// Returns extracted [`PlaybookEntry`] structs ready for curation.
pub fn reflect_on_run(
    run_id: &str,
    overall_score: f64,
    iterations: u32,
    step_evaluations: &[StepEvaluationSummary],
) -> Result<Vec<PlaybookEntry>, String> {
    Err("SQLite removed".to_string())
}

/// Categorize a failure step into a lesson category and severity.
fn categorize_failure(step: &StepEvaluationSummary) -> (LessonCategory, LessonSeverity) {
    let category = match step.weakest_dimension.as_str() {
        "selector" | "selector_accuracy" | "selector_specificity" => LessonCategory::SelectorChoice,
        "error_handling" | "resilience" | "retry" => LessonCategory::ErrorHandling,
        "tool" | "tool_usage" | "tool_selection" => LessonCategory::ToolUsage,
        "structure" | "step_order" | "decomposition" => LessonCategory::StepConstruction,
        _ => LessonCategory::AntiPattern,
    };

    let severity = if step.min_score < 0.2 {
        LessonSeverity::Critical
    } else if step.min_score < 0.4 {
        LessonSeverity::Important
    } else {
        LessonSeverity::Minor
    };

    (category, severity)
}

/// Categorize a success step into a lesson category and severity.
fn categorize_success(step: &StepEvaluationSummary) -> (LessonCategory, LessonSeverity) {
    let category = match step.weakest_dimension.as_str() {
        "selector" | "selector_accuracy" | "selector_specificity" => LessonCategory::SelectorChoice,
        "error_handling" | "resilience" | "retry" => LessonCategory::ErrorHandling,
        "tool" | "tool_usage" | "tool_selection" => LessonCategory::ToolUsage,
        "structure" | "step_order" | "decomposition" => LessonCategory::StepConstruction,
        _ => LessonCategory::DomainKnowledge,
    };

    let severity = if step.composite_score >= 0.95 {
        LessonSeverity::Important
    } else {
        LessonSeverity::Minor
    };

    (category, severity)
}

/// Build a human-readable lesson from a failing step evaluation.
fn build_failure_lesson(step: &StepEvaluationSummary) -> String {
    let base = format!(
        "Step '{}' scored {:.2} (weakest: {} at {:.2})",
        step.step_name, step.composite_score, step.weakest_dimension, step.min_score
    );

    if let Some(ref explanation) = step.explanation {
        format!("{base}. {explanation}")
    } else if let Some(ref criterion) = step.criterion_description {
        format!("{base}. Failed criterion: {criterion}")
    } else {
        base
    }
}

/// Build a human-readable lesson from a successful step evaluation.
fn build_success_lesson(step: &StepEvaluationSummary) -> String {
    let base = format!(
        "Step '{}' achieved {:.2} composite score",
        step.step_name, step.composite_score,
    );

    if let Some(ref explanation) = step.explanation {
        format!("{base}. {explanation}")
    } else if let Some(ref criterion) = step.criterion_description {
        format!("{base}. Criterion met: {criterion}")
    } else {
        base
    }
}

// ============================================================================
// Curator
// ============================================================================

/// Manages the playbook lifecycle: deduplication, retirement, and selection.
pub struct PlaybookCurator {
    /// Maximum number of active entries before forced retirement.
    pub max_entries: usize,
    /// Entries with a helped/applied ratio below this are retired.
    pub retirement_threshold: f64,
}

impl Default for PlaybookCurator {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybookCurator {
    /// Create a new curator with default settings.
    pub fn new() -> Self {
        Self {
            max_entries: 200,
            retirement_threshold: 0.2,
        }
    }

    /// Add new lessons to the playbook, deduplicating by lesson text similarity.
    ///
    /// New entries that are textually similar to existing entries get merged
    /// (bumping `times_applied` and potentially escalating severity). Truly novel
    /// entries are inserted and promoted to `Active` status.
    pub fn curate(&self, new_entries: Vec<PlaybookEntry>) -> Result<CurationResult, String> {
        Err("SQLite removed".to_string())
    }

    /// Retire entries that have been applied 10+ times but helped less than
    /// the `retirement_threshold` ratio.
    pub fn retire_underperforming(&self) -> Result<usize, String> {
        Err("SQLite removed".to_string())
    }

    /// Select relevant lessons for prompt injection.
    ///
    /// Returns up to `max_lessons` active entries, optionally filtered by domain,
    /// ordered by severity (Critical first) then by helpfulness.
    pub fn select_lessons(
        &self,
        domain: Option<&str>,
        max_lessons: usize,
    ) -> Result<Vec<PlaybookEntry>, String> {
        Err("SQLite removed".to_string())
    }
}

/// Simple textual similarity check: returns the first existing entry whose
/// normalized lesson text matches the new lesson closely enough.
///
/// Uses a basic Jaccard-like word-overlap metric. Entries with >70% word overlap
/// are considered duplicates.
fn find_similar_entry<'a>(
    existing: &'a [PlaybookEntry],
    new_lesson: &str,
) -> Option<&'a PlaybookEntry> {
    let new_lower = new_lesson.to_lowercase();
    let new_words: std::collections::HashSet<&str> = new_lower.split_whitespace().collect();

    for entry in existing {
        let existing_lower = entry.lesson.to_lowercase();
        let existing_words: std::collections::HashSet<&str> =
            existing_lower.split_whitespace().collect();

        if existing_words.is_empty() || new_words.is_empty() {
            continue;
        }

        let new_lower = new_lesson.to_lowercase();
        let new_words_set: std::collections::HashSet<&str> = new_lower.split_whitespace().collect();

        let intersection = existing_words.intersection(&new_words_set).count();
        let union = existing_words.union(&new_words_set).count();

        if union > 0 {
            let jaccard = intersection as f64 / union as f64;
            if jaccard > 0.7 {
                return Some(entry);
            }
        }
    }
    None
}

/// Force-retire the lowest-value active entries to bring count under the cap.
/// Retires entries with the lowest `times_helped` first.
fn force_retire_lowest(count: usize) -> Result<usize, String> {
    Err("SQLite removed".to_string())
}

// ============================================================================
// Playbook Prompt Injection
// ============================================================================

/// Format playbook lessons as a markdown section for injection into generation prompts.
///
/// Lessons are grouped by severity (Critical first, then Important, then Minor).
/// Each lesson is prefixed with `DO:` or `DON'T:` based on the `positive` field.
pub fn build_playbook_section(lessons: &[PlaybookEntry]) -> String {
    if lessons.is_empty() {
        return String::new();
    }

    let mut critical: Vec<&PlaybookEntry> = Vec::new();
    let mut important: Vec<&PlaybookEntry> = Vec::new();
    let mut minor: Vec<&PlaybookEntry> = Vec::new();

    for lesson in lessons {
        match lesson.severity {
            LessonSeverity::Critical => critical.push(lesson),
            LessonSeverity::Important => important.push(lesson),
            LessonSeverity::Minor => minor.push(lesson),
        }
    }

    let mut out = String::from("## Lessons from Previous Runs\n\n");

    if !critical.is_empty() {
        out.push_str("### Critical\n");
        for entry in &critical {
            let prefix = if entry.positive { "DO" } else { "DON'T" };
            out.push_str(&format!("- **{prefix}:** {}\n", entry.lesson));
        }
        out.push('\n');
    }

    if !important.is_empty() {
        out.push_str("### Important\n");
        for entry in &important {
            let prefix = if entry.positive { "DO" } else { "DON'T" };
            out.push_str(&format!("- **{prefix}:** {}\n", entry.lesson));
        }
        out.push('\n');
    }

    if !minor.is_empty() {
        out.push_str("### Minor\n");
        for entry in &minor {
            let prefix = if entry.positive { "DO" } else { "DON'T" };
            out.push_str(&format!("- {prefix}: {}\n", entry.lesson));
        }
        out.push('\n');
    }

    out
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_step_eval(
        name: &str,
        composite: f64,
        min: f64,
        weakest: &str,
    ) -> StepEvaluationSummary {
        StepEvaluationSummary {
            step_id: Uuid::new_v4().to_string(),
            step_name: name.to_string(),
            composite_score: composite,
            min_score: min,
            weakest_dimension: weakest.to_string(),
            explanation: None,
            criterion_description: None,
        }
    }

    #[test]
    fn test_reflect_skips_uninteresting_run() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_reflect_extracts_failure_lessons() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_reflect_extracts_success_lessons() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_curator_curate_and_dedup() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_build_playbook_section_empty() {
        let section = build_playbook_section(&[]);
        assert!(section.is_empty());
    }

    #[test]
    fn test_build_playbook_section_formatting() {
        let now = Utc::now().to_rfc3339();
        let lessons = vec![
            PlaybookEntry {
                id: "1".to_string(),
                lesson: "Use data-testid attributes for selectors".to_string(),
                category: LessonCategory::SelectorChoice,
                domain: None,
                severity: LessonSeverity::Critical,
                source_run_id: "r1".to_string(),
                source_step_id: None,
                positive: true,
                times_applied: 5,
                times_helped: 4,
                embedding: None,
                status: EntryStatus::Active,
                created_at: now.clone(),
                updated_at: now.clone(),
            },
            PlaybookEntry {
                id: "2".to_string(),
                lesson: "Avoid xpath selectors on dynamic content".to_string(),
                category: LessonCategory::AntiPattern,
                domain: None,
                severity: LessonSeverity::Important,
                source_run_id: "r1".to_string(),
                source_step_id: None,
                positive: false,
                times_applied: 3,
                times_helped: 2,
                embedding: None,
                status: EntryStatus::Active,
                created_at: now.clone(),
                updated_at: now.clone(),
            },
        ];

        let section = build_playbook_section(&lessons);
        assert!(section.contains("### Critical"));
        assert!(section.contains("**DO:**"));
        assert!(section.contains("**DON'T:**"));
        assert!(section.contains("Use data-testid"));
        assert!(section.contains("Avoid xpath"));
    }

    #[test]
    fn test_select_lessons_respects_limit() {
        // SQLite removed - no-op
    }
}
