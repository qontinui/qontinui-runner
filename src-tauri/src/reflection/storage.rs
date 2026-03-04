//! Storage operations for reflection fixes in SQLite.
//!
//! Provides CRUD operations and queries for the reflection_fixes table.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use tracing::{debug, info, warn};

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
        content_hash: row.get("content_hash")?,
        status: row.get("status")?,
        effectiveness: row.get("effectiveness")?,
        effectiveness_evidence: row.get("effectiveness_evidence")?,
        applied_at: row.get("applied_at")?,
        evaluated_at: row.get("evaluated_at")?,
        created_at: row.get("created_at")?,
        source_agent: row.get("source_agent").unwrap_or(None),
    })
}

const SELECT_ALL_COLUMNS: &str = r#"
    id, source_task_run_id, reflection_task_run_id,
    source_finding_id, source_knowledge_id,
    fix_type, fix_description, file_changed,
    old_value, new_value, confidence, content_hash,
    status, effectiveness, effectiveness_evidence,
    applied_at, evaluated_at, created_at, source_agent
"#;

/// Normalize text for semantic deduplication.
///
/// Strips run-specific content (UUIDs, timestamps, hex hashes) and normalizes
/// whitespace/case so that semantically identical fixes with slightly different
/// wording produce the same hash.
fn normalize_for_hash(text: &str) -> String {
    use once_cell::sync::Lazy;
    use regex::Regex;

    static UUID_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
            .unwrap()
    });
    static TIMESTAMP_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)\d{4}-\d{2}-\d{2}t\d{2}:\d{2}:\d{2}[^\s]*").unwrap());
    static HEX_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b[0-9a-fA-F]{8,}\b").unwrap());
    static RUN_NUMBER_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)\b(?:run|iteration|session)\s*\d+").unwrap());

    let text = text.to_lowercase();
    let text = UUID_RE.replace_all(&text, "").to_string();
    let text = TIMESTAMP_RE.replace_all(&text, "").to_string();
    let text = HEX_RE.replace_all(&text, "").to_string();
    let text = RUN_NUMBER_RE.replace_all(&text, "").to_string();

    // Collapse whitespace
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Compute a content hash for deduplication of reflection fixes.
///
/// Uses semantic normalization to catch near-duplicate fixes with slightly
/// different wording, UUIDs, or timestamps but identical meaning.
fn compute_content_hash(
    fix_type: &str,
    description: &str,
    old_value: Option<&str>,
    new_value: Option<&str>,
) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    fix_type.hash(&mut hasher);
    normalize_for_hash(description).hash(&mut hasher);
    old_value.map(normalize_for_hash).hash(&mut hasher);
    new_value.map(normalize_for_hash).hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

