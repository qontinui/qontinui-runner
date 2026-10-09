//! PostgreSQL task run event operations via Clorinde-generated queries.

use super::PgDb;
use crate::database::types::*;

impl PgDb {
    /// Create a task run event. Returns the auto-generated event ID.
    pub async fn create_task_run_event(
        &self,
        input: &CreateTaskRunEventInput,
    ) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let duration = input.duration_ms.map(|v| v as i64);
        let event_subtype = input.event_subtype.clone();
        let data = input.data.clone();
        let workflow_name = input.workflow_name.clone();
        let state_name = input.state_name.clone();
        let action_id = input.action_id.clone();

        let id = qontinui_db::queries::task_run_events::create_task_run_event()
            .bind(
                &conn,
                &input.task_run_id.as_str(),
                &input.event_type.as_str(),
                &event_subtype,
                &input.message.as_str(),
                &data,
                &workflow_name,
                &state_name,
                &action_id,
                &input.timestamp.as_str(),
                &duration,
            )
            .one()
            .await
            .map_err(|e| crate::database::pg::pg_err("PG insert task_run_event", &e))?;

        Ok(id)
    }

    /// Get events for a task run. When event_type is None, returns all events.
    pub async fn get_task_run_events(
        &self,
        task_run_id: &str,
        event_type: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<TaskRunEvent>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Helper to convert a Clorinde row (non-optional COALESCE'd fields) to app type
        fn map_event_row(
            id: i64,
            task_run_id: String,
            event_type: String,
            event_subtype: String,
            message: String,
            data: String,
            workflow_name: String,
            state_name: String,
            action_id: String,
            timestamp: String,
            duration_ms: i64,
        ) -> TaskRunEvent {
            TaskRunEvent {
                id,
                task_run_id,
                event_type,
                event_subtype: if event_subtype.is_empty() {
                    None
                } else {
                    Some(event_subtype)
                },
                message,
                data: if data.is_empty() { None } else { Some(data) },
                workflow_name: if workflow_name.is_empty() {
                    None
                } else {
                    Some(workflow_name)
                },
                state_name: if state_name.is_empty() {
                    None
                } else {
                    Some(state_name)
                },
                action_id: if action_id.is_empty() {
                    None
                } else {
                    Some(action_id)
                },
                timestamp,
                duration_ms: if duration_ms == 0 {
                    None
                } else {
                    Some(duration_ms)
                },
            }
        }

        if let Some(et) = event_type {
            let rows = qontinui_db::queries::task_run_events::get_task_run_events_by_type()
                .bind(&conn, &task_run_id, &et)
                .all()
                .await
                .map_err(|e| crate::database::pg::pg_err("PG query task_run_events", &e))?;
            Ok(rows
                .into_iter()
                .map(|r| {
                    map_event_row(
                        r.id,
                        r.task_run_id,
                        r.event_type,
                        r.event_subtype,
                        r.message,
                        r.data,
                        r.workflow_name,
                        r.state_name,
                        r.action_id,
                        r.timestamp,
                        r.duration_ms,
                    )
                })
                .collect())
        } else if let Some(lim) = limit {
            let lim_i = lim as i64;
            let rows = qontinui_db::queries::task_run_events::get_task_run_events_limited()
                .bind(&conn, &task_run_id, &lim_i)
                .all()
                .await
                .map_err(|e| crate::database::pg::pg_err("PG query task_run_events", &e))?;
            Ok(rows
                .into_iter()
                .map(|r| {
                    map_event_row(
                        r.id,
                        r.task_run_id,
                        r.event_type,
                        r.event_subtype,
                        r.message,
                        r.data,
                        r.workflow_name,
                        r.state_name,
                        r.action_id,
                        r.timestamp,
                        r.duration_ms,
                    )
                })
                .collect())
        } else {
            let rows = qontinui_db::queries::task_run_events::get_task_run_events_all()
                .bind(&conn, &task_run_id)
                .all()
                .await
                .map_err(|e| crate::database::pg::pg_err("PG query task_run_events", &e))?;
            Ok(rows
                .into_iter()
                .map(|r| {
                    map_event_row(
                        r.id,
                        r.task_run_id,
                        r.event_type,
                        r.event_subtype,
                        r.message,
                        r.data,
                        r.workflow_name,
                        r.state_name,
                        r.action_id,
                        r.timestamp,
                        r.duration_ms,
                    )
                })
                .collect())
        }
    }

    /// Delete all events for a task run.
    pub async fn delete_task_run_events(&self, task_run_id: &str) -> Result<usize, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let count = qontinui_db::queries::task_run_events::delete_task_run_events()
            .bind(&conn, &task_run_id)
            .await
            .map_err(|e| crate::database::pg::pg_err("PG delete task_run_events", &e))?;
        Ok(count as usize)
    }

    /// Create a task run screenshot record.
    pub async fn create_task_run_screenshot(
        &self,
        input: &CreateTaskRunScreenshotInput,
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let id = uuid::Uuid::new_v4().to_string();
        let event_id = input.event_id.map(|v| v as i64);
        let file_size = input.file_size_bytes.map(|v| v as i64);
        let template_name = input.template_name.clone();
        let match_location = input.match_location.clone();

        qontinui_db::queries::task_run_events::create_task_run_screenshot()
            .bind(
                &conn,
                &id.as_str(),
                &input.task_run_id.as_str(),
                &event_id,
                &input.file_path.as_str(),
                &input.screenshot_type.as_str(),
                &template_name,
                &input.confidence,
                &match_location,
                &input.width,
                &input.height,
                &file_size,
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG insert task_run_screenshot", &e))?;

        Ok(id)
    }

    /// Get screenshots for a task run.
    pub async fn get_task_run_screenshots(
        &self,
        task_run_id: &str,
        screenshot_type: Option<&str>,
    ) -> Result<Vec<TaskRunScreenshot>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Helper to convert Clorinde row to app type
        fn map_screenshot_row(
            id: String,
            task_run_id: String,
            event_id: i64,
            file_path: String,
            screenshot_type: String,
            template_name: String,
            confidence: f64,
            match_location: String,
            width: i32,
            height: i32,
            file_size_bytes: i64,
            created_at: chrono::DateTime<chrono::FixedOffset>,
        ) -> TaskRunScreenshot {
            TaskRunScreenshot {
                id,
                task_run_id,
                event_id: if event_id == 0 { None } else { Some(event_id) },
                file_path,
                screenshot_type,
                template_name: if template_name.is_empty() {
                    None
                } else {
                    Some(template_name)
                },
                confidence: if confidence == 0.0 {
                    None
                } else {
                    Some(confidence)
                },
                match_location: if match_location.is_empty() {
                    None
                } else {
                    Some(match_location)
                },
                width: if width == 0 { None } else { Some(width) },
                height: if height == 0 { None } else { Some(height) },
                file_size_bytes: if file_size_bytes == 0 {
                    None
                } else {
                    Some(file_size_bytes)
                },
                created_at: created_at.to_rfc3339(),
            }
        }

        if let Some(st) = screenshot_type {
            let rows = qontinui_db::queries::task_run_events::get_task_run_screenshots_by_type()
                .bind(&conn, &task_run_id, &st)
                .all()
                .await
                .map_err(|e| crate::database::pg::pg_err("PG query task_run_screenshots", &e))?;
            Ok(rows
                .into_iter()
                .map(|r| {
                    map_screenshot_row(
                        r.id,
                        r.task_run_id,
                        r.event_id,
                        r.file_path,
                        r.screenshot_type,
                        r.template_name,
                        r.confidence,
                        r.match_location,
                        r.width,
                        r.height,
                        r.file_size_bytes,
                        r.created_at,
                    )
                })
                .collect())
        } else {
            let rows = qontinui_db::queries::task_run_events::get_task_run_screenshots_all()
                .bind(&conn, &task_run_id)
                .all()
                .await
                .map_err(|e| crate::database::pg::pg_err("PG query task_run_screenshots", &e))?;
            Ok(rows
                .into_iter()
                .map(|r| {
                    map_screenshot_row(
                        r.id,
                        r.task_run_id,
                        r.event_id,
                        r.file_path,
                        r.screenshot_type,
                        r.template_name,
                        r.confidence,
                        r.match_location,
                        r.width,
                        r.height,
                        r.file_size_bytes,
                        r.created_at,
                    )
                })
                .collect())
        }
    }

    /// Create a Playwright test result.
    pub async fn create_task_run_playwright_result(
        &self,
        input: &CreateTaskRunPlaywrightResultInput,
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let id = uuid::Uuid::new_v4().to_string();
        let duration = input.duration_ms.map(|v| v as i64);
        let spec_file = input.spec_file.clone();
        let stdout = input.stdout.clone();
        let stderr = input.stderr.clone();
        let console_output = input.console_output.clone();
        let page_snapshot = input.page_snapshot.clone();
        let error_message = input.error_message.clone();
        let failure_screenshot_path = input.failure_screenshot_path.clone();

        qontinui_db::queries::task_run_events::create_task_run_playwright_result()
            .bind(
                &conn,
                &id.as_str(),
                &input.task_run_id.as_str(),
                &input.test_name.as_str(),
                &spec_file,
                &input.status.as_str(),
                &duration,
                &stdout,
                &stderr,
                &console_output,
                &page_snapshot,
                &error_message,
                &failure_screenshot_path,
                &input.assertions_passed,
                &input.assertions_failed,
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG insert task_run_playwright_result", &e))?;

        Ok(id)
    }

    /// Get Playwright results for a task run.
    pub async fn get_task_run_playwright_results(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<TaskRunPlaywrightResult>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::task_run_events::get_task_run_playwright_results_all()
            .bind(&conn, &task_run_id)
            .all()
            .await
            .map_err(|e| crate::database::pg::pg_err("PG query task_run_playwright_results", &e))?;

        Ok(rows
            .into_iter()
            .map(|r| {
                // Clorinde returns non-optional (COALESCE'd) — convert empty to None
                let spec_file = if r.spec_file.is_empty() {
                    None
                } else {
                    Some(r.spec_file)
                };
                let duration_ms = if r.duration_ms == 0 {
                    None
                } else {
                    Some(r.duration_ms)
                };
                let stdout = if r.stdout.is_empty() {
                    None
                } else {
                    Some(r.stdout)
                };
                let stderr = if r.stderr.is_empty() {
                    None
                } else {
                    Some(r.stderr)
                };
                let console_output = if r.console_output.is_empty() {
                    None
                } else {
                    Some(r.console_output)
                };
                let page_snapshot = if r.page_snapshot.is_empty() {
                    None
                } else {
                    Some(r.page_snapshot)
                };
                let error_message = if r.error_message.is_empty() {
                    None
                } else {
                    Some(r.error_message)
                };
                let failure_screenshot_path = if r.failure_screenshot_path.is_empty() {
                    None
                } else {
                    Some(r.failure_screenshot_path)
                };

                TaskRunPlaywrightResult {
                    id: r.id,
                    task_run_id: r.task_run_id,
                    test_name: r.test_name,
                    spec_file,
                    status: r.status,
                    duration_ms,
                    stdout,
                    stderr,
                    console_output,
                    page_snapshot,
                    error_message,
                    failure_screenshot_path,
                    assertions_passed: r.assertions_passed,
                    assertions_failed: r.assertions_failed,
                    created_at: r.created_at.to_rfc3339(),
                }
            })
            .collect())
    }

    /// Create a task run API request record.
    pub async fn create_task_run_api_request(
        &self,
        input: &CreateTaskRunApiRequestInput,
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let id = uuid::Uuid::new_v4().to_string();
        let response_time = input.response_time_ms as i64;
        let response_size = input.response_size_bytes.map(|v| v as i64);
        let step_name = input.step_name.clone();
        let request_headers = input.request_headers.clone();
        let request_body = input.request_body.clone();
        let status_text = input.status_text.clone();
        let response_headers = input.response_headers.clone();
        let response_body = input.response_body.clone();
        let extractions = input.extractions.clone();
        let assertions = input.assertions.clone();
        let error_message = input.error_message.clone();

        qontinui_db::queries::task_run_events::create_task_run_api_request()
            .bind(
                &conn,
                &id.as_str(),
                &input.task_run_id.as_str(),
                &input.step_id.as_str(),
                &step_name,
                &input.method.as_str(),
                &input.url.as_str(),
                &input.resolved_url.as_str(),
                &request_headers,
                &request_body,
                &input.status_code,
                &status_text,
                &response_headers,
                &response_time,
                &input.response_body_type.as_str(),
                &response_body,
                &response_size,
                &extractions,
                &assertions,
                &input.success,
                &error_message,
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG insert task_run_api_request", &e))?;

        Ok(id)
    }

    /// Get API requests for a task run.
    pub async fn get_task_run_api_requests(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<TaskRunApiRequest>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::task_run_events::get_task_run_api_requests_all()
            .bind(&conn, &task_run_id)
            .all()
            .await
            .map_err(|e| crate::database::pg::pg_err("PG query task_run_api_requests", &e))?;

        Ok(rows
            .into_iter()
            .map(|r| {
                // Clorinde returns non-optional (COALESCE'd) — convert empty to None
                let step_name = if r.step_name.is_empty() {
                    None
                } else {
                    Some(r.step_name)
                };
                let request_headers = if r.request_headers.is_empty() {
                    None
                } else {
                    Some(r.request_headers)
                };
                let request_body = if r.request_body.is_empty() {
                    None
                } else {
                    Some(r.request_body)
                };
                let status_text = if r.status_text.is_empty() {
                    None
                } else {
                    Some(r.status_text)
                };
                let response_headers = if r.response_headers.is_empty() {
                    None
                } else {
                    Some(r.response_headers)
                };
                let response_body = if r.response_body.is_empty() {
                    None
                } else {
                    Some(r.response_body)
                };
                let response_size_bytes = if r.response_size_bytes == 0 {
                    None
                } else {
                    Some(r.response_size_bytes)
                };
                let extractions = if r.extractions.is_empty() {
                    None
                } else {
                    Some(r.extractions)
                };
                let assertions = if r.assertions.is_empty() {
                    None
                } else {
                    Some(r.assertions)
                };
                let error_message = if r.error_message.is_empty() {
                    None
                } else {
                    Some(r.error_message)
                };

                TaskRunApiRequest {
                    id: r.id,
                    task_run_id: r.task_run_id,
                    step_id: r.step_id,
                    step_name,
                    method: r.method,
                    url: r.url,
                    resolved_url: r.resolved_url,
                    request_headers,
                    request_body,
                    status_code: r.status_code,
                    status_text,
                    response_headers,
                    response_time_ms: r.response_time_ms,
                    response_body_type: r.response_body_type,
                    response_body,
                    response_size_bytes,
                    extractions,
                    assertions,
                    success: r.success,
                    error_message,
                    created_at: r.created_at.to_rfc3339(),
                }
            })
            .collect())
    }

    /// Create a task run AWAS step record.
    pub async fn create_task_run_awas_step(
        &self,
        input: &CreateTaskRunAwasStepInput,
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let id = uuid::Uuid::new_v4().to_string();
        let duration = input.duration_ms.map(|v| v as i64);
        let step_id = input.step_id.clone();
        let step_name = input.step_name.clone();
        let url = input.url.clone();
        let action_id = input.action_id.clone();
        let parameters = input.parameters.clone();
        let response_data = input.response_data.clone();
        let error_message = input.error_message.clone();

        qontinui_db::queries::task_run_events::create_task_run_awas_step()
            .bind(
                &conn,
                &id.as_str(),
                &input.task_run_id.as_str(),
                &step_id,
                &step_name,
                &input.step_type.as_str(),
                &url,
                &action_id,
                &parameters,
                &response_data,
                &input.success,
                &error_message,
                &duration,
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG insert task_run_awas_step", &e))?;

        Ok(id)
    }

    /// Get AWAS steps for a task run.
    pub async fn get_task_run_awas_steps(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<TaskRunAwasStep>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::task_run_events::get_task_run_awas_steps()
            .bind(&conn, &task_run_id)
            .all()
            .await
            .map_err(|e| crate::database::pg::pg_err("PG query task_run_awas_steps", &e))?;

        Ok(rows
            .into_iter()
            .map(|r| {
                // Clorinde returns non-optional (COALESCE'd) — convert empty to None
                let step_id = if r.step_id.is_empty() {
                    None
                } else {
                    Some(r.step_id)
                };
                let step_name = if r.step_name.is_empty() {
                    None
                } else {
                    Some(r.step_name)
                };
                let url = if r.url.is_empty() { None } else { Some(r.url) };
                let action_id = if r.action_id.is_empty() {
                    None
                } else {
                    Some(r.action_id)
                };
                let parameters = if r.parameters.is_empty() {
                    None
                } else {
                    Some(r.parameters)
                };
                let response_data = if r.response_data.is_empty() {
                    None
                } else {
                    Some(r.response_data)
                };
                let error_message = if r.error_message.is_empty() {
                    None
                } else {
                    Some(r.error_message)
                };
                let duration_ms = if r.duration_ms == 0 {
                    None
                } else {
                    Some(r.duration_ms)
                };

                TaskRunAwasStep {
                    id: r.id,
                    task_run_id: r.task_run_id,
                    step_id,
                    step_name,
                    step_type: r.step_type,
                    url,
                    action_id,
                    parameters,
                    response_data,
                    success: r.success,
                    error_message,
                    duration_ms,
                    created_at: r.created_at.to_rfc3339(),
                }
            })
            .collect())
    }
}

// =============================================================================
// Keyset pages over the per-run log tables
// =============================================================================

/// One page of a per-run log table (`task_run_playwright_results`,
/// `task_run_api_requests`, `task_run_awas_steps`), walked by keyset on the
/// immutable `(created_at, id)` in `created_at ASC, id ASC` order (plan
/// `2026-09-05-every-bounded-read-is-a-page-that-reads-as-a-corpus`, Phase 5b
/// and D8).
///
/// `created_at` is written once, by the INSERT's `NOW()`, and no statement
/// updates it or `id` — `commands::ai_data`'s pinning test greps for one. The
/// walk is ascending (chronological, the order a run's log is read in) rather
/// than the codec's usual descending, so rows a still-running task appends
/// land AHEAD of the cursor and are reached, never skipped.
///
/// The three counts are window counts (`COUNT(*) OVER ()`) carried by every
/// row of the statement, so they cover the rows matching from this page's
/// START POSITION onward — the whole match set on the first page, exactly as
/// `qontinui_types::page::Bound::Exact` defines `total`.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskRunLogPage<T> {
    /// The rows, in walk order, each with its exact `created_at` (the keyset
    /// value the next cursor is minted from).
    pub rows: Vec<(T, chrono::DateTime<chrono::Utc>)>,
    /// Rows matching from this page's start position onward.
    pub total_from_start: i64,
    /// Of those, rows that passed (Playwright) or succeeded.
    pub succeeded_from_start: i64,
    /// Of those, rows that failed.
    pub failed_from_start: i64,
}

impl<T> TaskRunLogPage<T> {
    fn empty() -> Self {
        TaskRunLogPage {
            rows: Vec::new(),
            total_from_start: 0,
            succeeded_from_start: 0,
            failed_from_start: 0,
        }
    }
}

/// `0001-01-01T00:00:00Z` — an instant both chrono and PostgreSQL's
/// `timestamptz` represent, before every `NOW()` a row was inserted at.
const WALK_START_UNIX_SECONDS: i64 = -62_135_596_800;

/// The `(created_at, id)` a walk resumes strictly after. The first page binds
/// a position before every row the table can hold (`created_at` is always an
/// INSERT's `NOW()`, and the empty id sorts before every id), so ONE statement
/// serves the first and every later page.
fn keyset_after(
    after: Option<qontinui_types::page::KeysetPosition>,
) -> (chrono::DateTime<chrono::FixedOffset>, String) {
    match after {
        Some(pos) => (pos.at.fixed_offset(), pos.id.to_string()),
        None => (
            chrono::DateTime::<chrono::Utc>::from_timestamp(WALK_START_UNIX_SECONDS, 0)
                .unwrap_or(chrono::DateTime::UNIX_EPOCH)
                .fixed_offset(),
            String::new(),
        ),
    }
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn non_zero(v: i64) -> Option<i64> {
    if v == 0 {
        None
    } else {
        Some(v)
    }
}

impl PgDb {
    /// One keyset page of a run's Playwright results. `limit` is the cap the
    /// statement applies; see [`TaskRunLogPage`] for the walk and the counts.
    pub async fn get_task_run_playwright_results_page(
        &self,
        task_run_id: &str,
        after: Option<qontinui_types::page::KeysetPosition>,
        limit: i64,
    ) -> Result<TaskRunLogPage<TaskRunPlaywrightResult>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let (after_created_at, after_id) = keyset_after(after);

        let rows = qontinui_db::queries::task_run_events::get_task_run_playwright_results_page()
            .bind(
                &conn,
                &task_run_id,
                &after_created_at,
                &after_id.as_str(),
                &limit,
            )
            .all()
            .await
            .map_err(|e| crate::database::pg::pg_err("PG query task_run_playwright_results", &e))?;

        let mut page = TaskRunLogPage::empty();
        if let Some(first) = rows.first() {
            page.total_from_start = first.total_from_start;
            page.succeeded_from_start = first.passed_from_start;
            page.failed_from_start = first.failed_from_start;
        }
        page.rows = rows
            .into_iter()
            .map(|r| {
                let created_at = r.created_at.with_timezone(&chrono::Utc);
                let row = TaskRunPlaywrightResult {
                    id: r.id,
                    task_run_id: r.task_run_id,
                    test_name: r.test_name,
                    spec_file: non_empty(r.spec_file),
                    status: r.status,
                    duration_ms: non_zero(r.duration_ms),
                    stdout: non_empty(r.stdout),
                    stderr: non_empty(r.stderr),
                    console_output: non_empty(r.console_output),
                    page_snapshot: non_empty(r.page_snapshot),
                    error_message: non_empty(r.error_message),
                    failure_screenshot_path: non_empty(r.failure_screenshot_path),
                    assertions_passed: r.assertions_passed,
                    assertions_failed: r.assertions_failed,
                    created_at: r.created_at.to_rfc3339(),
                };
                (row, created_at)
            })
            .collect();
        Ok(page)
    }

    /// One keyset page of a run's API requests, optionally only the
    /// succeeded (`Some(true)`) or failed (`Some(false)`) ones — filtered in
    /// the statement, so the window counts and the page agree.
    pub async fn get_task_run_api_requests_page(
        &self,
        task_run_id: &str,
        success_filter: Option<bool>,
        after: Option<qontinui_types::page::KeysetPosition>,
        limit: i64,
    ) -> Result<TaskRunLogPage<TaskRunApiRequest>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let (after_created_at, after_id) = keyset_after(after);
        let filter_by_success = success_filter.is_some();
        let success = success_filter.unwrap_or(false);

        let rows = qontinui_db::queries::task_run_events::get_task_run_api_requests_page()
            .bind(
                &conn,
                &task_run_id,
                &filter_by_success,
                &success,
                &after_created_at,
                &after_id.as_str(),
                &limit,
            )
            .all()
            .await
            .map_err(|e| crate::database::pg::pg_err("PG query task_run_api_requests", &e))?;

        let mut page = TaskRunLogPage::empty();
        if let Some(first) = rows.first() {
            page.total_from_start = first.total_from_start;
            page.succeeded_from_start = first.succeeded_from_start;
            page.failed_from_start = first.total_from_start - first.succeeded_from_start;
        }
        page.rows = rows
            .into_iter()
            .map(|r| {
                let created_at = r.created_at.with_timezone(&chrono::Utc);
                let row = TaskRunApiRequest {
                    id: r.id,
                    task_run_id: r.task_run_id,
                    step_id: r.step_id,
                    step_name: non_empty(r.step_name),
                    method: r.method,
                    url: r.url,
                    resolved_url: r.resolved_url,
                    request_headers: non_empty(r.request_headers),
                    request_body: non_empty(r.request_body),
                    status_code: r.status_code,
                    status_text: non_empty(r.status_text),
                    response_headers: non_empty(r.response_headers),
                    response_time_ms: r.response_time_ms,
                    response_body_type: r.response_body_type,
                    response_body: non_empty(r.response_body),
                    response_size_bytes: non_zero(r.response_size_bytes),
                    extractions: non_empty(r.extractions),
                    assertions: non_empty(r.assertions),
                    success: r.success,
                    error_message: non_empty(r.error_message),
                    created_at: r.created_at.to_rfc3339(),
                };
                (row, created_at)
            })
            .collect();
        Ok(page)
    }

    /// One keyset page of a run's AWAS steps, optionally only one
    /// `step_type` — filtered in the statement, so the window counts and the
    /// page agree.
    pub async fn get_task_run_awas_steps_page(
        &self,
        task_run_id: &str,
        step_type: Option<&str>,
        after: Option<qontinui_types::page::KeysetPosition>,
        limit: i64,
    ) -> Result<TaskRunLogPage<TaskRunAwasStep>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let (after_created_at, after_id) = keyset_after(after);
        let filter_by_step_type = step_type.is_some();
        let step_type = step_type.unwrap_or("");

        let rows = qontinui_db::queries::task_run_events::get_task_run_awas_steps_page()
            .bind(
                &conn,
                &task_run_id,
                &filter_by_step_type,
                &step_type,
                &after_created_at,
                &after_id.as_str(),
                &limit,
            )
            .all()
            .await
            .map_err(|e| crate::database::pg::pg_err("PG query task_run_awas_steps", &e))?;

        let mut page = TaskRunLogPage::empty();
        if let Some(first) = rows.first() {
            page.total_from_start = first.total_from_start;
            page.succeeded_from_start = first.succeeded_from_start;
            page.failed_from_start = first.total_from_start - first.succeeded_from_start;
        }
        page.rows = rows
            .into_iter()
            .map(|r| {
                let created_at = r.created_at.with_timezone(&chrono::Utc);
                let row = TaskRunAwasStep {
                    id: r.id,
                    task_run_id: r.task_run_id,
                    step_id: non_empty(r.step_id),
                    step_name: non_empty(r.step_name),
                    step_type: r.step_type,
                    url: non_empty(r.url),
                    action_id: non_empty(r.action_id),
                    parameters: non_empty(r.parameters),
                    response_data: non_empty(r.response_data),
                    success: r.success,
                    error_message: non_empty(r.error_message),
                    duration_ms: non_zero(r.duration_ms),
                    created_at: r.created_at.to_rfc3339(),
                };
                (row, created_at)
            })
            .collect();
        Ok(page)
    }
}
