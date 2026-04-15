//! PostgreSQL operations for small CRUD tables:
//! shell commands, saved API requests, MCP servers.

use super::PgDb;

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn json_or_default<T: serde::de::DeserializeOwned + Default>(s: &str) -> T {
    if s.is_empty() {
        T::default()
    } else {
        serde_json::from_str(s).unwrap_or_default()
    }
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let id = format!("sc-{}", uuid::Uuid::new_v4());
        let tags_json = serde_json::to_string(&input.tags).unwrap_or_else(|_| "[]".to_string());
        let timeout: Option<i32> = if input.timeout_seconds > 0 {
            Some(input.timeout_seconds)
        } else {
            None
        };

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

        self.get_shell_command(&id)
            .await?
            .ok_or_else(|| "Failed to retrieve created shell command".to_string())
    }

    /// Delete a shell command by ID.
    pub async fn delete_shell_command(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
    pub async fn get_saved_api_request(
        &self,
        id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let deleted = qontinui_db::queries::misc_crud::delete_saved_api_request()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG delete_saved_api_request: {}", e))?;
        Ok(deleted.is_some())
    }

    /// Create a new saved API request.
    pub async fn create_saved_api_request(
        &self,
        input: &crate::saved_api_requests::CreateSavedApiRequestRequest,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = format!("sar-{}", uuid::Uuid::new_v4());
        let method_str = serde_json::to_string(&input.method)
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_else(|_| "GET".to_string());
        let headers_json =
            serde_json::to_string(&input.headers).unwrap_or_else(|_| "{}".to_string());
        let tags_json = serde_json::to_string(&input.tags).unwrap_or_else(|_| "[]".to_string());
        let extractions_json =
            serde_json::to_string(&input.variable_extractions).unwrap_or_else(|_| "[]".to_string());
        let assertions_json =
            serde_json::to_string(&input.assertions).unwrap_or_else(|_| "[]".to_string());
        let timeout_ms: i32 = input.timeout_ms as i32;

        conn.execute(
            r#"
            INSERT INTO saved_api_requests (
                id, name, description, category, tags, method, url, headers,
                body, body_content_type, timeout_ms, follow_redirects,
                variable_extractions, assertions, credential_id,
                created_at, updated_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15,
                NOW(), NOW()
            )
            "#,
            &[
                &id,
                &input.name,
                &input.description,
                &input.category,
                &tags_json,
                &method_str,
                &input.url,
                &headers_json,
                &input.body,
                &input.body_content_type,
                &timeout_ms,
                &input.follow_redirects,
                &extractions_json,
                &assertions_json,
                &input.credential_id,
            ],
        )
        .await
        .map_err(|e| format!("PG create_saved_api_request: {}", e))?;

        self.get_saved_api_request(&id)
            .await?
            .ok_or_else(|| "Failed to retrieve created saved API request".to_string())
    }

    /// Update an existing saved API request.
    /// Applies only the fields present in `input`; unset fields keep their current values.
    pub async fn update_saved_api_request(
        &self,
        id: &str,
        input: &crate::saved_api_requests::UpdateSavedApiRequestRequest,
    ) -> Result<Option<serde_json::Value>, String> {
        let current = match self.get_saved_api_request(id).await? {
            Some(v) => v,
            None => return Ok(None),
        };

        let name = input
            .name
            .clone()
            .unwrap_or_else(|| current["name"].as_str().unwrap_or("").to_string());
        let description = input
            .description
            .clone()
            .unwrap_or_else(|| current["description"].as_str().unwrap_or("").to_string());
        let category = input
            .category
            .clone()
            .unwrap_or_else(|| current["category"].as_str().unwrap_or("").to_string());
        let tags_json = match &input.tags {
            Some(t) => serde_json::to_string(t).unwrap_or_else(|_| "[]".to_string()),
            None => serde_json::to_string(&current["tags"]).unwrap_or_else(|_| "[]".to_string()),
        };
        let method_str = match &input.method {
            Some(m) => serde_json::to_string(m)
                .map(|s| s.trim_matches('"').to_string())
                .unwrap_or_else(|_| "GET".to_string()),
            None => current["method"].as_str().unwrap_or("GET").to_string(),
        };
        let url = input
            .url
            .clone()
            .unwrap_or_else(|| current["url"].as_str().unwrap_or("").to_string());
        let headers_json = match &input.headers {
            Some(h) => serde_json::to_string(h).unwrap_or_else(|_| "{}".to_string()),
            None => serde_json::to_string(&current["headers"]).unwrap_or_else(|_| "{}".to_string()),
        };
        let body: Option<String> = match &input.body {
            Some(b) => Some(b.clone()),
            None => current["body"].as_str().map(|s| s.to_string()),
        };
        let body_content_type: Option<String> = match &input.body_content_type {
            Some(b) => Some(b.clone()),
            None => current["body_content_type"].as_str().map(|s| s.to_string()),
        };
        let timeout_ms: i32 = match input.timeout_ms {
            Some(t) => t as i32,
            None => current["timeout_ms"].as_i64().unwrap_or(30000) as i32,
        };
        let follow_redirects: bool = input
            .follow_redirects
            .unwrap_or_else(|| current["follow_redirects"].as_bool().unwrap_or(true));
        let extractions_json = match &input.variable_extractions {
            Some(e) => serde_json::to_string(e).unwrap_or_else(|_| "[]".to_string()),
            None => serde_json::to_string(&current["variable_extractions"])
                .unwrap_or_else(|_| "[]".to_string()),
        };
        let assertions_json = match &input.assertions {
            Some(a) => serde_json::to_string(a).unwrap_or_else(|_| "[]".to_string()),
            None => {
                serde_json::to_string(&current["assertions"]).unwrap_or_else(|_| "[]".to_string())
            }
        };
        let credential_id: Option<String> = match &input.credential_id {
            Some(c) => Some(c.clone()),
            None => current["credential_id"].as_str().map(|s| s.to_string()),
        };

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let count = conn
            .execute(
                r#"
                UPDATE saved_api_requests SET
                    name = $1, description = $2, category = $3, tags = $4,
                    method = $5, url = $6, headers = $7, body = $8,
                    body_content_type = $9, timeout_ms = $10, follow_redirects = $11,
                    variable_extractions = $12, assertions = $13, credential_id = $14,
                    updated_at = NOW()
                WHERE id = $15
                "#,
                &[
                    &name,
                    &description,
                    &category,
                    &tags_json,
                    &method_str,
                    &url,
                    &headers_json,
                    &body,
                    &body_content_type,
                    &timeout_ms,
                    &follow_redirects,
                    &extractions_json,
                    &assertions_json,
                    &credential_id,
                    &id,
                ],
            )
            .await
            .map_err(|e| format!("PG update_saved_api_request: {}", e))?;

        if count == 0 {
            return Ok(None);
        }

        self.get_saved_api_request(id).await
    }

    /// Search saved API requests by name/description/url, category, and tag.
    pub async fn search_saved_api_requests(
        &self,
        query: Option<&str>,
        category: Option<&str>,
        tag: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let mut conditions: Vec<String> = vec!["1=1".to_string()];
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        let mut idx = 1u32;

        if let Some(q) = query {
            if !q.is_empty() {
                let pattern = format!("%{}%", q);
                conditions.push(format!(
                    "(name ILIKE ${idx} OR description ILIKE ${idx} OR url ILIKE ${idx})",
                    idx = idx
                ));
                params.push(Box::new(pattern));
                idx += 1;
            }
        }

        if let Some(cat) = category {
            if !cat.is_empty() {
                conditions.push(format!("category = ${}", idx));
                params.push(Box::new(cat.to_string()));
                idx += 1;
            }
        }

        if let Some(t) = tag {
            if !t.is_empty() {
                conditions.push(format!("tags LIKE ${}", idx));
                params.push(Box::new(format!("%\"{}\"%", t)));
                idx += 1;
            }
        }

        let _ = idx;

        let sql = format!(
            r#"
            SELECT id, name, COALESCE(description, '') AS description,
                   COALESCE(category, '') AS category, COALESCE(tags, '[]') AS tags,
                   method, url, COALESCE(headers, '{{}}') AS headers,
                   COALESCE(body, '') AS body,
                   COALESCE(body_content_type, '') AS body_content_type,
                   COALESCE(timeout_ms, 30000) AS timeout_ms,
                   COALESCE(follow_redirects, true) AS follow_redirects,
                   COALESCE(variable_extractions, '[]') AS variable_extractions,
                   COALESCE(assertions, '[]') AS assertions,
                   COALESCE(credential_id, '') AS credential_id,
                   created_at::TEXT, updated_at::TEXT
            FROM saved_api_requests
            WHERE {}
            ORDER BY updated_at DESC
            "#,
            conditions.join(" AND ")
        );

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG search_saved_api_requests: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| {
                let headers_str: String = row.get(7);
                let tags_str: String = row.get(4);
                let extractions_str: String = row.get(12);
                let assertions_str: String = row.get(13);
                serde_json::json!({
                    "id": row.get::<_, String>(0),
                    "name": row.get::<_, String>(1),
                    "description": row.get::<_, String>(2),
                    "category": row.get::<_, String>(3),
                    "tags": serde_json::from_str::<serde_json::Value>(&tags_str).unwrap_or(serde_json::json!([])),
                    "method": row.get::<_, String>(5),
                    "url": row.get::<_, String>(6),
                    "headers": serde_json::from_str::<serde_json::Value>(&headers_str).unwrap_or(serde_json::json!({})),
                    "body": row.get::<_, String>(8),
                    "body_content_type": row.get::<_, String>(9),
                    "timeout_ms": row.get::<_, i32>(10),
                    "follow_redirects": row.get::<_, bool>(11),
                    "variable_extractions": serde_json::from_str::<serde_json::Value>(&extractions_str).unwrap_or(serde_json::json!([])),
                    "assertions": serde_json::from_str::<serde_json::Value>(&assertions_str).unwrap_or(serde_json::json!([])),
                    "credential_id": row.get::<_, String>(14),
                    "created_at": row.get::<_, String>(15),
                    "updated_at": row.get::<_, String>(16),
                })
            })
            .collect())
    }

    /// Get distinct categories from saved API requests.
    pub async fn get_saved_api_request_categories(&self) -> Result<Vec<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT DISTINCT category
                FROM saved_api_requests
                WHERE category IS NOT NULL AND category != ''
                ORDER BY category ASC
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_saved_api_request_categories: {}", e))?;

        Ok(rows.iter().map(|row| row.get::<_, String>(0)).collect())
    }

    /// Duplicate a saved API request (creates a copy with "(Copy)" suffix).
    pub async fn duplicate_saved_api_request(
        &self,
        id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let source = match self.get_saved_api_request(id).await? {
            Some(v) => v,
            None => return Ok(None),
        };

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let new_id = format!("sar-{}", uuid::Uuid::new_v4());
        let name = format!("{} (Copy)", source["name"].as_str().unwrap_or(""));
        let description = source["description"].as_str().unwrap_or("").to_string();
        let category = source["category"].as_str().unwrap_or("").to_string();
        let tags_json = serde_json::to_string(&source["tags"]).unwrap_or_else(|_| "[]".to_string());
        let method_str = source["method"].as_str().unwrap_or("GET").to_string();
        let url = source["url"].as_str().unwrap_or("").to_string();
        let headers_json =
            serde_json::to_string(&source["headers"]).unwrap_or_else(|_| "{}".to_string());
        let body: Option<String> = source["body"].as_str().map(|s| s.to_string());
        let body_content_type: Option<String> =
            source["body_content_type"].as_str().map(|s| s.to_string());
        let timeout_ms: i32 = source["timeout_ms"].as_i64().unwrap_or(30000) as i32;
        let follow_redirects: bool = source["follow_redirects"].as_bool().unwrap_or(true);
        let extractions_json = serde_json::to_string(&source["variable_extractions"])
            .unwrap_or_else(|_| "[]".to_string());
        let assertions_json =
            serde_json::to_string(&source["assertions"]).unwrap_or_else(|_| "[]".to_string());
        let credential_id: Option<String> = source["credential_id"].as_str().map(|s| s.to_string());

        conn.execute(
            r#"
            INSERT INTO saved_api_requests (
                id, name, description, category, tags, method, url, headers,
                body, body_content_type, timeout_ms, follow_redirects,
                variable_extractions, assertions, credential_id,
                created_at, updated_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15,
                NOW(), NOW()
            )
            "#,
            &[
                &new_id,
                &name,
                &description,
                &category,
                &tags_json,
                &method_str,
                &url,
                &headers_json,
                &body,
                &body_content_type,
                &timeout_ms,
                &follow_redirects,
                &extractions_json,
                &assertions_json,
                &credential_id,
            ],
        )
        .await
        .map_err(|e| format!("PG duplicate_saved_api_request: {}", e))?;

        self.get_saved_api_request(&new_id).await
    }

    /// Get distinct tags from saved API requests.
    pub async fn get_saved_api_request_tags(&self) -> Result<Vec<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
    pub async fn list_mcp_servers(
        &self,
    ) -> Result<Vec<crate::mcp_client::McpServerConfig>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::misc_crud::list_mcp_servers()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG list_mcp_servers: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| crate::mcp_client::McpServerConfig {
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
            })
            .collect())
    }

    /// Create a new MCP server configuration.
    pub async fn create_mcp_server(
        &self,
        input: crate::mcp_client::CreateMcpServerInput,
    ) -> Result<crate::mcp_client::McpServerConfig, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
        let timeout_seconds = input.timeout_seconds.unwrap_or(30) as i32;

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

        self.get_mcp_server(&id)
            .await?
            .ok_or_else(|| "Failed to retrieve created MCP server".to_string())
    }

    /// Update an existing MCP server configuration.
    ///
    /// When `transport` is changed, the config for the *old* transport is explicitly
    /// cleared to NULL so the row doesn't retain stale config from the previous
    /// transport type.  When `transport` is `None` (unchanged), the existing
    /// COALESCE pattern keeps the current value unless a new one is supplied.
    pub async fn update_mcp_server(
        &self,
        id: &str,
        input: crate::mcp_client::UpdateMcpServerInput,
    ) -> Result<crate::mcp_client::McpServerConfig, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let transport_str = input.transport.as_ref().map(|t| match t {
            crate::mcp_client::McpTransport::Http => "http".to_string(),
            crate::mcp_client::McpTransport::Stdio => "stdio".to_string(),
        });
        let timeout_seconds = input.timeout_seconds.map(|t| t as i32);

        // When transport is being changed, clear the opposing config to NULL.
        // When transport is unchanged, use normal COALESCE (None = keep current).
        let (stdio_config_json, http_config_json, clear_stdio, clear_http) =
            if let Some(ref transport) = input.transport {
                match transport {
                    crate::mcp_client::McpTransport::Stdio => {
                        // Switching to stdio: update stdio_config if provided, clear http_config
                        (
                            input
                                .stdio_config
                                .as_ref()
                                .map(|c| serde_json::to_string(c).unwrap_or_default()),
                            None::<String>,
                            false,
                            true, // clear http_config
                        )
                    }
                    crate::mcp_client::McpTransport::Http => {
                        // Switching to http: update http_config if provided, clear stdio_config
                        (
                            None::<String>,
                            input
                                .http_config
                                .as_ref()
                                .map(|c| serde_json::to_string(c).unwrap_or_default()),
                            true, // clear stdio_config
                            false,
                        )
                    }
                }
            } else {
                // No transport change — normal COALESCE behavior
                (
                    input
                        .stdio_config
                        .as_ref()
                        .map(|c| serde_json::to_string(c).unwrap_or_default()),
                    input
                        .http_config
                        .as_ref()
                        .map(|c| serde_json::to_string(c).unwrap_or_default()),
                    false,
                    false,
                )
            };

        // Use CASE expressions for stdio_config/http_config so we can express
        // three states: update (param is non-null), clear to NULL (flag param is true),
        // or keep current (COALESCE fallback). Explicit ::TEXT casts are required
        // so PostgreSQL can infer parameter types when branches return NULL.
        let rows_affected = conn
            .execute(
                r#"UPDATE mcp_servers SET
                    name = COALESCE($2::TEXT, name),
                    description = COALESCE($3::TEXT, description),
                    transport = COALESCE($4::TEXT, transport),
                    stdio_config = CASE
                        WHEN $10::BOOLEAN THEN NULL
                        WHEN $5::TEXT IS NOT NULL THEN $5::TEXT
                        ELSE stdio_config
                    END,
                    http_config = CASE
                        WHEN $11::BOOLEAN THEN NULL
                        WHEN $6::TEXT IS NOT NULL THEN $6::TEXT
                        ELSE http_config
                    END,
                    enabled = COALESCE($7::BOOLEAN, enabled),
                    auto_start = COALESCE($8::BOOLEAN, auto_start),
                    timeout_seconds = COALESCE($9::INTEGER, timeout_seconds),
                    updated_at = NOW()
                WHERE id = $1"#,
                &[
                    &id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.name as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.description as &(dyn tokio_postgres::types::ToSql + Sync),
                    &transport_str as &(dyn tokio_postgres::types::ToSql + Sync),
                    &stdio_config_json as &(dyn tokio_postgres::types::ToSql + Sync),
                    &http_config_json as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.enabled as &(dyn tokio_postgres::types::ToSql + Sync),
                    &input.auto_start as &(dyn tokio_postgres::types::ToSql + Sync),
                    &timeout_seconds as &(dyn tokio_postgres::types::ToSql + Sync),
                    &clear_stdio as &(dyn tokio_postgres::types::ToSql + Sync),
                    &clear_http as &(dyn tokio_postgres::types::ToSql + Sync),
                ],
            )
            .await
            .map_err(|e| {
                // Include the DbError source chain for debugging (default Display is "db error")
                let mut msg = format!("PG update_mcp_server: {}", e);
                let mut source = std::error::Error::source(&e);
                while let Some(src) = source {
                    msg.push_str(&format!(" — {}", src));
                    source = src.source();
                }
                msg
            })?;

        if rows_affected == 0 {
            return Err(format!("MCP server not found: {}", id));
        }

        self.get_mcp_server(id)
            .await?
            .ok_or_else(|| "Failed to retrieve updated MCP server".to_string())
    }

    /// Delete an MCP server by ID.
    pub async fn delete_mcp_server(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows_affected = conn
            .execute("DELETE FROM mcp_servers WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_mcp_server: {}", e))?;
        Ok(rows_affected > 0)
    }

    /// List MCP servers marked for auto-start.
    /// Reuses `list_mcp_servers` with a filter to avoid fragile positional column indexing.
    pub async fn list_auto_start_mcp_servers(
        &self,
    ) -> Result<Vec<crate::mcp_client::McpServerConfig>, String> {
        let all = self.list_mcp_servers().await?;
        Ok(all
            .into_iter()
            .filter(|s| s.auto_start && s.enabled)
            .collect())
    }

    /// Update cached tools for an MCP server.
    pub async fn update_mcp_server_cached_tools(
        &self,
        id: &str,
        cached_tools: Option<&str>,
        tools_cached_at: Option<&str>,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows_affected = conn
            .execute(
                r#"UPDATE mcp_servers SET
                    cached_tools = $2,
                    tools_cached_at = $3::TIMESTAMPTZ,
                    updated_at = NOW()
                WHERE id = $1"#,
                &[
                    &id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &cached_tools as &(dyn tokio_postgres::types::ToSql + Sync),
                    &tools_cached_at as &(dyn tokio_postgres::types::ToSql + Sync),
                ],
            )
            .await
            .map_err(|e| format!("PG update_mcp_server_cached_tools: {}", e))?;
        Ok(rows_affected > 0)
    }

    /// Create a new mobile log entry.
    pub async fn create_mobile_log(
        &self,
        input: &crate::database::types::CreateMobileLogInput,
    ) -> Result<crate::database::types::MobileLog, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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
    pub async fn get_mcp_server(
        &self,
        id: &str,
    ) -> Result<Option<crate::mcp_client::McpServerConfig>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
            )
            .await
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
            )
            .await
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
            )
            .await
        }
        .map_err(|e| format!("PG get_mobile_logs: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
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
            })
            .collect())
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
    pub async fn save_artifact(
        &self,
        artifact: &crate::database::types::Artifact,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
    pub async fn get_artifact(
        &self,
        artifact_id: &str,
    ) -> Result<Option<crate::database::types::Artifact>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
    pub async fn query_artifacts(
        &self,
        query: &crate::database::types::ArtifactQuery,
    ) -> Result<Vec<crate::database::types::Artifact>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG query_artifacts: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let created: chrono::DateTime<chrono::Utc> = r.get(4);
                crate::database::types::Artifact {
                    artifact_id: r.get(0),
                    source_json: r.get(1),
                    result_json: r.get(2),
                    environment_json: r.get(3),
                    created_at: created.to_rfc3339(),
                    passed: r.get(5),
                }
            })
            .collect())
    }

    /// Count artifacts with optional filters.
    pub async fn count_artifacts(
        &self,
        query: &crate::database::types::ArtifactCountQuery,
    ) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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
        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let row = conn
            .query_one(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG count_artifacts: {}", e))?;
        Ok(row.get(0))
    }
}
