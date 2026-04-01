//! PostgreSQL operations for small CRUD tables:
//! shell commands, saved API requests, MCP servers.

use super::PgDb;

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

fn json_or_default<T: serde::de::DeserializeOwned + Default>(s: &str) -> T {
    if s.is_empty() { T::default() } else { serde_json::from_str(s).unwrap_or_default() }
}

impl PgDb {
    // ========================================================================
    // Shell Commands
    // ========================================================================

    /// Get a shell command by ID.
    pub async fn get_shell_command(
        &self,
        id: &str,
    ) -> Result<Option<crate::database::types::ShellCommand>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = qontinui_db::queries::misc_crud::get_shell_command()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_shell_command: {}", e))?;

        Ok(row.map(|r| crate::database::types::ShellCommand {
            id: r.id,
            name: r.name,
            description: non_empty(r.description),
            command: r.command,
            working_directory: non_empty(r.working_directory),
            timeout_seconds: r.timeout_seconds,
            fail_on_error: r.fail_on_error,
            category: r.category,
            tags: json_or_default(&r.tags),
            enabled: r.enabled,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }))
    }

    /// Create a shell command.
    pub async fn create_shell_command(
        &self,
        input: &crate::database::types::CreateShellCommandInput,
    ) -> Result<crate::database::types::ShellCommand, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let id = format!("sc-{}", uuid::Uuid::new_v4());
        let tags_json = serde_json::to_string(&input.tags).unwrap_or_else(|_| "[]".to_string());
        let timeout: Option<i32> = if input.timeout_seconds > 0 { Some(input.timeout_seconds) } else { None };

        qontinui_db::queries::misc_crud::create_shell_command()
            .bind(
                &conn,
                &id.as_str(),
                &input.name.as_str(),
                &input.description.as_deref(),
                &input.command.as_str(),
                &input.working_directory.as_deref(),
                &timeout,
                &input.fail_on_error,
                &input.category.as_str(),
                &tags_json.as_str(),
                &input.enabled,
            )
            .one()
            .await
            .map_err(|e| format!("PG create_shell_command: {}", e))?;

        self.get_shell_command(&id).await?
            .ok_or_else(|| "Failed to retrieve created shell command".to_string())
    }

    /// Delete a shell command by ID.
    pub async fn delete_shell_command(&self, id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let deleted = qontinui_db::queries::misc_crud::delete_shell_command()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG delete_shell_command: {}", e))?;
        Ok(deleted.is_some())
    }

    // ========================================================================
    // Saved API Requests
    // ========================================================================

    /// List all saved API requests (returns JSON values).
    pub async fn list_saved_api_requests(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::misc_crud::list_saved_api_requests()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG list_saved_api_requests: {}", e))?;

        Ok(rows.into_iter().map(|r| serde_json::json!({
            "id": r.id,
            "name": r.name,
            "description": non_empty(r.description),
            "category": r.category,
            "tags": json_or_default::<Vec<String>>(&r.tags),
            "method": r.method,
            "url": r.url,
            "headers": serde_json::from_str::<serde_json::Value>(&r.headers).unwrap_or_default(),
            "body": non_empty(r.body),
            "body_content_type": r.body_content_type,
            "timeout_ms": r.timeout_ms,
            "follow_redirects": r.follow_redirects,
            "variable_extractions": serde_json::from_str::<serde_json::Value>(&r.variable_extractions).unwrap_or_default(),
            "assertions": serde_json::from_str::<serde_json::Value>(&r.assertions).unwrap_or_default(),
            "credential_id": non_empty(r.credential_id),
            "created_at": r.created_at.to_rfc3339(),
            "updated_at": r.updated_at.to_rfc3339(),
        })).collect())
    }

    /// Get a saved API request by ID.
    pub async fn get_saved_api_request(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = qontinui_db::queries::misc_crud::get_saved_api_request()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_saved_api_request: {}", e))?;

        Ok(row.map(|r| serde_json::json!({
            "id": r.id,
            "name": r.name,
            "description": non_empty(r.description),
            "category": r.category,
            "tags": json_or_default::<Vec<String>>(&r.tags),
            "method": r.method,
            "url": r.url,
            "headers": serde_json::from_str::<serde_json::Value>(&r.headers).unwrap_or_default(),
            "body": non_empty(r.body),
            "body_content_type": r.body_content_type,
            "timeout_ms": r.timeout_ms,
            "follow_redirects": r.follow_redirects,
            "variable_extractions": serde_json::from_str::<serde_json::Value>(&r.variable_extractions).unwrap_or_default(),
            "assertions": serde_json::from_str::<serde_json::Value>(&r.assertions).unwrap_or_default(),
            "credential_id": non_empty(r.credential_id),
            "created_at": r.created_at.to_rfc3339(),
            "updated_at": r.updated_at.to_rfc3339(),
        })))
    }

    /// Delete a saved API request by ID.
    pub async fn delete_saved_api_request(&self, id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let deleted = qontinui_db::queries::misc_crud::delete_saved_api_request()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG delete_saved_api_request: {}", e))?;
        Ok(deleted.is_some())
    }

    /// Get distinct tags from saved API requests.
    pub async fn get_saved_api_request_tags(&self) -> Result<Vec<String>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::misc_crud::get_saved_api_request_tags()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG get_saved_api_request_tags: {}", e))?;

        // Each row has a 'tags' JSON array string — parse and collect unique tags
        let mut all_tags = std::collections::HashSet::new();
        for tags_str in rows {
            if let Ok(tags) = serde_json::from_str::<Vec<String>>(&tags_str) {
                for tag in tags {
                    all_tags.insert(tag);
                }
            }
        }
        let mut result: Vec<String> = all_tags.into_iter().collect();
        result.sort();
        Ok(result)
    }

    // ========================================================================
    // MCP Servers
    // ========================================================================

    /// List all MCP servers.
    pub async fn list_mcp_servers(&self) -> Result<Vec<crate::mcp_client::McpServerConfig>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::misc_crud::list_mcp_servers()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG list_mcp_servers: {}", e))?;

        Ok(rows.into_iter().map(|r| crate::mcp_client::McpServerConfig {
            id: r.id,
            name: r.name,
            description: non_empty(r.description),
            transport: match r.transport.as_str() {
                "http" => crate::mcp_client::McpTransport::Http,
                _ => crate::mcp_client::McpTransport::Stdio,
            },
            stdio_config: non_empty(r.stdio_config).and_then(|s| serde_json::from_str(&s).ok()),
            http_config: non_empty(r.http_config).and_then(|s| serde_json::from_str(&s).ok()),
            enabled: r.enabled,
            auto_start: r.auto_start,
            timeout_seconds: r.timeout_seconds as u64,
            cached_tools: non_empty(r.cached_tools).and_then(|s| serde_json::from_str(&s).ok()),
            tools_cached_at: non_empty(r.tools_cached_at),
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }).collect())
    }

    /// Create a new MCP server configuration.
    pub async fn create_mcp_server(
        &self,
        input: crate::mcp_client::CreateMcpServerInput,
    ) -> Result<crate::mcp_client::McpServerConfig, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let id = uuid::Uuid::new_v4().to_string();

        let transport_str = match input.transport {
            crate::mcp_client::McpTransport::Http => "http",
            crate::mcp_client::McpTransport::Stdio => "stdio",
        };
        let stdio_config_json = input
            .stdio_config
            .as_ref()
            .map(|c| serde_json::to_string(c).unwrap_or_default());
        let http_config_json = input
            .http_config
            .as_ref()
            .map(|c| serde_json::to_string(c).unwrap_or_default());
        let enabled = input.enabled.unwrap_or(true);
        let auto_start = input.auto_start.unwrap_or(false);
        let timeout_seconds = input.timeout_seconds.unwrap_or(30) as i64;

        conn.execute(
            r#"INSERT INTO mcp_servers
               (id, name, description, transport, stdio_config, http_config,
                enabled, auto_start, timeout_seconds, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW(), NOW())"#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.name as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.description as &(dyn tokio_postgres::types::ToSql + Sync),
                &transport_str as &(dyn tokio_postgres::types::ToSql + Sync),
                &stdio_config_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &http_config_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &enabled as &(dyn tokio_postgres::types::ToSql + Sync),
                &auto_start as &(dyn tokio_postgres::types::ToSql + Sync),
                &timeout_seconds as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG create_mcp_server: {}", e))?;

        self.get_mcp_server(&id).await?
            .ok_or_else(|| "Failed to retrieve created MCP server".to_string())
    }

    /// Create a new mobile log entry.
    pub async fn create_mobile_log(
        &self,
        input: &crate::database::types::CreateMobileLogInput,
    ) -> Result<crate::database::types::MobileLog, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                r#"INSERT INTO task_run_mobile_logs
                   (task_run_id, mobile_state_id, log_source, log_level, log_tag,
                    message, raw_line, data, error_type, error_code,
                    stack_trace, file_path, line_number, column_number,
                    timestamp, device_timestamp, created_at)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, NOW(), $15, NOW())
                   RETURNING id, task_run_id, mobile_state_id, log_source, log_level, log_tag,
                             message, raw_line, data, error_type, error_code,
                             stack_trace, file_path, line_number, column_number,
                             timestamp, device_timestamp, created_at"#,
                &[
                    &input.task_run_id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.mobile_state_id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.log_source as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.log_level as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.log_tag as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.message as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.raw_line as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.data as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.error_type as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.error_code as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.stack_trace as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.file_path as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.line_number as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.column_number as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.device_timestamp as &(dyn tokio_postgres::types::ToSql + Sync),
                ],
            )
            .await
            .map_err(|e| format!("PG create_mobile_log: {}", e))?;

        let ts: chrono::DateTime<chrono::Utc> = row.get(15);
        let created: chrono::DateTime<chrono::Utc> = row.get(17);

        Ok(crate::database::types::MobileLog {
            id: row.get(0),
            task_run_id: row.get(1),
            mobile_state_id: row.get(2),
            log_source: row.get(3),
            log_level: row.get(4),
            log_tag: row.get(5),
            message: row.get(6),
            raw_line: row.get(7),
            data: row.get(8),
            error_type: row.get(9),
            error_code: row.get(10),
            stack_trace: row.get(11),
            file_path: row.get(12),
            line_number: row.get(13),
            column_number: row.get(14),
            timestamp: ts.to_rfc3339(),
            device_timestamp: row.get(16),
            created_at: created.to_rfc3339(),
        })
    }

    /// Create a new mobile state capture.
    pub async fn create_mobile_state(
        &self,
        input: &crate::database::types::CreateMobileStateInput,
    ) -> Result<crate::database::types::MobileState, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                r#"INSERT INTO task_run_mobile_state
                   (task_run_id, timestamp, device_id, device_type, device_model,
                    app_package, app_activity, app_state, metro_connected, bundle_status,
                    last_reload_type, last_reload_time, screenshot_path, logcat_path,
                    has_errors, error_summary, created_at)
                   VALUES ($1, NOW(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, NOW())
                   RETURNING id, task_run_id, timestamp, device_id, device_type, device_model,
                             app_package, app_activity, app_state, metro_connected, bundle_status,
                             last_reload_type, last_reload_time, screenshot_path, logcat_path,
                             has_errors, error_summary, created_at"#,
                &[
                    &input.task_run_id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.device_id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.device_type as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.device_model as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.app_package as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.app_activity as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.app_state as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.metro_connected as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.bundle_status as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.last_reload_type as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.last_reload_time as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.screenshot_path as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.logcat_path as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.has_errors as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.error_summary as &(dyn tokio_postgres::types::ToSql + Sync),
                ],
            )
            .await
            .map_err(|e| format!("PG create_mobile_state: {}", e))?;

        let ts: chrono::DateTime<chrono::Utc> = row.get(2);
        let created: chrono::DateTime<chrono::Utc> = row.get(17);

        Ok(crate::database::types::MobileState {
            id: row.get(0),
            task_run_id: row.get(1),
            timestamp: ts.to_rfc3339(),
            device_id: row.get(3),
            device_type: row.get(4),
            device_model: row.get(5),
            app_package: row.get(6),
            app_activity: row.get(7),
            app_state: row.get(8),
            metro_connected: row.get::<_, Option<bool>>(9).unwrap_or(false),
            bundle_status: row.get(10),
            last_reload_type: row.get(11),
            last_reload_time: row.get(12),
            screenshot_path: row.get(13),
            logcat_path: row.get(14),
            has_errors: row.get::<_, Option<bool>>(15).unwrap_or(false),
            error_summary: row.get(16),
            created_at: created.to_rfc3339(),
        })
    }

    /// Get a single MCP server by ID.
    pub async fn get_mcp_server(&self, id: &str) -> Result<Option<crate::mcp_client::McpServerConfig>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = qontinui_db::queries::misc_crud::get_mcp_server()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_mcp_server: {}", e))?;

        Ok(row.map(|r| crate::mcp_client::McpServerConfig {
            id: r.id,
            name: r.name,
            description: non_empty(r.description),
            transport: match r.transport.as_str() {
                "http" => crate::mcp_client::McpTransport::Http,
                _ => crate::mcp_client::McpTransport::Stdio,
            },
            stdio_config: non_empty(r.stdio_config).and_then(|s| serde_json::from_str(&s).ok()),
            http_config: non_empty(r.http_config).and_then(|s| serde_json::from_str(&s).ok()),
            enabled: r.enabled,
            auto_start: r.auto_start,
            timeout_seconds: r.timeout_seconds as u64,
            cached_tools: non_empty(r.cached_tools).and_then(|s| serde_json::from_str(&s).ok()),
            tools_cached_at: non_empty(r.tools_cached_at),
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }))
    }

    // ========================================================================
    // Shell Command Results (raw SQL)
    // ========================================================================

    /// Save a shell command execution result.
    pub async fn save_shell_command_result(
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"INSERT INTO shell_command_results (
                id, shell_command_id, task_run_id, status,
                exit_code, stdout, stderr, duration_ms,
                started_at, completed_at, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
            &[
                &id,
                &shell_command_id,
                &task_run_id,
                &status,
                &exit_code,
                &stdout,
                &stderr,
                &duration_ms,
                &started_at,
                &completed_at,
                &now,
            ],
        )
        .await
        .map_err(|e| format!("PG save_shell_command_result: {}", e))?;

        Ok(id)
    }

    /// Get shell command results, ordered by creation date descending.
    pub async fn get_shell_command_results(
        &self,
        shell_command_id: &str,
        limit: u32,
    ) -> Result<Vec<crate::database::types::ShellCommandResult>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let limit_i64 = limit as i64;

        let rows = conn
            .query(
                r#"SELECT id, shell_command_id, task_run_id, status,
                       exit_code, stdout, stderr, duration_ms,
                       started_at, completed_at, created_at
                FROM shell_command_results
                WHERE shell_command_id = $1
                ORDER BY created_at DESC
                LIMIT $2"#,
                &[&shell_command_id, &limit_i64],
            )
            .await
            .map_err(|e| format!("PG get_shell_command_results: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| crate::database::types::ShellCommandResult {
                id: r.get(0),
                shell_command_id: r.get(1),
                task_run_id: r.get(2),
                status: r.get(3),
                exit_code: r.get(4),
                stdout: r.get(5),
                stderr: r.get(6),
                duration_ms: r.get(7),
                started_at: r.get(8),
                completed_at: r.get(9),
                created_at: r.get(10),
            })
            .collect())
    }

    /// Get mobile states for a task run.
    pub async fn get_mobile_states(
        &self,
        task_run_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<crate::database::types::MobileState>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let limit_i64 = limit.unwrap_or(100) as i64;

        let rows = conn
            .query(
                r#"SELECT id, task_run_id, timestamp, device_id, device_type, device_model,
                       app_package, app_activity, app_state, metro_connected, bundle_status,
                       last_reload_type, last_reload_time, screenshot_path, logcat_path,
                       has_errors, error_summary, created_at
                FROM task_run_mobile_state
                WHERE task_run_id = $1
                ORDER BY created_at DESC
                LIMIT $2"#,
                &[&task_run_id, &limit_i64],
            )
            .await
            .map_err(|e| format!("PG get_mobile_states: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let ts: chrono::DateTime<chrono::Utc> = r.get(2);
                let created: chrono::DateTime<chrono::Utc> = r.get(17);
                crate::database::types::MobileState {
                    id: r.get(0),
                    task_run_id: r.get(1),
                    timestamp: ts.to_rfc3339(),
                    device_id: r.get(3),
                    device_type: r.get(4),
                    device_model: r.get(5),
                    app_package: r.get(6),
                    app_activity: r.get(7),
                    app_state: r.get(8),
                    metro_connected: r.get(9),
                    bundle_status: r.get(10),
                    last_reload_type: r.get(11),
                    last_reload_time: r.get(12),
                    screenshot_path: r.get(13),
                    logcat_path: r.get(14),
                    has_errors: r.get(15),
                    error_summary: r.get(16),
                    created_at: created.to_rfc3339(),
                }
            })
            .collect())
    }

    /// Get mobile logs for a task run.
    pub async fn get_mobile_logs(
        &self,
        task_run_id: &str,
        log_source: Option<&str>,
        errors_only: bool,
        limit: Option<u32>,
    ) -> Result<Vec<crate::database::types::MobileLog>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let limit_i64 = limit.unwrap_or(100) as i64;

        let rows = if errors_only {
            conn.query(
                r#"SELECT id, task_run_id, mobile_state_id, log_source, log_level, log_tag,
                       message, raw_line, data, error_type, error_code,
                       stack_trace, file_path, line_number, column_number,
                       timestamp, device_timestamp, created_at
                FROM task_run_mobile_logs
                WHERE task_run_id = $1 AND log_level IN ('error', 'fatal')
                ORDER BY created_at DESC LIMIT $2"#,
                &[&task_run_id, &limit_i64],
            ).await
        } else if let Some(source) = log_source {
            conn.query(
                r#"SELECT id, task_run_id, mobile_state_id, log_source, log_level, log_tag,
                       message, raw_line, data, error_type, error_code,
                       stack_trace, file_path, line_number, column_number,
                       timestamp, device_timestamp, created_at
                FROM task_run_mobile_logs
                WHERE task_run_id = $1 AND log_source = $2
                ORDER BY created_at DESC LIMIT $3"#,
                &[&task_run_id, &source, &limit_i64],
            ).await
        } else {
            conn.query(
                r#"SELECT id, task_run_id, mobile_state_id, log_source, log_level, log_tag,
                       message, raw_line, data, error_type, error_code,
                       stack_trace, file_path, line_number, column_number,
                       timestamp, device_timestamp, created_at
                FROM task_run_mobile_logs
                WHERE task_run_id = $1
                ORDER BY created_at DESC LIMIT $2"#,
                &[&task_run_id, &limit_i64],
            ).await
        }.map_err(|e| format!("PG get_mobile_logs: {}", e))?;

        Ok(rows.iter().map(|r| {
            let ts: chrono::DateTime<chrono::Utc> = r.get(15);
            let created: chrono::DateTime<chrono::Utc> = r.get(17);
            crate::database::types::MobileLog {
                id: r.get(0),
                task_run_id: r.get(1),
                mobile_state_id: r.get(2),
                log_source: r.get(3),
                log_level: r.get(4),
                log_tag: r.get(5),
                message: r.get(6),
                raw_line: r.get(7),
                data: r.get(8),
                error_type: r.get(9),
                error_code: r.get(10),
                stack_trace: r.get(11),
                file_path: r.get(12),
                line_number: r.get(13),
                column_number: r.get(14),
                timestamp: ts.to_rfc3339(),
                device_timestamp: r.get(16),
                created_at: created.to_rfc3339(),
            }
        }).collect())
    }

    /// Get mobile error logs for a task run.
    pub async fn get_mobile_errors(
        &self,
        task_run_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<crate::database::types::MobileLog>, String> {
        self.get_mobile_logs(task_run_id, None, true, limit).await
    }

    // ========================================================================
    // Artifacts (UI Bridge)
    // ========================================================================

    /// Save an artifact to the database.
    pub async fn save_artifact(&self, artifact: &crate::database::types::Artifact) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            r#"INSERT INTO artifacts (artifact_id, source_json, result_json, environment_json, passed, created_at)
               VALUES ($1, $2, $3, $4, $5, NOW())
               ON CONFLICT (artifact_id) DO NOTHING"#,
            &[
                &artifact.artifact_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &artifact.source_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &artifact.result_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &artifact.environment_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &artifact.passed as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        ).await.map_err(|e| format!("PG save_artifact: {}", e))?;
        Ok(())
    }

    /// Get an artifact by ID.
    pub async fn get_artifact(&self, artifact_id: &str) -> Result<Option<crate::database::types::Artifact>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_opt(
            "SELECT artifact_id, source_json, result_json, environment_json, created_at, passed FROM artifacts WHERE artifact_id = $1",
            &[&artifact_id],
        ).await.map_err(|e| format!("PG get_artifact: {}", e))?;

        Ok(row.map(|r| {
            let created: chrono::DateTime<chrono::Utc> = r.get(4);
            crate::database::types::Artifact {
                artifact_id: r.get(0),
                source_json: r.get(1),
                result_json: r.get(2),
                environment_json: r.get(3),
                created_at: created.to_rfc3339(),
                passed: r.get(5),
            }
        }))
    }

    /// Query artifacts with filters.
    pub async fn query_artifacts(&self, query: &crate::database::types::ArtifactQuery) -> Result<Vec<crate::database::types::Artifact>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let limit_i64 = query.limit.unwrap_or(100) as i64;
        let offset_i64 = query.offset.unwrap_or(0) as i64;

        // Build dynamic query
        let mut conditions = vec!["1=1".to_string()];
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        let mut idx = 1u32;

        if let Some(ref spec_id) = query.spec_id {
            conditions.push(format!("source_json::jsonb->>'specId' = ${}", idx));
            params.push(Box::new(spec_id.clone()));
            idx += 1;
        }
        if let Some(ref date_from) = query.date_from {
            conditions.push(format!("created_at >= ${}::timestamptz", idx));
            params.push(Box::new(date_from.clone()));
            idx += 1;
        }
        if let Some(ref date_to) = query.date_to {
            conditions.push(format!("created_at <= ${}::timestamptz", idx));
            params.push(Box::new(date_to.clone()));
            idx += 1;
        }
        if query.passed_only == Some(true) {
            conditions.push("passed = true".to_string());
        }
        if query.failed_only == Some(true) {
            conditions.push("passed = false".to_string());
        }

        let sql = format!(
            "SELECT artifact_id, source_json, result_json, environment_json, created_at, passed FROM artifacts WHERE {} ORDER BY created_at DESC LIMIT ${} OFFSET ${}",
            conditions.join(" AND "), idx, idx + 1
        );
        params.push(Box::new(limit_i64));
        params.push(Box::new(offset_i64));

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let rows = conn.query(&sql, &param_refs).await.map_err(|e| format!("PG query_artifacts: {}", e))?;

        Ok(rows.iter().map(|r| {
            let created: chrono::DateTime<chrono::Utc> = r.get(4);
            crate::database::types::Artifact {
                artifact_id: r.get(0),
                source_json: r.get(1),
                result_json: r.get(2),
                environment_json: r.get(3),
                created_at: created.to_rfc3339(),
                passed: r.get(5),
            }
        }).collect())
    }

    /// Count artifacts with optional filters.
    pub async fn count_artifacts(&self, query: &crate::database::types::ArtifactCountQuery) -> Result<i64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let mut conditions = vec!["1=1".to_string()];
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        let mut idx = 1u32;

        if let Some(ref spec_id) = query.spec_id {
            conditions.push(format!("source_json::jsonb->>'specId' = ${}", idx));
            params.push(Box::new(spec_id.clone()));
            idx += 1;
        }
        if let Some(ref date_from) = query.date_from {
            conditions.push(format!("created_at >= ${}::timestamptz", idx));
            params.push(Box::new(date_from.clone()));
            idx += 1;
        }
        if let Some(ref date_to) = query.date_to {
            conditions.push(format!("created_at <= ${}::timestamptz", idx));
            params.push(Box::new(date_to.clone()));
            idx += 1;
        }
        if query.passed_only == Some(true) {
            conditions.push("passed = true".to_string());
        }
        if query.failed_only == Some(true) {
            conditions.push("passed = false".to_string());
        }

        let _ = idx;
        let sql = format!(
            "SELECT COUNT(*) FROM artifacts WHERE {}",
            conditions.join(" AND ")
        );
        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let row = conn.query_one(&sql, &param_refs).await.map_err(|e| format!("PG count_artifacts: {}", e))?;
        Ok(row.get(0))
    }
}
