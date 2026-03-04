//! Storage operations for error monitoring.
//!
//! Provides database access for log sources and error events.

use super::types::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;

/// Storage operations for log sources
pub struct LogSourceStorage;

impl LogSourceStorage {
    /// Insert a new log source configuration
    pub fn insert(conn: &Connection, config: &LogSourceConfig) -> Result<i64, String> {
        let error_patterns_json = config
            .error_patterns
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap_or_default());
        let warning_patterns_json = config
            .warning_patterns
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap_or_default());
        let ignore_patterns_json = config
            .ignore_patterns
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap_or_default());

        conn.execute(
            r#"
            INSERT INTO log_sources (
                name, description, path, path_type, format, parser,
                timestamp_pattern, timezone, error_patterns, warning_patterns,
                ignore_patterns, enabled, poll_interval_ms, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, datetime('now'), datetime('now'))
            "#,
            params![
                config.name,
                config.description,
                config.path,
                config.path_type.as_str(),
                config.format.as_str(),
                config.parser.as_str(),
                config.timestamp_pattern,
                config.timezone,
                error_patterns_json,
                warning_patterns_json,
                ignore_patterns_json,
                config.enabled,
                config.poll_interval_ms,
            ],
        )
        .map_err(|e| format!("Failed to insert log source: {}", e))?;

        Ok(conn.last_insert_rowid())
    }

    /// Update an existing log source configuration
    pub fn update(conn: &Connection, id: i64, config: &LogSourceConfig) -> Result<(), String> {
        let error_patterns_json = config
            .error_patterns
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap_or_default());
        let warning_patterns_json = config
            .warning_patterns
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap_or_default());
        let ignore_patterns_json = config
            .ignore_patterns
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap_or_default());

        conn.execute(
            r#"
            UPDATE log_sources SET
                name = ?1, description = ?2, path = ?3, path_type = ?4,
                format = ?5, parser = ?6, timestamp_pattern = ?7, timezone = ?8,
                error_patterns = ?9, warning_patterns = ?10, ignore_patterns = ?11,
                enabled = ?12, poll_interval_ms = ?13, updated_at = datetime('now')
            WHERE id = ?14
            "#,
            params![
                config.name,
                config.description,
                config.path,
                config.path_type.as_str(),
                config.format.as_str(),
                config.parser.as_str(),
                config.timestamp_pattern,
                config.timezone,
                error_patterns_json,
                warning_patterns_json,
                ignore_patterns_json,
                config.enabled,
                config.poll_interval_ms,
                id,
            ],
        )
        .map_err(|e| format!("Failed to update log source: {}", e))?;

        Ok(())
    }

    /// Delete a log source configuration
    pub fn delete(conn: &Connection, id: i64) -> Result<(), String> {
        conn.execute("DELETE FROM log_sources WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete log source: {}", e))?;
        Ok(())
    }

    /// Get a log source by ID
    pub fn get_by_id(conn: &Connection, id: i64) -> Result<Option<LogSourceConfig>, String> {
        conn.query_row(
            r#"
            SELECT id, name, description, path, path_type, format, parser,
                   timestamp_pattern, timezone, error_patterns, warning_patterns,
                   ignore_patterns, enabled, poll_interval_ms, created_at, updated_at
            FROM log_sources WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(LogSourceConfig {
                    id: Some(row.get(0)?),
                    name: row.get(1)?,
                    description: row.get(2)?,
                    path: row.get(3)?,
                    path_type: PathType::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                    format: LogFormat::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
                    parser: ParserType::from_str(&row.get::<_, String>(6)?).unwrap_or_default(),
                    timestamp_pattern: row.get(7)?,
                    timezone: row
                        .get::<_, Option<String>>(8)?
                        .unwrap_or_else(|| "local".to_string()),
                    error_patterns: row
                        .get::<_, Option<String>>(9)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    warning_patterns: row
                        .get::<_, Option<String>>(10)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    ignore_patterns: row
                        .get::<_, Option<String>>(11)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    enabled: row.get(12)?,
                    poll_interval_ms: row.get(13)?,
                    created_at: row.get(14)?,
                    updated_at: row.get(15)?,
                })
            },
        )
        .optional()
        .map_err(|e| format!("Failed to get log source: {}", e))
    }

    /// Get all log sources
    pub fn get_all(conn: &Connection) -> Result<Vec<LogSourceConfig>, String> {
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, path, path_type, format, parser,
                       timestamp_pattern, timezone, error_patterns, warning_patterns,
                       ignore_patterns, enabled, poll_interval_ms, created_at, updated_at
                FROM log_sources ORDER BY name
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(LogSourceConfig {
                    id: Some(row.get(0)?),
                    name: row.get(1)?,
                    description: row.get(2)?,
                    path: row.get(3)?,
                    path_type: PathType::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                    format: LogFormat::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
                    parser: ParserType::from_str(&row.get::<_, String>(6)?).unwrap_or_default(),
                    timestamp_pattern: row.get(7)?,
                    timezone: row
                        .get::<_, Option<String>>(8)?
                        .unwrap_or_else(|| "local".to_string()),
                    error_patterns: row
                        .get::<_, Option<String>>(9)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    warning_patterns: row
                        .get::<_, Option<String>>(10)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    ignore_patterns: row
                        .get::<_, Option<String>>(11)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    enabled: row.get(12)?,
                    poll_interval_ms: row.get(13)?,
                    created_at: row.get(14)?,
                    updated_at: row.get(15)?,
                })
            })
            .map_err(|e| format!("Failed to query log sources: {}", e))?;

        let mut sources = Vec::new();
        for row in rows {
            sources.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
        }
        Ok(sources)
    }

    /// List log sources with optional enabled filter
    pub fn list(conn: &Connection, enabled_only: bool) -> Result<Vec<LogSourceConfig>, String> {
        if enabled_only {
            Self::get_enabled(conn)
        } else {
            Self::get_all(conn)
        }
    }

    /// Delete a log source by name
    pub fn delete_by_name(conn: &Connection, name: &str) -> Result<(), String> {
        conn.execute("DELETE FROM log_sources WHERE name = ?1", params![name])
            .map_err(|e| format!("Failed to delete log source: {}", e))?;
        Ok(())
    }

    /// Get enabled log sources
    pub fn get_enabled(conn: &Connection) -> Result<Vec<LogSourceConfig>, String> {
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, path, path_type, format, parser,
                       timestamp_pattern, timezone, error_patterns, warning_patterns,
                       ignore_patterns, enabled, poll_interval_ms, created_at, updated_at
                FROM log_sources WHERE enabled = 1 ORDER BY name
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(LogSourceConfig {
                    id: Some(row.get(0)?),
                    name: row.get(1)?,
                    description: row.get(2)?,
                    path: row.get(3)?,
                    path_type: PathType::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                    format: LogFormat::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
                    parser: ParserType::from_str(&row.get::<_, String>(6)?).unwrap_or_default(),
                    timestamp_pattern: row.get(7)?,
                    timezone: row
                        .get::<_, Option<String>>(8)?
                        .unwrap_or_else(|| "local".to_string()),
                    error_patterns: row
                        .get::<_, Option<String>>(9)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    warning_patterns: row
                        .get::<_, Option<String>>(10)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    ignore_patterns: row
                        .get::<_, Option<String>>(11)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    enabled: row.get(12)?,
                    poll_interval_ms: row.get(13)?,
                    created_at: row.get(14)?,
                    updated_at: row.get(15)?,
                })
            })
            .map_err(|e| format!("Failed to query log sources: {}", e))?;

        let mut sources = Vec::new();
        for row in rows {
            sources.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
        }
        Ok(sources)
    }
}