/// Insert a new reflection fix into the database.
/// Deduplicates by content hash — if an identical fix already exists with status 'applied',
/// the existing fix is returned instead of creating a duplicate.
pub fn insert_fix(
    conn: &Connection,
    input: &CreateReflectionFixInput,
) -> Result<ReflectionFix, String> {
    let content_hash = compute_content_hash(
        &input.fix_type,
        &input.fix_description,
        input.old_value.as_deref(),
        input.new_value.as_deref(),
    );

    // Check for existing fix with same content hash that's already applied
    let existing_id: Option<String> = conn
        .query_row(
            "SELECT id FROM reflection_fixes WHERE content_hash = ?1 AND status = 'applied' LIMIT 1",
            params![content_hash],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Failed to check for duplicate fix: {}", e))?;

    if let Some(existing_id) = existing_id {
        warn!(
            "Skipping duplicate reflection fix (hash: {}, existing: {})",
            content_hash, existing_id
        );
        return get_fix(conn, &existing_id)?
            .ok_or_else(|| "Existing fix not found after dedup check".to_string());
    }

    let now = Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();

    conn.execute(
        r#"
        INSERT INTO reflection_fixes (
            id, source_task_run_id, reflection_task_run_id,
            source_finding_id, source_knowledge_id,
            fix_type, fix_description, file_changed,
            old_value, new_value, confidence, content_hash,
            status, applied_at, created_at, source_agent
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'applied', ?13, ?13, ?14)
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
            content_hash,
            now,
            input.source_agent,
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
    let prefixed_columns = SELECT_ALL_COLUMNS
        .split(',')
        .map(|col| {
            let col = col.trim();
            if col.is_empty() {
                String::new()
            } else {
                format!("rf.{}", col)
            }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");

    let mut sql = format!(
        r#"SELECT {} FROM reflection_fixes rf
        INNER JOIN task_runs tr ON rf.source_task_run_id = tr.id
        WHERE tr.workflow_name = ?1"#,
        prefixed_columns
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
                content_hash TEXT,
                status TEXT NOT NULL DEFAULT 'applied',
                effectiveness TEXT,
                effectiveness_evidence TEXT,
                applied_at TEXT NOT NULL,
                evaluated_at TEXT,
                created_at TEXT NOT NULL,
                source_agent TEXT,
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
            source_agent: None,
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
                source_agent: None,
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
            source_agent: None,
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

    #[test]
    fn test_dedup_skips_duplicate_fix() {
        let conn = setup_test_db();

        conn.execute(
            "INSERT INTO task_runs (id, task_name, created_at, updated_at) VALUES ('s1', 'T', datetime('now'), datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO task_runs (id, task_name, is_reflection, created_at, updated_at) VALUES ('r1', 'R', 1, datetime('now'), datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO task_runs (id, task_name, is_reflection, created_at, updated_at) VALUES ('r2', 'R2', 1, datetime('now'), datetime('now'))",
            [],
        ).unwrap();

        let input = CreateReflectionFixInput {
            source_task_run_id: "s1".to_string(),
            reflection_task_run_id: "r1".to_string(),
            source_finding_id: None,
            source_knowledge_id: None,
            fix_type: "knowledge_base_update".to_string(),
            fix_description: "Added missing context about API endpoints".to_string(),
            file_changed: None,
            old_value: None,
            new_value: Some("New API docs".to_string()),
            confidence: "high".to_string(),
            source_agent: None,
        };

        let fix1 = insert_fix(&conn, &input).unwrap();
        assert!(fix1.content_hash.is_some());

        // Insert identical fix from a different reflection run — should be deduped
        let input2 = CreateReflectionFixInput {
            source_task_run_id: "s1".to_string(),
            reflection_task_run_id: "r2".to_string(),
            source_finding_id: None,
            source_knowledge_id: None,
            fix_type: "knowledge_base_update".to_string(),
            fix_description: "Added missing context about API endpoints".to_string(),
            file_changed: None,
            old_value: None,
            new_value: Some("New API docs".to_string()),
            confidence: "high".to_string(),
            source_agent: None,
        };

        let fix2 = insert_fix(&conn, &input2).unwrap();

        // Should return the existing fix, not create a new one
        assert_eq!(fix1.id, fix2.id);

        // Only one fix should exist in the database
        let all_fixes = get_fixes_for_source_run(&conn, "s1").unwrap();
        assert_eq!(all_fixes.len(), 1);
    }

    #[test]
    fn test_dedup_allows_different_content() {
        let conn = setup_test_db();

        conn.execute(
            "INSERT INTO task_runs (id, task_name, created_at, updated_at) VALUES ('s1', 'T', datetime('now'), datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO task_runs (id, task_name, is_reflection, created_at, updated_at) VALUES ('r1', 'R', 1, datetime('now'), datetime('now'))",
            [],
        ).unwrap();

        let input1 = CreateReflectionFixInput {
            source_task_run_id: "s1".to_string(),
            reflection_task_run_id: "r1".to_string(),
            source_finding_id: None,
            source_knowledge_id: None,
            fix_type: "knowledge_base_update".to_string(),
            fix_description: "Fix A".to_string(),
            file_changed: None,
            old_value: None,
            new_value: None,
            confidence: "high".to_string(),
            source_agent: None,
        };

        let input2 = CreateReflectionFixInput {
            source_task_run_id: "s1".to_string(),
            reflection_task_run_id: "r1".to_string(),
            source_finding_id: None,
            source_knowledge_id: None,
            fix_type: "knowledge_base_update".to_string(),
            fix_description: "Fix B".to_string(),
            file_changed: None,
            old_value: None,
            new_value: None,
            confidence: "high".to_string(),
            source_agent: None,
        };

        let fix1 = insert_fix(&conn, &input1).unwrap();
        let fix2 = insert_fix(&conn, &input2).unwrap();

        // Different descriptions → different hashes → both should be created
        assert_ne!(fix1.id, fix2.id);

        let all_fixes = get_fixes_for_source_run(&conn, "s1").unwrap();
        assert_eq!(all_fixes.len(), 2);
    }

    #[test]
    fn test_normalize_strips_uuids_and_timestamps() {
        let text1 = "Confirmed effectiveness across runs d6a87e38-bfb1-4a2d-9a7a-0f4405a4bdd5 and 650cfd40-27d5-4a2d-9a7a-abc123456789 at 2026-02-15T22:44:08.411Z";
        let text2 = "Confirmed effectiveness across runs a1b2c3d4-1111-4aaa-b111-c1d1e1f11111 and 77645c1e-6d41-4a64-bd51-401d500e4099 at 2026-02-16T10:00:00Z";
        assert_eq!(normalize_for_hash(text1), normalize_for_hash(text2));
    }

    #[test]
    fn test_normalize_strips_run_numbers() {
        let text1 = "Evaluated effectiveness for Run 5 iteration 3";
        let text2 = "Evaluated effectiveness for Run 8 iteration 1";
        assert_eq!(normalize_for_hash(text1), normalize_for_hash(text2));
    }

    #[test]
    fn test_normalize_collapses_whitespace() {
        let text1 = "Fix   the   selector   issue";
        let text2 = "fix the selector issue";
        assert_eq!(normalize_for_hash(text1), normalize_for_hash(text2));
    }

    #[test]
    fn test_semantic_dedup_catches_near_duplicates() {
        // Two descriptions that differ only by UUIDs and run numbers
        let hash1 = compute_content_hash(
            "knowledge_base_update",
            "Confirmed effectiveness of JSON preamble fixes across run d6a87e38-bfb1-4a2d-9a7a-0f4405a4bdd5",
            None,
            None,
        );
        let hash2 = compute_content_hash(
            "knowledge_base_update",
            "Confirmed effectiveness of JSON preamble fixes across run 650cfd40-27d5-4a2d-9a7a-abc123456789",
            None,
            None,
        );
        assert_eq!(
            hash1, hash2,
            "Near-duplicate descriptions should produce the same hash"
        );
    }

    #[test]
    fn test_semantic_dedup_preserves_real_differences() {
        let hash1 = compute_content_hash(
            "knowledge_base_update",
            "Fix for JSON preamble parsing",
            None,
            None,
        );
        let hash2 = compute_content_hash(
            "knowledge_base_update",
            "Fix for template variable resolution",
            None,
            None,
        );
        assert_ne!(
            hash1, hash2,
            "Genuinely different descriptions should produce different hashes"
        );
    }
}
