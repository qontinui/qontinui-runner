//! Reflector-Curator Learning Loop
//!
//! Implements the ace-playbook-inspired Generator-Reflector-Curator pattern
//! for learning from workflow execution. The Reflector analyzes completed runs
//! and extracts lessons (both positive and negative). The Curator manages the
//! playbook: deduplicating entries, retiring underperformers, and selecting
//! relevant lessons for prompt injection into future generation runs.

use chrono::Utc;
use rusqlite::{params, Connection};
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
fn ensure_table(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS playbook_entries (
            id TEXT PRIMARY KEY,
            lesson TEXT NOT NULL,
            category TEXT NOT NULL,
            domain TEXT,
            severity TEXT NOT NULL DEFAULT 'minor',
            source_run_id TEXT NOT NULL,
            source_step_id TEXT,
            positive INTEGER NOT NULL DEFAULT 1,
            times_applied INTEGER NOT NULL DEFAULT 0,
            times_helped INTEGER NOT NULL DEFAULT 0,
            embedding BLOB,
            status TEXT NOT NULL DEFAULT 'staged',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )
    .map_err(|e| format!("Failed to create playbook_entries table: {e}"))
}

/// Insert a new playbook entry into the database.
fn insert_entry(conn: &Connection, entry: &PlaybookEntry) -> Result<(), String> {
    let embedding_blob: Option<Vec<u8>> = entry.embedding.as_ref().map(|v| {
        v.iter()
            .flat_map(|f| f.to_le_bytes())
            .collect()
    });

    conn.execute(
        "INSERT INTO playbook_entries
            (id, lesson, category, domain, severity, source_run_id, source_step_id,
             positive, times_applied, times_helped, embedding, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            entry.id,
            entry.lesson,
            entry.category.to_string(),
            entry.domain,
            entry.severity.to_string(),
            entry.source_run_id,
            entry.source_step_id,
            entry.positive as i32,
            entry.times_applied,
            entry.times_helped,
            embedding_blob,
            entry.status.to_string(),
            entry.created_at,
            entry.updated_at,
        ],
    )
    .map_err(|e| format!("Failed to insert playbook entry: {e}"))?;
    Ok(())
}

