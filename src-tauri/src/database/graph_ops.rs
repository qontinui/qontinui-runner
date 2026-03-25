//! CRUD operations for workflow generation graph tables.
//!
//! Covers: workflow_versions, step_finding_links, step_provenance,
//! generation_pipeline_events, and rule_influence_log.

use chrono::Utc;
use rusqlite::{params, Connection};
use uuid::Uuid;

// ============================================================================
// Struct definitions
// ============================================================================

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkflowVersion {
    pub id: String,
    pub workflow_id: String,
    pub version_number: i32,
    pub parent_version_id: Option<String>,
    pub generation_task_run_id: Option<String>,
    pub workflow_json: String,
    pub diff_summary: Option<String>,
    pub diff_json: Option<String>,
    pub trigger: String,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StepFindingLink {
    pub id: String,
    pub task_run_id: String,
    pub step_name: String,
    pub step_index: i32,
    pub finding_id: String,
    pub link_type: String,
    pub confidence: f64,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StepProvenance {
    pub id: String,
    pub workflow_id: String,
    pub workflow_version_id: Option<String>,
    pub step_name: String,
    pub step_index: i32,
    pub phase: String,
    pub generating_agent: String,
    pub generation_iteration: Option<i32>,
    pub original_step_json: Option<String>,
    pub final_step_json: Option<String>,
    pub ui_bridge_event_ids: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PipelineEvent {
    pub id: String,
    pub task_run_id: String,
    pub workflow_id: Option<String>,
    pub event_type: String,
    pub phase: Option<String>,
    pub iteration: Option<i32>,
    pub payload: Option<String>,
    pub duration_ms: Option<i64>,
    pub token_count: Option<i64>,
    pub validation_errors_before: Option<i32>,
    pub validation_errors_after: Option<i32>,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RuleInfluence {
    pub id: String,
    pub rule_id: String,
    pub task_run_id: String,
    pub workflow_id: Option<String>,
    pub influence_type: String,
    pub evidence: Option<String>,
    pub phase: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IneffectiveRule {
    pub rule_id: String,
    pub no_effect_count: i64,
    pub prevented_error_count: i64,
    pub total_loaded_count: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PhaseStats {
    pub phase: String,
    pub event_count: i64,
    pub avg_duration_ms: Option<f64>,
    pub total_token_count: i64,
    pub avg_errors_before: Option<f64>,
    pub avg_errors_after: Option<f64>,
}

// ============================================================================
// workflow_versions
// ============================================================================

/// Insert a new workflow version, returning the generated ID.
pub fn insert_workflow_version(
    conn: &Connection,
    workflow_id: &str,
    version_number: i32,
    parent_version_id: Option<&str>,
    generation_task_run_id: Option<&str>,
    workflow_json: &str,
    diff_summary: Option<&str>,
    diff_json: Option<&str>,
    trigger: &str,
) -> Result<String, String> {
    let id = format!("wv-{}", Uuid::new_v4());
    let now = Utc::now().to_rfc3339();

    conn.execute(
        r#"INSERT INTO workflow_versions
           (id, workflow_id, version_number, parent_version_id,
            generation_task_run_id, workflow_json, diff_summary,
            diff_json, trigger, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
        params![
            id,
            workflow_id,
            version_number,
            parent_version_id,
            generation_task_run_id,
            workflow_json,
            diff_summary,
            diff_json,
            trigger,
            now,
        ],
    )
    .map_err(|e| format!("Failed to insert workflow version: {}", e))?;

    Ok(id)
}

/// Get all versions for a workflow, ordered by version_number DESC.
pub fn get_workflow_versions(
    conn: &Connection,
    workflow_id: &str,
) -> Result<Vec<WorkflowVersion>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT id, workflow_id, version_number, parent_version_id,
                      generation_task_run_id, workflow_json, diff_summary,
                      diff_json, trigger, created_at
               FROM workflow_versions
               WHERE workflow_id = ?1
               ORDER BY version_number DESC"#,
        )
        .map_err(|e| format!("Failed to prepare workflow versions query: {}", e))?;

    let results = stmt
        .query_map(params![workflow_id], |row| {
            Ok(WorkflowVersion {
                id: row.get(0)?,
                workflow_id: row.get(1)?,
                version_number: row.get(2)?,
                parent_version_id: row.get(3)?,
                generation_task_run_id: row.get(4)?,
                workflow_json: row.get(5)?,
                diff_summary: row.get(6)?,
                diff_json: row.get(7)?,
                trigger: row.get(8)?,
                created_at: row.get(9)?,
            })
        })
        .map_err(|e| format!("Failed to query workflow versions: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Get the latest workflow version by version_number.
pub fn get_latest_workflow_version(
    conn: &Connection,
    workflow_id: &str,
) -> Result<Option<WorkflowVersion>, String> {
    let result = conn.query_row(
        r#"SELECT id, workflow_id, version_number, parent_version_id,
                  generation_task_run_id, workflow_json, diff_summary,
                  diff_json, trigger, created_at
           FROM workflow_versions
           WHERE workflow_id = ?1
           ORDER BY version_number DESC
           LIMIT 1"#,
        params![workflow_id],
        |row| {
            Ok(WorkflowVersion {
                id: row.get(0)?,
                workflow_id: row.get(1)?,
                version_number: row.get(2)?,
                parent_version_id: row.get(3)?,
                generation_task_run_id: row.get(4)?,
                workflow_json: row.get(5)?,
                diff_summary: row.get(6)?,
                diff_json: row.get(7)?,
                trigger: row.get(8)?,
                created_at: row.get(9)?,
            })
        },
    );

    match result {
        Ok(version) => Ok(Some(version)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!("Failed to get latest workflow version: {}", e)),
    }
}

// ============================================================================
// step_finding_links
// ============================================================================

/// Insert a new step-finding link, returning the generated ID.
pub fn insert_step_finding_link(
    conn: &Connection,
    task_run_id: &str,
    step_name: &str,
    step_index: i32,
    finding_id: &str,
    link_type: &str,
    confidence: f64,
) -> Result<String, String> {
    let id = format!("sfl-{}", Uuid::new_v4());
    let now = Utc::now().to_rfc3339();

    conn.execute(
        r#"INSERT INTO step_finding_links
           (id, task_run_id, step_name, step_index, finding_id,
            link_type, confidence, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"#,
        params![
            id,
            task_run_id,
            step_name,
            step_index,
            finding_id,
            link_type,
            confidence,
            now,
        ],
    )
    .map_err(|e| format!("Failed to insert step finding link: {}", e))?;

    Ok(id)
}

/// Get all finding links for a specific step in a task run.
pub fn get_findings_for_step(
    conn: &Connection,
    task_run_id: &str,
    step_name: &str,
) -> Result<Vec<StepFindingLink>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT id, task_run_id, step_name, step_index, finding_id,
                      link_type, confidence, created_at
               FROM step_finding_links
               WHERE task_run_id = ?1 AND step_name = ?2
               ORDER BY created_at ASC"#,
        )
        .map_err(|e| format!("Failed to prepare findings for step query: {}", e))?;

    let results = stmt
        .query_map(params![task_run_id, step_name], |row| {
            Ok(StepFindingLink {
                id: row.get(0)?,
                task_run_id: row.get(1)?,
                step_name: row.get(2)?,
                step_index: row.get(3)?,
                finding_id: row.get(4)?,
                link_type: row.get(5)?,
                confidence: row.get(6)?,
                created_at: row.get(7)?,
            })
        })
        .map_err(|e| format!("Failed to query findings for step: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Get all step links for a specific finding.
pub fn get_steps_for_finding(
    conn: &Connection,
    finding_id: &str,
) -> Result<Vec<StepFindingLink>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT id, task_run_id, step_name, step_index, finding_id,
                      link_type, confidence, created_at
               FROM step_finding_links
               WHERE finding_id = ?1
               ORDER BY created_at ASC"#,
        )
        .map_err(|e| format!("Failed to prepare steps for finding query: {}", e))?;

    let results = stmt
        .query_map(params![finding_id], |row| {
            Ok(StepFindingLink {
                id: row.get(0)?,
                task_run_id: row.get(1)?,
                step_name: row.get(2)?,
                step_index: row.get(3)?,
                finding_id: row.get(4)?,
                link_type: row.get(5)?,
                confidence: row.get(6)?,
                created_at: row.get(7)?,
            })
        })
        .map_err(|e| format!("Failed to query steps for finding: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

// ============================================================================
// step_provenance
// ============================================================================

/// Insert a new step provenance record, returning the generated ID.
pub fn insert_step_provenance(
    conn: &Connection,
    workflow_id: &str,
    workflow_version_id: Option<&str>,
    step_name: &str,
    step_index: i32,
    phase: &str,
    generating_agent: &str,
    generation_iteration: Option<i32>,
    original_step_json: Option<&str>,
    final_step_json: Option<&str>,
) -> Result<String, String> {
    insert_step_provenance_with_events(
        conn,
        workflow_id,
        workflow_version_id,
        step_name,
        step_index,
        phase,
        generating_agent,
        generation_iteration,
        original_step_json,
        final_step_json,
        None,
    )
}

/// Insert step provenance with optional UI Bridge event IDs.
///
/// `ui_bridge_event_ids` is a JSON array of event IDs from `ui_bridge_events`
/// that occurred during this step's execution, linking UI interactions to provenance.
pub fn insert_step_provenance_with_events(
    conn: &Connection,
    workflow_id: &str,
    workflow_version_id: Option<&str>,
    step_name: &str,
    step_index: i32,
    phase: &str,
    generating_agent: &str,
    generation_iteration: Option<i32>,
    original_step_json: Option<&str>,
    final_step_json: Option<&str>,
    ui_bridge_event_ids: Option<&str>,
) -> Result<String, String> {
    let id = format!("sp-{}", Uuid::new_v4());
    let now = Utc::now().to_rfc3339();

    conn.execute(
        r#"INSERT INTO step_provenance
           (id, workflow_id, workflow_version_id, step_name, step_index,
            phase, generating_agent, generation_iteration,
            original_step_json, final_step_json, ui_bridge_event_ids, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"#,
        params![
            id,
            workflow_id,
            workflow_version_id,
            step_name,
            step_index,
            phase,
            generating_agent,
            generation_iteration,
            original_step_json,
            final_step_json,
            ui_bridge_event_ids,
            now,
        ],
    )
    .map_err(|e| format!("Failed to insert step provenance: {}", e))?;

    Ok(id)
}

/// Get all provenance records for a workflow, ordered by step_index.
pub fn get_provenance_for_workflow(
    conn: &Connection,
    workflow_id: &str,
) -> Result<Vec<StepProvenance>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT id, workflow_id, workflow_version_id, step_name, step_index,
                      phase, generating_agent, generation_iteration,
                      original_step_json, final_step_json, ui_bridge_event_ids, created_at
               FROM step_provenance
               WHERE workflow_id = ?1
               ORDER BY step_index ASC, created_at ASC"#,
        )
        .map_err(|e| format!("Failed to prepare provenance for workflow query: {}", e))?;

    let results = stmt
        .query_map(params![workflow_id], |row| {
            Ok(StepProvenance {
                id: row.get(0)?,
                workflow_id: row.get(1)?,
                workflow_version_id: row.get(2)?,
                step_name: row.get(3)?,
                step_index: row.get(4)?,
                phase: row.get(5)?,
                generating_agent: row.get(6)?,
                generation_iteration: row.get(7)?,
                original_step_json: row.get(8)?,
                final_step_json: row.get(9)?,
                ui_bridge_event_ids: row.get(10)?,
                created_at: row.get(11)?,
            })
        })
        .map_err(|e| format!("Failed to query provenance for workflow: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Get provenance records by generating agent, with a limit.
pub fn get_provenance_by_agent(
    conn: &Connection,
    generating_agent: &str,
    limit: u32,
) -> Result<Vec<StepProvenance>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT id, workflow_id, workflow_version_id, step_name, step_index,
                      phase, generating_agent, generation_iteration,
                      original_step_json, final_step_json, ui_bridge_event_ids, created_at
               FROM step_provenance
               WHERE generating_agent = ?1
               ORDER BY created_at DESC
               LIMIT ?2"#,
        )
        .map_err(|e| format!("Failed to prepare provenance by agent query: {}", e))?;

    let results = stmt
        .query_map(params![generating_agent, limit], |row| {
            Ok(StepProvenance {
                id: row.get(0)?,
                workflow_id: row.get(1)?,
                workflow_version_id: row.get(2)?,
                step_name: row.get(3)?,
                step_index: row.get(4)?,
                phase: row.get(5)?,
                generating_agent: row.get(6)?,
                generation_iteration: row.get(7)?,
                original_step_json: row.get(8)?,
                final_step_json: row.get(9)?,
                ui_bridge_event_ids: row.get(10)?,
                created_at: row.get(11)?,
            })
        })
        .map_err(|e| format!("Failed to query provenance by agent: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

// ============================================================================
// generation_pipeline_events
// ============================================================================

/// Insert a new pipeline event, returning the generated ID.
pub fn insert_pipeline_event(
    conn: &Connection,
    task_run_id: &str,
    workflow_id: Option<&str>,
    event_type: &str,
    phase: Option<&str>,
    iteration: Option<i32>,
    payload: Option<&str>,
    duration_ms: Option<i64>,
    token_count: Option<i64>,
    errors_before: Option<i32>,
    errors_after: Option<i32>,
) -> Result<String, String> {
    let id = format!("gpe-{}", Uuid::new_v4());
    let now = Utc::now().to_rfc3339();

    conn.execute(
        r#"INSERT INTO generation_pipeline_events
           (id, task_run_id, workflow_id, event_type, phase, iteration,
            payload, duration_ms, token_count,
            validation_errors_before, validation_errors_after, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"#,
        params![
            id,
            task_run_id,
            workflow_id,
            event_type,
            phase,
            iteration,
            payload,
            duration_ms,
            token_count,
            errors_before,
            errors_after,
            now,
        ],
    )
    .map_err(|e| format!("Failed to insert pipeline event: {}", e))?;

    Ok(id)
}

/// Get all pipeline events for a task run, ordered by created_at.
pub fn get_pipeline_events(
    conn: &Connection,
    task_run_id: &str,
) -> Result<Vec<PipelineEvent>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT id, task_run_id, workflow_id, event_type, phase, iteration,
                      payload, duration_ms, token_count,
                      validation_errors_before, validation_errors_after, created_at
               FROM generation_pipeline_events
               WHERE task_run_id = ?1
               ORDER BY created_at ASC"#,
        )
        .map_err(|e| format!("Failed to prepare pipeline events query: {}", e))?;

    let results = stmt
        .query_map(params![task_run_id], |row| {
            Ok(PipelineEvent {
                id: row.get(0)?,
                task_run_id: row.get(1)?,
                workflow_id: row.get(2)?,
                event_type: row.get(3)?,
                phase: row.get(4)?,
                iteration: row.get(5)?,
                payload: row.get(6)?,
                duration_ms: row.get(7)?,
                token_count: row.get(8)?,
                validation_errors_before: row.get(9)?,
                validation_errors_after: row.get(10)?,
                created_at: row.get(11)?,
            })
        })
        .map_err(|e| format!("Failed to query pipeline events: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Get aggregate phase statistics for a workflow's pipeline events.
pub fn get_phase_stats(
    conn: &Connection,
    workflow_id: &str,
) -> Result<Vec<PhaseStats>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT
                   phase,
                   COUNT(*) as event_count,
                   AVG(duration_ms) as avg_duration_ms,
                   COALESCE(SUM(token_count), 0) as total_token_count,
                   AVG(validation_errors_before) as avg_errors_before,
                   AVG(validation_errors_after) as avg_errors_after
               FROM generation_pipeline_events
               WHERE workflow_id = ?1 AND phase IS NOT NULL
               GROUP BY phase
               ORDER BY phase"#,
        )
        .map_err(|e| format!("Failed to prepare phase stats query: {}", e))?;

    let results = stmt
        .query_map(params![workflow_id], |row| {
            Ok(PhaseStats {
                phase: row.get(0)?,
                event_count: row.get(1)?,
                avg_duration_ms: row.get(2)?,
                total_token_count: row.get(3)?,
                avg_errors_before: row.get(4)?,
                avg_errors_after: row.get(5)?,
            })
        })
        .map_err(|e| format!("Failed to query phase stats: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

// ============================================================================
// rule_influence_log
// ============================================================================

/// Insert a new rule influence record, returning the generated ID.
pub fn insert_rule_influence(
    conn: &Connection,
    rule_id: &str,
    task_run_id: &str,
    workflow_id: Option<&str>,
    influence_type: &str,
    evidence: Option<&str>,
    phase: Option<&str>,
) -> Result<String, String> {
    let id = format!("ri-{}", Uuid::new_v4());
    let now = Utc::now().to_rfc3339();

    conn.execute(
        r#"INSERT INTO rule_influence_log
           (id, rule_id, task_run_id, workflow_id, influence_type,
            evidence, phase, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"#,
        params![
            id,
            rule_id,
            task_run_id,
            workflow_id,
            influence_type,
            evidence,
            phase,
            now,
        ],
    )
    .map_err(|e| format!("Failed to insert rule influence: {}", e))?;

    Ok(id)
}

/// Get all influence records for a specific rule.
pub fn get_rule_influences(
    conn: &Connection,
    rule_id: &str,
) -> Result<Vec<RuleInfluence>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT id, rule_id, task_run_id, workflow_id, influence_type,
                      evidence, phase, created_at
               FROM rule_influence_log
               WHERE rule_id = ?1
               ORDER BY created_at DESC"#,
        )
        .map_err(|e| format!("Failed to prepare rule influences query: {}", e))?;

    let results = stmt
        .query_map(params![rule_id], |row| {
            Ok(RuleInfluence {
                id: row.get(0)?,
                rule_id: row.get(1)?,
                task_run_id: row.get(2)?,
                workflow_id: row.get(3)?,
                influence_type: row.get(4)?,
                evidence: row.get(5)?,
                phase: row.get(6)?,
                created_at: row.get(7)?,
            })
        })
        .map_err(|e| format!("Failed to query rule influences: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Get rules that appear ineffective: at least `min_no_effect_count` 'no_effect'
/// entries and zero 'prevented_error' entries.
pub fn get_ineffective_rules(
    conn: &Connection,
    min_no_effect_count: i64,
) -> Result<Vec<IneffectiveRule>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT
                   rule_id,
                   SUM(CASE WHEN influence_type = 'no_effect' THEN 1 ELSE 0 END) as no_effect_count,
                   SUM(CASE WHEN influence_type = 'prevented_error' THEN 1 ELSE 0 END) as prevented_error_count,
                   COUNT(*) as total_loaded_count
               FROM rule_influence_log
               GROUP BY rule_id
               HAVING no_effect_count >= ?1 AND prevented_error_count = 0
               ORDER BY no_effect_count DESC"#,
        )
        .map_err(|e| format!("Failed to prepare ineffective rules query: {}", e))?;

    let results = stmt
        .query_map(params![min_no_effect_count], |row| {
            Ok(IneffectiveRule {
                rule_id: row.get(0)?,
                no_effect_count: row.get(1)?,
                prevented_error_count: row.get(2)?,
                total_loaded_count: row.get(3)?,
            })
        })
        .map_err(|e| format!("Failed to query ineffective rules: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Link findings to steps by matching creation timestamps within the same task run.
/// Called after step execution completes. Finds all findings that were detected
/// during the step's execution window and creates step_finding_links for them.
pub fn link_findings_to_steps_by_timestamp(
    conn: &Connection,
    task_run_id: &str,
    step_name: &str,
    step_index: i32,
    step_started_at: &str,
    step_completed_at: &str,
) -> Result<u32, String> {
    let findings: Vec<String> = conn
        .prepare(
            r#"SELECT id FROM task_run_findings
               WHERE task_run_id = ?1
               AND detected_at >= ?2
               AND detected_at <= ?3
               AND id NOT IN (SELECT finding_id FROM step_finding_links WHERE task_run_id = ?1)"#,
        )
        .and_then(|mut stmt| {
            stmt.query_map(params![task_run_id, step_started_at, step_completed_at], |row| {
                row.get::<_, String>(0)
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .map_err(|e| format!("Failed to find unlinked findings: {}", e))?;

    let mut count = 0u32;
    for finding_id in &findings {
        if insert_step_finding_link(
            conn,
            task_run_id,
            step_name,
            step_index,
            finding_id,
            "detected_during",
            1.0,
        )
        .is_ok()
        {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Create an in-memory SQLite database with all tables needed by graph_ops.
    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();

        conn.execute_batch(
            r#"
            -- Dependency tables (minimal stubs for FK constraints)
            CREATE TABLE IF NOT EXISTS task_runs (
                id TEXT PRIMARY KEY,
                task_name TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'running',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS unified_workflows (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS task_run_findings (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS reflection_fixes (
                id TEXT PRIMARY KEY,
                source_task_run_id TEXT NOT NULL DEFAULT '',
                reflection_task_run_id TEXT NOT NULL DEFAULT '',
                fix_description TEXT NOT NULL DEFAULT '',
                applied_at TEXT NOT NULL DEFAULT (datetime('now')),
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS generation_rules (
                id TEXT PRIMARY KEY,
                rule_text TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- The 5 graph tables
            CREATE TABLE IF NOT EXISTS workflow_versions (
                id TEXT PRIMARY KEY,
                workflow_id TEXT NOT NULL,
                version_number INTEGER NOT NULL,
                parent_version_id TEXT,
                generation_task_run_id TEXT,
                workflow_json TEXT NOT NULL,
                diff_summary TEXT,
                diff_json TEXT,
                trigger TEXT NOT NULL DEFAULT 'manual',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(workflow_id, version_number),
                FOREIGN KEY (workflow_id) REFERENCES unified_workflows(id) ON DELETE CASCADE,
                FOREIGN KEY (parent_version_id) REFERENCES workflow_versions(id) ON DELETE SET NULL,
                FOREIGN KEY (generation_task_run_id) REFERENCES task_runs(id) ON DELETE SET NULL
            );

            CREATE TABLE IF NOT EXISTS step_finding_links (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                step_name TEXT NOT NULL,
                step_index INTEGER NOT NULL,
                finding_id TEXT NOT NULL,
                link_type TEXT NOT NULL DEFAULT 'detected_during',
                confidence REAL NOT NULL DEFAULT 1.0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                FOREIGN KEY (finding_id) REFERENCES task_run_findings(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS step_provenance (
                id TEXT PRIMARY KEY,
                workflow_id TEXT NOT NULL,
                workflow_version_id TEXT,
                step_name TEXT NOT NULL,
                step_index INTEGER NOT NULL,
                phase TEXT NOT NULL,
                generating_agent TEXT NOT NULL,
                generation_iteration INTEGER,
                original_step_json TEXT,
                final_step_json TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (workflow_id) REFERENCES unified_workflows(id) ON DELETE CASCADE,
                FOREIGN KEY (workflow_version_id) REFERENCES workflow_versions(id) ON DELETE SET NULL
            );

            CREATE TABLE IF NOT EXISTS generation_pipeline_events (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                workflow_id TEXT,
                event_type TEXT NOT NULL,
                phase TEXT,
                iteration INTEGER,
                payload TEXT,
                duration_ms INTEGER,
                token_count INTEGER,
                validation_errors_before INTEGER,
                validation_errors_after INTEGER,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                FOREIGN KEY (workflow_id) REFERENCES unified_workflows(id) ON DELETE SET NULL
            );

            CREATE TABLE IF NOT EXISTS rule_influence_log (
                id TEXT PRIMARY KEY,
                rule_id TEXT NOT NULL,
                task_run_id TEXT NOT NULL,
                workflow_id TEXT,
                influence_type TEXT NOT NULL DEFAULT 'loaded',
                evidence TEXT,
                phase TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (rule_id) REFERENCES generation_rules(id) ON DELETE CASCADE,
                FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                FOREIGN KEY (workflow_id) REFERENCES unified_workflows(id) ON DELETE SET NULL
            );
            "#,
        )
        .expect("Failed to create test schema");

        // Seed dependency rows referenced by FK constraints
        conn.execute_batch(
            r#"
            INSERT INTO task_runs (id) VALUES ('tr-1'), ('tr-2'), ('tr-3');
            INSERT INTO unified_workflows (id, name) VALUES ('wf-1', 'test-workflow'), ('wf-2', 'other-workflow');
            INSERT INTO task_run_findings (id, task_run_id) VALUES ('f-1', 'tr-1'), ('f-2', 'tr-1'), ('f-3', 'tr-2');
            INSERT INTO generation_rules (id) VALUES ('rule-1'), ('rule-2');
            "#,
        )
        .expect("Failed to seed dependency rows");

        conn
    }

    // ========================================================================
    // workflow_versions
    // ========================================================================

    #[test]
    fn test_insert_and_get_workflow_versions() {
        let conn = setup_test_db();

        let id1 = insert_workflow_version(
            &conn, "wf-1", 1, None, Some("tr-1"),
            r#"{"steps":[]}"#, None, None, "manual",
        )
        .unwrap();
        let id2 = insert_workflow_version(
            &conn, "wf-1", 2, Some(&id1), Some("tr-2"),
            r#"{"steps":[1]}"#, Some("added step"), Some(r#"{"added":1}"#), "reflection",
        )
        .unwrap();
        let id3 = insert_workflow_version(
            &conn, "wf-1", 3, Some(&id2), None,
            r#"{"steps":[1,2]}"#, Some("added another"), None, "hardening",
        )
        .unwrap();

        let versions = get_workflow_versions(&conn, "wf-1").unwrap();

        assert_eq!(versions.len(), 3);
        // DESC ordering by version_number
        assert_eq!(versions[0].version_number, 3);
        assert_eq!(versions[1].version_number, 2);
        assert_eq!(versions[2].version_number, 1);
        assert_eq!(versions[0].id, id3);
        assert_eq!(versions[1].id, id2);
        assert_eq!(versions[2].id, id1);
        assert_eq!(versions[1].diff_summary, Some("added step".to_string()));
        assert_eq!(versions[2].parent_version_id, None);
        assert_eq!(versions[1].parent_version_id, Some(id1.clone()));
    }

    #[test]
    fn test_get_latest_workflow_version() {
        let conn = setup_test_db();

        insert_workflow_version(
            &conn, "wf-1", 1, None, None,
            r#"{"v":1}"#, None, None, "manual",
        )
        .unwrap();
        insert_workflow_version(
            &conn, "wf-1", 2, None, None,
            r#"{"v":2}"#, Some("updated"), None, "reflection",
        )
        .unwrap();

        let latest = get_latest_workflow_version(&conn, "wf-1").unwrap();

        assert!(latest.is_some());
        let latest = latest.unwrap();
        assert_eq!(latest.version_number, 2);
        assert_eq!(latest.trigger, "reflection");
        assert_eq!(latest.workflow_json, r#"{"v":2}"#);
    }

    #[test]
    fn test_get_latest_returns_none_for_empty() {
        let conn = setup_test_db();

        let latest = get_latest_workflow_version(&conn, "wf-1").unwrap();

        assert!(latest.is_none());
    }

    // ========================================================================
    // step_finding_links
    // ========================================================================

    #[test]
    fn test_insert_and_get_findings_for_step() {
        let conn = setup_test_db();

        insert_step_finding_link(
            &conn, "tr-1", "validate_schema", 0, "f-1", "detected_during", 0.95,
        )
        .unwrap();
        insert_step_finding_link(
            &conn, "tr-1", "validate_schema", 0, "f-2", "caused_by", 0.80,
        )
        .unwrap();

        let links = get_findings_for_step(&conn, "tr-1", "validate_schema").unwrap();

        assert_eq!(links.len(), 2);
        assert_eq!(links[0].finding_id, "f-1");
        assert_eq!(links[1].finding_id, "f-2");
        assert!((links[0].confidence - 0.95).abs() < f64::EPSILON);
        assert_eq!(links[1].link_type, "caused_by");
    }

    #[test]
    fn test_get_steps_for_finding() {
        let conn = setup_test_db();

        // Two different steps link to the same finding
        insert_step_finding_link(
            &conn, "tr-1", "build_steps", 0, "f-1", "detected_during", 1.0,
        )
        .unwrap();
        insert_step_finding_link(
            &conn, "tr-2", "validate_output", 3, "f-1", "caused_by", 0.7,
        )
        .unwrap();

        let links = get_steps_for_finding(&conn, "f-1").unwrap();

        assert_eq!(links.len(), 2);
        // Both link to the same finding
        assert!(links.iter().all(|l| l.finding_id == "f-1"));
        // Different step names
        let step_names: Vec<&str> = links.iter().map(|l| l.step_name.as_str()).collect();
        assert!(step_names.contains(&"build_steps"));
        assert!(step_names.contains(&"validate_output"));
    }

    // ========================================================================
    // step_provenance
    // ========================================================================

    #[test]
    fn test_insert_and_get_provenance_for_workflow() {
        let conn = setup_test_db();

        insert_step_provenance(
            &conn, "wf-1", None, "fetch_data", 0, "building", "builder",
            Some(1), None, Some(r#"{"action":"fetch"}"#),
        )
        .unwrap();
        insert_step_provenance(
            &conn, "wf-1", None, "validate", 1, "fixing", "fixer",
            Some(1), Some(r#"{"old":"val"}"#), Some(r#"{"new":"val"}"#),
        )
        .unwrap();
        insert_step_provenance(
            &conn, "wf-1", None, "submit", 2, "hardening", "hardener",
            None, None, Some(r#"{"action":"submit"}"#),
        )
        .unwrap();

        let records = get_provenance_for_workflow(&conn, "wf-1").unwrap();

        assert_eq!(records.len(), 3);
        // Ordered by step_index ASC
        assert_eq!(records[0].step_name, "fetch_data");
        assert_eq!(records[0].step_index, 0);
        assert_eq!(records[0].generating_agent, "builder");
        assert_eq!(records[1].step_name, "validate");
        assert_eq!(records[1].phase, "fixing");
        assert_eq!(records[2].step_name, "submit");
        assert_eq!(records[2].generating_agent, "hardener");
        assert!(records[2].generation_iteration.is_none());
    }

    #[test]
    fn test_get_provenance_by_agent() {
        let conn = setup_test_db();

        // Insert entries from different agents
        insert_step_provenance(
            &conn, "wf-1", None, "step_a", 0, "building", "builder",
            Some(1), None, None,
        )
        .unwrap();
        insert_step_provenance(
            &conn, "wf-1", None, "step_b", 1, "fixing", "fixer",
            Some(1), None, None,
        )
        .unwrap();
        insert_step_provenance(
            &conn, "wf-2", None, "step_c", 0, "building", "builder",
            Some(2), None, None,
        )
        .unwrap();

        let builder_records = get_provenance_by_agent(&conn, "builder", 100).unwrap();

        assert_eq!(builder_records.len(), 2);
        assert!(builder_records.iter().all(|r| r.generating_agent == "builder"));

        let fixer_records = get_provenance_by_agent(&conn, "fixer", 100).unwrap();
        assert_eq!(fixer_records.len(), 1);
        assert_eq!(fixer_records[0].step_name, "step_b");
    }

    // ========================================================================
    // generation_pipeline_events
    // ========================================================================

    #[test]
    fn test_insert_and_get_pipeline_events() {
        let conn = setup_test_db();

        insert_pipeline_event(
            &conn, "tr-1", Some("wf-1"), "phase_start", Some("building"),
            Some(1), None, Some(100), Some(500), Some(3), None,
        )
        .unwrap();
        insert_pipeline_event(
            &conn, "tr-1", Some("wf-1"), "phase_end", Some("building"),
            Some(1), Some(r#"{"status":"ok"}"#), Some(200), Some(800), None, Some(1),
        )
        .unwrap();
        insert_pipeline_event(
            &conn, "tr-1", Some("wf-1"), "phase_start", Some("fixing"),
            Some(1), None, Some(150), Some(600), Some(1), None,
        )
        .unwrap();

        let events = get_pipeline_events(&conn, "tr-1").unwrap();

        assert_eq!(events.len(), 3);
        // Ordered by created_at ASC
        assert_eq!(events[0].event_type, "phase_start");
        assert_eq!(events[0].phase, Some("building".to_string()));
        assert_eq!(events[1].event_type, "phase_end");
        assert_eq!(events[1].payload, Some(r#"{"status":"ok"}"#.to_string()));
        assert_eq!(events[2].phase, Some("fixing".to_string()));
        assert_eq!(events[0].duration_ms, Some(100));
        assert_eq!(events[1].token_count, Some(800));
    }

    #[test]
    fn test_get_phase_stats() {
        let conn = setup_test_db();

        // Insert multiple events per phase for wf-1
        // "building" phase: 2 events
        insert_pipeline_event(
            &conn, "tr-1", Some("wf-1"), "step_complete", Some("building"),
            Some(1), None, Some(100), Some(500), Some(4), Some(2),
        )
        .unwrap();
        insert_pipeline_event(
            &conn, "tr-1", Some("wf-1"), "step_complete", Some("building"),
            Some(2), None, Some(200), Some(300), Some(2), Some(0),
        )
        .unwrap();
        // "fixing" phase: 1 event
        insert_pipeline_event(
            &conn, "tr-1", Some("wf-1"), "step_complete", Some("fixing"),
            Some(1), None, Some(150), Some(600), Some(2), Some(1),
        )
        .unwrap();

        let stats = get_phase_stats(&conn, "wf-1").unwrap();

        assert_eq!(stats.len(), 2);
        // Ordered by phase alphabetically
        let building = stats.iter().find(|s| s.phase == "building").unwrap();
        let fixing = stats.iter().find(|s| s.phase == "fixing").unwrap();

        assert_eq!(building.event_count, 2);
        // avg_duration_ms for building = (100+200)/2 = 150.0
        assert!((building.avg_duration_ms.unwrap() - 150.0).abs() < 0.01);
        // total_token_count for building = 500+300 = 800
        assert_eq!(building.total_token_count, 800);
        // avg_errors_before for building = (4+2)/2 = 3.0
        assert!((building.avg_errors_before.unwrap() - 3.0).abs() < 0.01);
        // avg_errors_after for building = (2+0)/2 = 1.0
        assert!((building.avg_errors_after.unwrap() - 1.0).abs() < 0.01);

        assert_eq!(fixing.event_count, 1);
        assert_eq!(fixing.total_token_count, 600);
    }

    // ========================================================================
    // rule_influence_log
    // ========================================================================

    #[test]
    fn test_insert_and_get_rule_influences() {
        let conn = setup_test_db();

        insert_rule_influence(
            &conn, "rule-1", "tr-1", Some("wf-1"), "loaded", None, Some("building"),
        )
        .unwrap();
        insert_rule_influence(
            &conn, "rule-1", "tr-2", Some("wf-1"), "no_effect",
            Some("rule did not match any step"), Some("building"),
        )
        .unwrap();
        insert_rule_influence(
            &conn, "rule-1", "tr-3", None, "prevented_error",
            Some("caught missing field"), Some("fixing"),
        )
        .unwrap();

        let influences = get_rule_influences(&conn, "rule-1").unwrap();

        assert_eq!(influences.len(), 3);
        // All belong to rule-1
        assert!(influences.iter().all(|i| i.rule_id == "rule-1"));
        // Check influence types are present
        let types: Vec<&str> = influences.iter().map(|i| i.influence_type.as_str()).collect();
        assert!(types.contains(&"loaded"));
        assert!(types.contains(&"no_effect"));
        assert!(types.contains(&"prevented_error"));
    }

    #[test]
    fn test_get_ineffective_rules() {
        let conn = setup_test_db();

        // rule-1: 5 no_effect, 0 prevented_error => ineffective
        for i in 0..5 {
            let tr = format!("tr-ne-{}", i);
            conn.execute(
                "INSERT INTO task_runs (id) VALUES (?1)",
                params![tr],
            )
            .unwrap();
            insert_rule_influence(&conn, "rule-1", &tr, None, "no_effect", None, None)
                .unwrap();
        }

        // rule-2: 3 no_effect, 1 prevented_error => NOT ineffective
        for i in 0..3 {
            let tr = format!("tr-ne2-{}", i);
            conn.execute(
                "INSERT INTO task_runs (id) VALUES (?1)",
                params![tr],
            )
            .unwrap();
            insert_rule_influence(&conn, "rule-2", &tr, None, "no_effect", None, None)
                .unwrap();
        }
        conn.execute("INSERT INTO task_runs (id) VALUES ('tr-pe-1')", [])
            .unwrap();
        insert_rule_influence(
            &conn, "rule-2", "tr-pe-1", None, "prevented_error", None, None,
        )
        .unwrap();

        let ineffective = get_ineffective_rules(&conn, 5).unwrap();

        assert_eq!(ineffective.len(), 1);
        assert_eq!(ineffective[0].rule_id, "rule-1");
        assert_eq!(ineffective[0].no_effect_count, 5);
        assert_eq!(ineffective[0].prevented_error_count, 0);
        assert_eq!(ineffective[0].total_loaded_count, 5);
    }

    #[test]
    fn test_get_ineffective_rules_threshold() {
        let conn = setup_test_db();

        // rule-1: 3 no_effect entries, 0 prevented_error
        for i in 0..3 {
            let tr = format!("tr-th-{}", i);
            conn.execute(
                "INSERT INTO task_runs (id) VALUES (?1)",
                params![tr],
            )
            .unwrap();
            insert_rule_influence(&conn, "rule-1", &tr, None, "no_effect", None, None)
                .unwrap();
        }

        // Threshold = 5: rule-1 has only 3, should NOT be returned
        let ineffective = get_ineffective_rules(&conn, 5).unwrap();
        assert_eq!(ineffective.len(), 0);

        // Threshold = 3: rule-1 has exactly 3, should be returned
        let ineffective = get_ineffective_rules(&conn, 3).unwrap();
        assert_eq!(ineffective.len(), 1);
        assert_eq!(ineffective[0].rule_id, "rule-1");
        assert_eq!(ineffective[0].no_effect_count, 3);

        // Threshold = 1: still returned
        let ineffective = get_ineffective_rules(&conn, 1).unwrap();
        assert_eq!(ineffective.len(), 1);
    }
}
