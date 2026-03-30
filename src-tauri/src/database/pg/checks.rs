//! PostgreSQL check and check group operations via Clorinde-generated queries.

use super::PgDb;
use crate::database::types::{Check, CheckGroup, TestType, VerificationTest};

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

fn json_or_default<T: serde::de::DeserializeOwned + Default>(s: &str) -> T {
    if s.is_empty() { T::default() } else { serde_json::from_str(s).unwrap_or_default() }
}

/// Map a Clorinde row to Check.
macro_rules! row_to_check {
    ($r:expr) => {{
        Check {
            id: $r.id,
            name: $r.name,
            description: non_empty($r.description),
            check_type: $r.check_type,
            tool: $r.tool,
            command: non_empty($r.command),
            working_directory: non_empty($r.working_directory),
            config_path: non_empty($r.config_path),
            auto_fix: $r.auto_fix,
            fail_on_warning: $r.fail_on_warning,
            timeout_seconds: if $r.timeout_seconds == 0 { None } else { Some($r.timeout_seconds as u32) },
            is_critical: $r.is_critical,
            enabled: $r.enabled,
            ai_generated: $r.ai_generated,
            ai_generation_prompt: non_empty($r.ai_generation_prompt),
            tags: json_or_default(&$r.tags),
            created_at: $r.created_at.to_rfc3339(),
            updated_at: $r.updated_at.to_rfc3339(),
        }
    }};
}

