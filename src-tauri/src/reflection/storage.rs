//! Storage operations for reflection fixes in SQLite.
//!
//! Provides CRUD operations and queries for the reflection_fixes table.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use tracing::{debug, info};

use super::types::{CreateReflectionFixInput, EffectivenessReport, ReflectionFix};

/// Convert a database row to a ReflectionFix struct.
fn row_to_fix(row: &rusqlite::Row) -> rusqlite::Result<ReflectionFix> {
    Ok(ReflectionFix {
        id: row.get("id")?,
        source_task_run_id: row.get("source_task_run_id")?,
        reflection_task_run_id: row.get("reflection_task_run_id")?,
        source_finding_id: row.get("source_finding_id")?,
        source_knowledge_id: row.get("source_knowledge_id")?,
        fix_type: row.get("fix_type")?,
        fix_description: row.get("fix_description")?,
        file_changed: row.get("file_changed")?,
        old_value: row.get("old_value")?,
        new_value: row.get("new_value")?,
        confidence: row.get("confidence")?,
        status: row.get("status")?,
        effectiveness: row.get("effectiveness")?,
        effectiveness_evidence: row.get("effectiveness_evidence")?,
        applied_at: row.get("applied_at")?,
        evaluated_at: row.get("evaluated_at")?,
        created_at: row.get("created_at")?,
    })
}

const SELECT_ALL_COLUMNS: &str = r#"
    id, source_task_run_id, reflection_task_run_id,
    source_finding_id, source_knowledge_id,
    fix_type, fix_description, file_changed,
    old_value, new_value, confidence, status,
    effectiveness, effectiveness_evidence,
    applied_at, evaluated_at, created_at
"#;

/// Insert a new reflection fix into the database.
pub fn insert_fix(
    conn: &Connection,
    input: &CreateReflectionFixInput,
) -> Result<ReflectionFix, String> {
    let now = Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();

    conn.execute(
        r#"
        INSERT INTO reflection_fixes (
            id, source_task_run_id, reflection_task_run_id,
            source_finding_id, source_knowledge_id,
            fix_type, fix_description, file_changed,
            old_value, new_value, confidence, status,
            applied_at, created_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'applied', ?12, ?12)
        "#,
        params![
            id,
            input.source_task_run_id,
            input.reflection_task_run_id,
            input.source_finding_id,
            input.source_knowledge_id,
            input.fix_type,
            input.fix_description,
            input.file_changed,
            input.old_value,
            input.new_value,
            input.confidence,
            now,
        ],
    )
    .map_err(|e| format!("Failed to insert reflection fix: {}", e))?;

    info!("Inserted reflection fix {} (type: {})", id, input.fix_type);

    get_fix(conn, &id)?.ok_or_else(|| "Fix not found after insert".to_string())
}

/// Get a single reflection fix by ID.
pub fn get_fix(conn: &Connection, id: &str) -> Result<Option<ReflectionFix>, String> {
    let sql = format!(
        "SELECT {} FROM reflection_fixes WHERE id = ?1",
        SELECT_ALL_COLUMNS
    );
    conn.query_row(&sql, params![id], row_to_fix)
        .optional()
        .map_err(|e| format!("Failed to get reflection fix: {}", e))
}

