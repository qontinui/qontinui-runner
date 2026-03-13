//! Causal Chain Engine for tracking cause→effect relationships between events.
//!
//! Provides types and functions for building, storing, and querying causal chains
//! that connect events like findings, fixes, errors, and verifications.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use tracing::{debug, info};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single causal edge: cause → effect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalEvent {
    pub id: String,
    pub cause_event_type: String,
    pub cause_event_id: String,
    pub effect_event_type: String,
    pub effect_event_id: String,
    pub relationship: String,
    pub confidence: String,
    pub source: String,
    pub task_run_id: Option<String>,
    pub workflow_name: Option<String>,
    pub description: Option<String>,
    pub created_at: String,
}

/// A traced chain of connected causal events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalChain {
    pub events: Vec<CausalEvent>,
    pub root_cause_type: String,
    pub root_cause_id: String,
    pub terminal_type: String,
    pub terminal_id: String,
    pub chain_length: usize,
}

/// Aggregate statistics for causal events in a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalSummary {
    pub total_links: u32,
    pub by_relationship: HashMap<String, u32>,
    pub by_cause_type: HashMap<String, u32>,
    pub avg_chain_length: f64,
}

// ---------------------------------------------------------------------------
// Insert
// ---------------------------------------------------------------------------

/// Insert a new causal event (directed edge) into the database.
///
/// Deduplicates by (cause_type, cause_id, effect_type, effect_id) using a UNIQUE
/// index and `INSERT OR IGNORE`. If a duplicate exists, the existing ID is returned.
pub fn insert_causal_event(
    conn: &Connection,
    cause_type: &str,
    cause_id: &str,
    effect_type: &str,
    effect_id: &str,
    relationship: &str,
    confidence: &str,
    source: &str,
    task_run_id: Option<&str>,
    workflow_name: Option<&str>,
    description: Option<&str>,
) -> Result<String, String> {
    let id = Uuid::new_v4().to_string();
    let rows_changed = conn
        .execute(
            r#"INSERT OR IGNORE INTO causal_events
           (id, cause_event_type, cause_event_id, effect_event_type, effect_event_id,
            relationship, confidence, source, task_run_id, workflow_name, description)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"#,
            params![
                id,
                cause_type,
                cause_id,
                effect_type,
                effect_id,
                relationship,
                confidence,
                source,
                task_run_id,
                workflow_name,
                description,
            ],
        )
        .map_err(|e| format!("Failed to insert causal event: {}", e))?;

    if rows_changed == 0 {
        // Duplicate — fetch the existing ID
        let existing_id: String = conn
            .query_row(
                r#"SELECT id FROM causal_events
                   WHERE cause_event_type = ?1 AND cause_event_id = ?2
                     AND effect_event_type = ?3 AND effect_event_id = ?4"#,
                params![cause_type, cause_id, effect_type, effect_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to fetch existing causal event: {}", e))?;

        debug!(
            "Causal link already exists: {} {} → {} {} (id={})",
            cause_type, cause_id, effect_type, effect_id, existing_id
        );
        return Ok(existing_id);
    }

    debug!(
        "Created causal link: {} {} → {} {} (relationship={}, id={})",
        cause_type, cause_id, effect_type, effect_id, relationship, id
    );

    Ok(id)
}

// ---------------------------------------------------------------------------
// Automated link building
// ---------------------------------------------------------------------------

/// Build automated causal links from existing data in a task run.
///
/// Scans for linkable events:
/// 1. fix_applied → finding_detected (via source_finding_id)
/// 2. fix_effective → error_occurred (via resolved_by_fix_id)
/// 3. fix_applied → error_occurred (via fix_applications + error_signature_hash)
/// 4. error_occurred → finding_detected (via signature_hash matching)
///
/// Returns the count of new links created.
pub fn build_automated_causal_links(
    conn: &Connection,
    task_run_id: &str,
    workflow_name: &str,
) -> Result<u32, String> {
    let mut count = 0u32;

    // 1. fix_applied → finding_detected (fixes linked to findings)
    {
        let mut stmt = conn
            .prepare(
                r#"SELECT id, source_finding_id FROM reflection_fixes
                   WHERE source_task_run_id = ?1 AND source_finding_id IS NOT NULL"#,
            )
            .map_err(|e| format!("Failed to prepare fix→finding query: {}", e))?;

        let rows: Vec<(String, String)> = stmt
            .query_map(params![task_run_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("Failed to query fix→finding: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        for (fix_id, finding_id) in rows {
            match insert_causal_event(
                conn,
                "fix_applied",
                &fix_id,
                "finding_detected",
                &finding_id,
                "triggered",
                "high",
                "automated",
                Some(task_run_id),
                Some(workflow_name),
                None,
            ) {
                Ok(_) => count += 1,
                Err(e) => debug!("Skipping fix→finding link: {}", e),
            }
        }
    }

    // 2. fix_effective → error_occurred (resolved errors)
    {
        let mut stmt = conn
            .prepare(
                r#"SELECT e.id as error_id, e.resolved_by_fix_id
                   FROM error_events e
                   WHERE e.task_run_id = ?1 AND e.resolved_by_fix_id IS NOT NULL"#,
            )
            .map_err(|e| format!("Failed to prepare error→fix query: {}", e))?;

        let rows: Vec<(String, String)> = stmt
            .query_map(params![task_run_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("Failed to query error→fix: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        for (error_id, fix_id) in rows {
            match insert_causal_event(
                conn,
                "fix_effective",
                &fix_id,
                "error_occurred",
                &error_id,
                "resolved",
                "high",
                "automated",
                Some(task_run_id),
                Some(workflow_name),
                None,
            ) {
                Ok(_) => count += 1,
                Err(e) => debug!("Skipping fix→error resolved link: {}", e),
            }
        }
    }

    // 3. fix_applied → error_occurred (via fix_applications table)
    {
        let mut stmt = conn
            .prepare(
                r#"SELECT fa.fix_id, fa.error_signature_hash
                   FROM fix_applications fa
                   WHERE fa.task_run_id = ?1 AND fa.error_signature_hash IS NOT NULL"#,
            )
            .map_err(|e| format!("Failed to prepare fix_application query: {}", e))?;

        let rows: Vec<(String, String)> = stmt
            .query_map(params![task_run_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("Failed to query fix_applications: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        for (fix_id, sig_hash) in rows {
            // Find matching error_events by signature_hash in this run
            let error_id: Option<String> = conn
                .query_row(
                    r#"SELECT id FROM error_events
                       WHERE task_run_id = ?1 AND signature_hash = ?2
                       LIMIT 1"#,
                    params![task_run_id, sig_hash],
                    |row| row.get(0),
                )
                .ok();

            if let Some(eid) = error_id {
                match insert_causal_event(
                    conn,
                    "fix_applied",
                    &fix_id,
                    "error_occurred",
                    &eid,
                    "triggered",
                    "high",
                    "automated",
                    Some(task_run_id),
                    Some(workflow_name),
                    None,
                ) {
                    Ok(_) => count += 1,
                    Err(e) => debug!("Skipping fix_application→error link: {}", e),
                }
            }
        }
    }

    // 4. error_occurred → finding_detected (errors that triggered findings)
    {
        let mut stmt = conn
            .prepare(
                r#"SELECT e.id as error_id, e.signature_hash
                   FROM error_events e
                   WHERE e.task_run_id = ?1 AND e.signature_hash IS NOT NULL"#,
            )
            .map_err(|e| format!("Failed to prepare error→finding query: {}", e))?;

        let rows: Vec<(String, String)> = stmt
            .query_map(params![task_run_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("Failed to query errors: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        for (error_id, sig_hash) in rows {
            // Find matching findings by signature_hash
            let finding_id: Option<String> = conn
                .query_row(
                    r#"SELECT id FROM task_run_findings
                       WHERE task_run_id = ?1 AND signature_hash = ?2
                       LIMIT 1"#,
                    params![task_run_id, sig_hash],
                    |row| row.get(0),
                )
                .ok();

            if let Some(fid) = finding_id {
                match insert_causal_event(
                    conn,
                    "error_occurred",
                    &error_id,
                    "finding_detected",
                    &fid,
                    "triggered",
                    "high",
                    "automated",
                    Some(task_run_id),
                    Some(workflow_name),
                    None,
                ) {
                    Ok(_) => count += 1,
                    Err(e) => debug!("Skipping error→finding link: {}", e),
                }
            }
        }
    }

    info!(
        "Built {} automated causal links for task_run={}, workflow={}",
        count, task_run_id, workflow_name
    );
    Ok(count)
}

// ---------------------------------------------------------------------------
// Chain tracing
// ---------------------------------------------------------------------------

/// Trace a causal chain forward from a cause event, following effect links.
///
/// BFS traversal with cycle detection and max depth.
pub fn trace_causal_chain_forward(
    conn: &Connection,
    event_type: &str,
    event_id: &str,
    max_depth: u32,
) -> Result<CausalChain, String> {
    let mut events = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    queue.push_back((event_type.to_string(), event_id.to_string(), 0u32));
    visited.insert(format!("{}:{}", event_type, event_id));

    while let Some((etype, eid, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }

        let mut stmt = conn
            .prepare(
                r#"SELECT id, cause_event_type, cause_event_id,
                          effect_event_type, effect_event_id,
                          relationship, confidence, source,
                          task_run_id, workflow_name, description, created_at
                   FROM causal_events
                   WHERE cause_event_type = ?1 AND cause_event_id = ?2
                   ORDER BY created_at ASC"#,
            )
            .map_err(|e| format!("Failed to prepare forward trace: {}", e))?;

        let rows: Vec<CausalEvent> = stmt
            .query_map(params![etype, eid], |row| {
                Ok(CausalEvent {
                    id: row.get(0)?,
                    cause_event_type: row.get(1)?,
                    cause_event_id: row.get(2)?,
                    effect_event_type: row.get(3)?,
                    effect_event_id: row.get(4)?,
                    relationship: row.get(5)?,
                    confidence: row.get(6)?,
                    source: row.get(7)?,
                    task_run_id: row.get(8)?,
                    workflow_name: row.get(9)?,
                    description: row.get(10)?,
                    created_at: row.get(11)?,
                })
            })
            .map_err(|e| format!("Failed to query forward trace: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        for event in rows {
            let key = format!("{}:{}", event.effect_event_type, event.effect_event_id);
            if !visited.contains(&key) {
                visited.insert(key);
                queue.push_back((
                    event.effect_event_type.clone(),
                    event.effect_event_id.clone(),
                    depth + 1,
                ));
            }
            events.push(event);
        }
    }

    let (terminal_type, terminal_id) = events
        .last()
        .map(|e| (e.effect_event_type.clone(), e.effect_event_id.clone()))
        .unwrap_or_else(|| (event_type.to_string(), event_id.to_string()));

    let chain_length = events.len();

    Ok(CausalChain {
        events,
        root_cause_type: event_type.to_string(),
        root_cause_id: event_id.to_string(),
        terminal_type,
        terminal_id,
        chain_length,
    })
}

/// Trace a causal chain backward from an effect event to find root causes.
///
/// BFS traversal following cause links backward.
pub fn trace_causal_chain_backward(
    conn: &Connection,
    event_type: &str,
    event_id: &str,
    max_depth: u32,
) -> Result<CausalChain, String> {
    let mut events = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    queue.push_back((event_type.to_string(), event_id.to_string(), 0u32));
    visited.insert(format!("{}:{}", event_type, event_id));

    while let Some((etype, eid, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }

        let mut stmt = conn
            .prepare(
                r#"SELECT id, cause_event_type, cause_event_id,
                          effect_event_type, effect_event_id,
                          relationship, confidence, source,
                          task_run_id, workflow_name, description, created_at
                   FROM causal_events
                   WHERE effect_event_type = ?1 AND effect_event_id = ?2
                   ORDER BY created_at ASC"#,
            )
            .map_err(|e| format!("Failed to prepare backward trace: {}", e))?;

        let rows: Vec<CausalEvent> = stmt
            .query_map(params![etype, eid], |row| {
                Ok(CausalEvent {
                    id: row.get(0)?,
                    cause_event_type: row.get(1)?,
                    cause_event_id: row.get(2)?,
                    effect_event_type: row.get(3)?,
                    effect_event_id: row.get(4)?,
                    relationship: row.get(5)?,
                    confidence: row.get(6)?,
                    source: row.get(7)?,
                    task_run_id: row.get(8)?,
                    workflow_name: row.get(9)?,
                    description: row.get(10)?,
                    created_at: row.get(11)?,
                })
            })
            .map_err(|e| format!("Failed to query backward trace: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        for event in rows {
            let key = format!("{}:{}", event.cause_event_type, event.cause_event_id);
            if !visited.contains(&key) {
                visited.insert(key);
                queue.push_back((
                    event.cause_event_type.clone(),
                    event.cause_event_id.clone(),
                    depth + 1,
                ));
            }
            events.push(event);
        }
    }

    let (root_type, root_id) = events
        .last()
        .map(|e| (e.cause_event_type.clone(), e.cause_event_id.clone()))
        .unwrap_or_else(|| (event_type.to_string(), event_id.to_string()));

    let chain_length = events.len();

    Ok(CausalChain {
        events,
        root_cause_type: root_type,
        root_cause_id: root_id,
        terminal_type: event_type.to_string(),
        terminal_id: event_id.to_string(),
        chain_length,
    })
}

// ---------------------------------------------------------------------------
// Query helpers
// ---------------------------------------------------------------------------

/// Get all causal events for a workflow, ordered by created_at desc.
pub fn get_causal_events_for_workflow(
    conn: &Connection,
    workflow_name: &str,
    limit: u32,
) -> Result<Vec<CausalEvent>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT id, cause_event_type, cause_event_id,
                      effect_event_type, effect_event_id,
                      relationship, confidence, source,
                      task_run_id, workflow_name, description, created_at
               FROM causal_events
               WHERE workflow_name = ?1
               ORDER BY created_at DESC
               LIMIT ?2"#,
        )
        .map_err(|e| format!("Failed to prepare causal events query: {}", e))?;

    let events: Vec<CausalEvent> = stmt
        .query_map(params![workflow_name, limit], |row| {
            Ok(CausalEvent {
                id: row.get(0)?,
                cause_event_type: row.get(1)?,
                cause_event_id: row.get(2)?,
                effect_event_type: row.get(3)?,
                effect_event_id: row.get(4)?,
                relationship: row.get(5)?,
                confidence: row.get(6)?,
                source: row.get(7)?,
                task_run_id: row.get(8)?,
                workflow_name: row.get(9)?,
                description: row.get(10)?,
                created_at: row.get(11)?,
            })
        })
        .map_err(|e| format!("Failed to query causal events: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(events)
}

/// Get aggregate causal statistics for a workflow.
pub fn get_causal_summary(conn: &Connection, workflow_name: &str) -> Result<CausalSummary, String> {
    // Total count
    let total_links: u32 = conn
        .query_row(
            "SELECT COUNT(*) FROM causal_events WHERE workflow_name = ?1",
            params![workflow_name],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to count causal events: {}", e))?;

    // By relationship
    let mut by_relationship = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                r#"SELECT relationship, COUNT(*) FROM causal_events
                   WHERE workflow_name = ?1 GROUP BY relationship"#,
            )
            .map_err(|e| format!("Failed to prepare relationship count: {}", e))?;

        let rows = stmt
            .query_map(params![workflow_name], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
            })
            .map_err(|e| format!("Failed to query relationship count: {}", e))?;

        for row in rows.flatten() {
            by_relationship.insert(row.0, row.1);
        }
    }

    // By cause type
    let mut by_cause_type = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                r#"SELECT cause_event_type, COUNT(*) FROM causal_events
                   WHERE workflow_name = ?1 GROUP BY cause_event_type"#,
            )
            .map_err(|e| format!("Failed to prepare cause type count: {}", e))?;

        let rows = stmt
            .query_map(params![workflow_name], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
            })
            .map_err(|e| format!("Failed to query cause type count: {}", e))?;

        for row in rows.flatten() {
            by_cause_type.insert(row.0, row.1);
        }
    }

    // Average chain length: approximate by counting distinct chains
    // A chain starts from events that are causes but not effects
    let avg_chain_length = if total_links > 0 {
        let root_count: u32 = conn
            .query_row(
                r#"SELECT COUNT(DISTINCT cause_event_type || ':' || cause_event_id)
                   FROM causal_events
                   WHERE workflow_name = ?1
                     AND (cause_event_type || ':' || cause_event_id) NOT IN (
                         SELECT effect_event_type || ':' || effect_event_id FROM causal_events WHERE workflow_name = ?1
                     )"#,
                params![workflow_name],
                |row| row.get(0),
            )
            .unwrap_or(1);

        if root_count > 0 {
            total_links as f64 / root_count as f64
        } else {
            total_links as f64
        }
    } else {
        0.0
    };

    Ok(CausalSummary {
        total_links,
        by_relationship,
        by_cause_type,
        avg_chain_length,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE causal_events (
                id TEXT PRIMARY KEY,
                cause_event_type TEXT NOT NULL,
                cause_event_id TEXT NOT NULL,
                effect_event_type TEXT NOT NULL,
                effect_event_id TEXT NOT NULL,
                relationship TEXT NOT NULL,
                confidence TEXT NOT NULL DEFAULT 'high',
                source TEXT NOT NULL DEFAULT 'automated',
                task_run_id TEXT,
                workflow_name TEXT,
                description TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX idx_causal_cause ON causal_events(cause_event_type, cause_event_id);
            CREATE INDEX idx_causal_effect ON causal_events(effect_event_type, effect_event_id);
            CREATE UNIQUE INDEX idx_causal_dedup ON causal_events(cause_event_type, cause_event_id, effect_event_type, effect_event_id);
            "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_insert_and_dedup() {
        let conn = setup_db();
        let id1 = insert_causal_event(
            &conn,
            "error_occurred",
            "err1",
            "finding_detected",
            "find1",
            "triggered",
            "high",
            "automated",
            Some("run1"),
            Some("wf1"),
            None,
        )
        .unwrap();

        // Duplicate should return same ID
        let id2 = insert_causal_event(
            &conn,
            "error_occurred",
            "err1",
            "finding_detected",
            "find1",
            "triggered",
            "high",
            "automated",
            Some("run1"),
            Some("wf1"),
            None,
        )
        .unwrap();

        assert_eq!(id1, id2);
    }

    #[test]
    fn test_forward_trace() {
        let conn = setup_db();

        // Build chain: error → finding → fix
        insert_causal_event(
            &conn,
            "error_occurred",
            "err1",
            "finding_detected",
            "find1",
            "triggered",
            "high",
            "automated",
            None,
            Some("wf1"),
            None,
        )
        .unwrap();

        insert_causal_event(
            &conn,
            "finding_detected",
            "find1",
            "fix_applied",
            "fix1",
            "triggered",
            "high",
            "automated",
            None,
            Some("wf1"),
            None,
        )
        .unwrap();

        let chain = trace_causal_chain_forward(&conn, "error_occurred", "err1", 10).unwrap();
        assert_eq!(chain.chain_length, 2);
        assert_eq!(chain.root_cause_type, "error_occurred");
        assert_eq!(chain.terminal_type, "fix_applied");
    }

    #[test]
    fn test_backward_trace() {
        let conn = setup_db();

        insert_causal_event(
            &conn,
            "error_occurred",
            "err1",
            "finding_detected",
            "find1",
            "triggered",
            "high",
            "automated",
            None,
            Some("wf1"),
            None,
        )
        .unwrap();

        insert_causal_event(
            &conn,
            "finding_detected",
            "find1",
            "fix_applied",
            "fix1",
            "triggered",
            "high",
            "automated",
            None,
            Some("wf1"),
            None,
        )
        .unwrap();

        let chain = trace_causal_chain_backward(&conn, "fix_applied", "fix1", 10).unwrap();
        assert_eq!(chain.chain_length, 2);
        assert_eq!(chain.root_cause_type, "error_occurred");
        assert_eq!(chain.terminal_type, "fix_applied");
    }

    #[test]
    fn test_causal_summary() {
        let conn = setup_db();

        insert_causal_event(
            &conn,
            "error_occurred",
            "err1",
            "finding_detected",
            "find1",
            "triggered",
            "high",
            "automated",
            None,
            Some("wf1"),
            None,
        )
        .unwrap();

        insert_causal_event(
            &conn,
            "fix_applied",
            "fix1",
            "error_occurred",
            "err1",
            "resolved",
            "high",
            "automated",
            None,
            Some("wf1"),
            None,
        )
        .unwrap();

        let summary = get_causal_summary(&conn, "wf1").unwrap();
        assert_eq!(summary.total_links, 2);
        assert_eq!(summary.by_relationship.get("triggered"), Some(&1));
        assert_eq!(summary.by_relationship.get("resolved"), Some(&1));
    }

    #[test]
    fn test_get_events_for_workflow() {
        let conn = setup_db();

        insert_causal_event(
            &conn,
            "error_occurred",
            "err1",
            "finding_detected",
            "find1",
            "triggered",
            "high",
            "automated",
            None,
            Some("wf1"),
            Some("Error triggered finding"),
        )
        .unwrap();

        let events = get_causal_events_for_workflow(&conn, "wf1", 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].description.as_deref(),
            Some("Error triggered finding")
        );
    }
}
