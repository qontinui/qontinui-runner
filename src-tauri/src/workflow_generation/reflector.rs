//! Reflector-Curator types.
//!
//! The ace-playbook-inspired Generator-Reflector-Curator pipeline was backed
//! by SQLite and has not been ported to PG. Only the shared types and the
//! pure `build_playbook_section` formatter remain. `LearningOrchestrator`
//! still owns a `PlaybookCurator` configuration struct but never invokes
//! the (removed) persistence hooks.

use serde::{Deserialize, Serialize};

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
// Curator configuration
// ============================================================================

/// Configuration struct for the playbook lifecycle manager.
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
    pub fn new() -> Self {
        Self {
            max_entries: 200,
            retirement_threshold: 0.2,
        }
    }
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
    use chrono::Utc;

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
}
