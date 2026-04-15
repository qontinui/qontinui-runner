//! PostgreSQL verification test and test result CRUD (Bug 9).
//!
//! Provides full CRUD for `verification_tests` and read+create access to
//! `test_results`. Supersedes the SQLite-era stubs in
//! `mcp/verification_tests.rs` and `commands/testing.rs`.

use chrono::{DateTime, Utc};
use tokio_postgres::Row;

use super::PgDb;
use crate::database::types::{
    CreateVerificationTestInput, TestResult, TestResultStatus, TestType, VerificationTest,
};

/// Filters for `list_verification_tests`.
#[derive(Debug, Default, Clone)]
pub struct VerificationTestFilter {
    pub enabled_only: bool,
    pub test_type: Option<String>,
    pub category: Option<String>,
}

const VT_COLUMNS: &str = "id, name, description, test_type, category, playwright_code, \
    vision_config, python_code, repo_test_config, success_criteria, config, timeout_seconds, \
    is_critical, enabled, ai_generated, ai_generation_prompt, creation_analysis, tags, \
    source_file, last_exported_at, created_at, updated_at";

const TR_COLUMNS: &str = "id, test_id, task_run_id, status, started_at, completed_at, \
    duration_ms, output, error_message, structured_output, assertions_passed, assertions_failed, \
    screenshots, visual_evidence, ai_analysis, created_at";

fn parse_json_text(value: Option<String>) -> Option<serde_json::Value> {
    value.and_then(|s| serde_json::from_str(&s).ok())
}

