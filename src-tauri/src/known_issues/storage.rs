//! Storage operations for known issues in SQLite.
//!
//! Provides CRUD operations and queries for the known_issues and
//! issue_pattern_templates tables.

#![allow(dead_code)]

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use tracing::{info, warn};
use uuid::Uuid;

use super::types::{
    CreateKnownIssueRequest, CreatePatternTemplateRequest, DetectionMethod, IssueCategory,
    IssuePatternTemplate, IssueProvenance, IssueSeverity, IssueStatus, KnownIssue,
    ListKnownIssuesQuery, ScopeType, TemplateParameter, UpdateKnownIssueRequest,
};

/// Ensure the known_issues and issue_pattern_templates tables exist.
pub fn ensure_tables(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS known_issues (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            description TEXT NOT NULL,
            category TEXT NOT NULL,
            scope_type TEXT NOT NULL DEFAULT 'global',
            scope_value TEXT,
            scope_tags TEXT NOT NULL DEFAULT '[]',
            detection_method TEXT NOT NULL DEFAULT 'ai_judgment',
            detection_config TEXT NOT NULL DEFAULT '{}',
            pattern_template_id TEXT,
            reproduction_context TEXT,
            trigger_conditions TEXT NOT NULL DEFAULT '[]',
            severity TEXT NOT NULL DEFAULT 'medium',
            status TEXT NOT NULL DEFAULT 'active',
            confidence REAL NOT NULL DEFAULT 0.5,
            provenance TEXT NOT NULL DEFAULT 'manual',
            source_finding_ids TEXT NOT NULL DEFAULT '[]',
            source_task_run_id TEXT,
            verification_hint TEXT,
            verification_step_template TEXT,
            times_detected INTEGER NOT NULL DEFAULT 1,
            times_checked INTEGER NOT NULL DEFAULT 0,
            last_detected_at TEXT,
            last_checked_at TEXT,
            resolved_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_known_issues_status ON known_issues(status);
        CREATE INDEX IF NOT EXISTS idx_known_issues_category ON known_issues(category);
        CREATE INDEX IF NOT EXISTS idx_known_issues_severity ON known_issues(severity);
        CREATE INDEX IF NOT EXISTS idx_known_issues_scope ON known_issues(scope_type, scope_value);

        CREATE TABLE IF NOT EXISTS issue_pattern_templates (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT NOT NULL,
            category TEXT NOT NULL,
            detection_type TEXT NOT NULL,
            step_template TEXT,
            ai_prompt_template TEXT,
            parameters TEXT NOT NULL DEFAULT '[]',
            built_in INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'active',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        "#,
    )
    .map_err(|e| format!("Failed to create known_issues tables: {}", e))?;

    Ok(())
}

/// Insert a new known issue. Returns the created KnownIssue.
pub fn insert_known_issue(
    conn: &Connection,
    req: &CreateKnownIssueRequest,
) -> Result<KnownIssue, String> {
    let now = Utc::now().to_rfc3339();
    let id = Uuid::new_v4().to_string();

    let scope_tags_json = serde_json::to_string(&req.scope_tags.clone().unwrap_or_default())
        .unwrap_or_else(|_| "[]".to_string());
    let detection_config_json = serde_json::to_string(
        &req.detection_config
            .clone()
            .unwrap_or(serde_json::json!({})),
    )
    .unwrap_or_else(|_| "{}".to_string());
    let trigger_conditions_json =
        serde_json::to_string(&req.trigger_conditions.clone().unwrap_or_default())
            .unwrap_or_else(|_| "[]".to_string());
    let source_finding_ids_json =
        serde_json::to_string(&req.source_finding_ids.clone().unwrap_or_default())
            .unwrap_or_else(|_| "[]".to_string());
    let verification_step_template_json = req
        .verification_step_template
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));

    let provenance = req
        .provenance
        .as_ref()
        .map(|p| p.as_str())
        .unwrap_or("manual");

    conn.execute(
        r#"
        INSERT INTO known_issues (
            id, title, description, category, scope_type, scope_value,
            scope_tags, detection_method, detection_config, pattern_template_id,
            reproduction_context, trigger_conditions, severity, status,
            confidence, provenance, source_finding_ids, source_task_run_id,
            verification_hint, verification_step_template,
            times_detected, times_checked, last_detected_at, last_checked_at,
            resolved_at, created_at, updated_at
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6,
            ?7, ?8, ?9, ?10,
            ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18,
            ?19, ?20,
            ?21, ?22, ?23, ?24,
            ?25, ?26, ?27
        )
        "#,
        params![
            id,
            req.title,
            req.description,
            req.category.as_str(),
            req.scope_type.as_str(),
            req.scope_value,
            scope_tags_json,
            req.detection_method.as_str(),
            detection_config_json,
            req.pattern_template_id,
            req.reproduction_context,
            trigger_conditions_json,
            req.severity.as_str(),
            "active",
            0.5_f64,
            provenance,
            source_finding_ids_json,
            req.source_task_run_id,
            req.verification_hint,
            verification_step_template_json,
            1_u32,                 // times_detected
            0_u32,                 // times_checked
            now,                   // last_detected_at
            rusqlite::types::Null, // last_checked_at
            rusqlite::types::Null, // resolved_at
            now,                   // created_at
            now,                   // updated_at
        ],
    )
    .map_err(|e| format!("Failed to insert known issue: {}", e))?;

    info!("Inserted known issue '{}' (id={})", req.title, id);

    get_known_issue(conn, &id)?.ok_or_else(|| "Known issue not found after insert".to_string())
}

