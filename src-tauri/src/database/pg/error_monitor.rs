//! PostgreSQL error_monitor operations.
//!
//! Provides PG-backed implementations of the error monitor queries used by MCP handlers.
//! Returns serde_json::Value since StoredErrorEvent lives in the error_monitor module.
//!
//! Note: tokio-postgres in the runner crate does NOT have the `with-chrono-0_4` feature,
//! so all timestamps are cast to TEXT in SQL and extracted as String.

use super::PgDb;
use serde_json::json;
use std::collections::HashMap;

impl PgDb {
    /// Get unresolved errors, optionally filtered by task_run_id.
    ///
    /// Returns errors with status IN ('new', 'recurring', 'acknowledged', 'in_progress', 'promoted'),
    /// ordered by severity priority, occurrence_count DESC, last_seen_at DESC.
    pub async fn get_unresolved_errors(
        &self,
        task_run_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let limit_i64 = limit as i64;

        let rows = if let Some(tid) = task_run_id {
            conn.query(
                r#"
                SELECT e.id, e.log_source_id, e.log_source_name, e.task_run_id, e.workflow_step_id,
                       e.log_timestamp::TEXT, e.captured_at::TEXT, e.severity, e.error_type, e.error_code,
                       e.message, e.stack_trace, e.context_lines, e.raw_entry,
                       e.file_path, e.line_number, e.column_number, e.function_name,
                       e.signature_hash, e.occurrence_count, e.first_seen_at::TEXT, e.last_seen_at::TEXT,
                       e.status, e.finding_id, e.resolved_by_task_run_id, e.resolution_notes,
                       e.acknowledged_at::TEXT, e.resolved_at::TEXT, tr.workflow_name, e.trace_id
                FROM error_events e
                LEFT JOIN task_runs tr ON e.task_run_id = tr.id
                WHERE e.status IN ('new', 'recurring', 'acknowledged', 'in_progress', 'promoted')
                  AND e.task_run_id = $1
                ORDER BY
                    CASE e.severity WHEN 'critical' THEN 0 WHEN 'error' THEN 1 ELSE 2 END,
                    e.occurrence_count DESC,
                    e.last_seen_at DESC
                LIMIT $2
                "#,
                &[&tid, &limit_i64],
            )
            .await
            .map_err(|e| format!("PG get_unresolved_errors: {}", e))?
        } else {
            conn.query(
                r#"
                SELECT e.id, e.log_source_id, e.log_source_name, e.task_run_id, e.workflow_step_id,
                       e.log_timestamp::TEXT, e.captured_at::TEXT, e.severity, e.error_type, e.error_code,
                       e.message, e.stack_trace, e.context_lines, e.raw_entry,
                       e.file_path, e.line_number, e.column_number, e.function_name,
                       e.signature_hash, e.occurrence_count, e.first_seen_at::TEXT, e.last_seen_at::TEXT,
                       e.status, e.finding_id, e.resolved_by_task_run_id, e.resolution_notes,
                       e.acknowledged_at::TEXT, e.resolved_at::TEXT, tr.workflow_name, e.trace_id
                FROM error_events e
                LEFT JOIN task_runs tr ON e.task_run_id = tr.id
                WHERE e.status IN ('new', 'recurring', 'acknowledged', 'in_progress', 'promoted')
                ORDER BY
                    CASE e.severity WHEN 'critical' THEN 0 WHEN 'error' THEN 1 ELSE 2 END,
                    e.occurrence_count DESC,
                    e.last_seen_at DESC
                LIMIT $1
                "#,
                &[&limit_i64],
            )
            .await
            .map_err(|e| format!("PG get_unresolved_errors: {}", e))?
        };

        Ok(rows
            .iter()
            .map(|row| Self::error_row_to_json(row))
            .collect())
    }

