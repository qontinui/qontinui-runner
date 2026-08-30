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

/// Coerce a parser-scraped timestamp into something Postgres will accept as
/// `timestamptz`, or `None`.
///
/// Accepted, in order: RFC3339 (`2026-08-25T12:00:00Z`, `...+02:00`), then a
/// naive `YYYY-MM-DD[ T]HH:MM:SS[.fff]`, which is emitted as RFC3339 in UTC.
/// `YYYY/MM/DD` separators (which `TIMESTAMP_COMMON` also matches) are
/// normalized to dashes first rather than left to Postgres' DateStyle, which
/// would read them MDY on some configurations and silently store the wrong day.
fn normalize_log_timestamp(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(dt.to_rfc3339());
    }
    // Only the DATE half may use slashes; a time is always `HH:MM:SS`.
    let dashed = match raw.split_once(['T', ' ']) {
        Some((date, time)) => format!("{}T{}", date.replace('/', "-"), time),
        None => raw.replace('/', "-"),
    };
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(&dashed, fmt) {
            return Some(naive.and_utc().to_rfc3339());
        }
    }
    None
}

impl PgDb {
    /// Persist one collected error, deduplicating on its signature hash.
    ///
    /// # Why this exists
    ///
    /// Before this, **nothing in the runner ever inserted a row into
    /// `error_events`**. The whole module was SELECT/UPDATE only: the pipeline
    /// parsed errors, deduplicated them, then dropped them on the floor, and
    /// its one exporter sent an EMPTY `NewErrors(Vec::new())` "presence" signal
    /// whose stated contract was that subscribers would re-fetch the records
    /// from the PG-backed query API. That contract had no other half. The store
    /// was therefore empty machine-wide no matter how log sources were
    /// configured, and `query_error_events` read a table this application never
    /// populated.
    ///
    /// # Dedup semantics
    ///
    /// `signature_hash` has a plain (non-unique) btree index, so `ON CONFLICT`
    /// is not available. A repeat of a known signature therefore bumps
    /// `occurrence_count`, advances `last_seen_at`, and promotes `new` to
    /// `recurring` — which is what `occurrence_count` and the `recurring`
    /// status exist for. A signature whose only rows are already `resolved` or
    /// `ignored` is NOT revived: that error came back after being closed, so it
    /// gets a fresh row rather than silently re-opening a resolution.
    ///
    /// Returns `(id, is_new)`.
    ///
    /// Timestamps are bound as TEXT with explicit `::TIMESTAMPTZ` casts — the
    /// runner's tokio-postgres has no `with-chrono-0_4` feature (see the module
    /// header).
    pub async fn upsert_error_events(
        &self,
        events: &[crate::error_monitor::types::ErrorEvent],
        task_run_id: Option<&str>,
    ) -> Result<(usize, usize, Option<String>), String> {
        if events.is_empty() {
            return Ok((0, 0, None));
        }
        // ONE checkout for the whole batch. Taking it per record put a pool
        // checkout and up to two round trips on every parsed error, inside the
        // error monitor's poll loop — a burst of log noise cost 2N queries
        // against the shared database instead of 2 per error at worst.
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let mut inserted = 0usize;
        let mut bumped = 0usize;
        let mut first_error: Option<String> = None;
        for event in events {
            // One bad record must not abort the batch; the FIRST failure is
            // retained so the caller can log a real diagnosis, not a count.
            match Self::upsert_one(&conn, event, task_run_id, None).await {
                Ok(true) => inserted += 1,
                Ok(false) => bumped += 1,
                Err(e) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        }
        Ok((inserted, bumped, first_error))
    }

    /// One upsert on an already-checked-out connection. Returns `true` when a
    /// new row was inserted, `false` when an existing one was bumped.
    async fn upsert_one(
        conn: &deadpool_postgres::Client,
        event: &crate::error_monitor::types::ErrorEvent,
        task_run_id: Option<&str>,
        workflow_step_id: Option<&str>,
    ) -> Result<bool, String> {
        let signature_hash = event.compute_signature_hash();
        let severity = event.severity.as_str();
        // `log_timestamp` is whatever the parser scraped off a log line
        // (`TIMESTAMP_ISO` / `TIMESTAMP_COMMON`), NOT a validated timestamp.
        // Binding it straight into `$n::TIMESTAMPTZ` lets one unparseable
        // string abort the INSERT for that record -- and losing a collected
        // error in silence is the exact defect this whole change closes. So
        // normalize here: a value Postgres will accept, or NULL (the column is
        // nullable, and `captured_at` still records when the runner saw it;
        // the original text survives verbatim in `raw_entry`).
        //
        // It is bound through `$4::TEXT::TIMESTAMPTZ`, not `$4::TIMESTAMPTZ`.
        // With the single cast Postgres infers the PARAMETER itself as
        // `timestamptz`, and tokio-postgres then refuses to serialize a Rust
        // `String` into it -- the insert failed with
        // `error serializing parameter 3` and every collected error was lost.
        // (Caught by live verification against a running runner, not by any
        // unit test: the type check happens at Parse time inside Postgres.)
        // The extra `::TEXT` pins the parameter's inferred type to `text`, which
        // is what the module header means by "all timestamps are cast to TEXT" --
        // this crate's tokio-postgres has no `with-chrono-0_4` feature.
        let log_timestamp = normalize_log_timestamp(event.log_timestamp.as_deref());

        // Bump an existing OPEN row for this signature. `FOR UPDATE` is not used:
        // a lost race just means two rows for one signature, which reads
        // correctly, whereas holding row locks across the monitor's ingest loop
        // would stall it behind the UI's own reads.
        let bumped = conn
            .query_opt(
                r#"
                UPDATE error_events
                   SET occurrence_count = COALESCE(occurrence_count, 0) + 1,
                       last_seen_at     = now(),
                       status           = CASE WHEN status = 'new' THEN 'recurring' ELSE status END,
                       -- FIRST writer wins. `COALESCE($3, task_run_id)` (the
                       -- other way round) re-attributed an error first seen in
                       -- run A to whatever run happened to be in flight when it
                       -- recurred, so `resolve_errors_by_task_run(A)` would
                       -- never resolve it and a read filtered by A lost it.
                       -- `first_seen_at` semantics say the original run owns it.
                       task_run_id      = COALESCE(task_run_id, $3)
                 WHERE id = (
                     SELECT id FROM error_events
                      WHERE signature_hash = $1
                        AND log_source_name = $2
                        AND status IN ('new', 'recurring', 'acknowledged', 'in_progress', 'promoted')
                      ORDER BY last_seen_at DESC
                      LIMIT 1
                 )
             RETURNING id
                "#,
                &[&signature_hash, &event.log_source_name, &task_run_id],
            )
            .await
            .map_err(|e| format!("PG upsert_error_event (bump): {}", e))?;

        if bumped.is_some() {
            return Ok(false);
        }

        let (file_path, line_number, column_number, function_name) = match event.location {
            Some(ref loc) => (
                Some(loc.file_path.clone()),
                loc.line_number.map(|n| n as i32),
                loc.column_number.map(|n| n as i32),
                loc.function_name.clone(),
            ),
            None => (None, None, None, None),
        };

        // `log_source_id` is resolved by name rather than passed in: the caller
        // (the pipeline) only ever knows the source's NAME, and a stale id would
        // point the row at the wrong source after a log-source re-sync. A miss
        // leaves it NULL, which the column allows -- `log_source_name` is the
        // NOT NULL half and the one every read projects.
        let row = conn
            .query_one(
                r#"
                INSERT INTO error_events (
                    log_source_id, log_source_name, task_run_id, workflow_step_id,
                    log_timestamp, captured_at, severity, error_type, error_code,
                    message, stack_trace, context_lines, raw_entry,
                    file_path, line_number, column_number, function_name,
                    signature_hash, occurrence_count, first_seen_at, last_seen_at,
                    status, trace_id
                ) VALUES (
                    (SELECT id FROM log_sources WHERE name = $1 LIMIT 1), $1,
                    -- Resolved through `task_runs`, not bound raw: the column
                    -- carries `FOREIGN KEY -> project.task_runs.id`, and an id
                    -- with no matching row hard-fails the INSERT. The exporter
                    -- only warns, so that would drop the whole batch of
                    -- collected errors rather than storing them unattributed.
                    (SELECT id FROM task_runs WHERE id = $2), $3,
                    $4::TEXT::TIMESTAMPTZ, now(), $5, $6, $7,
                    $8, $9, $10, $11,
                    $12, $13, $14, $15,
                    $16, 1, now(), now(),
                    'new', $17
                )
                RETURNING id
                "#,
                &[
                    &event.log_source_name,
                    &task_run_id,
                    &workflow_step_id,
                    &log_timestamp,
                    &severity,
                    &event.error_type,
                    &event.error_code,
                    &event.message,
                    &event.stack_trace,
                    &event.context_lines,
                    &event.raw_entry,
                    &file_path,
                    &line_number,
                    &column_number,
                    &function_name,
                    &signature_hash,
                    &event.trace_id,
                ],
            )
            .await
            .map_err(|e| format!("PG upsert_error_event (insert): {}", e))?;

        let _id: i64 = row.get(0);
        Ok(true)
    }

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

        // SQL casts every count to ::INTEGER (pg i32), so decode as i32 —
        // reading them as i64 panics with "error deserializing column 0" at
        // runtime. Widen to i64 in Rust for the downstream JSON payload.
        let total: i64 = counts_row.get::<_, i32>(0) as i64;
        let new_count: i64 = counts_row.get::<_, i32>(1) as i64;
        let unresolved_count: i64 = counts_row.get::<_, i32>(2) as i64;
        let critical_count: i64 = counts_row.get::<_, i32>(3) as i64;
        let error_count: i64 = counts_row.get::<_, i32>(4) as i64;
        let warning_count: i64 = counts_row.get::<_, i32>(5) as i64;

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
                acknowledged_at = COALESCE($3::TEXT::TIMESTAMPTZ, acknowledged_at),
                resolved_at = COALESCE($4::TEXT::TIMESTAMPTZ, resolved_at)
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
        // NULLABLE in the schema (`occurrence_count INTEGER NULL DEFAULT 1`),
        // and `Row::get::<i32>` PANICS on a NULL rather than returning an Err —
        // so a NULL here aborts the task instead of being skipped by the
        // `filter_map(|v| ... .ok())` the callers rely on.
        let occurrence_count: i32 = row.get::<_, Option<i32>>(19).unwrap_or(1);
        let first_seen_at: String = row.get(20);
        let last_seen_at: String = row.get(21);
        // Nullable too (`status TEXT NULL DEFAULT 'new'`) — same panic.
        let status: String = row
            .get::<_, Option<String>>(22)
            .unwrap_or_else(|| "new".to_string());
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
            conditions.push(format!(
                "e.captured_at >= ${}::TEXT::TIMESTAMPTZ",
                param_idx
            ));
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
                    acknowledged_at = $1::TEXT::TIMESTAMPTZ
                WHERE status IN ('new', 'recurring') AND task_run_id = $2
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
                    acknowledged_at = $1::TEXT::TIMESTAMPTZ
                WHERE status IN ('new', 'recurring')
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

#[cfg(test)]
mod normalize_log_timestamp_tests {
    use super::normalize_log_timestamp;

    #[test]
    fn rfc3339_passes_through() {
        assert!(normalize_log_timestamp(Some("2026-08-25T12:00:00Z"))
            .unwrap()
            .starts_with("2026-08-25T12:00:00"));
    }

    #[test]
    fn a_naive_log_timestamp_becomes_utc_rfc3339() {
        let got = normalize_log_timestamp(Some("2026-08-25 12:00:00.123")).unwrap();
        assert!(got.starts_with("2026-08-25T12:00:00.123"), "{got}");
        assert!(got.ends_with("+00:00"), "{got}");
    }

    #[test]
    fn slash_dates_are_normalized_rather_than_left_to_postgres_datestyle() {
        // `2026/08/25` read MDY would be a different day, stored silently.
        let got = normalize_log_timestamp(Some("2026/08/25 12:00:00")).unwrap();
        assert!(got.starts_with("2026-08-25T12:00:00"), "{got}");
    }

    /// The load-bearing arm: garbage must become NULL, never an INSERT that
    /// fails and drops the whole collected error.
    #[test]
    fn an_unparseable_timestamp_becomes_none() {
        assert_eq!(normalize_log_timestamp(Some("not a timestamp")), None);
        assert_eq!(normalize_log_timestamp(Some("   ")), None);
        assert_eq!(normalize_log_timestamp(None), None);
    }
}

/// PG-backed regression tests for the TIMESTAMPTZ **parameter-inference**
/// defect that made the whole operator-action surface fail on every row.
///
/// ## What broke
///
/// A **bare** `$n::TIMESTAMPTZ` in a statement literal does not cast a text
/// parameter server-side — Postgres absorbs the cast into parameter typing and
/// infers `$n` ITSELF as `timestamp with time zone`. The Rust callers bind
/// `String` / `Option<String>`, and `tokio_postgres`'s `ToSql for String` does
/// not accept that OID, so every such call died before it reached the server
/// with `error serializing parameter N`. `acknowledge_error`, `resolve_error`,
/// `ignore_error`, `update_error_status` and `acknowledge_all_errors` all route
/// through the two statements below, so all five failed 100% of the time while
/// the UI optimistically rendered success and then reverted.
///
/// `upsert_one` already had the correct form (`$4::TEXT::TIMESTAMPTZ`, added
/// when the same bug was fixed at that ONE site) — the fix is to force the
/// parameter to `text` and let the server do the cast.
///
/// ## Why these tests must hit a real server
///
/// The failure lives in Postgres's parse analysis and in the client's BIND, so
/// a test with no server proves nothing at all. Verified live:
///
/// ```text
/// PREPARE p_before AS UPDATE error_events SET acknowledged_at = COALESCE($3::TIMESTAMPTZ, ...)
///   -> {text,text,"timestamp with time zone","timestamp with time zone",bigint}
/// PREPARE p_after  AS UPDATE error_events SET acknowledged_at = COALESCE($3::TEXT::TIMESTAMPTZ, ...)
///   -> {text,text,text,text,bigint}
/// ```
///
/// Point `DATABASE_URL` at an **isolated scratch cluster** (never the
/// machine-shared one) and run with `--ignored`.
#[cfg(test)]
mod timestamptz_parameter_binding_pg_tests {
    use super::PgDb;
    use crate::error_monitor::types::{ErrorEvent, ErrorSeverity, ErrorStatus};

    fn db_url() -> String {
        std::env::var("DATABASE_URL")
            .expect("DATABASE_URL must point at an ISOLATED scratch cluster")
    }

    /// One runtime per test, owning the pool it builds. `new_blocking_for_test`
    /// makes and drops its own runtime, which would leave the connection tasks
    /// orphaned for the rest of the test.
    fn run<F, T>(f: F) -> T
    where
        F: for<'a> FnOnce(
            &'a std::sync::Arc<PgDb>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>,
    {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime for test");
        rt.block_on(async {
            let db =
                std::sync::Arc::new(PgDb::new(&db_url()).await.expect("connect to scratch PG"));
            f(&db).await
        })
    }

    fn unique(tag: &str) -> String {
        format!(
            "tstz-{tag}-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        )
    }

    fn event(source: &str, message: &str) -> ErrorEvent {
        ErrorEvent {
            log_source_name: source.to_string(),
            severity: ErrorSeverity::Error,
            error_type: Some("TstzProbe".to_string()),
            error_code: None,
            message: message.to_string(),
            stack_trace: None,
            location: None,
            context_lines: None,
            raw_entry: message.to_string(),
            // Deliberately populated: this is the `$4::TEXT::TIMESTAMPTZ` bind
            // in `upsert_one`, the site that already had the correct form.
            log_timestamp: Some("2026-08-25T12:00:00Z".to_string()),
            trace_id: None,
        }
    }

    /// Seed one real `error_events` row through the production ingest path and
    /// return its id.
    async fn seed(db: &PgDb, source: &str, task_run_id: Option<&str>) -> i64 {
        if let Some(tid) = task_run_id {
            let conn = db.pool().get().await.expect("pool");
            conn.execute(
                "INSERT INTO task_runs (id, workflow_name) VALUES ($1, 'tstz-probe') \
                 ON CONFLICT (id) DO NOTHING",
                &[&tid],
            )
            .await
            .expect("seed task_run");
        }

        let ev = event(source, &format!("{source} boom"));
        let (inserted, _bumped, err) = db
            .upsert_error_events(std::slice::from_ref(&ev), task_run_id)
            .await
            .expect("upsert_error_events");
        assert_eq!(inserted, 1, "seed must insert exactly one row: {err:?}");
        assert!(err.is_none(), "seed reported a per-record error: {err:?}");

        let conn = db.pool().get().await.expect("pool");
        let row = conn
            .query_one(
                "SELECT id FROM error_events WHERE log_source_name = $1",
                &[&source],
            )
            .await
            .expect("read back seeded id");
        row.get(0)
    }

    /// Refetch through the read path the page uses — NOT the write's return
    /// value. The whole symptom was an optimistic UI masking a failed write, so
    /// only a re-read proves persistence.
    async fn refetch(db: &PgDb, id: i64) -> serde_json::Value {
        db.get_error_event_by_id(id)
            .await
            .expect("get_error_event_by_id")
            .expect("row must still exist")
    }

    #[test]
    #[ignore = "requires an ISOLATED PG via DATABASE_URL"]
    fn acknowledge_error_persists_across_a_refetch() {
        run(|db| {
            Box::pin(async move {
                let source = unique("ack");
                let id = seed(db, &source, None).await;

                // The exact call `acknowledge_error` makes.
                db.update_error_status(id, "acknowledged", None)
                    .await
                    .expect("acknowledge_error must not fail");

                let row = refetch(db, id).await;
                assert_eq!(row["status"], "acknowledged");
                assert!(
                    row["acknowledgedAt"].is_string(),
                    "acknowledged_at must be written, got {row}"
                );
                assert!(row["resolvedAt"].is_null(), "resolved_at must stay NULL");
            })
        });
    }

    #[test]
    #[ignore = "requires an ISOLATED PG via DATABASE_URL"]
    fn resolve_error_persists_across_a_refetch() {
        run(|db| {
            Box::pin(async move {
                let source = unique("resolve");
                let id = seed(db, &source, None).await;

                db.update_error_status(id, "resolved", Some("fixed by the tstz repair"))
                    .await
                    .expect("resolve_error must not fail");

                let row = refetch(db, id).await;
                assert_eq!(row["status"], "resolved");
                assert!(
                    row["resolvedAt"].is_string(),
                    "resolved_at must be written, got {row}"
                );
                assert_eq!(row["resolutionNotes"], "fixed by the tstz repair");
            })
        });
    }

    #[test]
    #[ignore = "requires an ISOLATED PG via DATABASE_URL"]
    fn ignore_error_persists_across_a_refetch() {
        run(|db| {
            Box::pin(async move {
                let source = unique("ignore");
                let id = seed(db, &source, None).await;

                db.update_error_status(id, "ignored", Some("known noise"))
                    .await
                    .expect("ignore_error must not fail");

                let row = refetch(db, id).await;
                assert_eq!(row["status"], "ignored");
                // `ignored` takes the resolved arm of the status match.
                assert!(row["resolvedAt"].is_string(), "got {row}");
                assert_eq!(row["resolutionNotes"], "known noise");
            })
        });
    }

    #[test]
    #[ignore = "requires an ISOLATED PG via DATABASE_URL"]
    fn update_error_status_persists_across_a_refetch() {
        run(|db| {
            Box::pin(async move {
                let source = unique("status");
                let id = seed(db, &source, None).await;

                db.update_error_status(id, "wont_fix", None)
                    .await
                    .expect("update_error_status must not fail");

                let row = refetch(db, id).await;
                assert_eq!(row["status"], "wont_fix");
                assert!(row["resolvedAt"].is_string(), "got {row}");
            })
        });
    }

    #[test]
    #[ignore = "requires an ISOLATED PG via DATABASE_URL"]
    fn acknowledge_all_errors_persists_across_a_refetch() {
        run(|db| {
            Box::pin(async move {
                let tid = unique("run");
                let a = seed(db, &unique("ackall-a"), Some(&tid)).await;
                let b = seed(db, &unique("ackall-b"), Some(&tid)).await;

                // Scoped arm.
                let count = db
                    .acknowledge_all_errors(Some(&tid))
                    .await
                    .expect("acknowledge_all_errors(scoped) must not fail");
                assert_eq!(count, 2, "both open rows for {tid} must be acknowledged");

                for id in [a, b] {
                    let row = refetch(db, id).await;
                    assert_eq!(row["status"], "acknowledged", "id {id}: {row}");
                    assert!(row["acknowledgedAt"].is_string(), "id {id}: {row}");
                }

                // Unscoped arm — a distinct statement, so it needs its own row.
                let c = seed(db, &unique("ackall-c"), None).await;
                let count = db
                    .acknowledge_all_errors(None)
                    .await
                    .expect("acknowledge_all_errors(unscoped) must not fail");
                assert!(count >= 1, "unscoped sweep must touch the fresh row");

                let row = refetch(db, c).await;
                assert_eq!(row["status"], "acknowledged", "{row}");
                assert!(row["acknowledgedAt"].is_string(), "{row}");
            })
        });
    }

    /// The dynamic sibling of the same defect: `query_error_events` builds
    /// `captured_at >= ${n}::TIMESTAMPTZ` with `format!` and pushes a `String`.
    #[test]
    #[ignore = "requires an ISOLATED PG via DATABASE_URL"]
    fn captured_after_filter_binds_a_string() {
        run(|db| {
            Box::pin(async move {
                let source = unique("after");
                let id = seed(db, &source, None).await;

                let rows = db
                    .query_error_events(
                        None,
                        None,
                        None,
                        Some(&source),
                        Some("2000-01-01T00:00:00Z"),
                        Some(50),
                    )
                    .await
                    .expect("captured_after filter must not fail");

                assert!(
                    rows.iter().any(|r| r["id"] == id),
                    "seeded row must survive an inclusive captured_after filter"
                );

                let none = db
                    .query_error_events(
                        None,
                        None,
                        None,
                        Some(&source),
                        Some("2999-01-01T00:00:00Z"),
                        Some(50),
                    )
                    .await
                    .expect("captured_after filter must not fail");
                assert!(
                    none.is_empty(),
                    "a future captured_after must exclude it — the bound value \
                     has to actually reach the comparison"
                );
            })
        });
    }

    /// Negative control: the repair must not turn into "accept everything".
    /// Rejection lives in `error_monitor::commands::update_error_status`, which
    /// gates on `ErrorStatus::from_str` before touching the database — so the
    /// gate is asserted here, together with the row being left untouched.
    #[test]
    #[ignore = "requires an ISOLATED PG via DATABASE_URL"]
    fn an_invalid_status_is_still_rejected_before_the_write() {
        run(|db| {
            Box::pin(async move {
                let source = unique("negctl");
                let id = seed(db, &source, None).await;

                assert!(
                    ErrorStatus::from_str("definitely_not_a_status").is_none(),
                    "the command's gate must still reject an unknown status"
                );
                for good in ["acknowledged", "resolved", "ignored", "wont_fix"] {
                    assert!(
                        ErrorStatus::from_str(good).is_some(),
                        "{good} must remain accepted"
                    );
                }

                // The gate short-circuits, so the row is untouched.
                let row = refetch(db, id).await;
                assert_eq!(row["status"], "new");
                assert!(row["acknowledgedAt"].is_null(), "{row}");
                assert!(row["resolvedAt"].is_null(), "{row}");
            })
        });
    }
}