/// Update an existing known issue.
pub fn update_known_issue(
    conn: &Connection,
    id: &str,
    req: &UpdateKnownIssueRequest,
) -> Result<KnownIssue, String> {
    // Fetch existing to merge
    let existing =
        get_known_issue(conn, id)?.ok_or_else(|| format!("Known issue not found: {}", id))?;

    let now = Utc::now().to_rfc3339();

    let title = req.title.as_deref().unwrap_or(&existing.title);
    let description = req.description.as_deref().unwrap_or(&existing.description);
    let category = req.category.as_ref().unwrap_or(&existing.category).as_str();
    let scope_type = req
        .scope_type
        .as_ref()
        .unwrap_or(&existing.scope_type)
        .as_str();
    let scope_value = req
        .scope_value
        .as_ref()
        .or(existing.scope_value.as_ref())
        .cloned();
    let scope_tags_json = req
        .scope_tags
        .as_ref()
        .map(|tags| serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string()))
        .unwrap_or_else(|| {
            serde_json::to_string(&existing.scope_tags).unwrap_or_else(|_| "[]".to_string())
        });
    let detection_method = req
        .detection_method
        .as_ref()
        .unwrap_or(&existing.detection_method)
        .as_str();
    let detection_config_json = req
        .detection_config
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()))
        .unwrap_or_else(|| {
            serde_json::to_string(&existing.detection_config).unwrap_or_else(|_| "{}".to_string())
        });
    let pattern_template_id = req
        .pattern_template_id
        .as_ref()
        .or(existing.pattern_template_id.as_ref())
        .cloned();
    let reproduction_context = req
        .reproduction_context
        .as_ref()
        .or(existing.reproduction_context.as_ref())
        .cloned();
    let trigger_conditions_json = req
        .trigger_conditions
        .as_ref()
        .map(|tc| serde_json::to_string(tc).unwrap_or_else(|_| "[]".to_string()))
        .unwrap_or_else(|| {
            serde_json::to_string(&existing.trigger_conditions).unwrap_or_else(|_| "[]".to_string())
        });
    let severity = req.severity.as_ref().unwrap_or(&existing.severity).as_str();
    let status = req.status.as_ref().unwrap_or(&existing.status).as_str();
    let confidence = req.confidence.unwrap_or(existing.confidence);
    let verification_hint = req
        .verification_hint
        .as_ref()
        .or(existing.verification_hint.as_ref())
        .cloned();
    let verification_step_template_json = req
        .verification_step_template
        .as_ref()
        .or(existing.verification_step_template.as_ref())
        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));

    // If status is changing to resolved, set resolved_at
    let resolved_at = if status == "resolved" && existing.status != IssueStatus::Resolved {
        Some(now.clone())
    } else {
        existing.resolved_at.clone()
    };

    conn.execute(
        r#"
        UPDATE known_issues SET
            title = ?2,
            description = ?3,
            category = ?4,
            scope_type = ?5,
            scope_value = ?6,
            scope_tags = ?7,
            detection_method = ?8,
            detection_config = ?9,
            pattern_template_id = ?10,
            reproduction_context = ?11,
            trigger_conditions = ?12,
            severity = ?13,
            status = ?14,
            confidence = ?15,
            verification_hint = ?16,
            verification_step_template = ?17,
            resolved_at = ?18,
            updated_at = ?19
        WHERE id = ?1
        "#,
        params![
            id,
            title,
            description,
            category,
            scope_type,
            scope_value,
            scope_tags_json,
            detection_method,
            detection_config_json,
            pattern_template_id,
            reproduction_context,
            trigger_conditions_json,
            severity,
            status,
            confidence,
            verification_hint,
            verification_step_template_json,
            resolved_at,
            now,
        ],
    )
    .map_err(|e| format!("Failed to update known issue: {}", e))?;

    info!("Updated known issue '{}'", id);

    get_known_issue(conn, id)?.ok_or_else(|| "Known issue not found after update".to_string())
}

/// Get a known issue by ID.
pub fn get_known_issue(conn: &Connection, id: &str) -> Result<Option<KnownIssue>, String> {
    let result = conn
        .query_row(
            r#"
            SELECT id, title, description, category, scope_type, scope_value,
                   scope_tags, detection_method, detection_config, pattern_template_id,
                   reproduction_context, trigger_conditions, severity, status,
                   confidence, provenance, source_finding_ids, source_task_run_id,
                   verification_hint, verification_step_template,
                   times_detected, times_checked, last_detected_at, last_checked_at,
                   resolved_at, created_at, updated_at
            FROM known_issues
            WHERE id = ?1
            "#,
            params![id],
            row_to_known_issue,
        )
        .optional()
        .map_err(|e| format!("Failed to get known issue: {}", e))?;

    Ok(result)
}

