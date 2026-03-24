//! CRUD and detection query operations for the `cross_run_patterns` table.
//!
//! Tracks patterns that recur across multiple workflow runs, such as
//! recurring findings and fix oscillations, enabling cross-run analysis.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossRunPattern {
    pub id: String,
    pub pattern_type: String,
    pub signature_hash: String,
    pub workflow_name: Option<String>,
    pub occurrence_count: i32,
    pub first_seen_task_run_id: Option<String>,
    pub last_seen_task_run_id: Option<String>,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub affected_components: Option<String>,
    pub pattern_data: Option<String>,
    pub status: String,
    pub resolved_by_fix_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Parse a CrossRunPattern from a database row.
/// Expected column order: id(0), pattern_type(1), signature_hash(2),
/// workflow_name(3), occurrence_count(4), first_seen_task_run_id(5),
/// last_seen_task_run_id(6), first_seen_at(7), last_seen_at(8),
/// affected_components(9), pattern_data(10), status(11),
/// resolved_by_fix_id(12), created_at(13), updated_at(14).
fn pattern_from_row(row: &rusqlite::Row) -> rusqlite::Result<CrossRunPattern> {
    Ok(CrossRunPattern {
        id: row.get(0)?,
        pattern_type: row.get(1)?,
        signature_hash: row.get(2)?,
        workflow_name: row.get(3)?,
        occurrence_count: row.get(4)?,
        first_seen_task_run_id: row.get(5)?,
        last_seen_task_run_id: row.get(6)?,
        first_seen_at: row.get(7)?,
        last_seen_at: row.get(8)?,
        affected_components: row.get(9)?,
        pattern_data: row.get(10)?,
        status: row.get(11)?,
        resolved_by_fix_id: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

const SELECT_ALL_COLUMNS: &str = r#"
    id, pattern_type, signature_hash, workflow_name, occurrence_count,
    first_seen_task_run_id, last_seen_task_run_id, first_seen_at, last_seen_at,
    affected_components, pattern_data, status, resolved_by_fix_id,
    created_at, updated_at
"#;

/// Insert or update a cross-run pattern.
///
/// If a pattern with the same `(pattern_type, signature_hash)` already exists,
/// increments `occurrence_count` and updates `last_seen_task_run_id` and
/// `last_seen_at`. Otherwise inserts a new row. Returns the pattern ID.
pub fn upsert_cross_run_pattern(
    conn: &Connection,
    pattern_type: &str,
    signature_hash: &str,
    workflow_name: Option<&str>,
    task_run_id: Option<&str>,
    affected_components: Option<&str>,
    pattern_data: Option<&str>,
) -> Result<String, String> {
    let now = chrono::Utc::now().to_rfc3339();

    // Check if a pattern with the same (pattern_type, signature_hash) exists
    let existing_id: Option<String> = conn
        .query_row(
            "SELECT id FROM cross_run_patterns WHERE pattern_type = ?1 AND signature_hash = ?2",
            params![pattern_type, signature_hash],
            |row| row.get(0),
        )
        .ok();

    match existing_id {
        Some(id) => {
            conn.execute(
                r#"UPDATE cross_run_patterns
                   SET occurrence_count = occurrence_count + 1,
                       last_seen_task_run_id = ?1,
                       last_seen_at = ?2,
                       affected_components = COALESCE(?3, affected_components),
                       pattern_data = COALESCE(?4, pattern_data),
                       updated_at = ?2
                   WHERE id = ?5"#,
                params![task_run_id, now, affected_components, pattern_data, id],
            )
            .map_err(|e| format!("Failed to update cross_run_pattern: {}", e))?;
            Ok(id)
        }
        None => {
            let id = format!("crp-{}", Uuid::new_v4());
            conn.execute(
                r#"INSERT INTO cross_run_patterns
                   (id, pattern_type, signature_hash, workflow_name, occurrence_count,
                    first_seen_task_run_id, last_seen_task_run_id,
                    first_seen_at, last_seen_at,
                    affected_components, pattern_data, status,
                    created_at, updated_at)
                   VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5, ?6, ?6, ?7, ?8, 'active', ?6, ?6)"#,
                params![
                    id,
                    pattern_type,
                    signature_hash,
                    workflow_name,
                    task_run_id,
                    now,
                    affected_components,
                    pattern_data,
                ],
            )
            .map_err(|e| format!("Failed to insert cross_run_pattern: {}", e))?;
            Ok(id)
        }
    }
}

/// Get all active patterns, optionally filtered by workflow name.
/// Results are ordered by occurrence_count descending.
pub fn get_active_patterns(
    conn: &Connection,
    workflow_name: Option<&str>,
) -> Result<Vec<CrossRunPattern>, String> {
    match workflow_name {
        Some(wf) => {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {} FROM cross_run_patterns WHERE status = 'active' AND workflow_name = ?1 ORDER BY occurrence_count DESC",
                    SELECT_ALL_COLUMNS
                ))
                .map_err(|e| format!("Failed to prepare get_active_patterns query: {}", e))?;

            let results = stmt
                .query_map(params![wf], pattern_from_row)
                .map_err(|e| format!("Failed to query active patterns: {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(results)
        }
        None => {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {} FROM cross_run_patterns WHERE status = 'active' ORDER BY occurrence_count DESC",
                    SELECT_ALL_COLUMNS
                ))
                .map_err(|e| format!("Failed to prepare get_active_patterns query: {}", e))?;

            let results = stmt
                .query_map([], pattern_from_row)
                .map_err(|e| format!("Failed to query active patterns: {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(results)
        }
    }
}

/// Get a single pattern by its ID.
pub fn get_pattern_by_id(
    conn: &Connection,
    id: &str,
) -> Result<Option<CrossRunPattern>, String> {
    let result = conn.query_row(
        &format!(
            "SELECT {} FROM cross_run_patterns WHERE id = ?1",
            SELECT_ALL_COLUMNS
        ),
        params![id],
        pattern_from_row,
    );

    match result {
        Ok(pattern) => Ok(Some(pattern)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!("Failed to query pattern by id: {}", e)),
    }
}

/// Mark a pattern as resolved, recording which fix resolved it.
pub fn resolve_pattern(
    conn: &Connection,
    pattern_id: &str,
    fix_id: &str,
) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        r#"UPDATE cross_run_patterns
           SET status = 'resolved', resolved_by_fix_id = ?1, updated_at = ?2
           WHERE id = ?3"#,
        params![fix_id, now, pattern_id],
    )
    .map_err(|e| format!("Failed to resolve pattern: {}", e))?;
    Ok(())
}