/// Load a playbook entry from a database row.
fn row_to_entry(row: &rusqlite::Row) -> rusqlite::Result<PlaybookEntry> {
    let embedding_blob: Option<Vec<u8>> = row.get(10)?;
    let embedding = embedding_blob.map(|blob| {
        blob.chunks_exact(4)
            .map(|chunk| {
                let arr: [u8; 4] = chunk.try_into().unwrap_or([0; 4]);
                f32::from_le_bytes(arr)
            })
            .collect()
    });

    Ok(PlaybookEntry {
        id: row.get(0)?,
        lesson: row.get(1)?,
        category: LessonCategory::from_str(&row.get::<_, String>(2)?),
        domain: row.get(3)?,
        severity: LessonSeverity::from_str(&row.get::<_, String>(4)?),
        source_run_id: row.get(5)?,
        source_step_id: row.get(6)?,
        positive: row.get::<_, i32>(7)? != 0,
        times_applied: row.get::<_, u32>(8)?,
        times_helped: row.get::<_, u32>(9)?,
        embedding,
        status: EntryStatus::from_str(&row.get::<_, String>(11)?),
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

/// Query active entries, optionally filtered by domain.
fn query_active_entries(
    conn: &Connection,
    domain: Option<&str>,
    limit: usize,
) -> Result<Vec<PlaybookEntry>, String> {
    let (query, has_domain) = if domain.is_some() {
        (
            "SELECT id, lesson, category, domain, severity, source_run_id, source_step_id,
                    positive, times_applied, times_helped, embedding, status, created_at, updated_at
             FROM playbook_entries
             WHERE status = 'active' AND (domain = ?1 OR domain IS NULL)
             ORDER BY
                 CASE severity WHEN 'critical' THEN 0 WHEN 'important' THEN 1 ELSE 2 END,
                 times_helped DESC
             LIMIT ?2",
            true,
        )
    } else {
        (
            "SELECT id, lesson, category, domain, severity, source_run_id, source_step_id,
                    positive, times_applied, times_helped, embedding, status, created_at, updated_at
             FROM playbook_entries
             WHERE status = 'active'
             ORDER BY
                 CASE severity WHEN 'critical' THEN 0 WHEN 'important' THEN 1 ELSE 2 END,
                 times_helped DESC
             LIMIT ?1",
            false,
        )
    };

    let mut stmt = conn
        .prepare(query)
        .map_err(|e| format!("Failed to prepare query: {e}"))?;

    let entries = if has_domain {
        let d = domain.unwrap();
        stmt.query_map(params![d, limit as i64], row_to_entry)
            .map_err(|e| format!("Failed to query entries: {e}"))?
            .filter_map(|r| r.ok())
            .collect()
    } else {
        stmt.query_map(params![limit as i64], row_to_entry)
            .map_err(|e| format!("Failed to query entries: {e}"))?
            .filter_map(|r| r.ok())
            .collect()
    };

    Ok(entries)
}

/// Query all non-retired entries (for dedup during curation).
fn query_non_retired_entries(conn: &Connection) -> Result<Vec<PlaybookEntry>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, lesson, category, domain, severity, source_run_id, source_step_id,
                    positive, times_applied, times_helped, embedding, status, created_at, updated_at
             FROM playbook_entries
             WHERE status != 'retired'",
        )
        .map_err(|e| format!("Failed to prepare non-retired query: {e}"))?;
    let entries = stmt
        .query_map([], row_to_entry)
        .map_err(|e| format!("Failed to query non-retired entries: {e}"))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(entries)
}

/// Merge a new entry into an existing one: bump `times_applied`, escalate severity if needed.
fn merge_into_existing(
    conn: &Connection,
    existing_id: &str,
    new_severity: LessonSeverity,
) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();
    // Escalate severity if the new observation is higher
    conn.execute(
        "UPDATE playbook_entries
         SET times_applied = times_applied + 1,
             severity = CASE
                 WHEN ?1 = 'critical' THEN 'critical'
                 WHEN ?1 = 'important' AND severity != 'critical' THEN 'important'
                 ELSE severity
             END,
             status = 'active',
             updated_at = ?2
         WHERE id = ?3",
        params![new_severity.to_string(), now, existing_id],
    )
    .map_err(|e| format!("Failed to merge playbook entry: {e}"))?;
    Ok(())
}

/// Retire entries that have been applied 10+ times but helped less than the threshold ratio.
fn retire_underperforming_entries(
    conn: &Connection,
    threshold: f64,
) -> Result<usize, String> {
    let now = Utc::now().to_rfc3339();
    let count = conn
        .execute(
            "UPDATE playbook_entries
             SET status = 'retired', updated_at = ?1
             WHERE status = 'active'
               AND times_applied >= 10
               AND CAST(times_helped AS REAL) / CAST(times_applied AS REAL) < ?2",
            params![now, threshold],
        )
        .map_err(|e| format!("Failed to retire underperforming entries: {e}"))?;
    Ok(count)
}