impl PgDb {
    /// List all checks.
    pub async fn list_checks(&self) -> Result<Vec<Check>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::checks::list_checks()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG list_checks: {}", e))?;
        Ok(rows.into_iter().map(|r| row_to_check!(r)).collect())
    }

    /// Get a single check by ID.
    pub async fn get_check(&self, id: &str) -> Result<Option<Check>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = qontinui_db::queries::checks::get_check()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_check: {}", e))?;
        Ok(row.map(|r| row_to_check!(r)))
    }

    /// Delete a check by ID.
    pub async fn delete_check(&self, id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let deleted = qontinui_db::queries::checks::delete_check()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG delete_check: {}", e))?;
        Ok(deleted.is_some())
    }

    /// Get a check group by ID.
    pub async fn get_check_group(&self, id: &str) -> Result<Option<CheckGroup>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = qontinui_db::queries::checks::get_check_group()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_check_group: {}", e))?;

        Ok(row.map(|r| CheckGroup {
            id: r.id,
            name: r.name,
            description: non_empty(r.description),
            color: non_empty(r.color),
            enabled: r.enabled,
            run_in_parallel: r.run_in_parallel,
            stop_on_failure: r.stop_on_failure,
            tags: json_or_default(&r.tags),
            checks: vec![], // populated separately if needed
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }))
    }

    /// Delete a check group by ID.
    pub async fn delete_check_group(&self, id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let deleted = qontinui_db::queries::checks::delete_check_group()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG delete_check_group: {}", e))?;
        Ok(deleted.is_some())
    }

    /// Get checks in a group.
    pub async fn get_checks_in_group(&self, group_id: &str) -> Result<Vec<Check>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::checks::get_checks_in_group()
            .bind(&conn, &group_id)
            .all()
            .await
            .map_err(|e| format!("PG get_checks_in_group: {}", e))?;
        Ok(rows.into_iter().map(|r| row_to_check!(r)).collect())
    }

    /// Create a new check.
    pub async fn create_check(&self, input: &crate::database::types::CreateCheckInput) -> Result<Check, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let id = format!("check-{}", uuid::Uuid::new_v4());
        let tags_json = serde_json::to_string(&input.tags).unwrap_or_else(|_| "[]".to_string());
        let timeout: Option<i32> = input.timeout_seconds.map(|t| t as i32);

        qontinui_db::queries::checks::create_check()
            .bind(
                &conn,
                &id.as_str(),
                &input.name.as_str(),
                &input.description.as_deref(),
                &input.check_type.as_str(),
                &input.tool.as_str(),
                &input.command.as_deref(),
                &input.working_directory.as_deref(),
                &input.config_path.as_deref(),
                &input.auto_fix,
                &input.fail_on_warning,
                &timeout,
                &input.is_critical,
                &input.enabled,
                &input.ai_generated,
                &input.ai_generation_prompt.as_deref(),
                &tags_json.as_str(),
            )
            .one()
            .await
            .map_err(|e| format!("PG create_check: {}", e))?;

        self.get_check(&id).await?
            .ok_or_else(|| "Failed to retrieve created check".to_string())
    }

    /// Update a check.
    pub async fn update_check(&self, id: &str, input: &crate::database::types::UpdateCheckInput) -> Result<Check, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let tags_json = input.tags.as_ref().map(|t| serde_json::to_string(t).unwrap_or_else(|_| "[]".to_string()));
        let timeout: Option<i32> = input.timeout_seconds.map(|t| t as i32);

        qontinui_db::queries::checks::update_check()
            .bind(
                &conn,
                &input.name.as_deref(),
                &input.description.as_deref(),
                &input.check_type.as_deref(),
                &input.tool.as_deref(),
                &input.command.as_deref(),
                &input.working_directory.as_deref(),
                &input.config_path.as_deref(),
                &input.auto_fix,
                &input.fail_on_warning,
                &timeout,
                &input.is_critical,
                &input.enabled,
                &input.ai_generation_prompt.as_deref(),
                &tags_json.as_deref(),
                &id,
            )
            .opt()
            .await
            .map_err(|e| format!("PG update_check: {}", e))?;

        self.get_check(id).await?
            .ok_or_else(|| format!("Check not found after update: {}", id))
    }

    /// Get a single verification test by ID.
    pub async fn get_verification_test(&self, id: &str) -> Result<Option<VerificationTest>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                r#"SELECT id, name, description, test_type, timeout_seconds, enabled, tags,
                          created_at, updated_at
                   FROM verification_tests
                   WHERE id = $1"#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_verification_test: {}", e))?;

        Ok(row.map(|r| {
            let test_type_str: String = r.get(3);
            let tags_str: Option<String> = r.get(6);
            let created: chrono::DateTime<chrono::Utc> = r.get(7);
            let updated: chrono::DateTime<chrono::Utc> = r.get(8);

            VerificationTest {
                id: r.get(0),
                name: r.get(1),
                description: r.get(2),
                test_type: test_type_str.parse().unwrap_or(TestType::PythonScript),
                category: None,
                playwright_code: None,
                vision_config: None,
                python_code: None,
                repo_test_config: None,
                success_criteria: None,
                config: serde_json::json!({}),
                timeout_seconds: r.get::<_, Option<i32>>(4).map(|v| v as u32),
                is_critical: false,
                enabled: r.get(5),
                ai_generated: false,
                ai_generation_prompt: None,
                creation_analysis: None,
                tags: tags_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                source_file: None,
                last_exported_at: None,
                created_at: created.to_rfc3339(),
                updated_at: updated.to_rfc3339(),
            }
        }))
    }

    /// Create a check group.
    pub async fn create_check_group(&self, input: &crate::database::types::CreateCheckGroupInput) -> Result<CheckGroup, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let id = format!("cg-{}", uuid::Uuid::new_v4());
        let tags_json = serde_json::to_string(&input.tags).unwrap_or_else(|_| "[]".to_string());

        qontinui_db::queries::checks::create_check_group()
            .bind(
                &conn,
                &id.as_str(),
                &input.name.as_str(),
                &input.description.as_deref(),
                &input.color.as_deref(),
                &input.enabled,
                &input.run_in_parallel,
                &input.stop_on_failure,
                &tags_json.as_str(),
            )
            .one()
            .await
            .map_err(|e| format!("PG create_check_group: {}", e))?;

        self.get_check_group(&id).await?
            .ok_or_else(|| "Failed to retrieve created check group".to_string())
    }

    /// List all check groups (optionally filtered to enabled only).
    pub async fn list_check_groups(&self, enabled_only: bool) -> Result<Vec<CheckGroup>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let sql = if enabled_only {
            r#"SELECT id, name, description, color, enabled, run_in_parallel,
                      stop_on_failure, tags, created_at, updated_at
               FROM check_groups WHERE enabled = true ORDER BY name ASC"#
        } else {
            r#"SELECT id, name, description, color, enabled, run_in_parallel,
                      stop_on_failure, tags, created_at, updated_at
               FROM check_groups ORDER BY name ASC"#
        };

        let rows = conn.query(sql, &[]).await
            .map_err(|e| format!("PG list_check_groups: {}", e))?;

        let mut groups = Vec::new();
        for r in &rows {
            let tags_str: String = r.get(7);
            let created: chrono::DateTime<chrono::Utc> = r.get(8);
            let updated: chrono::DateTime<chrono::Utc> = r.get(9);
            let id: String = r.get(0);

            let checks = self.get_checks_in_group(&id).await.unwrap_or_default();

            groups.push(CheckGroup {
                id,
                name: r.get(1),
                description: non_empty(r.get::<_, String>(2)),
                color: non_empty(r.get::<_, String>(3)),
                enabled: r.get(4),
                run_in_parallel: r.get(5),
                stop_on_failure: r.get(6),
                tags: json_or_default(&tags_str),
                checks,
                created_at: created.to_rfc3339(),
                updated_at: updated.to_rfc3339(),
            });
        }

        Ok(groups)
    }

    /// Update a check group.
    pub async fn update_check_group(&self, id: &str, input: &crate::database::types::UpdateCheckGroupInput) -> Result<CheckGroup, String> {
        let existing = self.get_check_group(id).await?
            .ok_or_else(|| format!("Check group not found: {}", id))?;

        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let name = input.name.as_ref().unwrap_or(&existing.name);
        let description = input.description.clone().or(existing.description);
        let color = input.color.clone().or(existing.color);
        let enabled = input.enabled.unwrap_or(existing.enabled);
        let run_in_parallel = input.run_in_parallel.unwrap_or(existing.run_in_parallel);
        let stop_on_failure = input.stop_on_failure.unwrap_or(existing.stop_on_failure);
        let tags = input.tags.clone().unwrap_or(existing.tags);
        let tags_json = serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string());

        let rows = conn.execute(
            r#"UPDATE check_groups SET
                name = $1, description = $2, color = $3, enabled = $4,
                run_in_parallel = $5, stop_on_failure = $6, tags = $7,
                updated_at = NOW()
            WHERE id = $8"#,
            &[
                &name as &(dyn tokio_postgres::types::ToSql + Sync),
                &description as &(dyn tokio_postgres::types::ToSql + Sync),
                &color as &(dyn tokio_postgres::types::ToSql + Sync),
                &enabled as &(dyn tokio_postgres::types::ToSql + Sync),
                &run_in_parallel as &(dyn tokio_postgres::types::ToSql + Sync),
                &stop_on_failure as &(dyn tokio_postgres::types::ToSql + Sync),
                &tags_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        ).await.map_err(|e| format!("PG update_check_group: {}", e))?;

        if rows == 0 {
            return Err(format!("Check group not found: {}", id));
        }

        self.get_check_group(id).await?
            .ok_or_else(|| "Failed to retrieve updated check group".to_string())
    }

    /// Set checks in a group (replace all existing assignments).
    pub async fn set_checks_in_group(&self, group_id: &str, check_ids: &[String]) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        // Remove all existing members
        conn.execute(
            "DELETE FROM check_group_members WHERE group_id = $1",
            &[&group_id],
        ).await.map_err(|e| format!("PG set_checks_in_group (delete): {}", e))?;

        // Add new members
        for (index, check_id) in check_ids.iter().enumerate() {
            let id = uuid::Uuid::new_v4().to_string();
            let sort_order = index as i32;
            conn.execute(
                r#"INSERT INTO check_group_members (id, group_id, check_id, sort_order, created_at)
                   VALUES ($1, $2, $3, $4, NOW())
                   ON CONFLICT (group_id, check_id) DO UPDATE SET sort_order = EXCLUDED.sort_order"#,
                &[
                    &id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &group_id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &check_id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &sort_order as &(dyn tokio_postgres::types::ToSql + Sync),
                ],
            ).await.map_err(|e| format!("PG set_checks_in_group (insert): {}", e))?;
        }

        Ok(())
    }

    /// Repair check-group associations based on naming convention.
    ///
    /// Checks named "{group_name} - {tool_name}" are linked to matching groups.
    /// Returns the number of associations created.
    pub async fn repair_check_group_associations(&self) -> Result<usize, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows_affected = conn.execute(
            r#"INSERT INTO check_group_members (id, group_id, check_id, sort_order, created_at)
               SELECT
                   gen_random_uuid()::text,
                   cg.id,
                   c.id,
                   COALESCE((SELECT MAX(sort_order) + 1 FROM check_group_members WHERE group_id = cg.id), 0),
                   NOW()
               FROM checks c
               JOIN check_groups cg ON c.name LIKE cg.name || ' - %'
               WHERE NOT EXISTS (
                   SELECT 1 FROM check_group_members cgm
                   WHERE cgm.group_id = cg.id AND cgm.check_id = c.id
               )
               ON CONFLICT DO NOTHING"#,
            &[],
        ).await.map_err(|e| format!("PG repair_check_group_associations: {}", e))?;

        if rows_affected > 0 {
            tracing::info!(
                "Repaired {} check-group associations based on naming convention",
                rows_affected
            );
        }

        Ok(rows_affected as usize)
    }

    // ========================================================================
    // Check Results (raw SQL)
    // ========================================================================

    /// Save a check execution result.
    pub async fn save_check_result(
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"INSERT INTO check_results (
                id, check_id, task_run_id, status,
                started_at, completed_at, duration_ms,
                output, error_message, issues_found,
                issues_fixed, files_checked, structured_output,
                created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)"#,
            &[
                &id,
                &check_id,
                &task_run_id,
                &status,
                &started_at,
                &completed_at,
                &duration_ms,
                &output,
                &error_message,
                &(issues_found as i64),
                &(issues_fixed as i64),
                &(files_checked as i64),
                &structured_output,
                &now,
            ],
        )
        .await
        .map_err(|e| format!("PG save_check_result: {}", e))?;

        Ok(id)
    }

    /// Get check results for a check, ordered by creation date descending.
    pub async fn get_check_results(
        &self,
        check_id: &str,
        limit: u32,
    ) -> Result<Vec<crate::database::types::CheckResult>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let limit_i64 = limit as i64;

        let rows = conn
            .query(
                r#"SELECT id, check_id, task_run_id, status,
                       started_at, completed_at, duration_ms,
                       output, error_message, issues_found,
                       issues_fixed, files_checked, structured_output,
                       created_at
                FROM check_results
                WHERE check_id = $1
                ORDER BY created_at DESC
                LIMIT $2"#,
                &[&check_id, &limit_i64],
            )
            .await
            .map_err(|e| format!("PG get_check_results: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| crate::database::types::CheckResult {
                id: r.get(0),
                check_id: r.get(1),
                task_run_id: r.get(2),
                status: r.get(3),
                started_at: r.get(4),
                completed_at: r.get(5),
                duration_ms: r.get(6),
                output: r.get(7),
                error_message: r.get(8),
                issues_found: r.get::<_, Option<i64>>(9).unwrap_or(0) as i32,
                issues_fixed: r.get::<_, Option<i64>>(10).unwrap_or(0) as i32,
                files_checked: r.get::<_, Option<i64>>(11).unwrap_or(0) as i32,
                structured_output: r.get(12),
                created_at: r.get(13),
            })
            .collect())
    }

    /// Get latest check results for a check (alias for get_check_results).
    pub async fn get_latest_check_results(
        &self,
        check_id: &str,
        limit: u32,
    ) -> Result<Vec<crate::database::types::CheckResult>, String> {
        self.get_check_results(check_id, limit).await
    }
}