fn parse_json_text_or_default(value: Option<String>) -> serde_json::Value {
    value
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

fn row_to_verification_test(r: &Row) -> VerificationTest {
    let test_type_str: String = r.get(3);
    let tags_str: Option<String> = r.get(17);
    let last_exported_at: Option<DateTime<Utc>> = r.get(19);
    let created: DateTime<Utc> = r.get(20);
    let updated: DateTime<Utc> = r.get(21);

    VerificationTest {
        id: r.get(0),
        name: r.get(1),
        description: r.get(2),
        test_type: test_type_str.parse().unwrap_or(TestType::PythonScript),
        category: r.get(4),
        playwright_code: r.get(5),
        vision_config: parse_json_text(r.get(6)),
        python_code: r.get(7),
        repo_test_config: parse_json_text(r.get(8)),
        success_criteria: r.get(9),
        config: parse_json_text_or_default(r.get(10)),
        timeout_seconds: r.get::<_, Option<i32>>(11).map(|v| v as u32),
        is_critical: r.get(12),
        enabled: r.get(13),
        ai_generated: r.get(14),
        ai_generation_prompt: r.get(15),
        creation_analysis: parse_json_text(r.get(16)),
        tags: tags_str
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        source_file: r.get(18),
        last_exported_at: last_exported_at.map(|d| d.to_rfc3339()),
        created_at: created.to_rfc3339(),
        updated_at: updated.to_rfc3339(),
    }
}

fn row_to_test_result(r: &Row) -> TestResult {
    let status_str: String = r.get(3);
    let started_at: Option<DateTime<Utc>> = r.get(4);
    let completed_at: Option<DateTime<Utc>> = r.get(5);
    let screenshots_str: Option<String> = r.get(12);
    let created: DateTime<Utc> = r.get(15);

    TestResult {
        id: r.get(0),
        test_id: r.get(1),
        task_run_id: r.get(2),
        status: status_str.parse().unwrap_or(TestResultStatus::Pending),
        started_at: started_at.map(|d| d.to_rfc3339()),
        completed_at: completed_at.map(|d| d.to_rfc3339()),
        duration_ms: r.get::<_, Option<i32>>(6).map(|v| v as i64),
        output: r.get(7),
        error_message: r.get(8),
        structured_output: parse_json_text(r.get(9)),
        assertions_passed: r.get::<_, i32>(10) as u32,
        assertions_failed: r.get::<_, i32>(11) as u32,
        screenshots: screenshots_str
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        visual_evidence: parse_json_text(r.get(13)),
        ai_analysis: r.get(14),
        created_at: created.to_rfc3339(),
    }
}

impl PgDb {
    // ────────────────────────────────────────────────────────────────────────
    // verification_tests CRUD
    // ────────────────────────────────────────────────────────────────────────

    /// Get a single verification test by ID.
    pub async fn get_verification_test(
        &self,
        id: &str,
    ) -> Result<Option<VerificationTest>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let sql = format!(
            "SELECT {} FROM verification_tests WHERE id = $1",
            VT_COLUMNS
        );
        let row = conn
            .query_opt(&sql, &[&id])
            .await
            .map_err(|e| format!("PG get_verification_test: {}", e))?;

        Ok(row.as_ref().map(row_to_verification_test))
    }

    /// List verification tests with optional filters.
    pub async fn list_verification_tests(
        &self,
        filter: VerificationTestFilter,
    ) -> Result<Vec<VerificationTest>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Build dynamic SQL with positional parameters.
        let mut sql = format!("SELECT {} FROM verification_tests WHERE 1=1", VT_COLUMNS);
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();

        if filter.enabled_only {
            sql.push_str(" AND enabled = true");
        }
        if let Some(t) = filter.test_type.as_ref() {
            params.push(Box::new(t.clone()));
            sql.push_str(&format!(" AND test_type = ${}", params.len()));
        }
        if let Some(c) = filter.category.as_ref() {
            params.push(Box::new(c.clone()));
            sql.push_str(&format!(" AND category = ${}", params.len()));
        }
        sql.push_str(" ORDER BY updated_at DESC");

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        let rows = conn
            .query(&sql, &param_refs[..])
            .await
            .map_err(|e| format!("PG list_verification_tests: {}", e))?;

        Ok(rows.iter().map(row_to_verification_test).collect())
    }

    /// Create a new verification test.
    pub async fn create_verification_test(
        &self,
        input: &CreateVerificationTestInput,
    ) -> Result<VerificationTest, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = format!("vt-{}", uuid::Uuid::new_v4());
        let test_type_str = input.test_type.to_string();
        let vision_config_str = input
            .vision_config
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()));
        let repo_test_config_str = input
            .repo_test_config
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()));
        let creation_analysis_str = input
            .creation_analysis
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()));
        let config_str = serde_json::to_string(&input.config).unwrap_or_else(|_| "{}".to_string());
        let tags_str = serde_json::to_string(&input.tags).unwrap_or_else(|_| "[]".to_string());
        let timeout: Option<i32> = input.timeout_seconds.map(|t| t as i32);

        conn.execute(
            r#"INSERT INTO verification_tests (
                id, name, description, test_type, category, playwright_code, vision_config,
                python_code, repo_test_config, success_criteria, config, timeout_seconds,
                is_critical, enabled, ai_generated, ai_generation_prompt, creation_analysis,
                tags, created_at, updated_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18,
                NOW(), NOW()
            )"#,
            &[
                &id,
                &input.name,
                &input.description,
                &test_type_str,
                &input.category,
                &input.playwright_code,
                &vision_config_str,
                &input.python_code,
                &repo_test_config_str,
                &input.success_criteria,
                &config_str,
                &timeout,
                &input.is_critical,
                &input.enabled,
                &input.ai_generated,
                &input.ai_generation_prompt,
                &creation_analysis_str,
                &tags_str,
            ],
        )
        .await
        .map_err(|e| format!("PG create_verification_test: {}", e))?;

        self.get_verification_test(&id)
            .await?
            .ok_or_else(|| "Failed to retrieve created verification test".to_string())
    }

    /// Update an existing verification test (full replace of mutable fields).
    pub async fn update_verification_test(
        &self,
        id: &str,
        input: &CreateVerificationTestInput,
    ) -> Result<VerificationTest, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let test_type_str = input.test_type.to_string();
        let vision_config_str = input
            .vision_config
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()));
        let repo_test_config_str = input
            .repo_test_config
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()));
        let creation_analysis_str = input
            .creation_analysis
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()));
        let config_str = serde_json::to_string(&input.config).unwrap_or_else(|_| "{}".to_string());
        let tags_str = serde_json::to_string(&input.tags).unwrap_or_else(|_| "[]".to_string());
        let timeout: Option<i32> = input.timeout_seconds.map(|t| t as i32);

        let updated = conn
            .execute(
                r#"UPDATE verification_tests SET
                    name = $2,
                    description = $3,
                    test_type = $4,
                    category = $5,
                    playwright_code = $6,
                    vision_config = $7,
                    python_code = $8,
                    repo_test_config = $9,
                    success_criteria = $10,
                    config = $11,
                    timeout_seconds = $12,
                    is_critical = $13,
                    enabled = $14,
                    ai_generated = $15,
                    ai_generation_prompt = $16,
                    creation_analysis = $17,
                    tags = $18,
                    updated_at = NOW()
                WHERE id = $1"#,
                &[
                    &id,
                    &input.name,
                    &input.description,
                    &test_type_str,
                    &input.category,
                    &input.playwright_code,
                    &vision_config_str,
                    &input.python_code,
                    &repo_test_config_str,
                    &input.success_criteria,
                    &config_str,
                    &timeout,
                    &input.is_critical,
                    &input.enabled,
                    &input.ai_generated,
                    &input.ai_generation_prompt,
                    &creation_analysis_str,
                    &tags_str,
                ],
            )
            .await
            .map_err(|e| format!("PG update_verification_test: {}", e))?;

        if updated == 0 {
            return Err(format!("Verification test not found: {}", id));
        }

        self.get_verification_test(id)
            .await?
            .ok_or_else(|| format!("Verification test disappeared after update: {}", id))
    }

    /// Delete a verification test by ID. Returns true if a row was deleted.
    pub async fn delete_verification_test(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let deleted = conn
            .execute("DELETE FROM verification_tests WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_verification_test: {}", e))?;

        Ok(deleted > 0)
    }

    // ────────────────────────────────────────────────────────────────────────
    // test_results
    // ────────────────────────────────────────────────────────────────────────

    /// List test results, optionally filtered by test_id, task_run_id, status.
    pub async fn list_test_results(
        &self,
        test_id: Option<&str>,
        task_run_id: Option<&str>,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<TestResult>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = format!("SELECT {} FROM test_results WHERE 1=1", TR_COLUMNS);
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();

        if let Some(t) = test_id {
            params.push(Box::new(t.to_string()));
            sql.push_str(&format!(" AND test_id = ${}", params.len()));
        }
        if let Some(t) = task_run_id {
            params.push(Box::new(t.to_string()));
            sql.push_str(&format!(" AND task_run_id = ${}", params.len()));
        }
        if let Some(s) = status {
            params.push(Box::new(s.to_string()));
            sql.push_str(&format!(" AND status = ${}", params.len()));
        }
        sql.push_str(" ORDER BY created_at DESC");
        if let Some(lim) = limit {
            sql.push_str(&format!(" LIMIT {}", lim.min(1000)));
        } else {
            sql.push_str(" LIMIT 200");
        }

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        let rows = conn
            .query(&sql, &param_refs[..])
            .await
            .map_err(|e| format!("PG list_test_results: {}", e))?;

        Ok(rows.iter().map(row_to_test_result).collect())
    }

    /// Get a single test result by ID.
    pub async fn get_test_result(&self, id: &str) -> Result<Option<TestResult>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let sql = format!("SELECT {} FROM test_results WHERE id = $1", TR_COLUMNS);
        let row = conn
            .query_opt(&sql, &[&id])
            .await
            .map_err(|e| format!("PG get_test_result: {}", e))?;
        Ok(row.as_ref().map(row_to_test_result))
    }

    /// Create a test result row from a `TestExecutionResult`.
    /// Returns the new test_result id.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_test_result(
        &self,
        test_id: &str,
        task_run_id: Option<&str>,
        status: &str,
        started_at: Option<&str>,
        completed_at: Option<&str>,
        duration_ms: Option<i64>,
        output: Option<&str>,
        error_message: Option<&str>,
        structured_output: Option<&serde_json::Value>,
        assertions_passed: u32,
        assertions_failed: u32,
        screenshots: &[String],
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = format!("tr-{}", uuid::Uuid::new_v4());
        let structured_str = structured_output
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()));
        let screenshots_str =
            serde_json::to_string(screenshots).unwrap_or_else(|_| "[]".to_string());
        let started_dt: Option<DateTime<Utc>> = started_at.and_then(|s| {
            DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|d| d.with_timezone(&Utc))
        });
        let completed_dt: Option<DateTime<Utc>> = completed_at.and_then(|s| {
            DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|d| d.with_timezone(&Utc))
        });
        let duration_i32: Option<i32> = duration_ms.map(|v| v.min(i32::MAX as i64) as i32);
        let assertions_passed_i32 = assertions_passed as i32;
        let assertions_failed_i32 = assertions_failed as i32;

        conn.execute(
            r#"INSERT INTO test_results (
                id, test_id, task_run_id, status, started_at, completed_at, duration_ms,
                output, error_message, structured_output, assertions_passed, assertions_failed,
                screenshots, created_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, NOW()
            )"#,
            &[
                &id,
                &test_id,
                &task_run_id,
                &status,
                &started_dt,
                &completed_dt,
                &duration_i32,
                &output,
                &error_message,
                &structured_str,
                &assertions_passed_i32,
                &assertions_failed_i32,
                &screenshots_str,
            ],
        )
        .await
        .map_err(|e| format!("PG create_test_result: {}", e))?;

        Ok(id)
    }

    /// Aggregate test history. If `test_id` is provided, restricts to that
    /// test; otherwise aggregates over all test results within the window.
    pub async fn get_test_history(
        &self,
        test_id: Option<&str>,
        limit: u32,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let (count_sql, params): (
            String,
            Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>>,
        ) = if let Some(t) = test_id {
            (
                "SELECT status, COUNT(*)::BIGINT, COALESCE(SUM(duration_ms), 0)::BIGINT \
                 FROM test_results WHERE test_id = $1 GROUP BY status"
                    .to_string(),
                vec![Box::new(t.to_string())],
            )
        } else {
            (
                "SELECT status, COUNT(*)::BIGINT, COALESCE(SUM(duration_ms), 0)::BIGINT \
                 FROM test_results GROUP BY status"
                    .to_string(),
                vec![],
            )
        };

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        let rows = conn
            .query(&count_sql, &param_refs[..])
            .await
            .map_err(|e| format!("PG get_test_history aggregate: {}", e))?;

        let mut total_runs: i64 = 0;
        let mut passed: i64 = 0;
        let mut failed: i64 = 0;
        let mut error_n: i64 = 0;
        let mut timeout: i64 = 0;
        let mut skipped: i64 = 0;
        let mut total_duration: i64 = 0;

        for r in &rows {
            let status: String = r.get(0);
            let count: i64 = r.get(1);
            let dur: i64 = r.get(2);
            total_runs += count;
            total_duration += dur;
            match status.as_str() {
                "passed" => passed += count,
                "failed" => failed += count,
                "error" => error_n += count,
                "timeout" => timeout += count,
                "skipped" => skipped += count,
                _ => {}
            }
        }

        let pass_rate = if total_runs > 0 {
            format!("{:.1}%", (passed as f64 / total_runs as f64) * 100.0)
        } else {
            "0.0%".to_string()
        };
        let avg_duration = if total_runs > 0 {
            total_duration / total_runs
        } else {
            0
        };

        // Recent results (most recent N).
        let recent = self
            .list_test_results(test_id, None, None, Some(limit))
            .await
            .unwrap_or_default();

        Ok(serde_json::json!({
            "total_runs": total_runs,
            "passed": passed,
            "failed": failed,
            "error": error_n,
            "timeout": timeout,
            "skipped": skipped,
            "pass_rate": pass_rate,
            "total_duration_ms": total_duration,
            "average_duration_ms": avg_duration,
            "recent_results": recent,
        }))
    }
}