/// Storage operations for error events
pub struct ErrorEventStorage;

impl ErrorEventStorage {
    /// Insert a new error event and return the stored event
    pub fn insert(
        conn: &Connection,
        event: &ErrorEvent,
        task_run_id: Option<&str>,
        workflow_name: Option<&str>,
    ) -> Result<StoredErrorEvent, String> {
        // Get log source ID by name
        let log_source_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM log_sources WHERE name = ?1",
                params![event.log_source_name],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Failed to lookup log source: {}", e))?;

        // Insert or increment
        let id = Self::insert_or_increment(conn, event, log_source_id, task_run_id, workflow_name)?;

        // Return the stored event
        Self::get_by_id(conn, id)?
            .ok_or_else(|| "Failed to retrieve inserted error event".to_string())
    }

    /// Insert a new error event or increment occurrence count if duplicate
    pub fn insert_or_increment(
        conn: &Connection,
        event: &ErrorEvent,
        log_source_id: Option<i64>,
        task_run_id: Option<&str>,
        workflow_step_id: Option<&str>,
    ) -> Result<i64, String> {
        let signature_hash = event.compute_signature_hash();
        let _location_json = event
            .location
            .as_ref()
            .map(|l| serde_json::to_string(l).unwrap_or_default());

        // Check if this error already exists (by signature hash)
        let existing_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM error_events WHERE signature_hash = ?1 AND status NOT IN ('resolved', 'wont_fix', 'ignored')",
                params![signature_hash],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Failed to check existing error: {}", e))?;

        if let Some(id) = existing_id {
            // Increment occurrence count and update last_seen_at
            conn.execute(
                r#"
                UPDATE error_events SET
                    occurrence_count = occurrence_count + 1,
                    last_seen_at = datetime('now'),
                    log_timestamp = COALESCE(?1, log_timestamp),
                    task_run_id = COALESCE(?2, task_run_id),
                    workflow_step_id = COALESCE(?3, workflow_step_id)
                WHERE id = ?4
                "#,
                params![event.log_timestamp, task_run_id, workflow_step_id, id],
            )
            .map_err(|e| format!("Failed to update error occurrence: {}", e))?;

            Ok(id)
        } else {
            // Insert new error event
            conn.execute(
                r#"
                INSERT INTO error_events (
                    log_source_id, log_source_name, task_run_id, workflow_step_id,
                    log_timestamp, captured_at, severity, error_type, error_code,
                    message, stack_trace, context_lines, raw_entry,
                    file_path, line_number, column_number, function_name,
                    signature_hash, occurrence_count, first_seen_at, last_seen_at,
                    status, trace_id
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, datetime('now'), ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                    ?13, ?14, ?15, ?16, ?17, 1, datetime('now'), datetime('now'), 'new', ?18
                )
                "#,
                params![
                    log_source_id,
                    event.log_source_name,
                    task_run_id,
                    workflow_step_id,
                    event.log_timestamp,
                    event.severity.as_str(),
                    event.error_type,
                    event.error_code,
                    event.message,
                    event.stack_trace,
                    event.context_lines,
                    event.raw_entry,
                    event.location.as_ref().map(|l| &l.file_path),
                    event.location.as_ref().and_then(|l| l.line_number),
                    event.location.as_ref().and_then(|l| l.column_number),
                    event
                        .location
                        .as_ref()
                        .and_then(|l| l.function_name.as_ref()),
                    signature_hash,
                    event.trace_id,
                ],
            )
            .map_err(|e| format!("Failed to insert error event: {}", e))?;

            Ok(conn.last_insert_rowid())
        }
    }