/// Mark a pattern as suppressed (acknowledged but not fixed).
pub fn suppress_pattern(conn: &Connection, pattern_id: &str) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        r#"UPDATE cross_run_patterns
           SET status = 'suppressed', updated_at = ?1
           WHERE id = ?2"#,
        params![now, pattern_id],
    )
    .map_err(|e| format!("Failed to suppress pattern: {}", e))?;
    Ok(())
}

/// Detect findings that recur across multiple task runs for a given workflow.
///
/// Finds entries in `task_run_findings` with the same `signature_hash` appearing
/// in at least `min_occurrences` distinct task runs. For each match, upserts a
/// cross-run pattern with `pattern_type='recurring_finding'`.
pub fn detect_recurring_findings(
    conn: &Connection,
    workflow_name: &str,
    min_occurrences: i32,
) -> Result<Vec<CrossRunPattern>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT f.signature_hash, COUNT(DISTINCT f.task_run_id) as run_count,
                      GROUP_CONCAT(DISTINCT f.task_run_id) as run_ids,
                      MIN(f.detected_at) as first_seen, MAX(f.detected_at) as last_seen
               FROM task_run_findings f
               JOIN task_runs t ON f.task_run_id = t.id
               WHERE t.workflow_name = ?1
               AND f.signature_hash IS NOT NULL
               GROUP BY f.signature_hash
               HAVING COUNT(DISTINCT f.task_run_id) >= ?2"#,
        )
        .map_err(|e| format!("Failed to prepare recurring findings query: {}", e))?;

    let rows: Vec<(String, i32, String, String, String)> = stmt
        .query_map(params![workflow_name, min_occurrences], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|e| format!("Failed to query recurring findings: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    let mut patterns = Vec::new();

    for (signature_hash, run_count, run_ids, _first_seen, _last_seen) in rows {
        // Extract first and last task_run_id from the comma-separated list
        let run_id_list: Vec<&str> = run_ids.split(',').collect();
        let last_task_run_id = run_id_list.last().map(|s| *s);

        let pattern_data = serde_json::json!({
            "run_count": run_count,
            "run_ids": run_ids,
        })
        .to_string();

        let id = upsert_cross_run_pattern(
            conn,
            "recurring_finding",
            &signature_hash,
            Some(workflow_name),
            last_task_run_id,
            None,
            Some(&pattern_data),
        )?;

        if let Some(pattern) = get_pattern_by_id(conn, &id)? {
            patterns.push(pattern);
        }
    }

    Ok(patterns)
}

/// Detect fix oscillations: fixes marked 'effective' whose target findings
/// later reappeared in subsequent runs.
///
/// For each such case, upserts a cross-run pattern with
/// `pattern_type='fix_oscillation'`.
pub fn detect_fix_oscillations(
    conn: &Connection,
    workflow_name: &str,
) -> Result<Vec<CrossRunPattern>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT rf.id as fix_id, rf.fix_description, rf.source_finding_id,
                      trf.signature_hash, rf.evaluated_at,
                      COUNT(later_f.id) as recurrence_count
               FROM reflection_fixes rf
               JOIN task_run_findings trf ON rf.source_finding_id = trf.id
               JOIN task_runs orig_t ON trf.task_run_id = orig_t.id
               JOIN task_run_findings later_f ON later_f.signature_hash = trf.signature_hash
               JOIN task_runs later_t ON later_f.task_run_id = later_t.id
               WHERE rf.effectiveness = 'effective'
               AND orig_t.workflow_name = ?1
               AND later_t.workflow_name = ?1
               AND later_f.detected_at > rf.evaluated_at
               AND later_f.task_run_id != rf.source_task_run_id
               GROUP BY rf.id
               HAVING COUNT(later_f.id) > 0"#,
        )
        .map_err(|e| format!("Failed to prepare fix oscillations query: {}", e))?;

    let rows: Vec<(String, Option<String>, Option<String>, Option<String>, Option<String>, i32)> = stmt
        .query_map(params![workflow_name], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i32>(5)?,
            ))
        })
        .map_err(|e| format!("Failed to query fix oscillations: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    let mut patterns = Vec::new();

    for (fix_id, fix_description, _source_finding_id, signature_hash, _evaluated_at, recurrence_count) in rows {
        let signature_hash = match signature_hash {
            Some(h) => h,
            None => continue,
        };

        let pattern_data = serde_json::json!({
            "fix_id": fix_id,
            "fix_description": fix_description,
            "recurrence_count": recurrence_count,
        })
        .to_string();

        let id = upsert_cross_run_pattern(
            conn,
            "fix_oscillation",
            &signature_hash,
            Some(workflow_name),
            None,
            None,
            Some(&pattern_data),
        )?;

        if let Some(pattern) = get_pattern_by_id(conn, &id)? {
            patterns.push(pattern);
        }
    }

    Ok(patterns)
}