/// Get all fixes created by analyzing a specific source run.
pub fn get_fixes_for_source_run(
    conn: &Connection,
    source_task_run_id: &str,
) -> Result<Vec<ReflectionFix>, String> {
    let sql = format!(
        "SELECT {} FROM reflection_fixes WHERE source_task_run_id = ?1 ORDER BY created_at",
        SELECT_ALL_COLUMNS
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare query: {}", e))?;
    let rows = stmt
        .query_map(params![source_task_run_id], row_to_fix)
        .map_err(|e| format!("Failed to query fixes for source run: {}", e))?;

    let mut fixes = Vec::new();
    for row in rows {
        fixes.push(row.map_err(|e| format!("Failed to read fix row: {}", e))?);
    }
    Ok(fixes)
}

/// Get all fixes created by a specific reflection run.
pub fn get_fixes_for_reflection_run(
    conn: &Connection,
    reflection_task_run_id: &str,
) -> Result<Vec<ReflectionFix>, String> {
    let sql = format!(
        "SELECT {} FROM reflection_fixes WHERE reflection_task_run_id = ?1 ORDER BY created_at",
        SELECT_ALL_COLUMNS
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare query: {}", e))?;
    let rows = stmt
        .query_map(params![reflection_task_run_id], row_to_fix)
        .map_err(|e| format!("Failed to query fixes for reflection run: {}", e))?;

    let mut fixes = Vec::new();
    for row in rows {
        fixes.push(row.map_err(|e| format!("Failed to read fix row: {}", e))?);
    }
    Ok(fixes)
}

/// Get all fixes for runs of a specific workflow, with optional status and effectiveness filters.
pub fn get_fixes_by_workflow_name(
    conn: &Connection,
    workflow_name: &str,
    status_filter: Option<&str>,
    effectiveness_filter: Option<&str>,
) -> Result<Vec<ReflectionFix>, String> {
    let mut sql = format!(
        r#"SELECT rf.{} FROM reflection_fixes rf
        INNER JOIN task_runs tr ON rf.source_task_run_id = tr.id
        WHERE tr.workflow_name = ?1"#,
        SELECT_ALL_COLUMNS
            .replace("id,", "rf.id,")
            .replace("source_task_run_id,", "rf.source_task_run_id,")
    );

    // Build dynamic WHERE clauses
    let mut param_idx = 2;
    if status_filter.is_some() {
        sql.push_str(&format!(" AND rf.status = ?{}", param_idx));
        param_idx += 1;
    }
    if effectiveness_filter.is_some() {
        if effectiveness_filter == Some("unevaluated") {
            sql.push_str(" AND rf.effectiveness IS NULL");
        } else {
            sql.push_str(&format!(" AND rf.effectiveness = ?{}", param_idx));
        }
    }
    sql.push_str(" ORDER BY rf.created_at DESC");

    // Unfortunately rusqlite doesn't support truly dynamic params easily,
    // so we build params manually based on what's present
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    // Execute with appropriate params
    let rows = match (status_filter, effectiveness_filter) {
        (Some(s), Some(e)) if e != "unevaluated" => stmt
            .query_map(params![workflow_name, s, e], row_to_fix)
            .map_err(|e| format!("Failed to query: {}", e))?,
        (Some(s), _) => stmt
            .query_map(params![workflow_name, s], row_to_fix)
            .map_err(|e| format!("Failed to query: {}", e))?,
        (None, Some(e)) if e != "unevaluated" => stmt
            .query_map(params![workflow_name, e], row_to_fix)
            .map_err(|e| format!("Failed to query: {}", e))?,
        _ => stmt
            .query_map(params![workflow_name], row_to_fix)
            .map_err(|e| format!("Failed to query: {}", e))?,
    };

    let mut fixes = Vec::new();
    for row in rows {
        fixes.push(row.map_err(|e| format!("Failed to read fix row: {}", e))?);
    }
    Ok(fixes)
}

/// Update the status of a reflection fix (applied/reverted/superseded).
pub fn update_fix_status(conn: &Connection, id: &str, status: &str) -> Result<(), String> {
    let affected = conn
        .execute(
            "UPDATE reflection_fixes SET status = ?1 WHERE id = ?2",
            params![status, id],
        )
        .map_err(|e| format!("Failed to update fix status: {}", e))?;

    if affected == 0 {
        return Err(format!("Reflection fix {} not found", id));
    }

    debug!("Updated reflection fix {} status to {}", id, status);
    Ok(())
}

/// Update the effectiveness evaluation of a reflection fix.
pub fn update_fix_effectiveness(
    conn: &Connection,
    id: &str,
    effectiveness: &str,
    evidence: Option<&str>,
) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();
    let affected = conn
        .execute(
            r#"UPDATE reflection_fixes
            SET effectiveness = ?1, effectiveness_evidence = ?2, evaluated_at = ?3
            WHERE id = ?4"#,
            params![effectiveness, evidence, now, id],
        )
        .map_err(|e| format!("Failed to update fix effectiveness: {}", e))?;

    if affected == 0 {
        return Err(format!("Reflection fix {} not found", id));
    }

    info!(
        "Updated reflection fix {} effectiveness to {}",
        id, effectiveness
    );
    Ok(())
}

/// Generate an aggregated effectiveness report for a workflow.
pub fn get_effectiveness_report(
    conn: &Connection,
    workflow_name: &str,
) -> Result<EffectivenessReport, String> {
    let fixes = get_fixes_by_workflow_name(conn, workflow_name, None, None)?;
    let total = fixes.len() as u32;

    let mut effective = 0u32;
    let mut ineffective = 0u32;
    let mut regression = 0u32;
    let mut inconclusive = 0u32;
    let mut unevaluated = 0u32;

    for fix in &fixes {
        match fix.effectiveness.as_deref() {
            Some("effective") => effective += 1,
            Some("ineffective") => ineffective += 1,
            Some("caused_regression") => regression += 1,
            Some("inconclusive") => inconclusive += 1,
            None => unevaluated += 1,
            _ => unevaluated += 1,
        }
    }

    let evaluated = effective + ineffective + regression;
    let effectiveness_rate = if evaluated > 0 {
        effective as f64 / evaluated as f64
    } else {
        0.0
    };

    Ok(EffectivenessReport {
        workflow_name: workflow_name.to_string(),
        total_fixes: total,
        effective_count: effective,
        ineffective_count: ineffective,
        regression_count: regression,
        inconclusive_count: inconclusive,
        unevaluated_count: unevaluated,
        effectiveness_rate,
        fixes,
    })
}

/// Get all reflection task runs for a workflow (history).
pub fn get_reflection_history(
    conn: &Connection,
    workflow_name: &str,
) -> Result<Vec<ReflectionRunSummary>, String> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT
                tr.id,
                tr.reflection_source_task_run_id,
                tr.status,
                tr.created_at,
                tr.completed_at,
                (SELECT COUNT(*) FROM reflection_fixes rf WHERE rf.reflection_task_run_id = tr.id) as fix_count
            FROM task_runs tr
            WHERE tr.is_reflection = 1
              AND tr.workflow_name LIKE '%' || ?1 || '%'
            ORDER BY tr.created_at DESC
            "#,
        )
        .map_err(|e| format!("Failed to prepare reflection history query: {}", e))?;

    let rows = stmt
        .query_map(params![workflow_name], |row| {
            Ok(ReflectionRunSummary {
                task_run_id: row.get("id")?,
                source_task_run_id: row.get("reflection_source_task_run_id")?,
                status: row.get("status")?,
                created_at: row.get("created_at")?,
                completed_at: row.get("completed_at")?,
                fix_count: row.get("fix_count")?,
            })
        })
        .map_err(|e| format!("Failed to query reflection history: {}", e))?;

    let mut summaries = Vec::new();
    for row in rows {
        summaries.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
    }
    Ok(summaries)
}

