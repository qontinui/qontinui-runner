//! Verification test, check, and quality operations.
//!
//! Contains CheckpointDb methods for verification tests, test results,
//! code quality checks, check groups, shell commands, and test associations.

use chrono::Utc;
use rusqlite::{params, Result as SqliteResult};

use super::types::*;
use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Verification Test Operations
    // ========================================================================

    /// Create a new verification test.
    pub fn create_verification_test(
        &self,
        input: &CreateVerificationTestInput,
    ) -> Result<VerificationTest, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&input.tags)
            .map_err(|e| format!("Failed to serialize tags: {}", e))?;
        let vision_config_json = input
            .vision_config
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Failed to serialize vision_config: {}", e))?;
        let repo_test_config_json = input
            .repo_test_config
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Failed to serialize repo_test_config: {}", e))?;
        let config_json = serde_json::to_string(&input.config)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        let creation_analysis_json = input
            .creation_analysis
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Failed to serialize creation_analysis: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO verification_tests (
                id, name, description, test_type, category,
                playwright_code, vision_config, python_code, repo_test_config,
                success_criteria, config, timeout_seconds, is_critical, enabled,
                ai_generated, ai_generation_prompt, creation_analysis, tags, source_file,
                created_at, updated_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9,
                ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19,
                ?20, ?20
            )
            "#,
            params![
                id,
                input.name,
                input.description,
                input.test_type.to_string(),
                input.category,
                input.playwright_code,
                vision_config_json,
                input.python_code,
                repo_test_config_json,
                input.success_criteria,
                config_json,
                input.timeout_seconds,
                input.is_critical as i32,
                input.enabled as i32,
                input.ai_generated as i32,
                input.ai_generation_prompt,
                creation_analysis_json,
                tags_json,
                input.source_file,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create verification test: {}", e))?;

        self.get_verification_test(&id)?
            .ok_or_else(|| "Failed to retrieve created test".to_string())
    }

    /// Get a verification test by ID.
    pub fn get_verification_test(&self, id: &str) -> Result<Option<VerificationTest>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<VerificationTest> = conn.query_row(
            r#"
            SELECT
                id, name, description, test_type, category,
                playwright_code, vision_config, python_code, repo_test_config,
                success_criteria, config, timeout_seconds, is_critical, enabled,
                ai_generated, ai_generation_prompt, creation_analysis, tags, source_file, last_exported_at,
                created_at, updated_at
            FROM verification_tests
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(VerificationTest {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2).ok(),
                    test_type: row
                        .get::<_, String>(3)?
                        .parse()
                        .unwrap_or(TestType::PythonScript),
                    category: row.get(4).ok(),
                    playwright_code: row.get(5).ok(),
                    vision_config: row
                        .get::<_, Option<String>>(6)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    python_code: row.get(7).ok(),
                    repo_test_config: row
                        .get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    success_criteria: row.get(9).ok(),
                    config: row
                        .get::<_, Option<String>>(10)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or(serde_json::json!({})),
                    timeout_seconds: row.get::<_, Option<i64>>(11)?.map(|v| v as u32),
                    is_critical: row.get::<_, i32>(12)? != 0,
                    enabled: row.get::<_, i32>(13)? != 0,
                    ai_generated: row.get::<_, i32>(14)? != 0,
                    ai_generation_prompt: row.get(15).ok(),
                    creation_analysis: row
                        .get::<_, Option<String>>(16)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    tags: row
                        .get::<_, Option<String>>(17)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    source_file: row.get(18).ok(),
                    last_exported_at: row.get(19).ok(),
                    created_at: row.get(20)?,
                    updated_at: row.get(21)?,
                })
            },
        );

        match result {
            Ok(test) => Ok(Some(test)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get verification test: {}", e)),
        }
    }

    /// List all verification tests.
    pub fn list_verification_tests(
        &self,
        enabled_only: bool,
        test_type: Option<&TestType>,
        category: Option<&str>,
    ) -> Result<Vec<VerificationTest>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT
                id, name, description, test_type, category,
                playwright_code, vision_config, python_code, repo_test_config,
                success_criteria, config, timeout_seconds, is_critical, enabled,
                ai_generated, ai_generation_prompt, creation_analysis, tags, source_file, last_exported_at,
                created_at, updated_at
            FROM verification_tests
            WHERE 1=1
            "#,
        );

        if enabled_only {
            sql.push_str(" AND enabled = 1");
        }
        if test_type.is_some() {
            sql.push_str(" AND test_type = ?1");
        }
        if category.is_some() {
            sql.push_str(" AND category = ?2");
        }
        sql.push_str(" ORDER BY created_at DESC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        // Handle parameter binding based on which filters are set
        let tests: Vec<VerificationTest> = if let Some(tt) = test_type {
            if let Some(cat) = category {
                stmt.query_map(params![tt.to_string(), cat], Self::row_to_verification_test)
            } else {
                stmt.query_map(params![tt.to_string()], Self::row_to_verification_test)
            }
        } else if let Some(cat) = category {
            stmt.query_map(params![cat], Self::row_to_verification_test)
        } else {
            stmt.query_map([], Self::row_to_verification_test)
        }
        .map_err(|e| format!("Failed to query verification tests: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

        Ok(tests)
    }

    /// Helper to map a row to VerificationTest.
    fn row_to_verification_test(row: &rusqlite::Row) -> SqliteResult<VerificationTest> {
        Ok(VerificationTest {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2).ok(),
            test_type: row
                .get::<_, String>(3)?
                .parse()
                .unwrap_or(TestType::PythonScript),
            category: row.get(4).ok(),
            playwright_code: row.get(5).ok(),
            vision_config: row
                .get::<_, Option<String>>(6)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            python_code: row.get(7).ok(),
            repo_test_config: row
                .get::<_, Option<String>>(8)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            success_criteria: row.get(9).ok(),
            config: row
                .get::<_, Option<String>>(10)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(serde_json::json!({})),
            timeout_seconds: row.get::<_, Option<i64>>(11)?.map(|v| v as u32),
            is_critical: row.get::<_, i32>(12)? != 0,
            enabled: row.get::<_, i32>(13)? != 0,
            ai_generated: row.get::<_, i32>(14)? != 0,
            ai_generation_prompt: row.get(15).ok(),
            creation_analysis: row
                .get::<_, Option<String>>(16)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            tags: row
                .get::<_, Option<String>>(17)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            source_file: row.get(18).ok(),
            last_exported_at: row.get(19).ok(),
            created_at: row.get(20)?,
            updated_at: row.get(21)?,
        })
    }

    /// Update a verification test.
    pub fn update_verification_test(
        &self,
        id: &str,
        input: &CreateVerificationTestInput,
    ) -> Result<VerificationTest, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&input.tags)
            .map_err(|e| format!("Failed to serialize tags: {}", e))?;
        let vision_config_json = input
            .vision_config
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Failed to serialize vision_config: {}", e))?;
        let repo_test_config_json = input
            .repo_test_config
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Failed to serialize repo_test_config: {}", e))?;
        let config_json = serde_json::to_string(&input.config)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        let creation_analysis_json = input
            .creation_analysis
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Failed to serialize creation_analysis: {}", e))?;

        let rows = conn
            .execute(
                r#"
            UPDATE verification_tests SET
                name = ?2,
                description = ?3,
                test_type = ?4,
                category = ?5,
                playwright_code = ?6,
                vision_config = ?7,
                python_code = ?8,
                repo_test_config = ?9,
                success_criteria = ?10,
                config = ?11,
                timeout_seconds = ?12,
                is_critical = ?13,
                enabled = ?14,
                ai_generated = ?15,
                ai_generation_prompt = ?16,
                creation_analysis = ?17,
                tags = ?18,
                source_file = ?19,
                updated_at = ?20
            WHERE id = ?1
            "#,
                params![
                    id,
                    input.name,
                    input.description,
                    input.test_type.to_string(),
                    input.category,
                    input.playwright_code,
                    vision_config_json,
                    input.python_code,
                    repo_test_config_json,
                    input.success_criteria,
                    config_json,
                    input.timeout_seconds,
                    input.is_critical as i32,
                    input.enabled as i32,
                    input.ai_generated as i32,
                    input.ai_generation_prompt,
                    creation_analysis_json,
                    tags_json,
                    input.source_file,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to update verification test: {}", e))?;

        if rows == 0 {
            return Err(format!("Verification test not found: {}", id));
        }

        self.get_verification_test(id)?
            .ok_or_else(|| "Failed to retrieve updated test".to_string())
    }

    /// Delete a verification test.
    pub fn delete_verification_test(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute("DELETE FROM verification_tests WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete verification test: {}", e))?;

        Ok(rows > 0)
    }

    // ========================================================================
    // Test Result Operations
    // ========================================================================

    /// Create a new test result.
    pub fn create_test_result(&self, input: &CreateTestResultInput) -> Result<TestResult, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO test_results (
                id, test_id, task_run_id, status, created_at
            ) VALUES (?1, ?2, ?3, 'pending', ?4)
            "#,
            params![id, input.test_id, input.task_run_id, now],
        )
        .map_err(|e| format!("Failed to create test result: {}", e))?;

        self.get_test_result(&id)?
            .ok_or_else(|| "Failed to retrieve created test result".to_string())
    }

    /// Get a test result by ID.
    pub fn get_test_result(&self, id: &str) -> Result<Option<TestResult>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<TestResult> = conn.query_row(
            r#"
            SELECT
                id, test_id, task_run_id, status,
                started_at, completed_at, duration_ms,
                output, error_message, structured_output,
                assertions_passed, assertions_failed,
                screenshots, visual_evidence, ai_analysis, created_at
            FROM test_results
            WHERE id = ?1
            "#,
            params![id],
            Self::row_to_test_result,
        );

        match result {
            Ok(result) => Ok(Some(result)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get test result: {}", e)),
        }
    }

    /// Helper to map a row to TestResult.
    fn row_to_test_result(row: &rusqlite::Row) -> SqliteResult<TestResult> {
        Ok(TestResult {
            id: row.get(0)?,
            test_id: row.get(1)?,
            task_run_id: row.get(2).ok(),
            status: row
                .get::<_, String>(3)?
                .parse()
                .unwrap_or(TestResultStatus::Pending),
            started_at: row.get(4).ok(),
            completed_at: row.get(5).ok(),
            duration_ms: row.get(6).ok(),
            output: row.get(7).ok(),
            error_message: row.get(8).ok(),
            structured_output: row
                .get::<_, Option<String>>(9)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            assertions_passed: row.get::<_, i64>(10)? as u32,
            assertions_failed: row.get::<_, i64>(11)? as u32,
            screenshots: row
                .get::<_, Option<String>>(12)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            visual_evidence: row
                .get::<_, Option<String>>(13)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            ai_analysis: row.get(14).ok(),
            created_at: row.get(15)?,
        })
    }

    /// Get test results for a test.
    pub fn get_results_for_test(
        &self,
        test_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<TestResult>, String> {
        let conn = self.get_conn()?;

        let sql = format!(
            r#"
            SELECT
                id, test_id, task_run_id, status,
                started_at, completed_at, duration_ms,
                output, error_message, structured_output,
                assertions_passed, assertions_failed,
                screenshots, visual_evidence, ai_analysis, created_at
            FROM test_results
            WHERE test_id = ?1
            ORDER BY created_at DESC
            {}
            "#,
            limit.map(|l| format!("LIMIT {}", l)).unwrap_or_default()
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results: Vec<TestResult> = stmt
            .query_map(params![test_id], Self::row_to_test_result)
            .map_err(|e| format!("Failed to query test results: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get test results for a task run.
    pub fn get_results_for_task_run(&self, task_run_id: &str) -> Result<Vec<TestResult>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    id, test_id, task_run_id, status,
                    started_at, completed_at, duration_ms,
                    output, error_message, structured_output,
                    assertions_passed, assertions_failed,
                    screenshots, visual_evidence, ai_analysis, created_at
                FROM test_results
                WHERE task_run_id = ?1
                ORDER BY created_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results: Vec<TestResult> = stmt
            .query_map(params![task_run_id], Self::row_to_test_result)
            .map_err(|e| format!("Failed to query test results: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// List test results with optional status filter.
    pub fn list_test_results(
        &self,
        status: Option<&TestResultStatus>,
        limit: u32,
    ) -> Result<Vec<TestResult>, String> {
        let conn = self.get_conn()?;

        let (sql, params): (String, Vec<Box<dyn rusqlite::ToSql>>) = match status {
            Some(s) => {
                let status_str = s.to_string();
                (
                    format!(
                        r#"
                        SELECT
                            id, test_id, task_run_id, status,
                            started_at, completed_at, duration_ms,
                            output, error_message, structured_output,
                            assertions_passed, assertions_failed,
                            screenshots, visual_evidence, ai_analysis, created_at
                        FROM test_results
                        WHERE status = ?1
                        ORDER BY created_at DESC
                        LIMIT {}
                        "#,
                        limit
                    ),
                    vec![Box::new(status_str)],
                )
            }
            None => (
                format!(
                    r#"
                    SELECT
                        id, test_id, task_run_id, status,
                        started_at, completed_at, duration_ms,
                        output, error_message, structured_output,
                        assertions_passed, assertions_failed,
                        screenshots, visual_evidence, ai_analysis, created_at
                    FROM test_results
                    ORDER BY created_at DESC
                    LIMIT {}
                    "#,
                    limit
                ),
                vec![],
            ),
        };

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results: Vec<TestResult> = if params.is_empty() {
            stmt.query_map([], Self::row_to_test_result)
                .map_err(|e| format!("Failed to query test results: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            let params_refs: Vec<&dyn rusqlite::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            stmt.query_map(params_refs.as_slice(), Self::row_to_test_result)
                .map_err(|e| format!("Failed to query test results: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        };

        Ok(results)
    }

    /// Update test result status and output.
    pub fn update_test_result(
        &self,
        id: &str,
        status: &TestResultStatus,
        output: Option<&str>,
        error_message: Option<&str>,
        structured_output: Option<&serde_json::Value>,
        assertions_passed: u32,
        assertions_failed: u32,
        screenshots: &[String],
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let structured_output_json = structured_output
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Failed to serialize structured_output: {}", e))?;
        let screenshots_json = serde_json::to_string(screenshots)
            .map_err(|e| format!("Failed to serialize screenshots: {}", e))?;

        // Calculate duration if completing
        let duration_sql = if matches!(
            status,
            TestResultStatus::Passed
                | TestResultStatus::Failed
                | TestResultStatus::Error
                | TestResultStatus::Timeout
                | TestResultStatus::Skipped
        ) {
            ", completed_at = ?9, duration_ms = CAST((julianday(?9) - julianday(started_at)) * 86400000 AS INTEGER)"
        } else {
            ""
        };

        let sql = format!(
            r#"
            UPDATE test_results SET
                status = ?2,
                output = COALESCE(?3, output),
                error_message = COALESCE(?4, error_message),
                structured_output = COALESCE(?5, structured_output),
                assertions_passed = ?6,
                assertions_failed = ?7,
                screenshots = ?8
                {}
            WHERE id = ?1
            "#,
            duration_sql
        );

        conn.execute(
            &sql,
            params![
                id,
                status.to_string(),
                output,
                error_message,
                structured_output_json,
                assertions_passed,
                assertions_failed,
                screenshots_json,
                now,
            ],
        )
        .map_err(|e| format!("Failed to update test result: {}", e))?;

        Ok(())
    }

    /// Mark test result as started.
    pub fn start_test_result(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE test_results SET
                status = 'running',
                started_at = ?2
            WHERE id = ?1
            "#,
            params![id, now],
        )
        .map_err(|e| format!("Failed to start test result: {}", e))?;

        Ok(())
    }

    // ========================================================================
    // Check Operations (Code Quality Checks)
    // ========================================================================

    /// List all checks with optional filters.
    pub fn list_checks(
        &self,
        enabled_only: bool,
        check_type: Option<&str>,
        tool: Option<&str>,
    ) -> Result<Vec<Check>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT
                id, name, description, check_type, tool,
                command, working_directory, config_path,
                auto_fix, fail_on_warning, timeout_seconds,
                is_critical, enabled, ai_generated, ai_generation_prompt,
                tags, created_at, updated_at
            FROM checks
            WHERE 1=1
            "#,
        );

        if enabled_only {
            sql.push_str(" AND enabled = 1");
        }
        if check_type.is_some() {
            sql.push_str(" AND check_type = ?1");
        }
        if tool.is_some() {
            if check_type.is_some() {
                sql.push_str(" AND tool = ?2");
            } else {
                sql.push_str(" AND tool = ?1");
            }
        }
        sql.push_str(" ORDER BY created_at DESC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        // Handle parameter binding based on which filters are set
        let checks: Vec<Check> = if let Some(ct) = check_type {
            if let Some(t) = tool {
                stmt.query_map(params![ct, t], Self::row_to_check)
            } else {
                stmt.query_map(params![ct], Self::row_to_check)
            }
        } else if let Some(t) = tool {
            stmt.query_map(params![t], Self::row_to_check)
        } else {
            stmt.query_map([], Self::row_to_check)
        }
        .map_err(|e| format!("Failed to query checks: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

        Ok(checks)
    }

    /// Helper to map a row to Check.
    fn row_to_check(row: &rusqlite::Row) -> SqliteResult<Check> {
        Ok(Check {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2).ok(),
            check_type: row.get(3)?,
            tool: row.get(4)?,
            command: row.get(5).ok(),
            working_directory: row.get(6).ok(),
            config_path: row.get(7).ok(),
            auto_fix: row.get::<_, i32>(8)? != 0,
            fail_on_warning: row.get::<_, i32>(9)? != 0,
            // Timeout is optional - None means disabled (no timeout)
            timeout_seconds: row.get::<_, Option<i64>>(10)?.map(|v| v as u32),
            is_critical: row.get::<_, i32>(11)? != 0,
            enabled: row.get::<_, i32>(12)? != 0,
            ai_generated: row.get::<_, i32>(13)? != 0,
            ai_generation_prompt: row.get(14).ok(),
            tags: row
                .get::<_, Option<String>>(15)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            created_at: row.get(16)?,
            updated_at: row.get(17)?,
        })
    }

    /// Get a check by ID.
    pub fn get_check(&self, id: &str) -> Result<Option<Check>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<Check> = conn.query_row(
            r#"
            SELECT
                id, name, description, check_type, tool,
                command, working_directory, config_path,
                auto_fix, fail_on_warning, timeout_seconds,
                is_critical, enabled, ai_generated, ai_generation_prompt,
                tags, created_at, updated_at
            FROM checks
            WHERE id = ?1
            "#,
            params![id],
            Self::row_to_check,
        );

        match result {
            Ok(check) => Ok(Some(check)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get check: {}", e)),
        }
    }

    /// Create a new check.
    pub fn create_check(&self, input: &CreateCheckInput) -> Result<Check, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&input.tags)
            .map_err(|e| format!("Failed to serialize tags: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO checks (
                id, name, description, check_type, tool,
                command, working_directory, config_path,
                auto_fix, fail_on_warning, timeout_seconds,
                is_critical, enabled, ai_generated, ai_generation_prompt,
                tags, created_at, updated_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8,
                ?9, ?10, ?11,
                ?12, ?13, ?14, ?15,
                ?16, ?17, ?17
            )
            "#,
            params![
                id,
                input.name,
                input.description,
                input.check_type,
                input.tool,
                input.command,
                input.working_directory,
                input.config_path,
                input.auto_fix as i32,
                input.fail_on_warning as i32,
                input.timeout_seconds,
                input.is_critical as i32,
                input.enabled as i32,
                input.ai_generated as i32,
                input.ai_generation_prompt,
                tags_json,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create check: {}", e))?;

        self.get_check(&id)?
            .ok_or_else(|| "Failed to retrieve created check".to_string())
    }

    /// Update an existing check.
    pub fn update_check(&self, id: &str, input: &UpdateCheckInput) -> Result<Check, String> {
        // First verify the check exists
        let existing = self
            .get_check(id)?
            .ok_or_else(|| format!("Check not found: {}", id))?;

        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Build the SET clause dynamically based on which fields are present
        let name = input.name.as_ref().unwrap_or(&existing.name);
        let description = input.description.clone().or(existing.description);
        let check_type = input.check_type.as_ref().unwrap_or(&existing.check_type);
        let tool = input.tool.as_ref().unwrap_or(&existing.tool);
        let command = input.command.clone().or(existing.command);
        let working_directory = input
            .working_directory
            .clone()
            .or(existing.working_directory);
        let config_path = input.config_path.clone().or(existing.config_path);
        let auto_fix = input.auto_fix.unwrap_or(existing.auto_fix);
        let fail_on_warning = input.fail_on_warning.unwrap_or(existing.fail_on_warning);
        // If input specifies a timeout (including None for disabled), use it; otherwise keep existing
        let timeout_seconds = input.timeout_seconds.or(existing.timeout_seconds);
        let is_critical = input.is_critical.unwrap_or(existing.is_critical);
        let enabled = input.enabled.unwrap_or(existing.enabled);
        let tags = input.tags.clone().unwrap_or(existing.tags);

        let tags_json =
            serde_json::to_string(&tags).map_err(|e| format!("Failed to serialize tags: {}", e))?;

        let rows = conn
            .execute(
                r#"
                UPDATE checks SET
                    name = ?2,
                    description = ?3,
                    check_type = ?4,
                    tool = ?5,
                    command = ?6,
                    working_directory = ?7,
                    config_path = ?8,
                    auto_fix = ?9,
                    fail_on_warning = ?10,
                    timeout_seconds = ?11,
                    is_critical = ?12,
                    enabled = ?13,
                    tags = ?14,
                    updated_at = ?15
                WHERE id = ?1
                "#,
                params![
                    id,
                    name,
                    description,
                    check_type,
                    tool,
                    command,
                    working_directory,
                    config_path,
                    auto_fix as i32,
                    fail_on_warning as i32,
                    timeout_seconds,
                    is_critical as i32,
                    enabled as i32,
                    tags_json,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to update check: {}", e))?;

        if rows == 0 {
            return Err(format!("Check not found: {}", id));
        }

        self.get_check(id)?
            .ok_or_else(|| "Failed to retrieve updated check".to_string())
    }

    /// Delete a check by ID.
    pub fn delete_check(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute("DELETE FROM checks WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete check: {}", e))?;

        Ok(rows > 0)
    }

    // ========================================================================
    // Check Group Operations
    // ========================================================================

    /// List all check groups.
    pub fn list_check_groups(&self, enabled_only: bool) -> Result<Vec<CheckGroup>, String> {
        let conn = self.get_conn()?;

        let sql = if enabled_only {
            r#"
            SELECT id, name, description, color, enabled, run_in_parallel,
                   stop_on_failure, tags, created_at, updated_at
            FROM check_groups
            WHERE enabled = 1
            ORDER BY name ASC
            "#
        } else {
            r#"
            SELECT id, name, description, color, enabled, run_in_parallel,
                   stop_on_failure, tags, created_at, updated_at
            FROM check_groups
            ORDER BY name ASC
            "#
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let groups: Vec<CheckGroup> = stmt
            .query_map([], |row| self.row_to_check_group_without_checks(row))
            .map_err(|e| format!("Failed to query check groups: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Populate checks for each group
        let mut result = Vec::new();
        for mut group in groups {
            group.checks = self.get_checks_in_group(&group.id)?;
            result.push(group);
        }

        Ok(result)
    }

    /// Get a check group by ID.
    pub fn get_check_group(&self, id: &str) -> Result<Option<CheckGroup>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<CheckGroup> = conn.query_row(
            r#"
            SELECT id, name, description, color, enabled, run_in_parallel,
                   stop_on_failure, tags, created_at, updated_at
            FROM check_groups
            WHERE id = ?1
            "#,
            params![id],
            |row| self.row_to_check_group_without_checks(row),
        );

        match result {
            Ok(mut group) => {
                group.checks = self.get_checks_in_group(id)?;
                Ok(Some(group))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get check group: {}", e)),
        }
    }

    /// Create a new check group.
    pub fn create_check_group(&self, input: &CreateCheckGroupInput) -> Result<CheckGroup, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&input.tags)
            .map_err(|e| format!("Failed to serialize tags: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO check_groups (
                id, name, description, color, enabled,
                run_in_parallel, stop_on_failure, tags,
                created_at, updated_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9
            )
            "#,
            params![
                id,
                input.name,
                input.description,
                input.color,
                input.enabled as i32,
                input.run_in_parallel as i32,
                input.stop_on_failure as i32,
                tags_json,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create check group: {}", e))?;

        // Add checks to the group
        for (index, check_id) in input.check_ids.iter().enumerate() {
            self.add_check_to_group(&id, check_id, index as i32)?;
        }

        self.get_check_group(&id)?
            .ok_or_else(|| "Failed to retrieve created check group".to_string())
    }

    /// Update an existing check group.
    pub fn update_check_group(
        &self,
        id: &str,
        input: &UpdateCheckGroupInput,
    ) -> Result<CheckGroup, String> {
        let existing = self
            .get_check_group(id)?
            .ok_or_else(|| format!("Check group not found: {}", id))?;

        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let name = input.name.as_ref().unwrap_or(&existing.name);
        let description = input.description.clone().or(existing.description);
        let color = input.color.clone().or(existing.color);
        let enabled = input.enabled.unwrap_or(existing.enabled);
        let run_in_parallel = input.run_in_parallel.unwrap_or(existing.run_in_parallel);
        let stop_on_failure = input.stop_on_failure.unwrap_or(existing.stop_on_failure);
        let tags = input.tags.clone().unwrap_or(existing.tags);

        let tags_json =
            serde_json::to_string(&tags).map_err(|e| format!("Failed to serialize tags: {}", e))?;

        let rows = conn
            .execute(
                r#"
                UPDATE check_groups SET
                    name = ?2,
                    description = ?3,
                    color = ?4,
                    enabled = ?5,
                    run_in_parallel = ?6,
                    stop_on_failure = ?7,
                    tags = ?8,
                    updated_at = ?9
                WHERE id = ?1
                "#,
                params![
                    id,
                    name,
                    description,
                    color,
                    enabled as i32,
                    run_in_parallel as i32,
                    stop_on_failure as i32,
                    tags_json,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to update check group: {}", e))?;

        if rows == 0 {
            return Err(format!("Check group not found: {}", id));
        }

        self.get_check_group(id)?
            .ok_or_else(|| "Failed to retrieve updated check group".to_string())
    }

    /// Delete a check group by ID.
    pub fn delete_check_group(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute("DELETE FROM check_groups WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete check group: {}", e))?;

        Ok(rows > 0)
    }

    /// Add a check to a group.
    pub fn add_check_to_group(
        &self,
        group_id: &str,
        check_id: &str,
        sort_order: i32,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT OR REPLACE INTO check_group_members (id, group_id, check_id, sort_order, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![id, group_id, check_id, sort_order, now],
        )
        .map_err(|e| format!("Failed to add check to group: {}", e))?;

        Ok(())
    }

    /// Remove a check from a group.
    pub fn remove_check_from_group(&self, group_id: &str, check_id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute(
                "DELETE FROM check_group_members WHERE group_id = ?1 AND check_id = ?2",
                params![group_id, check_id],
            )
            .map_err(|e| format!("Failed to remove check from group: {}", e))?;

        Ok(rows > 0)
    }

    /// Get all checks in a group (ordered by sort_order).
    pub fn get_checks_in_group(&self, group_id: &str) -> Result<Vec<Check>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT c.id, c.name, c.description, c.check_type, c.tool,
                       c.command, c.working_directory, c.config_path,
                       c.auto_fix, c.fail_on_warning, c.timeout_seconds,
                       c.is_critical, c.enabled, c.ai_generated, c.ai_generation_prompt,
                       c.tags, c.created_at, c.updated_at
                FROM checks c
                INNER JOIN check_group_members cgm ON c.id = cgm.check_id
                WHERE cgm.group_id = ?1
                ORDER BY cgm.sort_order ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let checks: Vec<Check> = stmt
            .query_map(params![group_id], Self::row_to_check)
            .map_err(|e| format!("Failed to query checks in group: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(checks)
    }

    /// Update the checks in a group (replace all).
    pub fn set_checks_in_group(&self, group_id: &str, check_ids: &[String]) -> Result<(), String> {
        let conn = self.get_conn()?;

        // Remove all existing members
        conn.execute(
            "DELETE FROM check_group_members WHERE group_id = ?1",
            params![group_id],
        )
        .map_err(|e| format!("Failed to clear group members: {}", e))?;

        // Add new members
        for (index, check_id) in check_ids.iter().enumerate() {
            self.add_check_to_group(group_id, check_id, index as i32)?;
        }

        Ok(())
    }

    /// Repair check-group associations based on naming convention.
    ///
    /// Checks are named with format "{group_name} - {tool_name}" (e.g., "multistate - Ruff Linting").
    /// This function finds checks that match groups by this pattern and ensures they are linked.
    ///
    /// Returns the number of associations created.
    pub fn repair_check_group_associations(&self) -> Result<usize, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Insert missing associations by matching check names that start with "group_name - "
        // Only insert if the association doesn't already exist
        let sql = r#"
            INSERT OR IGNORE INTO check_group_members (id, group_id, check_id, sort_order, created_at)
            SELECT
                lower(hex(randomblob(16))),
                cg.id,
                c.id,
                (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM check_group_members WHERE group_id = cg.id),
                ?1
            FROM checks c
            JOIN check_groups cg ON c.name LIKE cg.name || ' - %'
            WHERE NOT EXISTS (
                SELECT 1 FROM check_group_members cgm
                WHERE cgm.group_id = cg.id AND cgm.check_id = c.id
            )
        "#;

        let rows = conn
            .execute(sql, params![now])
            .map_err(|e| format!("Failed to repair check-group associations: {}", e))?;

        if rows > 0 {
            tracing::info!(
                "Repaired {} check-group associations based on naming convention",
                rows
            );
        }

        Ok(rows)
    }

    /// Helper to map a row to CheckGroup (without checks populated).
    fn row_to_check_group_without_checks(&self, row: &rusqlite::Row) -> SqliteResult<CheckGroup> {
        let tags_json: String = row.get(7)?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

        Ok(CheckGroup {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2).ok(),
            color: row.get(3).ok(),
            enabled: row.get::<_, i32>(4)? != 0,
            run_in_parallel: row.get::<_, i32>(5)? != 0,
            stop_on_failure: row.get::<_, i32>(6)? != 0,
            tags,
            checks: Vec::new(), // Will be populated separately
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    }

    /// Get check results for a specific check.
    pub fn get_check_results(
        &self,
        check_id: &str,
        limit: u32,
    ) -> Result<Vec<CheckResult>, String> {
        let conn = self.get_conn()?;

        let sql = format!(
            r#"
            SELECT
                id, check_id, task_run_id, status,
                started_at, completed_at, duration_ms,
                output, error_message, issues_found,
                issues_fixed, files_checked, structured_output,
                created_at
            FROM check_results
            WHERE check_id = ?1
            ORDER BY created_at DESC
            LIMIT {}
            "#,
            limit
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results: Vec<CheckResult> = stmt
            .query_map(params![check_id], Self::row_to_check_result)
            .map_err(|e| format!("Failed to query check results: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Helper to map a row to CheckResult.
    fn row_to_check_result(row: &rusqlite::Row) -> SqliteResult<CheckResult> {
        Ok(CheckResult {
            id: row.get(0)?,
            check_id: row.get(1)?,
            task_run_id: row.get(2).ok(),
            status: row.get(3)?,
            started_at: row.get(4).ok(),
            completed_at: row.get(5).ok(),
            duration_ms: row.get(6).ok(),
            output: row.get(7).ok(),
            error_message: row.get(8).ok(),
            issues_found: row.get::<_, i64>(9)? as i32,
            issues_fixed: row.get::<_, i64>(10)? as i32,
            files_checked: row.get::<_, i64>(11)? as i32,
            structured_output: row.get(12).ok(),
            created_at: row.get(13)?,
        })
    }

    /// Save a check execution result.
    ///
    /// Takes a CheckExecutionResult from the check_executor module and stores it in the database.
    pub fn save_check_result(
        &self,
        check_id: &str,
        status: &str,
        started_at: Option<&str>,
        completed_at: Option<&str>,
        duration_ms: Option<i64>,
        output: Option<&str>,
        error_message: Option<&str>,
        issues_found: i32,
        issues_fixed: i32,
        files_checked: i32,
        structured_output: Option<&str>,
        task_run_id: Option<&str>,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO check_results (
                id, check_id, task_run_id, status,
                started_at, completed_at, duration_ms,
                output, error_message, issues_found,
                issues_fixed, files_checked, structured_output,
                created_at
            ) VALUES (
                ?1, ?2, ?3, ?4,
                ?5, ?6, ?7,
                ?8, ?9, ?10,
                ?11, ?12, ?13,
                ?14
            )
            "#,
            params![
                id,
                check_id,
                task_run_id,
                status,
                started_at,
                completed_at,
                duration_ms,
                output,
                error_message,
                issues_found,
                issues_fixed,
                files_checked,
                structured_output,
                now,
            ],
        )
        .map_err(|e| format!("Failed to save check result: {}", e))?;

        Ok(id)
    }

    // ========================================================================
    // Shell Command Operations
    // ========================================================================

    /// List all shell commands with optional filters.
    pub fn list_shell_commands(
        &self,
        enabled_only: bool,
        category: Option<&str>,
    ) -> Result<Vec<ShellCommand>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT
                id, name, description, command, working_directory,
                timeout_seconds, fail_on_error, category, tags,
                enabled, created_at, updated_at
            FROM shell_commands
            WHERE 1=1
            "#,
        );

        if enabled_only {
            sql.push_str(" AND enabled = 1");
        }
        if category.is_some() {
            sql.push_str(" AND category = ?1");
        }
        sql.push_str(" ORDER BY created_at DESC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let shell_commands: Vec<ShellCommand> = if let Some(cat) = category {
            stmt.query_map(params![cat], Self::row_to_shell_command)
        } else {
            stmt.query_map([], Self::row_to_shell_command)
        }
        .map_err(|e| format!("Failed to query shell commands: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

        Ok(shell_commands)
    }

    /// Helper to map a row to ShellCommand.
    fn row_to_shell_command(row: &rusqlite::Row) -> SqliteResult<ShellCommand> {
        Ok(ShellCommand {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2).ok(),
            command: row.get(3)?,
            working_directory: row.get(4).ok(),
            timeout_seconds: row.get::<_, i64>(5)? as i32,
            fail_on_error: row.get::<_, i32>(6)? != 0,
            category: row
                .get::<_, Option<String>>(7)?
                .unwrap_or_else(|| "general".to_string()),
            tags: row
                .get::<_, Option<String>>(8)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            enabled: row.get::<_, i32>(9)? != 0,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        })
    }

    /// Get a shell command by ID.
    pub fn get_shell_command(&self, id: &str) -> Result<Option<ShellCommand>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<ShellCommand> = conn.query_row(
            r#"
            SELECT
                id, name, description, command, working_directory,
                timeout_seconds, fail_on_error, category, tags,
                enabled, created_at, updated_at
            FROM shell_commands
            WHERE id = ?1
            "#,
            params![id],
            Self::row_to_shell_command,
        );

        match result {
            Ok(cmd) => Ok(Some(cmd)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get shell command: {}", e)),
        }
    }

    /// Create a new shell command.
    pub fn create_shell_command(
        &self,
        input: &CreateShellCommandInput,
    ) -> Result<ShellCommand, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&input.tags)
            .map_err(|e| format!("Failed to serialize tags: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO shell_commands (
                id, name, description, command, working_directory,
                timeout_seconds, fail_on_error, category, tags,
                enabled, created_at, updated_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9,
                ?10, ?11, ?11
            )
            "#,
            params![
                id,
                input.name,
                input.description,
                input.command,
                input.working_directory,
                input.timeout_seconds,
                input.fail_on_error as i32,
                input.category,
                tags_json,
                input.enabled as i32,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create shell command: {}", e))?;

        self.get_shell_command(&id)?
            .ok_or_else(|| "Failed to retrieve created shell command".to_string())
    }

    /// Update an existing shell command.
    pub fn update_shell_command(
        &self,
        id: &str,
        input: &UpdateShellCommandInput,
    ) -> Result<ShellCommand, String> {
        // First verify the shell command exists
        let existing = self
            .get_shell_command(id)?
            .ok_or_else(|| format!("Shell command not found: {}", id))?;

        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Build the SET clause based on which fields are present
        let name = input.name.as_ref().unwrap_or(&existing.name);
        let description = input.description.clone().or(existing.description);
        let command = input.command.as_ref().unwrap_or(&existing.command);
        let working_directory = input
            .working_directory
            .clone()
            .or(existing.working_directory);
        let timeout_seconds = input.timeout_seconds.unwrap_or(existing.timeout_seconds);
        let fail_on_error = input.fail_on_error.unwrap_or(existing.fail_on_error);
        let category = input.category.as_ref().unwrap_or(&existing.category);
        let tags = input.tags.clone().unwrap_or(existing.tags);
        let enabled = input.enabled.unwrap_or(existing.enabled);

        let tags_json =
            serde_json::to_string(&tags).map_err(|e| format!("Failed to serialize tags: {}", e))?;

        let rows = conn
            .execute(
                r#"
                UPDATE shell_commands SET
                    name = ?2,
                    description = ?3,
                    command = ?4,
                    working_directory = ?5,
                    timeout_seconds = ?6,
                    fail_on_error = ?7,
                    category = ?8,
                    tags = ?9,
                    enabled = ?10,
                    updated_at = ?11
                WHERE id = ?1
                "#,
                params![
                    id,
                    name,
                    description,
                    command,
                    working_directory,
                    timeout_seconds,
                    fail_on_error as i32,
                    category,
                    tags_json,
                    enabled as i32,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to update shell command: {}", e))?;

        if rows == 0 {
            return Err(format!("Shell command not found: {}", id));
        }

        self.get_shell_command(id)?
            .ok_or_else(|| "Failed to retrieve updated shell command".to_string())
    }

    /// Delete a shell command by ID.
    pub fn delete_shell_command(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute("DELETE FROM shell_commands WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete shell command: {}", e))?;

        Ok(rows > 0)
    }

    /// Get shell command results for a specific shell command.
    pub fn get_shell_command_results(
        &self,
        shell_command_id: &str,
        limit: u32,
    ) -> Result<Vec<ShellCommandResult>, String> {
        let conn = self.get_conn()?;

        let sql = format!(
            r#"
            SELECT
                id, shell_command_id, task_run_id, status,
                exit_code, stdout, stderr, duration_ms,
                started_at, completed_at, created_at
            FROM shell_command_results
            WHERE shell_command_id = ?1
            ORDER BY created_at DESC
            LIMIT {}
            "#,
            limit
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results: Vec<ShellCommandResult> = stmt
            .query_map(params![shell_command_id], Self::row_to_shell_command_result)
            .map_err(|e| format!("Failed to query shell command results: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Helper to map a row to ShellCommandResult.
    fn row_to_shell_command_result(row: &rusqlite::Row) -> SqliteResult<ShellCommandResult> {
        Ok(ShellCommandResult {
            id: row.get(0)?,
            shell_command_id: row.get(1)?,
            task_run_id: row.get(2).ok(),
            status: row.get(3)?,
            exit_code: row.get(4).ok(),
            stdout: row.get(5).ok(),
            stderr: row.get(6).ok(),
            duration_ms: row.get(7).ok(),
            started_at: row.get(8).ok(),
            completed_at: row.get(9).ok(),
            created_at: row.get(10)?,
        })
    }

    /// Save a shell command execution result.
    pub fn save_shell_command_result(
        &self,
        shell_command_id: &str,
        status: &str,
        exit_code: Option<i32>,
        stdout: Option<&str>,
        stderr: Option<&str>,
        duration_ms: Option<i64>,
        started_at: Option<&str>,
        completed_at: Option<&str>,
        task_run_id: Option<&str>,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO shell_command_results (
                id, shell_command_id, task_run_id, status,
                exit_code, stdout, stderr, duration_ms,
                started_at, completed_at, created_at
            ) VALUES (
                ?1, ?2, ?3, ?4,
                ?5, ?6, ?7, ?8,
                ?9, ?10, ?11
            )
            "#,
            params![
                id,
                shell_command_id,
                task_run_id,
                status,
                exit_code,
                stdout,
                stderr,
                duration_ms,
                started_at,
                completed_at,
                now,
            ],
        )
        .map_err(|e| format!("Failed to save shell command result: {}", e))?;

        Ok(id)
    }

    /// Execute a shell command and save the result.
    ///
    /// Runs the shell command in a subprocess, captures stdout/stderr,
    /// and stores the execution result in the database.
    pub fn execute_shell_command(
        &self,
        id: &str,
        task_run_id: Option<&str>,
    ) -> Result<ShellCommandResult, String> {
        use std::time::Instant;

        // Get the shell command
        let cmd = self
            .get_shell_command(id)?
            .ok_or_else(|| format!("Shell command not found: {}", id))?;

        if !cmd.enabled {
            return Err(format!("Shell command '{}' is disabled", cmd.name));
        }

        let start_time = Instant::now();
        let started_at = Utc::now().to_rfc3339();

        // Build the command - use shell to execute the command string
        #[cfg(target_os = "windows")]
        let mut process = crate::process_helpers::cmd_no_window();
        #[cfg(target_os = "windows")]
        process.args(["/C", &cmd.command]);

        #[cfg(not(target_os = "windows"))]
        let mut process = crate::process_helpers::no_window("sh");
        #[cfg(not(target_os = "windows"))]
        process.args(["-c", &cmd.command]);

        // Set working directory if specified
        if let Some(ref wd) = cmd.working_directory {
            process.current_dir(wd);
        }

        // Execute with timeout
        let output = process
            .output()
            .map_err(|e| format!("Failed to execute command: {}", e))?;

        let duration_ms = start_time.elapsed().as_millis() as i64;
        let completed_at = Utc::now().to_rfc3339();

        let exit_code = output.status.code();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // Determine status based on exit code
        let status = if output.status.success() {
            "success"
        } else {
            "failed"
        };

        // Save the result
        let result_id = self.save_shell_command_result(
            id,
            status,
            exit_code,
            Some(&stdout),
            Some(&stderr),
            Some(duration_ms),
            Some(&started_at),
            Some(&completed_at),
            task_run_id,
        )?;

        // Return the result
        Ok(ShellCommandResult {
            id: result_id,
            shell_command_id: id.to_string(),
            task_run_id: task_run_id.map(|s| s.to_string()),
            status: status.to_string(),
            exit_code,
            stdout: Some(stdout),
            stderr: Some(stderr),
            duration_ms: Some(duration_ms),
            started_at: Some(started_at),
            completed_at: Some(completed_at),
            created_at: Utc::now().to_rfc3339(),
        })
    }

    // ========================================================================
    // Test Association Operations
    // ========================================================================

    /// Create a test association.
    pub fn create_test_association(
        &self,
        test_id: &str,
        config_id: Option<&str>,
        workflow_name: Option<&str>,
        trigger_point: &TriggerPoint,
        action_id: Option<&str>,
        execution_order: i32,
    ) -> Result<TestAssociation, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO test_associations (
                id, test_id, config_id, workflow_name,
                trigger_point, action_id, execution_order, enabled,
                created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?8)
            "#,
            params![
                id,
                test_id,
                config_id,
                workflow_name,
                trigger_point.to_string(),
                action_id,
                execution_order,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create test association: {}", e))?;

        self.get_test_association(&id)?
            .ok_or_else(|| "Failed to retrieve created association".to_string())
    }

    /// Get a test association by ID.
    pub fn get_test_association(&self, id: &str) -> Result<Option<TestAssociation>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<TestAssociation> = conn.query_row(
            r#"
            SELECT
                id, test_id, config_id, workflow_name,
                trigger_point, action_id, execution_order, enabled,
                created_at, updated_at
            FROM test_associations
            WHERE id = ?1
            "#,
            params![id],
            Self::row_to_test_association,
        );

        match result {
            Ok(assoc) => Ok(Some(assoc)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get test association: {}", e)),
        }
    }

    /// Helper to map a row to TestAssociation.
    fn row_to_test_association(row: &rusqlite::Row) -> SqliteResult<TestAssociation> {
        Ok(TestAssociation {
            id: row.get(0)?,
            test_id: row.get(1)?,
            config_id: row.get(2).ok(),
            workflow_name: row.get(3).ok(),
            trigger_point: row
                .get::<_, String>(4)?
                .parse()
                .unwrap_or(TriggerPoint::Manual),
            action_id: row.get(5).ok(),
            execution_order: row.get(6)?,
            enabled: row.get::<_, i32>(7)? != 0,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    }

    /// Get test associations for a config.
    pub fn get_associations_for_config(
        &self,
        config_id: &str,
        trigger_point: Option<&TriggerPoint>,
    ) -> Result<Vec<TestAssociation>, String> {
        let conn = self.get_conn()?;

        let sql = if trigger_point.is_some() {
            r#"
            SELECT
                id, test_id, config_id, workflow_name,
                trigger_point, action_id, execution_order, enabled,
                created_at, updated_at
            FROM test_associations
            WHERE config_id = ?1 AND trigger_point = ?2 AND enabled = 1
            ORDER BY execution_order ASC
            "#
        } else {
            r#"
            SELECT
                id, test_id, config_id, workflow_name,
                trigger_point, action_id, execution_order, enabled,
                created_at, updated_at
            FROM test_associations
            WHERE config_id = ?1 AND enabled = 1
            ORDER BY execution_order ASC
            "#
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let associations: Vec<TestAssociation> = if let Some(tp) = trigger_point {
            stmt.query_map(
                params![config_id, tp.to_string()],
                Self::row_to_test_association,
            )
        } else {
            stmt.query_map(params![config_id], Self::row_to_test_association)
        }
        .map_err(|e| format!("Failed to query associations: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

        Ok(associations)
    }

    /// Get test associations for a test.
    pub fn get_associations_for_test(&self, test_id: &str) -> Result<Vec<TestAssociation>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    id, test_id, config_id, workflow_name,
                    trigger_point, action_id, execution_order, enabled,
                    created_at, updated_at
                FROM test_associations
                WHERE test_id = ?1
                ORDER BY created_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let associations: Vec<TestAssociation> = stmt
            .query_map(params![test_id], Self::row_to_test_association)
            .map_err(|e| format!("Failed to query associations: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(associations)
    }

    /// Delete a test association.
    pub fn delete_test_association(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute("DELETE FROM test_associations WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete test association: {}", e))?;

        Ok(rows > 0)
    }

    /// Enable or disable a test association.
    pub fn set_test_association_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE test_associations SET
                enabled = ?2,
                updated_at = ?3
            WHERE id = ?1
            "#,
            params![id, enabled as i32, now],
        )
        .map_err(|e| format!("Failed to update test association: {}", e))?;

        Ok(())
    }
}
