//! The knowledge ranker and the change-velocity read (plan
//! 2026-11-09-reflection-reports-only-what-it-judged-and-the-dead-verifier-leaves-no-residue,
//! Phase 3, design decisions D1 and D6).
//!
//! Both reads used to fail without saying so. The ranker selected
//! `tk.last_validated_at` and `tk.reflection_fix_id`, columns
//! `project.task_knowledge` has never had, so it errored on every database
//! and its only workflow caller fell back to a chronological list without a
//! log. Velocity keyed on `reflection_fixes.target_component`, which nothing
//! in the runner writes, and read a failed count as 0. Here every read that
//! fails returns `Err`, and every column named is one the schema declares
//! (`ranker_query_names_only_real_columns` holds the ranker's text to the
//! generated schema).

use super::PgDb;
use crate::reflection::architecture::normalize_component_path;
use crate::reflection::prediction::ScoredKnowledge;
use crate::reflection::types::FixEffectiveness;
use std::collections::HashMap;

/// SQL for `normalize_component_path` over the `file_changed` column: backslash
/// to slash, lowercase, strip an `X:/` drive, strip one leading `./`, in that
/// order. `[\x01-\x7f]` is the drive letter because the Rust version tests
/// byte 1 for `:`, which only a one-byte first character allows.
/// `normalized_sql_agrees_with_normalize_component_path` (a disposable-database
/// test) holds the two to the same literals.
pub(super) const NORMALIZED_FILE_CHANGED_SQL: &str = r"regexp_replace(regexp_replace(lower(replace(file_changed, chr(92), '/')), '^[\x01-\x7f]:/', ''), '^\./', '')";

/// Fixes applied to a component inside the window of its last `$2` distinct
/// apply instants, keyed on the normalized `file_changed` (D6).
fn velocity_count_sql() -> String {
    format!(
        r#"SELECT COUNT(*)
           FROM reflection_fixes
           WHERE {norm} = $1
             AND applied_at >= (
                 SELECT COALESCE(MIN(applied_at), '1970-01-01')
                 FROM (
                     SELECT DISTINCT applied_at FROM reflection_fixes
                     WHERE {norm} = $1
                     ORDER BY applied_at DESC
                     LIMIT $2
                 ) sub
             )"#,
        norm = NORMALIZED_FILE_CHANGED_SQL
    )
}

/// The ranker's one query. A knowledge entry's linked fix is the most
/// recently applied `reflection_fixes` row naming it in `source_knowledge_id`
/// (D1 option b); `lf.fix_id` is NULL exactly when no fix is linked, which is
/// the only thing that tells "no fix" from "a fix not yet judged".
pub(super) const SCORED_KNOWLEDGE_SQL: &str = r#"SELECT tk.id, tk.content, tk.category, tk.created_at,
          lf.fix_id, lf.effectiveness, lf.file_changed
   FROM task_knowledge tk
   INNER JOIN task_runs tr ON tr.id = tk.task_run_id
   LEFT JOIN LATERAL (
       SELECT rf.id AS fix_id, rf.effectiveness, rf.file_changed
       FROM reflection_fixes rf
       WHERE rf.source_knowledge_id = tk.id
       ORDER BY rf.applied_at DESC
       LIMIT 1
   ) lf ON true
   WHERE tr.workflow_name = $1
     AND (tk.is_resolved = false OR tk.is_resolved IS NULL)
     AND tk.category IN ('recurring_pattern', 'context', 'finding', 'root_cause',
                         'project_environment', 'project_architecture',
                         'project_test_pattern', 'project_recurring_issue')
   ORDER BY tk.created_at DESC
   LIMIT 50"#;

/// Fix-count window the ranker measures a linked fix's component over.
const RANKER_VELOCITY_WINDOW: u32 = 5;

/// What a linked fix is worth to the entry that spawned it. A fix that
/// carries no link scores 0.7 (neither vouched for nor against). A linked
/// fix scores by its verdict: effective 1.0, ineffective or caused_regression
/// 0.0, inconclusive or not yet judged 0.5. A verdict that is none of those
/// is `Err`, never a default.
pub(super) fn effectiveness_weight(
    fix_linked: bool,
    effectiveness: Option<&str>,
) -> Result<f64, String> {
    if !fix_linked {
        return Ok(0.7);
    }
    let Some(verdict) = effectiveness else {
        return Ok(0.5);
    };
    match FixEffectiveness::from_str(verdict) {
        Some(FixEffectiveness::Effective) => Ok(1.0),
        Some(FixEffectiveness::Ineffective | FixEffectiveness::CausedRegression) => Ok(0.0),
        Some(FixEffectiveness::Inconclusive) => Ok(0.5),
        None => Err(format!(
            "PG score_knowledge: a linked fix carries the unrecognised verdict {verdict:?}"
        )),
    }
}