/// Summary of a reflection run for history display.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReflectionRunSummary {
    pub task_run_id: String,
    pub source_task_run_id: Option<String>,
    pub status: String,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub fix_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE task_runs (
                id TEXT PRIMARY KEY,
                task_name TEXT NOT NULL,
                workflow_name TEXT,
                status TEXT DEFAULT 'running',
                is_reflection INTEGER DEFAULT 0,
                reflection_source_task_run_id TEXT,
                created_at TEXT NOT NULL,
                completed_at TEXT,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE task_run_findings (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                signature_hash TEXT,
                category TEXT,
                title TEXT,
                detected_at TEXT,
                reflection_fix_id TEXT
            );
            CREATE TABLE task_knowledge (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                reflection_fix_id TEXT
            );
            CREATE TABLE reflection_fixes (
                id TEXT PRIMARY KEY,
                source_task_run_id TEXT NOT NULL,
                reflection_task_run_id TEXT NOT NULL,
                source_finding_id TEXT,
                source_knowledge_id TEXT,
                fix_type TEXT NOT NULL,
                fix_description TEXT NOT NULL,
                file_changed TEXT,
                old_value TEXT,
                new_value TEXT,
                confidence TEXT NOT NULL DEFAULT 'medium',
                status TEXT NOT NULL DEFAULT 'applied',
                effectiveness TEXT,
                effectiveness_evidence TEXT,
                applied_at TEXT NOT NULL,
                evaluated_at TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (source_task_run_id) REFERENCES task_runs(id),
                FOREIGN KEY (reflection_task_run_id) REFERENCES task_runs(id)
            );
            "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_insert_and_get_fix() {
        let conn = setup_test_db();

        // Create prerequisite task runs
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, created_at, updated_at) VALUES ('src-1', 'Test', 'my-workflow', datetime('now'), datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, is_reflection, reflection_source_task_run_id, created_at, updated_at) VALUES ('ref-1', 'Reflection', 'Reflection: my-workflow', 1, 'src-1', datetime('now'), datetime('now'))",
            [],
        ).unwrap();

        let input = CreateReflectionFixInput {
            source_task_run_id: "src-1".to_string(),
            reflection_task_run_id: "ref-1".to_string(),
            source_finding_id: None,
            source_knowledge_id: None,
            fix_type: "context_addition".to_string(),
            fix_description: "Added missing API endpoint docs".to_string(),
            file_changed: Some("docs/api.md".to_string()),
            old_value: None,
            new_value: Some("New docs content".to_string()),
            confidence: "high".to_string(),
        };

        let fix = insert_fix(&conn, &input).unwrap();
        assert_eq!(fix.source_task_run_id, "src-1");
        assert_eq!(fix.fix_type, "context_addition");
        assert_eq!(fix.status, "applied");
        assert!(fix.effectiveness.is_none());

        // Verify get_fix works
        let fetched = get_fix(&conn, &fix.id).unwrap().unwrap();
        assert_eq!(fetched.id, fix.id);
    }

    #[test]
    fn test_get_fixes_for_source_run() {
        let conn = setup_test_db();

        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, created_at, updated_at) VALUES ('src-1', 'Test', 'wf', datetime('now'), datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO task_runs (id, task_name, is_reflection, reflection_source_task_run_id, created_at, updated_at) VALUES ('ref-1', 'Ref', 1, 'src-1', datetime('now'), datetime('now'))",
            [],
        ).unwrap();

        // Insert two fixes
        for desc in &["Fix A", "Fix B"] {
            let input = CreateReflectionFixInput {
                source_task_run_id: "src-1".to_string(),
                reflection_task_run_id: "ref-1".to_string(),
                source_finding_id: None,
                source_knowledge_id: None,
                fix_type: "selector_fix".to_string(),
                fix_description: desc.to_string(),
                file_changed: None,
                old_value: None,
                new_value: None,
                confidence: "medium".to_string(),
            };
            insert_fix(&conn, &input).unwrap();
        }

        let fixes = get_fixes_for_source_run(&conn, "src-1").unwrap();
        assert_eq!(fixes.len(), 2);
    }

    #[test]
    fn test_update_effectiveness() {
        let conn = setup_test_db();

        conn.execute(
            "INSERT INTO task_runs (id, task_name, created_at, updated_at) VALUES ('s1', 'T', datetime('now'), datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO task_runs (id, task_name, is_reflection, created_at, updated_at) VALUES ('r1', 'R', 1, datetime('now'), datetime('now'))",
            [],
        ).unwrap();

        let input = CreateReflectionFixInput {
            source_task_run_id: "s1".to_string(),
            reflection_task_run_id: "r1".to_string(),
            source_finding_id: None,
            source_knowledge_id: None,
            fix_type: "tool_config_update".to_string(),
            fix_description: "Increased timeout".to_string(),
            file_changed: None,
            old_value: Some("5000".to_string()),
            new_value: Some("15000".to_string()),
            confidence: "high".to_string(),
        };

        let fix = insert_fix(&conn, &input).unwrap();
        assert!(fix.effectiveness.is_none());

        update_fix_effectiveness(&conn, &fix.id, "effective", Some("No recurrence in 3 runs"))
            .unwrap();

        let updated = get_fix(&conn, &fix.id).unwrap().unwrap();
        assert_eq!(updated.effectiveness.as_deref(), Some("effective"));
        assert_eq!(
            updated.effectiveness_evidence.as_deref(),
            Some("No recurrence in 3 runs")
        );
        assert!(updated.evaluated_at.is_some());
    }
}
