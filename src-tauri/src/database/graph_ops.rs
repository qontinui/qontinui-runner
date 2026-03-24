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
    let id = format!("sp-{}", Uuid::new_v4());
    let now = Utc::now().to_rfc3339();

    conn.execute(
        r#"INSERT INTO step_provenance
           (id, workflow_id, workflow_version_id, step_name, step_index,
            phase, generating_agent, generation_iteration,
            original_step_json, final_step_json, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"#,
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
                      original_step_json, final_step_json, created_at
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
                created_at: row.get(10)?,
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
                      original_step_json, final_step_json, created_at
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
                created_at: row.get(10)?,
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