/// List known issues with optional filters.
pub fn list_known_issues(
    conn: &Connection,
    query: &ListKnownIssuesQuery,
) -> Result<Vec<KnownIssue>, String> {
    let mut where_clauses: Vec<String> = Vec::new();
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    // Handle spec_id convenience shortcut
    if let Some(ref spec_id) = query.spec_id {
        where_clauses.push("scope_type = 'spec'".to_string());
        where_clauses.push(format!("scope_value = ?{}", param_values.len() + 1));
        param_values.push(Box::new(spec_id.clone()));
    } else {
        if let Some(ref scope_type) = query.scope_type {
            where_clauses.push(format!("scope_type = ?{}", param_values.len() + 1));
            param_values.push(Box::new(scope_type.clone()));
        }
        if let Some(ref scope_value) = query.scope_value {
            where_clauses.push(format!("scope_value = ?{}", param_values.len() + 1));
            param_values.push(Box::new(scope_value.clone()));
        }
    }

    if let Some(ref category) = query.category {
        where_clauses.push(format!("category = ?{}", param_values.len() + 1));
        param_values.push(Box::new(category.clone()));
    }

    if let Some(ref severity) = query.severity {
        where_clauses.push(format!("severity = ?{}", param_values.len() + 1));
        param_values.push(Box::new(severity.clone()));
    }

    if let Some(ref status) = query.status {
        where_clauses.push(format!("status = ?{}", param_values.len() + 1));
        param_values.push(Box::new(status.clone()));
    }

    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };

    let sql = format!(
        r#"
        SELECT id, title, description, category, scope_type, scope_value,
               scope_tags, detection_method, detection_config, pattern_template_id,
               reproduction_context, trigger_conditions, severity, status,
               confidence, provenance, source_finding_ids, source_task_run_id,
               verification_hint, verification_step_template,
               times_detected, times_checked, last_detected_at, last_checked_at,
               resolved_at, created_at, updated_at
        FROM known_issues
        {}
        ORDER BY
            CASE severity WHEN 'critical' THEN 0 WHEN 'high' THEN 1 WHEN 'medium' THEN 2 WHEN 'low' THEN 3 END,
            created_at DESC
        "#,
        where_sql
    );

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare list query: {}", e))?;

    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();

    let issues = stmt
        .query_map(param_refs.as_slice(), row_to_known_issue)
        .map_err(|e| format!("Failed to query known issues: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(issues)
}

/// Delete a known issue by ID. Returns true if a row was deleted.
pub fn delete_known_issue(conn: &Connection, id: &str) -> Result<bool, String> {
    let rows_affected = conn
        .execute("DELETE FROM known_issues WHERE id = ?1", params![id])
        .map_err(|e| format!("Failed to delete known issue: {}", e))?;

    if rows_affected > 0 {
        info!("Deleted known issue '{}'", id);
    }

    Ok(rows_affected > 0)
}

/// Resolve a known issue (set status to resolved + resolved_at).
pub fn resolve_known_issue(
    conn: &Connection,
    id: &str,
    resolution: Option<&str>,
) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();

    // If a resolution note is provided, append it to the description
    let rows_affected = if let Some(note) = resolution {
        conn.execute(
            r#"
            UPDATE known_issues
            SET status = 'resolved',
                resolved_at = ?2,
                description = description || char(10) || char(10) || 'Resolution: ' || ?3,
                updated_at = ?2
            WHERE id = ?1
            "#,
            params![id, now, note],
        )
        .map_err(|e| format!("Failed to resolve known issue: {}", e))?
    } else {
        conn.execute(
            r#"
            UPDATE known_issues
            SET status = 'resolved',
                resolved_at = ?2,
                updated_at = ?2
            WHERE id = ?1
            "#,
            params![id, now],
        )
        .map_err(|e| format!("Failed to resolve known issue: {}", e))?
    };

    if rows_affected == 0 {
        warn!("resolve_known_issue: no issue found with id '{}'", id);
    } else {
        info!("Resolved known issue '{}'", id);
    }

    Ok(())
}

/// Increment times_detected and update last_detected_at.
pub fn increment_detected(conn: &Connection, id: &str) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();

    conn.execute(
        r#"
        UPDATE known_issues
        SET times_detected = times_detected + 1,
            last_detected_at = ?2,
            updated_at = ?2
        WHERE id = ?1
        "#,
        params![id, now],
    )
    .map_err(|e| format!("Failed to increment detected count: {}", e))?;

    Ok(())
}

/// Increment times_checked and update last_checked_at.
pub fn increment_checked(conn: &Connection, id: &str) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();

    conn.execute(
        r#"
        UPDATE known_issues
        SET times_checked = times_checked + 1,
            last_checked_at = ?2,
            updated_at = ?2
        WHERE id = ?1
        "#,
        params![id, now],
    )
    .map_err(|e| format!("Failed to increment checked count: {}", e))?;

    Ok(())
}