    /// Get an error event by ID
    pub fn get_by_id(conn: &Connection, id: i64) -> Result<Option<StoredErrorEvent>, String> {
        conn.query_row(
            r#"
            SELECT e.id, e.log_source_id, e.log_source_name, e.task_run_id, e.workflow_step_id,
                   e.log_timestamp, e.captured_at, e.severity, e.error_type, e.error_code,
                   e.message, e.stack_trace, e.context_lines, e.raw_entry,
                   e.file_path, e.line_number, e.column_number, e.function_name,
                   e.signature_hash, e.occurrence_count, e.first_seen_at, e.last_seen_at,
                   e.status, e.finding_id, e.resolved_by_task_run_id, e.resolution_notes,
                   e.acknowledged_at, e.resolved_at, tr.workflow_name, e.trace_id
            FROM error_events e
            LEFT JOIN task_runs tr ON e.task_run_id = tr.id
            WHERE e.id = ?1
            "#,
            params![id],
            Self::row_to_stored_event,
        )
        .optional()
        .map_err(|e| format!("Failed to get error event: {}", e))
    }

    /// Query error events with filters
    pub fn query(conn: &Connection, query: &ErrorQuery) -> Result<Vec<StoredErrorEvent>, String> {
        let mut sql = String::from(
            r#"
            SELECT e.id, e.log_source_id, e.log_source_name, e.task_run_id, e.workflow_step_id,
                   e.log_timestamp, e.captured_at, e.severity, e.error_type, e.error_code,
                   e.message, e.stack_trace, e.context_lines, e.raw_entry,
                   e.file_path, e.line_number, e.column_number, e.function_name,
                   e.signature_hash, e.occurrence_count, e.first_seen_at, e.last_seen_at,
                   e.status, e.finding_id, e.resolved_by_task_run_id, e.resolution_notes,
                   e.acknowledged_at, e.resolved_at, tr.workflow_name, e.trace_id
            FROM error_events e
            LEFT JOIN task_runs tr ON e.task_run_id = tr.id
            WHERE 1=1
            "#,
        );

        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(ref task_run_id) = query.task_run_id {
            sql.push_str(&format!(" AND e.task_run_id = ?{}", params_vec.len() + 1));
            params_vec.push(Box::new(task_run_id.clone()));
        }

        if let Some(ref source_name) = query.log_source_name {
            sql.push_str(&format!(
                " AND e.log_source_name = ?{}",
                params_vec.len() + 1
            ));
            params_vec.push(Box::new(source_name.clone()));
        }

        if let Some(ref statuses) = query.status {
            if !statuses.is_empty() {
                let placeholders: Vec<String> = statuses
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", params_vec.len() + i + 1))
                    .collect();
                sql.push_str(&format!(" AND e.status IN ({})", placeholders.join(",")));
                for status in statuses {
                    params_vec.push(Box::new(status.as_str().to_string()));
                }
            }
        }

        if let Some(ref severities) = query.severity {
            if !severities.is_empty() {
                let placeholders: Vec<String> = severities
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", params_vec.len() + i + 1))
                    .collect();
                sql.push_str(&format!(" AND e.severity IN ({})", placeholders.join(",")));
                for severity in severities {
                    params_vec.push(Box::new(severity.as_str().to_string()));
                }
            }
        }

        if let Some(ref error_type) = query.error_type {
            sql.push_str(&format!(" AND e.error_type = ?{}", params_vec.len() + 1));
            params_vec.push(Box::new(error_type.clone()));
        }

        if let Some(ref after) = query.captured_after {
            sql.push_str(&format!(" AND e.captured_at > ?{}", params_vec.len() + 1));
            params_vec.push(Box::new(after.clone()));
        }

        if let Some(ref before) = query.captured_before {
            sql.push_str(&format!(" AND e.captured_at < ?{}", params_vec.len() + 1));
            params_vec.push(Box::new(before.clone()));
        }

        // Order by severity (critical first), then by occurrence count, then by recency
        sql.push_str(
            r#"
            ORDER BY
                CASE e.severity WHEN 'critical' THEN 0 WHEN 'error' THEN 1 ELSE 2 END,
                e.occurrence_count DESC,
                e.last_seen_at DESC
            "#,
        );

        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT {}", limit));
        }