/// `1 / (1 + 0.1 * days_old)`; an entry stamped in the future counts as new.
pub(super) fn recency_weight(
    now: chrono::DateTime<chrono::Utc>,
    created_at: chrono::DateTime<chrono::Utc>,
) -> f64 {
    let days_old = (now - created_at).num_days().max(0) as f64;
    1.0 / (1.0 + days_old * 0.1)
}

/// `1 / (1 + velocity)`: a component that keeps being fixed is a worse bet.
pub(super) fn velocity_factor_for(velocity: f64) -> f64 {
    1.0 / (1.0 + velocity)
}

/// Fixes per window slot. A window of 0 has no per-slot rate and is refused.
pub(super) fn velocity_from_count(fix_count: i64, window_size: u32) -> Result<f64, String> {
    require_window(window_size)?;
    Ok(fix_count as f64 / window_size as f64)
}

/// The refusal `compute_change_velocity` and its HTTP handler share.
pub(crate) fn require_window(window_size: u32) -> Result<(), String> {
    if window_size == 0 {
        return Err("change-velocity window must be at least 1".to_string());
    }
    Ok(())
}

/// A column of `row`, read typed; a missing or mistyped column is `Err`
/// naming it, where `Row::get` would panic.
fn column<'a, T: tokio_postgres::types::FromSql<'a>>(
    row: &'a tokio_postgres::Row,
    index: usize,
    name: &str,
) -> Result<T, String> {
    row.try_get::<_, T>(index)
        .map_err(|e| format!("PG score_knowledge: reading {name}: {e}"))
}

impl PgDb {
    /// PG equivalent of prediction::compute_change_velocity: fixes per window
    /// slot for `component_path`, counted over `reflection_fixes` whose
    /// normalized `file_changed` equals the normalized `component_path`.
    ///
    /// `Err` for a window of 0 and for any failed read; a count of 0 means
    /// zero fixes were found, never a count that failed.
    pub async fn compute_change_velocity(
        &self,
        component_path: &str,
        window_size: u32,
    ) -> Result<f64, String> {
        require_window(window_size)?;
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let component = normalize_component_path(component_path);
        let window = i64::from(window_size);
        let row = conn
            .query_one(velocity_count_sql().as_str(), &[&component, &window])
            .await
            .map_err(|e| crate::database::pg::pg_err("PG compute_change_velocity", &e))?;
        let fix_count: i64 = row
            .try_get(0)
            .map_err(|e| format!("PG compute_change_velocity: reading the count: {e}"))?;
        velocity_from_count(fix_count, window_size)
    }

    /// PG equivalent of prediction::score_knowledge_relevance: the workflow's
    /// unresolved knowledge, ranked by what its linked fix proved, how new it
    /// is, and how often the fixed component keeps changing. Any failed read
    /// is `Err`; the caller decides what a ranker that cannot run falls back
    /// to, and says so.
    pub async fn score_knowledge_relevance(
        &self,
        workflow_name: &str,
        limit: u32,
    ) -> Result<Vec<ScoredKnowledge>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let rows = conn
            .query(SCORED_KNOWLEDGE_SQL, &[&workflow_name])
            .await
            .map_err(|e| crate::database::pg::pg_err("PG score_knowledge", &e))?;

        let now = chrono::Utc::now();
        let mut velocities: HashMap<String, f64> = HashMap::new();
        let mut scored: Vec<ScoredKnowledge> = Vec::with_capacity(rows.len());

        for row in &rows {
            let id: String = column(row, 0, "tk.id")?;
            let content: String = column(row, 1, "tk.content")?;
            let category: String = column(row, 2, "tk.category")?;
            let created_at: chrono::DateTime<chrono::Utc> = column(row, 3, "tk.created_at")?;
            let linked_fix_id: Option<String> = column(row, 4, "lf.fix_id")?;
            let effectiveness: Option<String> = column(row, 5, "lf.effectiveness")?;
            let file_changed: Option<String> = column(row, 6, "lf.file_changed")?;

            let fix_linked = linked_fix_id.is_some();
            let effectiveness_weight = effectiveness_weight(fix_linked, effectiveness.as_deref())?;
            let recency_weight = recency_weight(now, created_at);

            let velocity_factor = match (fix_linked, file_changed) {
                (true, Some(path)) => {
                    let component = normalize_component_path(&path);
                    let velocity = match velocities.get(&component) {
                        Some(v) => *v,
                        None => {
                            let v = self
                                .compute_change_velocity(&component, RANKER_VELOCITY_WINDOW)
                                .await?;
                            velocities.insert(component, v);
                            v
                        }
                    };
                    velocity_factor_for(velocity)
                }
                _ => 1.0,
            };

            scored.push(ScoredKnowledge {
                knowledge_id: id,
                content,
                category,
                relevance_score: effectiveness_weight * recency_weight * velocity_factor,
                effectiveness_weight,
                recency_weight,
                velocity_factor,
            });
        }

        scored.sort_by(|a, b| {
            b.relevance_score
                .partial_cmp(&a.relevance_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(limit as usize);
        Ok(scored)
    }
}