/// Count the total number of active entries.
fn count_active_entries(conn: &Connection) -> Result<usize, String> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM playbook_entries WHERE status = 'active'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to count active entries: {e}"))?;
    Ok(count as usize)
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
    conn: &Connection,
) -> Result<Vec<PlaybookEntry>, String> {
    ensure_table(conn)?;

    // Gate: only reflect on interesting runs
    let is_failure_that_iterated = iterations > 0;
    let is_strong_first_attempt = iterations == 0 && overall_score > 0.8;

    if !is_failure_that_iterated && !is_strong_first_attempt {
        debug!(
            run_id,
            overall_score,
            iterations,
            "Skipping reflection — run is not interesting"
        );
        return Ok(Vec::new());
    }

    let now = Utc::now().to_rfc3339();
    let mut entries = Vec::new();

    if is_failure_that_iterated {
        // Extract anti-pattern lessons from low-scoring steps
        for step in step_evaluations {
            if step.composite_score < 0.5 {
                let (category, severity) = categorize_failure(step);
                let lesson = build_failure_lesson(step);

                entries.push(PlaybookEntry {
                    id: Uuid::new_v4().to_string(),
                    lesson,
                    category,
                    domain: None,
                    severity,
                    source_run_id: run_id.to_string(),
                    source_step_id: Some(step.step_id.clone()),
                    positive: false,
                    times_applied: 0,
                    times_helped: 0,
                    embedding: None,
                    status: EntryStatus::Staged,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                });
            }
        }

        info!(
            run_id,
            iterations,
            lessons = entries.len(),
            "Reflected on iterated run — extracted anti-pattern lessons"
        );
    }

    if is_strong_first_attempt {
        // Extract positive lessons from high-scoring steps
        for step in step_evaluations {
            if step.composite_score >= 0.85 {
                let (category, severity) = categorize_success(step);
                let lesson = build_success_lesson(step);

                entries.push(PlaybookEntry {
                    id: Uuid::new_v4().to_string(),
                    lesson,
                    category,
                    domain: None,
                    severity,
                    source_run_id: run_id.to_string(),
                    source_step_id: Some(step.step_id.clone()),
                    positive: true,
                    times_applied: 0,
                    times_helped: 0,
                    embedding: None,
                    status: EntryStatus::Staged,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                });
            }
        }

        info!(
            run_id,
            overall_score,
            lessons = entries.len(),
            "Reflected on strong first-attempt — extracted positive lessons"
        );
    }

    Ok(entries)
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
    pub fn curate(
        &self,
        new_entries: Vec<PlaybookEntry>,
        conn: &Connection,
    ) -> Result<CurationResult, String> {
        ensure_table(conn)?;

        let existing = query_non_retired_entries(conn)?;
        let mut result = CurationResult::default();

        for entry in &new_entries {
            // Check for textual dedup against existing entries
            if let Some(existing_entry) = find_similar_entry(&existing, &entry.lesson) {
                debug!(
                    existing_id = %existing_entry.id,
                    new_lesson = %entry.lesson,
                    "Merging duplicate playbook entry"
                );
                merge_into_existing(conn, &existing_entry.id, entry.severity)?;
                result.merged += 1;
            } else {
                // Insert as new active entry
                let mut active_entry = entry.clone();
                active_entry.status = EntryStatus::Active;
                insert_entry(conn, &active_entry)?;
                result.added += 1;
            }
        }

        // Retire underperforming entries
        let retired = self.retire_underperforming(conn)?;
        result.retired = retired;

        // If we're over capacity, retire the lowest-value entries
        let active_count = count_active_entries(conn)?;
        if active_count > self.max_entries {
            let overflow = active_count - self.max_entries;
            let force_retired = force_retire_lowest(conn, overflow)?;
            result.retired += force_retired;
            info!(
                overflow,
                force_retired, "Force-retired entries to stay under max_entries cap"
            );
        }

        info!(
            added = result.added,
            merged = result.merged,
            retired = result.retired,
            "Curation pass complete"
        );

        Ok(result)
    }

    /// Retire entries that have been applied 10+ times but helped less than
    /// the `retirement_threshold` ratio.
    pub fn retire_underperforming(
        &self,
        conn: &Connection,
    ) -> Result<usize, String> {
        ensure_table(conn)?;
        let count = retire_underperforming_entries(conn, self.retirement_threshold)?;
        if count > 0 {
            info!(count, threshold = self.retirement_threshold, "Retired underperforming entries");
        }
        Ok(count)
    }

    /// Select relevant lessons for prompt injection.
    ///
    /// Returns up to `max_lessons` active entries, optionally filtered by domain,
    /// ordered by severity (Critical first) then by helpfulness.
    pub fn select_lessons(
        &self,
        domain: Option<&str>,
        max_lessons: usize,
        conn: &Connection,
    ) -> Result<Vec<PlaybookEntry>, String> {
        ensure_table(conn)?;
        query_active_entries(conn, domain, max_lessons)
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
        let new_words_set: std::collections::HashSet<&str> =
            new_lower.split_whitespace().collect();

        let intersection = existing_words
            .intersection(&new_words_set)
            .count();
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
fn force_retire_lowest(conn: &Connection, count: usize) -> Result<usize, String> {
    let now = Utc::now().to_rfc3339();

    // Find the IDs of the lowest-value active entries
    let mut stmt = conn
        .prepare(
            "SELECT id FROM playbook_entries
             WHERE status = 'active'
             ORDER BY times_helped ASC, times_applied ASC
             LIMIT ?1",
        )
        .map_err(|e| format!("Failed to prepare force-retire query: {e}"))?;

    let ids: Vec<String> = stmt
        .query_map(params![count as i64], |row| row.get::<_, String>(0))
        .map_err(|e| format!("Failed to query lowest entries: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    let mut retired = 0;
    for id in &ids {
        conn.execute(
            "UPDATE playbook_entries SET status = 'retired', updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )
        .map_err(|e| format!("Failed to force-retire entry {id}: {e}"))?;
        retired += 1;
    }

    Ok(retired)
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
        let conn = Connection::open_in_memory().unwrap();
        let evals = vec![make_step_eval("step1", 0.6, 0.5, "selector")];
        let result = reflect_on_run("run1", 0.6, 0, &evals, &conn).unwrap();
        assert!(result.is_empty(), "Non-interesting run should produce no lessons");
    }

    #[test]
    fn test_reflect_extracts_failure_lessons() {
        let conn = Connection::open_in_memory().unwrap();
        let evals = vec![
            make_step_eval("bad_step", 0.3, 0.1, "selector"),
            make_step_eval("ok_step", 0.7, 0.6, "structure"),
        ];
        let result = reflect_on_run("run2", 0.4, 2, &evals, &conn).unwrap();
        assert_eq!(result.len(), 1, "Should extract one failure lesson");
        assert!(!result[0].positive);
        assert_eq!(result[0].category, LessonCategory::SelectorChoice);
        assert_eq!(result[0].severity, LessonSeverity::Critical);
    }

    #[test]
    fn test_reflect_extracts_success_lessons() {
        let conn = Connection::open_in_memory().unwrap();
        let evals = vec![
            make_step_eval("great_step", 0.95, 0.9, "tool"),
            make_step_eval("meh_step", 0.75, 0.6, "structure"),
        ];
        let result = reflect_on_run("run3", 0.9, 0, &evals, &conn).unwrap();
        assert_eq!(result.len(), 1, "Should extract one success lesson");
        assert!(result[0].positive);
    }

    #[test]
    fn test_curator_curate_and_dedup() {
        let conn = Connection::open_in_memory().unwrap();
        let curator = PlaybookCurator::new();

        let now = Utc::now().to_rfc3339();
        let entries = vec![
            PlaybookEntry {
                id: Uuid::new_v4().to_string(),
                lesson: "Always use specific CSS selectors instead of generic ones".to_string(),
                category: LessonCategory::SelectorChoice,
                domain: None,
                severity: LessonSeverity::Important,
                source_run_id: "run1".to_string(),
                source_step_id: None,
                positive: true,
                times_applied: 0,
                times_helped: 0,
                embedding: None,
                status: EntryStatus::Staged,
                created_at: now.clone(),
                updated_at: now.clone(),
            },
        ];

        let result = curator.curate(entries, &conn).unwrap();
        assert_eq!(result.added, 1);
        assert_eq!(result.merged, 0);

        // Insert a near-duplicate
        let dup_entries = vec![
            PlaybookEntry {
                id: Uuid::new_v4().to_string(),
                lesson: "Always use specific CSS selectors instead of generic ones for stability".to_string(),
                category: LessonCategory::SelectorChoice,
                domain: None,
                severity: LessonSeverity::Critical,
                source_run_id: "run2".to_string(),
                source_step_id: None,
                positive: true,
                times_applied: 0,
                times_helped: 0,
                embedding: None,
                status: EntryStatus::Staged,
                created_at: now.clone(),
                updated_at: now.clone(),
            },
        ];

        let result2 = curator.curate(dup_entries, &conn).unwrap();
        assert_eq!(result2.merged, 1, "Near-duplicate should be merged");
        assert_eq!(result2.added, 0);
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
        let conn = Connection::open_in_memory().unwrap();
        ensure_table(&conn).unwrap();

        let now = Utc::now().to_rfc3339();
        for i in 0..5 {
            let entry = PlaybookEntry {
                id: format!("entry-{i}"),
                lesson: format!("Lesson number {i} is unique and different"),
                category: LessonCategory::DomainKnowledge,
                domain: None,
                severity: LessonSeverity::Minor,
                source_run_id: "run-x".to_string(),
                source_step_id: None,
                positive: true,
                times_applied: 0,
                times_helped: 0,
                embedding: None,
                status: EntryStatus::Active,
                created_at: now.clone(),
                updated_at: now.clone(),
            };
            insert_entry(&conn, &entry).unwrap();
        }

        let curator = PlaybookCurator::new();
        let selected = curator.select_lessons(None, 3, &conn).unwrap();
        assert_eq!(selected.len(), 3);
    }
}