        if let Some(offset) = query.offset {
            sql.push_str(&format!(" OFFSET {}", offset));
        }

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();

        let rows = stmt
            .query_map(params_refs.as_slice(), Self::row_to_stored_event)
            .map_err(|e| format!("Failed to query error events: {}", e))?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
        }
        Ok(events)
    }

    /// Get unresolved errors (for debug agent context)
    pub fn get_unresolved(
        conn: &Connection,
        task_run_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredErrorEvent>, String> {
        let query = ErrorQuery {
            task_run_id: task_run_id.map(|s| s.to_string()),
            status: Some(vec![
                ErrorStatus::New,
                ErrorStatus::Acknowledged,
                ErrorStatus::InProgress,
                ErrorStatus::Promoted,
            ]),
            limit: Some(limit as u32),
            ..Default::default()
        };
        Self::query(conn, &query)
    }

    /// Update error status
    pub fn update_status(
        conn: &Connection,
        id: i64,
        status: ErrorStatus,
        resolution_notes: Option<&str>,
    ) -> Result<(), String> {
        let now = chrono::Utc::now().to_rfc3339();

        let (ack_at, resolved_at) = match status {
            ErrorStatus::Acknowledged => (Some(now.clone()), None),
            ErrorStatus::Resolved | ErrorStatus::WontFix | ErrorStatus::Ignored => {
                (None, Some(now.clone()))
            }
            _ => (None, None),
        };

        conn.execute(
            r#"
            UPDATE error_events SET
                status = ?1,
                resolution_notes = COALESCE(?2, resolution_notes),
                acknowledged_at = COALESCE(?3, acknowledged_at),
                resolved_at = COALESCE(?4, resolved_at)
            WHERE id = ?5
            "#,
            params![status.as_str(), resolution_notes, ack_at, resolved_at, id],
        )
        .map_err(|e| format!("Failed to update error status: {}", e))?;

        Ok(())
    }

    /// Link an error event to a finding
    pub fn link_to_finding(
        conn: &Connection,
        error_id: i64,
        finding_id: i64,
    ) -> Result<(), String> {
        conn.execute(
            r#"
            UPDATE error_events SET
                finding_id = ?1,
                status = 'promoted'
            WHERE id = ?2
            "#,
            params![finding_id, error_id],
        )
        .map_err(|e| format!("Failed to link error to finding: {}", e))?;

        Ok(())
    }

    /// Mark an error as resolved by a task run
    pub fn mark_resolved_by_task(
        conn: &Connection,
        error_id: i64,
        task_run_id: &str,
        resolution_notes: Option<&str>,
    ) -> Result<(), String> {
        conn.execute(
            r#"
            UPDATE error_events SET
                status = 'resolved',
                resolved_by_task_run_id = ?1,
                resolution_notes = ?2,
                resolved_at = datetime('now')
            WHERE id = ?3
            "#,
            params![task_run_id, resolution_notes, error_id],
        )
        .map_err(|e| format!("Failed to mark error resolved: {}", e))?;

        Ok(())
    }

    /// Auto-resolve any unresolved SPEC verification error events.
    ///
    /// SPEC events (action_failed with "SPEC: " prefix) are verification test results,
    /// not application errors. The JSONL preprocessor now filters them out, but older
    /// events may still exist in the database from before the filter was added.
    /// This cleans them up on startup.
    pub fn auto_resolve_spec_events(conn: &Connection) -> Result<usize, String> {
        let count = conn
            .execute(
                r#"
                UPDATE error_events SET
                    status = 'resolved',
                    resolution_notes = 'Auto-resolved: SPEC verification results are not application errors',
                    resolved_at = datetime('now')
                WHERE status IN ('new', 'acknowledged', 'in_progress')
                  AND message LIKE '%SPEC: %'
                  AND message LIKE 'action_failed%'
                "#,
                [],
            )
            .map_err(|e| format!("Failed to auto-resolve spec events: {}", e))?;

        Ok(count)
    }

    /// Bulk-resolve all unresolved errors scoped to a specific task run.
    ///
    /// This is called when a workflow completes successfully to clean up errors
    /// that were captured during its execution. Errors with status 'promoted'
    /// (linked to findings) are excluded — they're resolved through that system.
    ///
    /// Returns the count of resolved errors.
    pub fn resolve_errors_by_task_run(
        conn: &Connection,
        task_run_id: &str,
        resolved_by_task_run_id: &str,
    ) -> Result<usize, String> {
        let count = conn
            .execute(
                r#"
                UPDATE error_events SET
                    status = 'resolved',
                    resolved_by_task_run_id = ?1,
                    resolution_notes = 'Auto-resolved: workflow completed successfully',
                    resolved_at = datetime('now')
                WHERE task_run_id = ?2
                  AND status IN ('new', 'acknowledged', 'in_progress')
                "#,
                params![resolved_by_task_run_id, task_run_id],
            )
            .map_err(|e| format!("Failed to bulk-resolve errors for task run: {}", e))?;

        Ok(count)
    }

    /// Get error summary statistics
    pub fn get_summary(
        conn: &Connection,
        task_run_id: Option<&str>,
    ) -> Result<ErrorSummary, String> {
        let where_clause = task_run_id.map(|_| "WHERE task_run_id = ?1").unwrap_or("");

        let sql = format!(
            r#"
            SELECT
                COUNT(*) as total,
                COUNT(*) FILTER (WHERE status = 'new') as new_count,
                COUNT(*) FILTER (WHERE status IN ('new', 'acknowledged', 'in_progress', 'promoted')) as unresolved_count,
                COUNT(*) FILTER (WHERE severity = 'critical' AND status IN ('new', 'acknowledged', 'in_progress', 'promoted')) as critical_count,
                COUNT(*) FILTER (WHERE severity = 'error' AND status IN ('new', 'acknowledged', 'in_progress', 'promoted')) as error_count,
                COUNT(*) FILTER (WHERE severity = 'warning' AND status IN ('new', 'acknowledged', 'in_progress', 'promoted')) as warning_count
            FROM error_events {}
            "#,
            where_clause
        );

        let (total, new_count, unresolved_count, critical_count, error_count, warning_count): (
            u32,
            u32,
            u32,
            u32,
            u32,
            u32,
        ) = if let Some(tid) = task_run_id {
            conn.query_row(&sql, params![tid], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
        } else {
            conn.query_row(&sql, [], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
        }
        .map_err(|e| format!("Failed to get error summary: {}", e))?;

        // Get breakdown by source
        let by_source = Self::get_count_by_column(conn, "log_source_name", task_run_id)?;
        let by_error_type = Self::get_count_by_column(conn, "error_type", task_run_id)?;
        let by_status = Self::get_count_by_column(conn, "status", task_run_id)?;

        Ok(ErrorSummary {
            total,
            new_count,
            unresolved_count,
            critical_count,
            error_count,
            warning_count,
            by_source,
            by_error_type,
            by_status,
            has_actionable_errors: critical_count > 0 || error_count > 0,
        })
    }

    /// Get count breakdown by a column
    fn get_count_by_column(
        conn: &Connection,
        column: &str,
        task_run_id: Option<&str>,
    ) -> Result<HashMap<String, u32>, String> {
        let where_clause = task_run_id
            .map(|_| format!("WHERE task_run_id = ?1 AND {} IS NOT NULL", column))
            .unwrap_or_else(|| format!("WHERE {} IS NOT NULL", column));

        let sql = format!(
            "SELECT {}, COUNT(*) FROM error_events {} GROUP BY {}",
            column, where_clause, column
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        // Use a single closure and handle both cases to avoid type mismatch
        let extract_row = |row: &rusqlite::Row| -> rusqlite::Result<(String, u32)> {
            Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
        };

        let mut map = HashMap::new();

        if let Some(tid) = task_run_id {
            let rows = stmt
                .query_map(params![tid], extract_row)
                .map_err(|e| format!("Failed to query counts: {}", e))?;
            for row in rows {
                let (key, count) = row.map_err(|e| format!("Failed to read row: {}", e))?;
                map.insert(key, count);
            }
        } else {
            let rows = stmt
                .query_map([], extract_row)
                .map_err(|e| format!("Failed to query counts: {}", e))?;
            for row in rows {
                let (key, count) = row.map_err(|e| format!("Failed to read row: {}", e))?;
                map.insert(key, count);
            }
        }

        Ok(map)
    }

    /// Search errors by message content (FTS)
    pub fn search(
        conn: &Connection,
        query: &str,
        limit: usize,
    ) -> Result<Vec<StoredErrorEvent>, String> {
        let mut stmt = conn
            .prepare(
                r#"
                SELECT e.id, e.log_source_id, e.log_source_name, e.task_run_id, e.workflow_step_id,
                       e.log_timestamp, e.captured_at, e.severity, e.error_type, e.error_code,
                       e.message, e.stack_trace, e.context_lines, e.raw_entry,
                       e.file_path, e.line_number, e.column_number, e.function_name,
                       e.signature_hash, e.occurrence_count, e.first_seen_at, e.last_seen_at,
                       e.status, e.finding_id, e.resolved_by_task_run_id, e.resolution_notes,
                       e.acknowledged_at, e.resolved_at, tr.workflow_name, e.trace_id
                FROM error_events e
                LEFT JOIN task_runs tr ON e.task_run_id = tr.id
                JOIN error_events_fts fts ON e.id = fts.rowid
                WHERE error_events_fts MATCH ?1
                ORDER BY rank
                LIMIT ?2
                "#,
            )
            .map_err(|e| format!("Failed to prepare FTS statement: {}", e))?;

        let rows = stmt
            .query_map(params![query, limit as i64], Self::row_to_stored_event)
            .map_err(|e| format!("Failed to search errors: {}", e))?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
        }
        Ok(events)
    }

    /// Convert a database row to StoredErrorEvent
    fn row_to_stored_event(row: &rusqlite::Row) -> rusqlite::Result<StoredErrorEvent> {
        let file_path: Option<String> = row.get(14)?;
        let line_number: Option<u32> = row.get(15)?;
        let column_number: Option<u32> = row.get(16)?;
        let function_name: Option<String> = row.get(17)?;

        let location = if file_path.is_some() {
            Some(ErrorLocation {
                file_path: file_path.unwrap_or_default(),
                line_number,
                column_number,
                function_name,
            })
        } else {
            None
        };

        Ok(StoredErrorEvent {
            id: row.get(0)?,
            log_source_id: row.get(1)?,
            log_source_name: row.get(2)?,
            task_run_id: row.get(3)?,
            workflow_name: row.get(28)?,
            workflow_step_id: row.get(4)?,
            log_timestamp: row.get(5)?,
            captured_at: row.get(6)?,
            severity: ErrorSeverity::from_str(&row.get::<_, String>(7)?)
                .unwrap_or(ErrorSeverity::Error),
            error_type: row.get(8)?,
            error_code: row.get(9)?,
            message: row.get(10)?,
            stack_trace: row.get(11)?,
            context_lines: row.get(12)?,
            raw_entry: row.get(13)?,
            location,
            signature_hash: row.get(18)?,
            occurrence_count: row.get(19)?,
            first_seen_at: row.get(20)?,
            last_seen_at: row.get(21)?,
            status: ErrorStatus::from_str(&row.get::<_, String>(22)?).unwrap_or(ErrorStatus::New),
            finding_id: row.get(23)?,
            resolved_by_task_run_id: row.get(24)?,
            resolution_notes: row.get(25)?,
            trace_id: row.get(29)?,
            acknowledged_at: row.get(26)?,
            resolved_at: row.get(27)?,
        })
    }
}