/// Decay confidence when a regression check passes (issue was NOT detected).
/// Confidence decays toward 0.0 by multiplying by a decay factor.
/// If confidence drops below the threshold (0.1), auto-resolve the issue.
pub fn decay_confidence_on_pass(conn: &Connection, id: &str) -> Result<(), String> {
    const DECAY_FACTOR: f64 = 0.85;
    const AUTO_RESOLVE_THRESHOLD: f64 = 0.1;

    let now = Utc::now().to_rfc3339();

    // Get current confidence
    let current_confidence: f64 = conn
        .query_row(
            "SELECT confidence FROM known_issues WHERE id = ?1 AND status = 'active'",
            params![id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Failed to get confidence: {}", e))?
        .unwrap_or(0.5);

    let new_confidence = current_confidence * DECAY_FACTOR;

    if new_confidence < AUTO_RESOLVE_THRESHOLD {
        // Auto-resolve: the issue has consistently passed regression checks
        conn.execute(
            r#"
            UPDATE known_issues
            SET confidence = ?2,
                status = 'resolved',
                resolved_at = ?3,
                description = description || char(10) || char(10) || 'Auto-resolved: confidence decayed below threshold after consistent passing.',
                updated_at = ?3
            WHERE id = ?1 AND status = 'active'
            "#,
            params![id, new_confidence, now],
        )
        .map_err(|e| format!("Failed to auto-resolve known issue: {}", e))?;

        info!(
            "Auto-resolved known issue '{}' (confidence decayed to {:.3})",
            id, new_confidence
        );
    } else {
        conn.execute(
            r#"
            UPDATE known_issues
            SET confidence = ?2,
                updated_at = ?3
            WHERE id = ?1 AND status = 'active'
            "#,
            params![id, new_confidence, now],
        )
        .map_err(|e| format!("Failed to decay confidence: {}", e))?;
    }

    Ok(())
}

/// Find issues relevant to a spec (by spec_id, URL, or global).
/// Returns active issues ordered by severity (critical first).
pub fn find_issues_for_spec(
    conn: &Connection,
    spec_id: &str,
    page_url: Option<&str>,
) -> Result<Vec<KnownIssue>, String> {
    let url = page_url.unwrap_or("");

    let mut stmt = conn
        .prepare(
            r#"
            SELECT id, title, description, category, scope_type, scope_value,
                   scope_tags, detection_method, detection_config, pattern_template_id,
                   reproduction_context, trigger_conditions, severity, status,
                   confidence, provenance, source_finding_ids, source_task_run_id,
                   verification_hint, verification_step_template,
                   times_detected, times_checked, last_detected_at, last_checked_at,
                   resolved_at, created_at, updated_at
            FROM known_issues
            WHERE status = 'active'
              AND (
                (scope_type = 'spec' AND scope_value = ?1)
                OR scope_type = 'global'
                OR (scope_type = 'url' AND scope_value = ?2)
              )
            ORDER BY
                CASE severity WHEN 'critical' THEN 0 WHEN 'high' THEN 1 WHEN 'medium' THEN 2 WHEN 'low' THEN 3 END
            "#,
        )
        .map_err(|e| format!("Failed to prepare find_issues_for_spec query: {}", e))?;

    let issues = stmt
        .query_map(params![spec_id, url], row_to_known_issue)
        .map_err(|e| format!("Failed to query issues for spec: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(issues)
}

/// List all pattern templates.
pub fn list_pattern_templates(conn: &Connection) -> Result<Vec<IssuePatternTemplate>, String> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT id, name, description, category, detection_type,
                   step_template, ai_prompt_template, parameters,
                   built_in, status, created_at, updated_at
            FROM issue_pattern_templates
            ORDER BY name
            "#,
        )
        .map_err(|e| format!("Failed to prepare list templates query: {}", e))?;

    let templates = stmt
        .query_map([], row_to_pattern_template)
        .map_err(|e| format!("Failed to query pattern templates: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(templates)
}

/// Get a pattern template by ID.
pub fn get_pattern_template(
    conn: &Connection,
    id: &str,
) -> Result<Option<IssuePatternTemplate>, String> {
    let result = conn
        .query_row(
            r#"
            SELECT id, name, description, category, detection_type,
                   step_template, ai_prompt_template, parameters,
                   built_in, status, created_at, updated_at
            FROM issue_pattern_templates
            WHERE id = ?1
            "#,
            params![id],
            row_to_pattern_template,
        )
        .optional()
        .map_err(|e| format!("Failed to get pattern template: {}", e))?;

    Ok(result)
}

/// Insert a new pattern template. Returns the created IssuePatternTemplate.
pub fn insert_pattern_template(
    conn: &Connection,
    req: &CreatePatternTemplateRequest,
) -> Result<IssuePatternTemplate, String> {
    let now = Utc::now().to_rfc3339();
    let id = Uuid::new_v4().to_string();

    // Parse parameters JSON string, default to empty array
    let parameters_json = req
        .parameters
        .as_deref()
        .and_then(|s| {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                // Validate it's valid JSON
                serde_json::from_str::<serde_json::Value>(trimmed)
                    .ok()
                    .map(|_| trimmed.to_string())
            }
        })
        .unwrap_or_else(|| "[]".to_string());

    conn.execute(
        r#"
        INSERT INTO issue_pattern_templates (
            id, name, description, category, detection_type,
            step_template, ai_prompt_template, parameters,
            built_in, status, created_at, updated_at
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5,
            ?6, ?7, ?8,
            ?9, ?10, ?11, ?12
        )
        "#,
        params![
            id,
            req.name,
            req.description,
            req.category,
            req.detection_type,
            rusqlite::types::Null, // step_template
            req.ai_prompt_template,
            parameters_json,
            0_i32, // built_in = false
            "active",
            now,
            now,
        ],
    )
    .map_err(|e| format!("Failed to insert pattern template: {}", e))?;

    info!("Inserted pattern template '{}' (id={})", req.name, id);

    get_pattern_template(conn, &id)?
        .ok_or_else(|| "Pattern template not found after insert".to_string())
}

/// Convert a database row to a KnownIssue.
fn row_to_known_issue(row: &rusqlite::Row) -> rusqlite::Result<KnownIssue> {
    let category_str: String = row.get(3)?;
    let scope_type_str: String = row.get(4)?;
    let detection_method_str: String = row.get(7)?;
    let severity_str: String = row.get(12)?;
    let status_str: String = row.get(13)?;
    let provenance_str: String = row.get(15)?;

    // Parse JSON array fields
    let scope_tags_json: String = row.get(6)?;
    let scope_tags: Vec<String> = serde_json::from_str(&scope_tags_json).unwrap_or_default();

    let detection_config_json: String = row.get(8)?;
    let detection_config: serde_json::Value =
        serde_json::from_str(&detection_config_json).unwrap_or(serde_json::json!({}));

    let trigger_conditions_json: String = row.get(11)?;
    let trigger_conditions: Vec<String> =
        serde_json::from_str(&trigger_conditions_json).unwrap_or_default();

    let source_finding_ids_json: String = row.get(16)?;
    let source_finding_ids: Vec<String> =
        serde_json::from_str(&source_finding_ids_json).unwrap_or_default();

    let verification_step_template_json: Option<String> = row.get(19)?;
    let verification_step_template: Option<serde_json::Value> =
        verification_step_template_json.and_then(|json| serde_json::from_str(&json).ok());

    Ok(KnownIssue {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        category: IssueCategory::from_str(&category_str).unwrap_or(IssueCategory::Other),
        scope_type: ScopeType::from_str(&scope_type_str).unwrap_or(ScopeType::Global),
        scope_value: row.get(5)?,
        scope_tags,
        detection_method: DetectionMethod::from_str(&detection_method_str)
            .unwrap_or(DetectionMethod::AiJudgment),
        detection_config,
        pattern_template_id: row.get(9)?,
        reproduction_context: row.get(10)?,
        trigger_conditions,
        severity: IssueSeverity::from_str(&severity_str).unwrap_or(IssueSeverity::Medium),
        status: IssueStatus::from_str(&status_str).unwrap_or(IssueStatus::Active),
        confidence: row.get(14)?,
        provenance: IssueProvenance::from_str(&provenance_str).unwrap_or(IssueProvenance::Manual),
        source_finding_ids,
        source_task_run_id: row.get(17)?,
        verification_hint: row.get(18)?,
        verification_step_template,
        times_detected: row.get(20)?,
        times_checked: row.get(21)?,
        last_detected_at: row.get(22)?,
        last_checked_at: row.get(23)?,
        resolved_at: row.get(24)?,
        created_at: row.get(25)?,
        updated_at: row.get(26)?,
    })
}

/// Convert a database row to an IssuePatternTemplate.
fn row_to_pattern_template(row: &rusqlite::Row) -> rusqlite::Result<IssuePatternTemplate> {
    let step_template_json: Option<String> = row.get(5)?;
    let step_template: Option<serde_json::Value> =
        step_template_json.and_then(|json| serde_json::from_str(&json).ok());

    let parameters_json: String = row.get(7)?;
    let parameters: Vec<TemplateParameter> =
        serde_json::from_str(&parameters_json).unwrap_or_default();

    let built_in_int: i32 = row.get(8)?;

    Ok(IssuePatternTemplate {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        category: row.get(3)?,
        detection_type: row.get(4)?,
        step_template,
        ai_prompt_template: row.get(6)?,
        parameters,
        built_in: built_in_int != 0,
        status: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

/// Find active known issues relevant for workflow generation based on depth level.
/// - "thorough": only critical + high severity
/// - "regression": all active issues
pub fn find_relevant_issues_for_generation(
    conn: &Connection,
    depth: &str,
) -> Result<Vec<KnownIssue>, String> {
    let sql = match depth {
        "thorough" => {
            "SELECT id, title, description, category, scope_type, scope_value, scope_tags, detection_method, detection_config, pattern_template_id, reproduction_context, trigger_conditions, severity, status, confidence, provenance, source_finding_ids, source_task_run_id, verification_hint, verification_step_template, times_detected, times_checked, last_detected_at, last_checked_at, resolved_at, created_at, updated_at FROM known_issues WHERE status = 'active' AND severity IN ('critical', 'high') ORDER BY severity ASC, times_detected DESC LIMIT 10"
        }
        "regression" => {
            "SELECT id, title, description, category, scope_type, scope_value, scope_tags, detection_method, detection_config, pattern_template_id, reproduction_context, trigger_conditions, severity, status, confidence, provenance, source_finding_ids, source_task_run_id, verification_hint, verification_step_template, times_detected, times_checked, last_detected_at, last_checked_at, resolved_at, created_at, updated_at FROM known_issues WHERE status = 'active' ORDER BY severity ASC, times_detected DESC LIMIT 30"
        }
        _ => return Ok(vec![]),
    };

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Failed to prepare query: {}", e))?;
    let issues = stmt
        .query_map([], row_to_known_issue)
        .map_err(|e| format!("Failed to query issues: {}", e))?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();

    Ok(issues)
}

/// Tokenize a string into lowercase words, filtering out words shorter than 3 characters.
fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.to_lowercase())
        .filter(|w| w.len() >= 3)
        .collect()
}

/// Compute a keyword-overlap relevance score (0.0–1.0) between task tokens and an issue.
///
/// Counts how many unique task tokens appear in the issue's title, description,
/// or scope_tags, then divides by the total number of task tokens.
fn compute_relevance_score(task_tokens: &[String], issue: &KnownIssue) -> f64 {
    if task_tokens.is_empty() {
        return 0.0;
    }

    let title_lower = issue.title.to_lowercase();
    let desc_lower = issue.description.to_lowercase();
    let tags_lower: String = issue
        .scope_tags
        .iter()
        .map(|t| t.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");

    let haystack = format!("{} {} {}", title_lower, desc_lower, tags_lower);

    let matches = task_tokens
        .iter()
        .filter(|token| haystack.contains(token.as_str()))
        .count();

    matches as f64 / task_tokens.len() as f64
}

/// Return a numeric ordering value for severity (lower = more severe).
fn severity_order(severity: &IssueSeverity) -> u8 {
    match severity {
        IssueSeverity::Critical => 0,
        IssueSeverity::High => 1,
        IssueSeverity::Medium => 2,
        IssueSeverity::Low => 3,
    }
}

/// Find active known issues relevant for workflow generation, ranked by keyword
/// relevance to the given task description.
///
/// Behaviour:
/// - Queries active issues filtered by depth (same severity/limit rules as
///   [`find_relevant_issues_for_generation`]).
/// - Scores each issue by keyword overlap between `task_description` and the
///   issue's title + description + scope_tags.
/// - Sorts by relevance score (descending), then severity, then times_detected.
/// - Issues with a positive relevance score appear before those with zero score.
pub fn find_relevant_issues_for_generation_with_context(
    conn: &Connection,
    depth: &str,
    task_description: &str,
) -> Result<Vec<KnownIssue>, String> {
    // Reuse the base query to get the candidate issues.
    let mut issues = find_relevant_issues_for_generation(conn, depth)?;

    if issues.is_empty() || task_description.trim().is_empty() {
        return Ok(issues);
    }

    let task_tokens = tokenize(task_description);
    if task_tokens.is_empty() {
        return Ok(issues);
    }

    // Compute scores and sort: relevance desc, severity asc, times_detected desc.
    issues.sort_by(|a, b| {
        let score_a = compute_relevance_score(&task_tokens, a);
        let score_b = compute_relevance_score(&task_tokens, b);

        // Higher relevance first
        score_b
            .partial_cmp(&score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Then by severity (critical < high < medium < low)
            .then_with(|| severity_order(&a.severity).cmp(&severity_order(&b.severity)))
            // Then by times_detected descending
            .then_with(|| b.times_detected.cmp(&a.times_detected))
    });

    Ok(issues)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_tables(&conn).unwrap();
        conn
    }

    fn sample_create_request() -> CreateKnownIssueRequest {
        CreateKnownIssueRequest {
            title: "Duplicate navigation items".to_string(),
            description: "The sidebar shows duplicate nav links after page refresh".to_string(),
            category: IssueCategory::Duplication,
            scope_type: ScopeType::Url,
            scope_value: Some("http://localhost:3001/dashboard".to_string()),
            scope_tags: Some(vec!["sidebar".to_string(), "navigation".to_string()]),
            detection_method: DetectionMethod::UiBridge,
            detection_config: Some(serde_json::json!({
                "selector": ".nav-item",
                "max_expected": 5
            })),
            pattern_template_id: Some("pt_text_duplication".to_string()),
            reproduction_context: Some("Refresh the dashboard page twice".to_string()),
            trigger_conditions: Some(vec!["page_refresh".to_string()]),
            severity: IssueSeverity::High,
            provenance: Some(IssueProvenance::AutoDetected),
            source_finding_ids: Some(vec!["finding-001".to_string()]),
            source_task_run_id: Some("task-run-abc".to_string()),
            verification_hint: Some("Check sidebar link count after refresh".to_string()),
            verification_step_template: Some(serde_json::json!({
                "type": "command",
                "command": "curl http://localhost:3001/api/ui-bridge/control/snapshot"
            })),
        }
    }

    #[test]
    fn test_insert_and_get() {
        let conn = create_test_db();
        let req = sample_create_request();

        let issue = insert_known_issue(&conn, &req).unwrap();

        assert_eq!(issue.title, "Duplicate navigation items");
        assert_eq!(issue.category, IssueCategory::Duplication);
        assert_eq!(issue.scope_type, ScopeType::Url);
        assert_eq!(
            issue.scope_value,
            Some("http://localhost:3001/dashboard".to_string())
        );
        assert_eq!(issue.scope_tags, vec!["sidebar", "navigation"]);
        assert_eq!(issue.detection_method, DetectionMethod::UiBridge);
        assert_eq!(issue.severity, IssueSeverity::High);
        assert_eq!(issue.status, IssueStatus::Active);
        assert_eq!(issue.provenance, IssueProvenance::AutoDetected);
        assert_eq!(issue.times_detected, 1);
        assert_eq!(issue.times_checked, 0);
        assert!(issue.last_detected_at.is_some());
        assert!(issue.last_checked_at.is_none());
        assert!(issue.resolved_at.is_none());

        // Get by ID
        let loaded = get_known_issue(&conn, &issue.id).unwrap().unwrap();
        assert_eq!(loaded.id, issue.id);
        assert_eq!(loaded.title, "Duplicate navigation items");
    }

    #[test]
    fn test_update() {
        let conn = create_test_db();
        let req = sample_create_request();
        let issue = insert_known_issue(&conn, &req).unwrap();

        let update = UpdateKnownIssueRequest {
            title: Some("Updated title".to_string()),
            severity: Some(IssueSeverity::Critical),
            status: Some(IssueStatus::Monitoring),
            confidence: Some(0.9),
            description: None,
            category: None,
            scope_type: None,
            scope_value: None,
            scope_tags: None,
            detection_method: None,
            detection_config: None,
            pattern_template_id: None,
            reproduction_context: None,
            trigger_conditions: None,
            verification_hint: None,
            verification_step_template: None,
        };

        let updated = update_known_issue(&conn, &issue.id, &update).unwrap();

        assert_eq!(updated.title, "Updated title");
        assert_eq!(updated.severity, IssueSeverity::Critical);
        assert_eq!(updated.status, IssueStatus::Monitoring);
        assert!((updated.confidence - 0.9).abs() < f64::EPSILON);
        // Unchanged fields should remain
        assert_eq!(updated.category, IssueCategory::Duplication);
        assert_eq!(updated.description, issue.description);
    }

    #[test]
    fn test_update_to_resolved_sets_resolved_at() {
        let conn = create_test_db();
        let req = sample_create_request();
        let issue = insert_known_issue(&conn, &req).unwrap();
        assert!(issue.resolved_at.is_none());

        let update = UpdateKnownIssueRequest {
            status: Some(IssueStatus::Resolved),
            title: None,
            description: None,
            category: None,
            scope_type: None,
            scope_value: None,
            scope_tags: None,
            detection_method: None,
            detection_config: None,
            pattern_template_id: None,
            reproduction_context: None,
            trigger_conditions: None,
            severity: None,
            confidence: None,
            verification_hint: None,
            verification_step_template: None,
        };

        let updated = update_known_issue(&conn, &issue.id, &update).unwrap();
        assert_eq!(updated.status, IssueStatus::Resolved);
        assert!(updated.resolved_at.is_some());
    }

    #[test]
    fn test_delete() {
        let conn = create_test_db();
        let req = sample_create_request();
        let issue = insert_known_issue(&conn, &req).unwrap();

        assert!(delete_known_issue(&conn, &issue.id).unwrap());
        assert!(get_known_issue(&conn, &issue.id).unwrap().is_none());

        // Deleting again returns false
        assert!(!delete_known_issue(&conn, &issue.id).unwrap());
    }

    #[test]
    fn test_resolve() {
        let conn = create_test_db();
        let req = sample_create_request();
        let issue = insert_known_issue(&conn, &req).unwrap();

        resolve_known_issue(&conn, &issue.id, Some("Fixed in PR #42")).unwrap();

        let resolved = get_known_issue(&conn, &issue.id).unwrap().unwrap();
        assert_eq!(resolved.status, IssueStatus::Resolved);
        assert!(resolved.resolved_at.is_some());
        assert!(resolved.description.contains("Resolution: Fixed in PR #42"));
    }

    #[test]
    fn test_increment_detected() {
        let conn = create_test_db();
        let req = sample_create_request();
        let issue = insert_known_issue(&conn, &req).unwrap();
        assert_eq!(issue.times_detected, 1);

        increment_detected(&conn, &issue.id).unwrap();
        increment_detected(&conn, &issue.id).unwrap();

        let updated = get_known_issue(&conn, &issue.id).unwrap().unwrap();
        assert_eq!(updated.times_detected, 3);
        assert!(updated.last_detected_at.is_some());
    }

    #[test]
    fn test_increment_checked() {
        let conn = create_test_db();
        let req = sample_create_request();
        let issue = insert_known_issue(&conn, &req).unwrap();
        assert_eq!(issue.times_checked, 0);

        increment_checked(&conn, &issue.id).unwrap();

        let updated = get_known_issue(&conn, &issue.id).unwrap().unwrap();
        assert_eq!(updated.times_checked, 1);
        assert!(updated.last_checked_at.is_some());
    }

    #[test]
    fn test_list_with_filters() {
        let conn = create_test_db();

        // Insert issues with different categories and severities
        let mut req1 = sample_create_request();
        req1.title = "Issue A".to_string();
        req1.category = IssueCategory::Duplication;
        req1.severity = IssueSeverity::Critical;
        insert_known_issue(&conn, &req1).unwrap();

        let mut req2 = sample_create_request();
        req2.title = "Issue B".to_string();
        req2.category = IssueCategory::Rendering;
        req2.severity = IssueSeverity::Low;
        insert_known_issue(&conn, &req2).unwrap();

        let mut req3 = sample_create_request();
        req3.title = "Issue C".to_string();
        req3.category = IssueCategory::Duplication;
        req3.severity = IssueSeverity::Medium;
        insert_known_issue(&conn, &req3).unwrap();

        // List all
        let all = list_known_issues(&conn, &ListKnownIssuesQuery::default()).unwrap();
        assert_eq!(all.len(), 3);
        // Should be sorted by severity: critical first
        assert_eq!(all[0].title, "Issue A");

        // Filter by category
        let duplication = list_known_issues(
            &conn,
            &ListKnownIssuesQuery {
                category: Some("duplication".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(duplication.len(), 2);

        // Filter by severity
        let low = list_known_issues(
            &conn,
            &ListKnownIssuesQuery {
                severity: Some("low".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(low.len(), 1);
        assert_eq!(low[0].title, "Issue B");
    }

    #[test]
    fn test_find_issues_for_spec() {
        let conn = create_test_db();

        // Global issue
        let mut req_global = sample_create_request();
        req_global.title = "Global issue".to_string();
        req_global.scope_type = ScopeType::Global;
        req_global.scope_value = None;
        req_global.severity = IssueSeverity::Low;
        insert_known_issue(&conn, &req_global).unwrap();

        // Spec-scoped issue
        let mut req_spec = sample_create_request();
        req_spec.title = "Spec issue".to_string();
        req_spec.scope_type = ScopeType::Spec;
        req_spec.scope_value = Some("spec-123".to_string());
        req_spec.severity = IssueSeverity::Critical;
        insert_known_issue(&conn, &req_spec).unwrap();

        // URL-scoped issue
        let mut req_url = sample_create_request();
        req_url.title = "URL issue".to_string();
        req_url.scope_type = ScopeType::Url;
        req_url.scope_value = Some("http://localhost:3001/page".to_string());
        req_url.severity = IssueSeverity::High;
        insert_known_issue(&conn, &req_url).unwrap();

        // Unrelated spec issue
        let mut req_other = sample_create_request();
        req_other.title = "Other spec issue".to_string();
        req_other.scope_type = ScopeType::Spec;
        req_other.scope_value = Some("spec-999".to_string());
        insert_known_issue(&conn, &req_other).unwrap();

        // Find for spec-123 with URL
        let issues =
            find_issues_for_spec(&conn, "spec-123", Some("http://localhost:3001/page")).unwrap();

        assert_eq!(issues.len(), 3);
        // Critical first, then high, then low
        assert_eq!(issues[0].title, "Spec issue");
        assert_eq!(issues[1].title, "URL issue");
        assert_eq!(issues[2].title, "Global issue");

        // Find for spec-123 without URL
        let issues_no_url = find_issues_for_spec(&conn, "spec-123", None).unwrap();
        assert_eq!(issues_no_url.len(), 2); // spec + global, no URL match
    }

    #[test]
    fn test_list_spec_id_shortcut() {
        let conn = create_test_db();

        let mut req = sample_create_request();
        req.scope_type = ScopeType::Spec;
        req.scope_value = Some("spec-abc".to_string());
        insert_known_issue(&conn, &req).unwrap();

        let mut req2 = sample_create_request();
        req2.title = "Other".to_string();
        req2.scope_type = ScopeType::Global;
        req2.scope_value = None;
        insert_known_issue(&conn, &req2).unwrap();

        let results = list_known_issues(
            &conn,
            &ListKnownIssuesQuery {
                spec_id: Some("spec-abc".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].scope_value, Some("spec-abc".to_string()));
    }

    #[test]
    fn test_pattern_template_crud() {
        let conn = create_test_db();

        // Seed templates
        super::super::templates::seed_templates(&conn).unwrap();

        let templates = list_pattern_templates(&conn).unwrap();
        assert_eq!(templates.len(), 6);

        // All should be built-in
        for t in &templates {
            assert!(t.built_in);
            assert_eq!(t.status, "active");
        }

        // Get by ID
        let template = get_pattern_template(&conn, "pt_text_duplication")
            .unwrap()
            .unwrap();
        assert_eq!(template.name, "Text Duplication");
        assert_eq!(template.category, "duplication");

        // Seeding again should not duplicate (INSERT OR IGNORE)
        super::super::templates::seed_templates(&conn).unwrap();
        let templates_again = list_pattern_templates(&conn).unwrap();
        assert_eq!(templates_again.len(), 6);
    }

    #[test]
    fn test_get_nonexistent() {
        let conn = create_test_db();
        assert!(get_known_issue(&conn, "nonexistent").unwrap().is_none());
        assert!(get_pattern_template(&conn, "nonexistent")
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_resolved_issues_excluded_from_spec_search() {
        let conn = create_test_db();

        let mut req = sample_create_request();
        req.scope_type = ScopeType::Spec;
        req.scope_value = Some("spec-123".to_string());
        let issue = insert_known_issue(&conn, &req).unwrap();

        // Should find it
        let found = find_issues_for_spec(&conn, "spec-123", None).unwrap();
        assert_eq!(found.len(), 1);

        // Resolve it
        resolve_known_issue(&conn, &issue.id, None).unwrap();

        // Should no longer find it (only active issues)
        let found_after = find_issues_for_spec(&conn, "spec-123", None).unwrap();
        assert_eq!(found_after.len(), 0);
    }

    #[test]
    fn test_confidence_decay() {
        let conn = create_test_db();
        let req = sample_create_request();
        let issue = insert_known_issue(&conn, &req).unwrap();

        // Initial confidence is 0.5
        let initial = get_known_issue(&conn, &issue.id).unwrap().unwrap();
        assert!((initial.confidence - 0.5).abs() < f64::EPSILON);

        // Decay once: 0.5 * 0.85 = 0.425
        decay_confidence_on_pass(&conn, &issue.id).unwrap();
        let after1 = get_known_issue(&conn, &issue.id).unwrap().unwrap();
        assert!((after1.confidence - 0.425).abs() < 0.01);
        assert_eq!(after1.status, IssueStatus::Active);

        // Decay many times until auto-resolved
        for _ in 0..20 {
            decay_confidence_on_pass(&conn, &issue.id).unwrap();
        }
        let after_many = get_known_issue(&conn, &issue.id).unwrap().unwrap();
        assert_eq!(after_many.status, IssueStatus::Resolved);
        assert!(after_many.resolved_at.is_some());
        assert!(after_many.description.contains("Auto-resolved"));
    }

    #[test]
    fn test_find_relevant_issues_with_context_ranks_by_keyword_overlap() {
        let conn = create_test_db();

        // Issue about sidebar navigation
        let mut req_nav = sample_create_request();
        req_nav.title = "Sidebar navigation broken".to_string();
        req_nav.description =
            "The sidebar navigation links are duplicated after refresh".to_string();
        req_nav.scope_tags = Some(vec!["sidebar".to_string(), "navigation".to_string()]);
        req_nav.severity = IssueSeverity::Medium;
        req_nav.scope_type = ScopeType::Global;
        req_nav.scope_value = None;
        insert_known_issue(&conn, &req_nav).unwrap();

        // Issue about login form
        let mut req_login = sample_create_request();
        req_login.title = "Login form validation fails".to_string();
        req_login.description =
            "The login form does not validate email addresses correctly".to_string();
        req_login.scope_tags = Some(vec![
            "login".to_string(),
            "form".to_string(),
            "validation".to_string(),
        ]);
        req_login.severity = IssueSeverity::High;
        req_login.scope_type = ScopeType::Global;
        req_login.scope_value = None;
        insert_known_issue(&conn, &req_login).unwrap();

        // Issue about database performance
        let mut req_db = sample_create_request();
        req_db.title = "Database query slow".to_string();
        req_db.description = "The database query for user profiles takes too long".to_string();
        req_db.scope_tags = Some(vec!["database".to_string(), "performance".to_string()]);
        req_db.severity = IssueSeverity::Critical;
        req_db.scope_type = ScopeType::Global;
        req_db.scope_value = None;
        insert_known_issue(&conn, &req_db).unwrap();

        // Search with a task description about login validation
        let results = find_relevant_issues_for_generation_with_context(
            &conn,
            "regression",
            "Fix the login form validation for email addresses",
        )
        .unwrap();

        assert_eq!(results.len(), 3);
        // Login issue should be first due to high keyword overlap
        assert_eq!(results[0].title, "Login form validation fails");

        // Search with a task description about sidebar
        let results2 = find_relevant_issues_for_generation_with_context(
            &conn,
            "regression",
            "Fix the sidebar navigation duplicate links after page refresh",
        )
        .unwrap();

        assert_eq!(results2.len(), 3);
        // Sidebar issue should be first due to high keyword overlap
        assert_eq!(results2[0].title, "Sidebar navigation broken");

        // Search with a task description about database
        let results3 = find_relevant_issues_for_generation_with_context(
            &conn,
            "regression",
            "Optimize the database query for user profiles",
        )
        .unwrap();

        assert_eq!(results3.len(), 3);
        // Database issue should be first
        assert_eq!(results3[0].title, "Database query slow");

        // Empty task description should fall back to the base query order
        let results4 =
            find_relevant_issues_for_generation_with_context(&conn, "regression", "").unwrap();

        assert_eq!(results4.len(), 3);
        // With empty description, original severity-based ordering is preserved
        // (critical first from the SQL ORDER BY)
        assert_eq!(results4[0].title, "Database query slow");

        // Thorough depth should only include critical + high severity
        let results5 = find_relevant_issues_for_generation_with_context(
            &conn,
            "thorough",
            "Fix the login form validation for email addresses",
        )
        .unwrap();

        assert_eq!(results5.len(), 2); // Only critical + high
                                       // Login issue (high) should be first due to keyword match beating
                                       // database issue (critical) which has zero relevance
        assert_eq!(results5[0].title, "Login form validation fails");
    }
}
