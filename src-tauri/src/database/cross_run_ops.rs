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
        let last_task_run_id = run_id_list.last().copied();

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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Create an in-memory SQLite database with the minimal schema needed for
    /// cross_run_patterns plus the dependency tables used by detection queries.
    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();

        // Disable FK enforcement so we can use minimal table stubs
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();

        conn.execute_batch(
            r#"
            CREATE TABLE task_runs (
                id TEXT PRIMARY KEY,
                task_name TEXT NOT NULL DEFAULT '',
                prompt TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'completed',
                workflow_name TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE task_run_findings (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                category TEXT NOT NULL DEFAULT 'bug',
                severity TEXT NOT NULL DEFAULT 'medium',
                signature_hash TEXT,
                title TEXT NOT NULL DEFAULT '',
                description TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'detected',
                action_type TEXT NOT NULL DEFAULT 'auto_fix',
                detected_in_session INTEGER NOT NULL DEFAULT 0,
                detected_at TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (task_run_id) REFERENCES task_runs(id)
            );

            CREATE TABLE reflection_fixes (
                id TEXT PRIMARY KEY,
                source_task_run_id TEXT NOT NULL,
                reflection_task_run_id TEXT NOT NULL,
                source_finding_id TEXT,
                fix_type TEXT NOT NULL DEFAULT 'code',
                fix_description TEXT NOT NULL DEFAULT '',
                effectiveness TEXT,
                evaluated_at TEXT,
                applied_at TEXT NOT NULL DEFAULT (datetime('now')),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (source_task_run_id) REFERENCES task_runs(id),
                FOREIGN KEY (source_finding_id) REFERENCES task_run_findings(id)
            );

            CREATE TABLE cross_run_patterns (
                id TEXT PRIMARY KEY,
                pattern_type TEXT NOT NULL,
                signature_hash TEXT NOT NULL,
                workflow_name TEXT,
                occurrence_count INTEGER NOT NULL DEFAULT 1,
                first_seen_task_run_id TEXT,
                last_seen_task_run_id TEXT,
                first_seen_at TEXT NOT NULL,
                last_seen_at TEXT NOT NULL,
                affected_components TEXT,
                pattern_data TEXT,
                status TEXT NOT NULL DEFAULT 'active',
                resolved_by_fix_id TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(pattern_type, signature_hash)
            );
            CREATE INDEX IF NOT EXISTS idx_crp_type ON cross_run_patterns(pattern_type);
            CREATE INDEX IF NOT EXISTS idx_crp_workflow ON cross_run_patterns(workflow_name);
            CREATE INDEX IF NOT EXISTS idx_crp_status ON cross_run_patterns(status);
            CREATE INDEX IF NOT EXISTS idx_crp_signature ON cross_run_patterns(signature_hash);
            "#,
        )
        .unwrap();

        conn
    }

    // -----------------------------------------------------------------------
    // Basic CRUD tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_upsert_insert_new_pattern() {
        let conn = setup_test_db();

        let id = upsert_cross_run_pattern(
            &conn,
            "recurring_finding",
            "hash-abc",
            Some("wf-alpha"),
            Some("run-1"),
            Some("component-a"),
            Some(r#"{"detail":"first"}"#),
        )
        .unwrap();

        assert!(id.starts_with("crp-"));

        let pattern = get_pattern_by_id(&conn, &id).unwrap().unwrap();
        assert_eq!(pattern.pattern_type, "recurring_finding");
        assert_eq!(pattern.signature_hash, "hash-abc");
        assert_eq!(pattern.workflow_name.as_deref(), Some("wf-alpha"));
        assert_eq!(pattern.occurrence_count, 1);
        assert_eq!(pattern.first_seen_task_run_id.as_deref(), Some("run-1"));
        assert_eq!(pattern.last_seen_task_run_id.as_deref(), Some("run-1"));
        assert_eq!(pattern.status, "active");
        assert!(pattern.resolved_by_fix_id.is_none());
    }

    #[test]
    fn test_upsert_increments_existing() {
        let conn = setup_test_db();

        let id1 = upsert_cross_run_pattern(
            &conn,
            "recurring_finding",
            "hash-dup",
            Some("wf-beta"),
            Some("run-1"),
            None,
            None,
        )
        .unwrap();

        let id2 = upsert_cross_run_pattern(
            &conn,
            "recurring_finding",
            "hash-dup",
            Some("wf-beta"),
            Some("run-2"),
            None,
            None,
        )
        .unwrap();

        // Same pattern row should be returned
        assert_eq!(id1, id2);

        let pattern = get_pattern_by_id(&conn, &id1).unwrap().unwrap();
        assert_eq!(pattern.occurrence_count, 2);
        assert_eq!(pattern.last_seen_task_run_id.as_deref(), Some("run-2"));
        // first_seen should still point to the original run
        assert_eq!(pattern.first_seen_task_run_id.as_deref(), Some("run-1"));
    }

    #[test]
    fn test_upsert_updates_pattern_data() {
        let conn = setup_test_db();

        upsert_cross_run_pattern(
            &conn,
            "recurring_finding",
            "hash-data",
            None,
            None,
            None,
            Some(r#"{"v":1}"#),
        )
        .unwrap();

        let id = upsert_cross_run_pattern(
            &conn,
            "recurring_finding",
            "hash-data",
            None,
            None,
            None,
            Some(r#"{"v":2}"#),
        )
        .unwrap();

        let pattern = get_pattern_by_id(&conn, &id).unwrap().unwrap();
        assert_eq!(pattern.pattern_data.as_deref(), Some(r#"{"v":2}"#));
    }

    #[test]
    fn test_get_active_patterns() {
        let conn = setup_test_db();

        let active_id = upsert_cross_run_pattern(
            &conn,
            "recurring_finding",
            "hash-active",
            Some("wf"),
            None,
            None,
            None,
        )
        .unwrap();

        let resolved_id = upsert_cross_run_pattern(
            &conn,
            "fix_oscillation",
            "hash-resolved",
            Some("wf"),
            None,
            None,
            None,
        )
        .unwrap();
        resolve_pattern(&conn, &resolved_id, "fix-1").unwrap();

        let suppressed_id = upsert_cross_run_pattern(
            &conn,
            "recurring_finding",
            "hash-suppressed",
            Some("wf"),
            None,
            None,
            None,
        )
        .unwrap();
        suppress_pattern(&conn, &suppressed_id).unwrap();

        let active = get_active_patterns(&conn, None).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, active_id);
    }

    #[test]
    fn test_get_active_patterns_filtered_by_workflow() {
        let conn = setup_test_db();

        upsert_cross_run_pattern(
            &conn,
            "recurring_finding",
            "hash-wf1",
            Some("workflow-one"),
            None,
            None,
            None,
        )
        .unwrap();

        upsert_cross_run_pattern(
            &conn,
            "recurring_finding",
            "hash-wf2",
            Some("workflow-two"),
            None,
            None,
            None,
        )
        .unwrap();

        let wf1 = get_active_patterns(&conn, Some("workflow-one")).unwrap();
        assert_eq!(wf1.len(), 1);
        assert_eq!(wf1[0].workflow_name.as_deref(), Some("workflow-one"));

        let wf2 = get_active_patterns(&conn, Some("workflow-two")).unwrap();
        assert_eq!(wf2.len(), 1);
        assert_eq!(wf2[0].workflow_name.as_deref(), Some("workflow-two"));

        let all = get_active_patterns(&conn, None).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_get_pattern_by_id() {
        let conn = setup_test_db();

        let id = upsert_cross_run_pattern(
            &conn,
            "fix_oscillation",
            "hash-byid",
            Some("wf-gamma"),
            Some("run-77"),
            Some("comp-x,comp-y"),
            Some(r#"{"key":"val"}"#),
        )
        .unwrap();

        let pattern = get_pattern_by_id(&conn, &id).unwrap().unwrap();
        assert_eq!(pattern.id, id);
        assert_eq!(pattern.pattern_type, "fix_oscillation");
        assert_eq!(pattern.signature_hash, "hash-byid");
        assert_eq!(pattern.workflow_name.as_deref(), Some("wf-gamma"));
        assert_eq!(pattern.occurrence_count, 1);
        assert_eq!(pattern.first_seen_task_run_id.as_deref(), Some("run-77"));
        assert_eq!(pattern.last_seen_task_run_id.as_deref(), Some("run-77"));
        assert_eq!(
            pattern.affected_components.as_deref(),
            Some("comp-x,comp-y")
        );
        assert_eq!(pattern.pattern_data.as_deref(), Some(r#"{"key":"val"}"#));
        assert_eq!(pattern.status, "active");
        assert!(pattern.resolved_by_fix_id.is_none());
        // Timestamps should be non-empty ISO strings
        assert!(!pattern.first_seen_at.is_empty());
        assert!(!pattern.last_seen_at.is_empty());
        assert!(!pattern.created_at.is_empty());
        assert!(!pattern.updated_at.is_empty());
    }

    #[test]
    fn test_get_pattern_by_id_not_found() {
        let conn = setup_test_db();
        let result = get_pattern_by_id(&conn, "nonexistent-id").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_pattern() {
        let conn = setup_test_db();

        let id = upsert_cross_run_pattern(
            &conn,
            "recurring_finding",
            "hash-resolve",
            None,
            None,
            None,
            None,
        )
        .unwrap();

        resolve_pattern(&conn, &id, "fix-42").unwrap();

        let pattern = get_pattern_by_id(&conn, &id).unwrap().unwrap();
        assert_eq!(pattern.status, "resolved");
        assert_eq!(pattern.resolved_by_fix_id.as_deref(), Some("fix-42"));
    }

    #[test]
    fn test_suppress_pattern() {
        let conn = setup_test_db();

        let id = upsert_cross_run_pattern(
            &conn,
            "recurring_finding",
            "hash-suppress",
            None,
            None,
            None,
            None,
        )
        .unwrap();

        suppress_pattern(&conn, &id).unwrap();

        let pattern = get_pattern_by_id(&conn, &id).unwrap().unwrap();
        assert_eq!(pattern.status, "suppressed");
        assert!(pattern.resolved_by_fix_id.is_none());
    }

    // -----------------------------------------------------------------------
    // Detection query tests
    // -----------------------------------------------------------------------

    /// Seed a task_run row.
    fn seed_task_run(conn: &Connection, id: &str, workflow_name: &str) {
        conn.execute(
            "INSERT INTO task_runs (id, workflow_name) VALUES (?1, ?2)",
            params![id, workflow_name],
        )
        .unwrap();
    }

    /// Seed a task_run_finding row.
    fn seed_finding(
        conn: &Connection,
        id: &str,
        task_run_id: &str,
        signature_hash: &str,
        detected_at: &str,
    ) {
        conn.execute(
            "INSERT INTO task_run_findings (id, task_run_id, signature_hash, detected_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, task_run_id, signature_hash, detected_at],
        )
        .unwrap();
    }

    /// Seed a reflection_fix row.
    fn seed_fix(
        conn: &Connection,
        id: &str,
        source_task_run_id: &str,
        source_finding_id: &str,
        effectiveness: &str,
        evaluated_at: &str,
    ) {
        conn.execute(
            r#"INSERT INTO reflection_fixes
               (id, source_task_run_id, reflection_task_run_id, source_finding_id,
                fix_description, effectiveness, evaluated_at)
               VALUES (?1, ?2, ?2, ?3, 'test fix', ?4, ?5)"#,
            params![id, source_task_run_id, source_finding_id, effectiveness, evaluated_at],
        )
        .unwrap();
    }

    #[test]
    fn test_detect_recurring_findings() {
        let conn = setup_test_db();

        // 3 task_runs in the same workflow
        seed_task_run(&conn, "run-a", "wf-detect");
        seed_task_run(&conn, "run-b", "wf-detect");
        seed_task_run(&conn, "run-c", "wf-detect");

        // Same signature_hash across all 3 runs
        seed_finding(&conn, "f-1", "run-a", "sig-recurring", "2026-01-01T00:00:00Z");
        seed_finding(&conn, "f-2", "run-b", "sig-recurring", "2026-01-02T00:00:00Z");
        seed_finding(&conn, "f-3", "run-c", "sig-recurring", "2026-01-03T00:00:00Z");

        let patterns = detect_recurring_findings(&conn, "wf-detect", 3).unwrap();
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].pattern_type, "recurring_finding");
        assert_eq!(patterns[0].signature_hash, "sig-recurring");
        assert_eq!(patterns[0].workflow_name.as_deref(), Some("wf-detect"));
        assert_eq!(patterns[0].status, "active");

        // pattern_data should contain run_count
        let data: serde_json::Value =
            serde_json::from_str(patterns[0].pattern_data.as_deref().unwrap()).unwrap();
        assert_eq!(data["run_count"], 3);
    }

    #[test]
    fn test_detect_recurring_findings_below_threshold() {
        let conn = setup_test_db();

        // Only 2 runs with the same signature
        seed_task_run(&conn, "run-x", "wf-below");
        seed_task_run(&conn, "run-y", "wf-below");

        seed_finding(&conn, "f-10", "run-x", "sig-few", "2026-02-01T00:00:00Z");
        seed_finding(&conn, "f-11", "run-y", "sig-few", "2026-02-02T00:00:00Z");

        let patterns = detect_recurring_findings(&conn, "wf-below", 3).unwrap();
        assert!(patterns.is_empty());
    }

    #[test]
    fn test_detect_fix_oscillations() {
        let conn = setup_test_db();

        // Original run with finding
        seed_task_run(&conn, "run-orig", "wf-osc");
        seed_finding(
            &conn,
            "f-orig",
            "run-orig",
            "sig-osc",
            "2026-03-01T00:00:00Z",
        );

        // Fix was applied and marked effective, evaluated at a specific time
        seed_fix(
            &conn,
            "fix-1",
            "run-orig",
            "f-orig",
            "effective",
            "2026-03-02T00:00:00Z",
        );

        // Later run: same signature reappears AFTER the fix was evaluated
        seed_task_run(&conn, "run-later", "wf-osc");
        seed_finding(
            &conn,
            "f-later",
            "run-later",
            "sig-osc",
            "2026-03-03T00:00:00Z",
        );

        let patterns = detect_fix_oscillations(&conn, "wf-osc").unwrap();
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].pattern_type, "fix_oscillation");
        assert_eq!(patterns[0].signature_hash, "sig-osc");

        let data: serde_json::Value =
            serde_json::from_str(patterns[0].pattern_data.as_deref().unwrap()).unwrap();
        assert_eq!(data["fix_id"], "fix-1");
        assert_eq!(data["recurrence_count"], 1);
    }

    #[test]
    fn test_detect_fix_oscillations_no_recurrence() {
        let conn = setup_test_db();

        // Original run with finding
        seed_task_run(&conn, "run-stable", "wf-stable");
        seed_finding(
            &conn,
            "f-stable",
            "run-stable",
            "sig-stable",
            "2026-04-01T00:00:00Z",
        );

        // Fix marked effective
        seed_fix(
            &conn,
            "fix-stable",
            "run-stable",
            "f-stable",
            "effective",
            "2026-04-02T00:00:00Z",
        );

        // Later run exists but has a DIFFERENT signature — no recurrence
        seed_task_run(&conn, "run-clean", "wf-stable");
        seed_finding(
            &conn,
            "f-clean",
            "run-clean",
            "sig-different",
            "2026-04-03T00:00:00Z",
        );

        let patterns = detect_fix_oscillations(&conn, "wf-stable").unwrap();
        assert!(patterns.is_empty());
    }
}

/// Fix effectiveness score for resolved patterns.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FixEffectivenessScore {
    pub fix_id: String,
    pub pattern_id: String,
    pub pattern_type: String,
    pub resolved_at: Option<String>,
    pub re_appeared: bool,
    pub occurrence_count_at_resolution: i64,
}

/// Get fix effectiveness scores: resolved patterns that stayed resolved vs re-appeared.
pub fn get_fix_effectiveness_scores(
    conn: &Connection,
    limit: i64,
) -> Result<Vec<FixEffectivenessScore>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT p.resolved_by_fix_id, p.id, p.pattern_type,
                      p.updated_at, p.status, p.occurrence_count
               FROM cross_run_patterns p
               WHERE p.resolved_by_fix_id IS NOT NULL
               ORDER BY p.updated_at DESC
               LIMIT ?1"#,
        )
        .map_err(|e| format!("Failed to prepare fix effectiveness query: {e}"))?;

    let results = stmt
        .query_map(params![limit], |row| {
            let status: String = row.get(4)?;
            Ok(FixEffectivenessScore {
                fix_id: row.get(0)?,
                pattern_id: row.get(1)?,
                pattern_type: row.get(2)?,
                resolved_at: row.get(3)?,
                re_appeared: status == "active", // re-appeared if back to active
                occurrence_count_at_resolution: row.get(5)?,
            })
        })
        .map_err(|e| format!("Failed to query fix effectiveness: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}