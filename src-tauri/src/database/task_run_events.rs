//! Task run event operations: hybrid logging, execution spans, API requests, and AWAS steps.
//!
//! Contains all CheckpointDb methods related to task run events and associated data.

use chrono::Utc;
use rusqlite::params;

use super::types::*;
use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Hybrid Logging Operations (Phase 10)
    // ========================================================================

    /// Create a task run event (batch insert for migration from JSONL).
    pub fn create_task_run_event(&self, input: &CreateTaskRunEventInput) -> Result<i64, String> {
        let conn = self.get_conn()?;

        conn.execute(
            r#"
            INSERT INTO task_run_events (
                task_run_id, event_type, event_subtype, message, data,
                workflow_name, state_name, action_id, timestamp, duration_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                input.task_run_id,
                input.event_type,
                input.event_subtype,
                input.message,
                input.data,
                input.workflow_name,
                input.state_name,
                input.action_id,
                input.timestamp,
                input.duration_ms,
            ],
        )
        .map_err(|e| format!("Failed to create task run event: {}", e))?;

        let id = conn.last_insert_rowid();
        Ok(id)
    }

    /// Batch insert task run events (efficient for JSONL migration).
    pub fn batch_create_task_run_events(
        &self,
        events: &[CreateTaskRunEventInput],
    ) -> Result<usize, String> {
        if events.is_empty() {
            return Ok(0);
        }

        let conn = self.get_conn()?;
        let mut count = 0;

        for event in events {
            conn.execute(
                r#"
                INSERT INTO task_run_events (
                    task_run_id, event_type, event_subtype, message, data,
                    workflow_name, state_name, action_id, timestamp, duration_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
                params![
                    event.task_run_id,
                    event.event_type,
                    event.event_subtype,
                    event.message,
                    event.data,
                    event.workflow_name,
                    event.state_name,
                    event.action_id,
                    event.timestamp,
                    event.duration_ms,
                ],
            )
            .map_err(|e| format!("Failed to create task run event: {}", e))?;
            count += 1;
        }

        Ok(count)
    }

    /// Get events for a task run with optional filtering.
    pub fn get_task_run_events(
        &self,
        task_run_id: &str,
        event_type: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<TaskRunEvent>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, task_run_id, event_type, event_subtype, message, data,
                   workflow_name, state_name, action_id, timestamp, duration_ms
            FROM task_run_events
            WHERE task_run_id = ?1
            "#,
        );

        let mut params_vec: Vec<String> = vec![task_run_id.to_string()];

        if let Some(et) = event_type {
            sql.push_str(" AND event_type = ?2");
            params_vec.push(et.to_string());
        }

        sql.push_str(" ORDER BY timestamp ASC");

        if let Some(lim) = limit {
            sql.push_str(&format!(" LIMIT {}", lim));
        }

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = params_vec
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        let events = stmt
            .query_map(params.as_slice(), |row| {
                Ok(TaskRunEvent {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    event_type: row.get(2)?,
                    event_subtype: row.get(3)?,
                    message: row.get(4)?,
                    data: row.get(5)?,
                    workflow_name: row.get(6)?,
                    state_name: row.get(7)?,
                    action_id: row.get(8)?,
                    timestamp: row.get(9)?,
                    duration_ms: row.get(10)?,
                })
            })
            .map_err(|e| format!("Failed to get task run events: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(events)
    }

    /// Create a task run screenshot record.
    pub fn create_task_run_screenshot(
        &self,
        input: &CreateTaskRunScreenshotInput,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO task_run_screenshots (
                id, task_run_id, event_id, file_path, screenshot_type,
                template_name, confidence, match_location,
                width, height, file_size_bytes, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
            params![
                id,
                input.task_run_id,
                input.event_id,
                input.file_path,
                input.screenshot_type,
                input.template_name,
                input.confidence,
                input.match_location,
                input.width,
                input.height,
                input.file_size_bytes,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create task run screenshot: {}", e))?;

        Ok(id)
    }

    /// Get screenshots for a task run.
    pub fn get_task_run_screenshots(
        &self,
        task_run_id: &str,
        screenshot_type: Option<&str>,
    ) -> Result<Vec<TaskRunScreenshot>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, task_run_id, event_id, file_path, screenshot_type,
                   template_name, confidence, match_location,
                   width, height, file_size_bytes, created_at
            FROM task_run_screenshots
            WHERE task_run_id = ?1
            "#,
        );

        let mut params_vec: Vec<String> = vec![task_run_id.to_string()];

        if let Some(st) = screenshot_type {
            sql.push_str(" AND screenshot_type = ?2");
            params_vec.push(st.to_string());
        }

        sql.push_str(" ORDER BY created_at ASC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = params_vec
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        let screenshots = stmt
            .query_map(params.as_slice(), |row| {
                Ok(TaskRunScreenshot {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    event_id: row.get(2)?,
                    file_path: row.get(3)?,
                    screenshot_type: row.get(4)?,
                    template_name: row.get(5)?,
                    confidence: row.get(6)?,
                    match_location: row.get(7)?,
                    width: row.get(8)?,
                    height: row.get(9)?,
                    file_size_bytes: row.get(10)?,
                    created_at: row.get(11)?,
                })
            })
            .map_err(|e| format!("Failed to get task run screenshots: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(screenshots)
    }

    /// Create a Playwright test result.
    pub fn create_task_run_playwright_result(
        &self,
        input: &CreateTaskRunPlaywrightResultInput,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO task_run_playwright_results (
                id, task_run_id, test_name, spec_file, status, duration_ms,
                stdout, stderr, console_output, page_snapshot,
                error_message, failure_screenshot_path,
                assertions_passed, assertions_failed, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            "#,
            params![
                id,
                input.task_run_id,
                input.test_name,
                input.spec_file,
                input.status,
                input.duration_ms,
                input.stdout,
                input.stderr,
                input.console_output,
                input.page_snapshot,
                input.error_message,
                input.failure_screenshot_path,
                input.assertions_passed,
                input.assertions_failed,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create Playwright result: {}", e))?;

        Ok(id)
    }

    /// Get Playwright results for a task run.
    pub fn get_task_run_playwright_results(
        &self,
        task_run_id: &str,
        status_filter: Option<&str>,
    ) -> Result<Vec<TaskRunPlaywrightResult>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, task_run_id, test_name, spec_file, status, duration_ms,
                   stdout, stderr, console_output, page_snapshot,
                   error_message, failure_screenshot_path,
                   assertions_passed, assertions_failed, created_at
            FROM task_run_playwright_results
            WHERE task_run_id = ?1
            "#,
        );

        let mut params_vec: Vec<String> = vec![task_run_id.to_string()];

        if let Some(status) = status_filter {
            sql.push_str(" AND status = ?2");
            params_vec.push(status.to_string());
        }

        sql.push_str(" ORDER BY created_at ASC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = params_vec
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        let results = stmt
            .query_map(params.as_slice(), |row| {
                Ok(TaskRunPlaywrightResult {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    test_name: row.get(2)?,
                    spec_file: row.get(3)?,
                    status: row.get(4)?,
                    duration_ms: row.get(5)?,
                    stdout: row.get(6)?,
                    stderr: row.get(7)?,
                    console_output: row.get(8)?,
                    page_snapshot: row.get(9)?,
                    error_message: row.get(10)?,
                    failure_screenshot_path: row.get(11)?,
                    assertions_passed: row.get(12)?,
                    assertions_failed: row.get(13)?,
                    created_at: row.get(14)?,
                })
            })
            .map_err(|e| format!("Failed to get Playwright results: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Delete all events for a task run (used when clearing/re-importing).
    pub fn delete_task_run_events(&self, task_run_id: &str) -> Result<usize, String> {
        let conn = self.get_conn()?;

        let rows_affected = conn
            .execute(
                "DELETE FROM task_run_events WHERE task_run_id = ?1",
                params![task_run_id],
            )
            .map_err(|e| format!("Failed to delete task run events: {}", e))?;

        Ok(rows_affected)
    }

    // ========================================================================
    // Execution Spans Operations
    // ========================================================================

    /// Get execution spans with optional filtering.
    ///
    /// Supports filtering by:
    /// - execution_id: Filter by task/execution ID
    /// - name_pattern: Filter span names using SQL LIKE pattern (e.g., "workflow.%")
    /// - min_duration_ms: Filter spans with duration >= this value
    /// - limit: Maximum number of spans to return (default: 100)
    pub fn get_execution_spans(
        &self,
        execution_id: Option<&str>,
        name_pattern: Option<&str>,
        min_duration_ms: Option<i64>,
        limit: Option<u32>,
    ) -> Result<Vec<ExecutionSpan>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, execution_id, trace_id, span_id, parent_span_id, name,
                   start_ts, end_ts, duration_ms, attributes, success, error, created_at
            FROM execution_spans
            WHERE 1=1
            "#,
        );

        let mut param_values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let mut param_idx = 1;

        if let Some(exec_id) = execution_id {
            sql.push_str(&format!(" AND execution_id = ?{}", param_idx));
            param_values.push(Box::new(exec_id.to_string()));
            param_idx += 1;
        }

        if let Some(pattern) = name_pattern {
            sql.push_str(&format!(" AND name LIKE ?{}", param_idx));
            param_values.push(Box::new(pattern.to_string()));
            param_idx += 1;
        }

        if let Some(min_dur) = min_duration_ms {
            sql.push_str(&format!(" AND duration_ms >= ?{}", param_idx));
            param_values.push(Box::new(min_dur));
            // param_idx += 1; // Not needed as it's the last
        }

        sql.push_str(" ORDER BY start_ts DESC");

        let lim = limit.unwrap_or(100);
        sql.push_str(&format!(" LIMIT {}", lim));

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = param_values
            .iter()
            .map(|p| p.as_ref() as &dyn rusqlite::ToSql)
            .collect();

        let spans = stmt
            .query_map(params.as_slice(), |row| {
                Ok(ExecutionSpan {
                    id: row.get(0)?,
                    execution_id: row.get(1)?,
                    trace_id: row.get(2)?,
                    span_id: row.get(3)?,
                    parent_span_id: row.get(4)?,
                    name: row.get(5)?,
                    start_ts: row.get(6)?,
                    end_ts: row.get(7)?,
                    duration_ms: row.get(8)?,
                    attributes: row.get(9)?,
                    success: row.get::<_, i32>(10)? == 1,
                    error: row.get(11)?,
                    created_at: row.get(12)?,
                })
            })
            .map_err(|e| format!("Failed to get execution spans: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(spans)
    }

    // ========================================================================
    // Task Run API Request Operations
    // ========================================================================

    /// Create a task run API request record.
    pub fn create_task_run_api_request(
        &self,
        input: &CreateTaskRunApiRequestInput,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            r#"
            INSERT INTO task_run_api_requests (
                id, task_run_id, step_id, step_name,
                method, url, resolved_url, request_headers, request_body,
                status_code, status_text, response_headers, response_time_ms,
                response_body_type, response_body, response_size_bytes,
                extractions, assertions, success, error_message, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
            "#,
            params![
                id,
                input.task_run_id,
                input.step_id,
                input.step_name,
                input.method,
                input.url,
                input.resolved_url,
                input.request_headers,
                input.request_body,
                input.status_code,
                input.status_text,
                input.response_headers,
                input.response_time_ms,
                input.response_body_type,
                input.response_body,
                input.response_size_bytes,
                input.extractions,
                input.assertions,
                input.success,
                input.error_message,
                input.timestamp,
            ],
        )
        .map_err(|e| format!("Failed to create task run API request: {}", e))?;

        Ok(id)
    }

    /// Batch insert task run API requests (efficient for JSONL migration).
    pub fn batch_create_task_run_api_requests(
        &self,
        requests: &[CreateTaskRunApiRequestInput],
    ) -> Result<usize, String> {
        if requests.is_empty() {
            return Ok(0);
        }

        let conn = self.get_conn()?;
        let mut count = 0;

        for input in requests {
            let id = uuid::Uuid::new_v4().to_string();

            conn.execute(
                r#"
                INSERT INTO task_run_api_requests (
                    id, task_run_id, step_id, step_name,
                    method, url, resolved_url, request_headers, request_body,
                    status_code, status_text, response_headers, response_time_ms,
                    response_body_type, response_body, response_size_bytes,
                    extractions, assertions, success, error_message, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
                "#,
                params![
                    id,
                    input.task_run_id,
                    input.step_id,
                    input.step_name,
                    input.method,
                    input.url,
                    input.resolved_url,
                    input.request_headers,
                    input.request_body,
                    input.status_code,
                    input.status_text,
                    input.response_headers,
                    input.response_time_ms,
                    input.response_body_type,
                    input.response_body,
                    input.response_size_bytes,
                    input.extractions,
                    input.assertions,
                    input.success,
                    input.error_message,
                    input.timestamp,
                ],
            )
            .map_err(|e| format!("Failed to create task run API request: {}", e))?;
            count += 1;
        }

        Ok(count)
    }

    /// Get API requests for a task run.
    pub fn get_task_run_api_requests(
        &self,
        task_run_id: &str,
        success_filter: Option<bool>,
    ) -> Result<Vec<TaskRunApiRequest>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, task_run_id, step_id, step_name,
                   method, url, resolved_url, request_headers, request_body,
                   status_code, status_text, response_headers, response_time_ms,
                   response_body_type, response_body, response_size_bytes,
                   extractions, assertions, success, error_message, created_at
            FROM task_run_api_requests
            WHERE task_run_id = ?1
            "#,
        );

        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(task_run_id.to_string())];

        if let Some(success) = success_filter {
            sql.push_str(" AND success = ?2");
            params_vec.push(Box::new(success));
        }

        sql.push_str(" ORDER BY created_at ASC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();

        let results = stmt
            .query_map(params.as_slice(), |row| {
                Ok(TaskRunApiRequest {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    step_id: row.get(2)?,
                    step_name: row.get(3)?,
                    method: row.get(4)?,
                    url: row.get(5)?,
                    resolved_url: row.get(6)?,
                    request_headers: row.get(7)?,
                    request_body: row.get(8)?,
                    status_code: row.get(9)?,
                    status_text: row.get(10)?,
                    response_headers: row.get(11)?,
                    response_time_ms: row.get(12)?,
                    response_body_type: row.get(13)?,
                    response_body: row.get(14)?,
                    response_size_bytes: row.get(15)?,
                    extractions: row.get(16)?,
                    assertions: row.get(17)?,
                    success: row.get(18)?,
                    error_message: row.get(19)?,
                    created_at: row.get(20)?,
                })
            })
            .map_err(|e| format!("Failed to get API requests: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Task Run AWAS Step Operations
    // ========================================================================

    /// Create a task run AWAS step record.
    pub fn create_task_run_awas_step(
        &self,
        input: &CreateTaskRunAwasStepInput,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            r#"
            INSERT INTO task_run_awas_steps (
                id, task_run_id, step_id, step_name, step_type,
                url, action_id, parameters, response_data,
                success, error_message, duration_ms, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            "#,
            params![
                id,
                input.task_run_id,
                input.step_id,
                input.step_name,
                input.step_type,
                input.url,
                input.action_id,
                input.parameters,
                input.response_data,
                input.success,
                input.error_message,
                input.duration_ms,
                input.timestamp,
            ],
        )
        .map_err(|e| format!("Failed to create task run AWAS step: {}", e))?;

        Ok(id)
    }

    /// Get AWAS steps for a task run.
    pub fn get_task_run_awas_steps(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<TaskRunAwasStep>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_run_id, step_id, step_name, step_type,
                       url, action_id, parameters, response_data,
                       success, error_message, duration_ms, created_at
                FROM task_run_awas_steps
                WHERE task_run_id = ?1
                ORDER BY created_at ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(params![task_run_id], |row| {
                Ok(TaskRunAwasStep {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    step_id: row.get(2)?,
                    step_name: row.get(3)?,
                    step_type: row.get(4)?,
                    url: row.get(5)?,
                    action_id: row.get(6)?,
                    parameters: row.get(7)?,
                    response_data: row.get(8)?,
                    success: row.get(9)?,
                    error_message: row.get(10)?,
                    duration_ms: row.get(11)?,
                    created_at: row.get(12)?,
                })
            })
            .map_err(|e| format!("Failed to get AWAS steps: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }
}