    /// Get error summary statistics, optionally filtered by task_run_id.
    pub async fn get_error_summary(
        &self,
        task_run_id: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Main counts
        let counts_row = if let Some(tid) = task_run_id {
            conn.query_one(
                r#"
                SELECT
                    COUNT(*)::INTEGER as total,
                    COUNT(*) FILTER (WHERE status = 'new')::INTEGER as new_count,
                    COUNT(*) FILTER (WHERE status IN ('new', 'recurring', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as unresolved_count,
                    COUNT(*) FILTER (WHERE severity = 'critical' AND status IN ('new', 'recurring', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as critical_count,
                    COUNT(*) FILTER (WHERE severity = 'error' AND status IN ('new', 'recurring', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as error_count,
                    COUNT(*) FILTER (WHERE severity = 'warning' AND status IN ('new', 'recurring', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as warning_count
                FROM error_events
                WHERE task_run_id = $1
                "#,
                &[&tid],
            )
            .await
            .map_err(|e| format!("PG get_error_summary: {}", e))?
        } else {
            conn.query_one(
                r#"
                SELECT
                    COUNT(*)::INTEGER as total,
                    COUNT(*) FILTER (WHERE status = 'new')::INTEGER as new_count,
                    COUNT(*) FILTER (WHERE status IN ('new', 'recurring', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as unresolved_count,
                    COUNT(*) FILTER (WHERE severity = 'critical' AND status IN ('new', 'recurring', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as critical_count,
                    COUNT(*) FILTER (WHERE severity = 'error' AND status IN ('new', 'recurring', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as error_count,
                    COUNT(*) FILTER (WHERE severity = 'warning' AND status IN ('new', 'recurring', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as warning_count
                FROM error_events
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_error_summary: {}", e))?
        };

        let total: i64 = counts_row.get(0);
        let new_count: i64 = counts_row.get(1);
        let unresolved_count: i64 = counts_row.get(2);
        let critical_count: i64 = counts_row.get(3);
        let error_count: i64 = counts_row.get(4);
        let warning_count: i64 = counts_row.get(5);

        // Breakdown by source
        let by_source = self
            .error_count_by_column("log_source_name", task_run_id)
            .await?;
        // Breakdown by error type
        let by_error_type = self
            .error_count_by_column("error_type", task_run_id)
            .await?;
        // Breakdown by status
        let by_status = self.error_count_by_column("status", task_run_id).await?;

        Ok(json!({
            "total": total,
            "newCount": new_count,
            "unresolvedCount": unresolved_count,
            "criticalCount": critical_count,
            "errorCount": error_count,
            "warningCount": warning_count,
            "bySource": by_source,
            "byErrorType": by_error_type,
            "byStatus": by_status,
            "hasActionableErrors": critical_count > 0 || error_count > 0,
        }))
    }

    /// Update error status with optional resolution notes and timestamps.
    pub async fn update_error_status(
        &self,
        id: i64,
        status: &str,
        resolution_notes: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let now = chrono::Utc::now().to_rfc3339();

        let (ack_at, resolved_at): (Option<String>, Option<String>) = match status {
            "acknowledged" => (Some(now.clone()), None),
            "resolved" | "wont_fix" | "ignored" => (None, Some(now.clone())),
            _ => (None, None),
        };

        conn.execute(
            r#"
            UPDATE error_events SET
                status = $1,
                resolution_notes = COALESCE($2, resolution_notes),
                acknowledged_at = COALESCE($3::TIMESTAMPTZ, acknowledged_at),
                resolved_at = COALESCE($4::TIMESTAMPTZ, resolved_at)
            WHERE id = $5
            "#,
            &[&status, &resolution_notes, &ack_at, &resolved_at, &id],
        )
        .await
        .map_err(|e| format!("PG update_error_status: {}", e))?;

        Ok(())
    }

    /// Mark an error as resolved by a specific task run.
    pub async fn mark_resolved_by_task(
        &self,
        error_id: i64,
        task_run_id: &str,
        resolution_notes: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"
            UPDATE error_events SET
                status = 'resolved',
                resolved_by_task_run_id = $1,
                resolution_notes = $2,
                resolved_at = NOW()
            WHERE id = $3
            "#,
            &[&task_run_id, &resolution_notes, &error_id],
        )
        .await
        .map_err(|e| format!("PG mark_resolved_by_task: {}", e))?;

        Ok(())
    }

    /// Get error debug context as unresolved error records.
    ///
    /// Returns recent unresolved errors (used when the full curator is not available).
    pub async fn get_error_debug_context(
        &self,
        task_run_id: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        // Reuse get_unresolved_errors with a reasonable limit for debug context
        self.get_unresolved_errors(task_run_id, 50).await
    }

    /// Bulk-resolve all unresolved errors scoped to a specific task run.
    pub async fn resolve_errors_by_task_run(
        &self,
        task_run_id: &str,
        resolved_by_task_run_id: &str,
    ) -> Result<u64, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let count = conn
            .execute(
                r#"
                UPDATE error_events SET
                    status = 'resolved',
                    resolved_by_task_run_id = $1,
                    resolution_notes = 'Auto-resolved: workflow completed successfully',
                    resolved_at = NOW()
                WHERE task_run_id = $2
                  AND status IN ('new', 'recurring', 'acknowledged', 'in_progress')
                "#,
                &[&resolved_by_task_run_id, &task_run_id],
            )
            .await
            .map_err(|e| format!("PG resolve_errors_by_task_run: {}", e))?;

        Ok(count)
    }

    /// Get a single error event by its ID.
    pub async fn get_error_event_by_id(
        &self,
        id: i64,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT e.id, e.log_source_id, e.log_source_name, e.task_run_id, e.workflow_step_id,
                       e.log_timestamp::TEXT, e.captured_at::TEXT, e.severity, e.error_type, e.error_code,
                       e.message, e.stack_trace, e.context_lines, e.raw_entry,
                       e.file_path, e.line_number, e.column_number, e.function_name,
                       e.signature_hash, e.occurrence_count, e.first_seen_at::TEXT, e.last_seen_at::TEXT,
                       e.status, e.finding_id, e.resolved_by_task_run_id, e.resolution_notes,
                       e.acknowledged_at::TEXT, e.resolved_at::TEXT, tr.workflow_name, e.trace_id
                FROM error_events e
                LEFT JOIN task_runs tr ON e.task_run_id = tr.id
                WHERE e.id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_error_event_by_id: {}", e))?;

        Ok(row.map(|r| Self::error_row_to_json(&r)))
    }

    /// Link an error event to a finding.
    pub async fn link_error_to_finding(
        &self,
        error_id: i64,
        finding_id: i64,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let finding_id_str = finding_id.to_string();
        conn.execute(
            "UPDATE error_events SET finding_id = $1, status = 'promoted' WHERE id = $2",
            &[&finding_id_str, &error_id],
        )
        .await
        .map_err(|e| format!("PG link_error_to_finding: {}", e))?;

        Ok(())
    }

    /// Get recurrence history for a given signature_hash.
    ///
    /// Returns past resolved/wont_fix entries so the UI can display how many
    /// times this error pattern has recurred and been resolved.
    pub async fn get_error_recurrence_history(
        &self,
        signature_hash: &str,
    ) -> Result<Vec<crate::error_monitor::commands::RecurrenceEntry>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, status, resolved_at::TEXT, resolution_notes,
                       first_seen_at::TEXT, last_seen_at::TEXT, occurrence_count
                FROM error_events
                WHERE signature_hash = $1 AND status IN ('resolved', 'wont_fix')
                ORDER BY resolved_at DESC NULLS LAST
                LIMIT 10
                "#,
                &[&signature_hash],
            )
            .await
            .map_err(|e| format!("PG get_error_recurrence_history: {}", e))?;

        let entries = rows
            .iter()
            .map(|row| crate::error_monitor::commands::RecurrenceEntry {
                id: row.get(0),
                status: row.get(1),
                resolved_at: row.get(2),
                resolution_notes: row.get(3),
                first_seen_at: row.get(4),
                last_seen_at: row.get(5),
                occurrence_count: row.get(6),
            })
            .collect();

        Ok(entries)
    }

    // ========================================================================
    // Internal helpers
    // ========================================================================

    /// Convert a tokio_postgres::Row to a serde_json::Value matching StoredErrorEvent shape.
    ///
    /// Timestamps are cast to TEXT in the SQL query, so they're extracted as String here.
    fn error_row_to_json(row: &tokio_postgres::Row) -> serde_json::Value {
        let id: i64 = row.get(0);
        let log_source_id: Option<i64> = row.get(1);
        let log_source_name: String = row.get(2);
        let task_run_id: Option<String> = row.get(3);
        let workflow_step_id: Option<String> = row.get(4);
        let log_timestamp: Option<String> = row.get(5);
        let captured_at: String = row.get(6);
        let severity: String = row.get(7);
        let error_type: Option<String> = row.get(8);
        let error_code: Option<String> = row.get(9);
        let message: String = row.get(10);
        let stack_trace: Option<String> = row.get(11);
        let context_lines: Option<String> = row.get(12);
        let raw_entry: Option<String> = row.get(13);
        let file_path: Option<String> = row.get(14);
        let line_number: Option<i32> = row.get(15);
        let column_number: Option<i32> = row.get(16);
        let function_name: Option<String> = row.get(17);
        let signature_hash: String = row.get(18);
        let occurrence_count: i32 = row.get(19);
        let first_seen_at: String = row.get(20);
        let last_seen_at: String = row.get(21);
        let status: String = row.get(22);
        let finding_id_str: Option<String> = row.get(23);
        let finding_id: Option<i64> = finding_id_str.and_then(|s| s.parse().ok());
        let resolved_by_task_run_id: Option<String> = row.get(24);
        let resolution_notes: Option<String> = row.get(25);
        let acknowledged_at: Option<String> = row.get(26);
        let resolved_at: Option<String> = row.get(27);
        let workflow_name: Option<String> = row.get(28);
        let trace_id: Option<String> = row.get(29);

        let location = file_path.map(|fp| {
            json!({
                "filePath": fp,
                "lineNumber": line_number,
                "columnNumber": column_number,
                "functionName": function_name,
            })
        });

        json!({
            "id": id,
            "logSourceId": log_source_id,
            "logSourceName": log_source_name,
            "taskRunId": task_run_id,
            "workflowName": workflow_name,
            "workflowStepId": workflow_step_id,
            "logTimestamp": log_timestamp,
            "capturedAt": captured_at,
            "severity": severity,
            "errorType": error_type,
            "errorCode": error_code,
            "message": message,
            "stackTrace": stack_trace,
            "contextLines": context_lines,
            "rawEntry": raw_entry,
            "location": location,
            "signatureHash": signature_hash,
            "occurrenceCount": occurrence_count,
            "firstSeenAt": first_seen_at,
            "lastSeenAt": last_seen_at,
            "status": status,
            "findingId": finding_id,
            "resolvedByTaskRunId": resolved_by_task_run_id,
            "resolutionNotes": resolution_notes,
            "traceId": trace_id,
            "acknowledgedAt": acknowledged_at,
            "resolvedAt": resolved_at,
        })
    }

    /// Query error events with flexible filters.
    ///
    /// Supports filtering by task_run_id, status list, severity list, source, captured_after, limit.
    /// Returns JSON values matching StoredErrorEvent shape.
    pub async fn query_error_events(
        &self,
        task_run_id: Option<&str>,
        statuses: Option<&[&str]>,
        severities: Option<&[&str]>,
        source: Option<&str>,
        captured_after: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Build dynamic WHERE clauses
        let mut conditions: Vec<String> = Vec::new();
        let mut param_idx = 1u32;
        // We'll use a Vec of dynamic params
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();

        if let Some(tid) = task_run_id {
            conditions.push(format!("e.task_run_id = ${}", param_idx));
            params.push(Box::new(tid.to_string()));
            param_idx += 1;
        }

        if let Some(sts) = statuses {
            if !sts.is_empty() {
                let placeholders: Vec<String> = sts
                    .iter()
                    .enumerate()
                    .map(|(i, _)| {
                        let idx = param_idx + i as u32;
                        format!("${}", idx)
                    })
                    .collect();
                conditions.push(format!("e.status IN ({})", placeholders.join(",")));
                for s in sts {
                    params.push(Box::new(s.to_string()));
                    param_idx += 1;
                }
            }
        }

        if let Some(sevs) = severities {
            if !sevs.is_empty() {
                let placeholders: Vec<String> = sevs
                    .iter()
                    .enumerate()
                    .map(|(i, _)| {
                        let idx = param_idx + i as u32;
                        format!("${}", idx)
                    })
                    .collect();
                conditions.push(format!("e.severity IN ({})", placeholders.join(",")));
                for s in sevs {
                    params.push(Box::new(s.to_string()));
                    param_idx += 1;
                }
            }
        }

        if let Some(src) = source {
            conditions.push(format!("e.log_source_name = ${}", param_idx));
            params.push(Box::new(src.to_string()));
            param_idx += 1;
        }

        if let Some(after) = captured_after {
            conditions.push(format!("e.captured_at >= ${}::TIMESTAMPTZ", param_idx));
            params.push(Box::new(after.to_string()));
            param_idx += 1;
        }

        let limit_val = limit.unwrap_or(100) as i64;
        let limit_param = format!("${}", param_idx);
        params.push(Box::new(limit_val));

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let sql = format!(
            r#"
            SELECT e.id, e.log_source_id, e.log_source_name, e.task_run_id, e.workflow_step_id,
                   e.log_timestamp::TEXT, e.captured_at::TEXT, e.severity, e.error_type, e.error_code,
                   e.message, e.stack_trace, e.context_lines, e.raw_entry,
                   e.file_path, e.line_number, e.column_number, e.function_name,
                   e.signature_hash, e.occurrence_count, e.first_seen_at::TEXT, e.last_seen_at::TEXT,
                   e.status, e.finding_id, e.resolved_by_task_run_id, e.resolution_notes,
                   e.acknowledged_at::TEXT, e.resolved_at::TEXT, tr.workflow_name, e.trace_id
            FROM error_events e
            LEFT JOIN task_runs tr ON e.task_run_id = tr.id
            {}
            ORDER BY
                CASE e.severity WHEN 'critical' THEN 0 WHEN 'error' THEN 1 ELSE 2 END,
                e.occurrence_count DESC,
                e.last_seen_at DESC
            LIMIT {}
            "#,
            where_clause, limit_param
        );

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG query_error_events: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| Self::error_row_to_json(row))
            .collect())
    }

    /// Search error events by message content using ILIKE.
    pub async fn search_errors(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let pattern = format!("%{}%", query);
        let limit_i64 = limit as i64;

        let rows = conn
            .query(
                r#"
                SELECT e.id, e.log_source_id, e.log_source_name, e.task_run_id, e.workflow_step_id,
                       e.log_timestamp::TEXT, e.captured_at::TEXT, e.severity, e.error_type, e.error_code,
                       e.message, e.stack_trace, e.context_lines, e.raw_entry,
                       e.file_path, e.line_number, e.column_number, e.function_name,
                       e.signature_hash, e.occurrence_count, e.first_seen_at::TEXT, e.last_seen_at::TEXT,
                       e.status, e.finding_id, e.resolved_by_task_run_id, e.resolution_notes,
                       e.acknowledged_at::TEXT, e.resolved_at::TEXT, tr.workflow_name, e.trace_id
                FROM error_events e
                LEFT JOIN task_runs tr ON e.task_run_id = tr.id
                WHERE e.message ILIKE $1 OR e.error_type ILIKE $1
                ORDER BY e.last_seen_at DESC
                LIMIT $2
                "#,
                &[&pattern, &limit_i64],
            )
            .await
            .map_err(|e| format!("PG search_errors: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| Self::error_row_to_json(row))
            .collect())
    }

    /// Acknowledge all new errors, optionally scoped to a task_run_id.
    /// Returns the count of acknowledged errors.
    pub async fn acknowledge_all_errors(&self, task_run_id: Option<&str>) -> Result<u32, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let now = chrono::Utc::now().to_rfc3339();

        let count = if let Some(tid) = task_run_id {
            conn.execute(
                r#"
                UPDATE error_events SET
                    status = 'acknowledged',
                    acknowledged_at = $1::TIMESTAMPTZ
                WHERE status = 'new' AND task_run_id = $2
                "#,
                &[&now, &tid],
            )
            .await
            .map_err(|e| format!("PG acknowledge_all_errors: {}", e))?
        } else {
            conn.execute(
                r#"
                UPDATE error_events SET
                    status = 'acknowledged',
                    acknowledged_at = $1::TIMESTAMPTZ
                WHERE status = 'new'
                "#,
                &[&now],
            )
            .await
            .map_err(|e| format!("PG acknowledge_all_errors: {}", e))?
        };

        Ok(count as u32)
    }

    /// Get count breakdown for a given column in error_events.
    async fn error_count_by_column(
        &self,
        column: &str,
        task_run_id: Option<&str>,
    ) -> Result<HashMap<String, u32>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Allowlist column names to prevent SQL injection
        let col = match column {
            "log_source_name" | "error_type" | "status" => column,
            _ => return Err(format!("Invalid column name: {}", column)),
        };

        let rows = if let Some(tid) = task_run_id {
            let sql = format!(
                "SELECT {col}, COUNT(*)::BIGINT FROM error_events WHERE task_run_id = $1 AND {col} IS NOT NULL GROUP BY {col}",
            );
            conn.query(&sql, &[&tid])
                .await
                .map_err(|e| format!("PG error_count_by_column({}): {}", col, e))?
        } else {
            let sql = format!(
                "SELECT {col}, COUNT(*)::BIGINT FROM error_events WHERE {col} IS NOT NULL GROUP BY {col}",
            );
            conn.query(&sql, &[])
                .await
                .map_err(|e| format!("PG error_count_by_column({}): {}", col, e))?
        };

        let mut map = HashMap::new();
        for row in &rows {
            let key: String = row.get(0);
            let count: i64 = row.get(1);
            map.insert(key, count as u32);
        }
        Ok(map)
    }
}
