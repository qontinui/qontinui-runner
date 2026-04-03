//! PostgreSQL log_sources CRUD operations (raw SQL).
//!
//! Mirrors the SQLite LogSourceStorage in error_monitor/storage.rs.

use super::PgDb;
use crate::error_monitor::types::{LogFormat, LogSourceConfig, ParserType, PathType};

impl PgDb {
    // ========================================================================
    // Log Sources
    // ========================================================================

    /// List all log sources ordered by name.
    pub async fn list_log_sources(&self) -> Result<Vec<LogSourceConfig>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, name, description, path, path_type, format, parser,
                       timestamp_pattern, timezone, error_patterns, warning_patterns,
                       ignore_patterns, enabled, poll_interval_ms,
                       created_at::TEXT, updated_at::TEXT
                FROM log_sources
                ORDER BY name
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG list_log_sources: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| Self::log_source_row_to_config(r))
            .collect())
    }

    /// Get a single log source by ID.
    pub async fn get_log_source(&self, id: i64) -> Result<Option<LogSourceConfig>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, name, description, path, path_type, format, parser,
                       timestamp_pattern, timezone, error_patterns, warning_patterns,
                       ignore_patterns, enabled, poll_interval_ms,
                       created_at::TEXT, updated_at::TEXT
                FROM log_sources
                WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_log_source: {}", e))?;

        Ok(row.map(|r| Self::log_source_row_to_config(&r)))
    }

    /// Create a new log source. Returns the assigned ID.
    pub async fn create_log_source(&self, config: &LogSourceConfig) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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
        let poll_ms = config.poll_interval_ms as i32;

        let row = conn
            .query_one(
                r#"
                INSERT INTO log_sources (
                    name, description, path, path_type, format, parser,
                    timestamp_pattern, timezone, error_patterns, warning_patterns,
                    ignore_patterns, enabled, poll_interval_ms
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
                RETURNING id
                "#,
                &[
                    &config.name as &(dyn tokio_postgres::types::ToSql + Sync),
                    &config.description as &(dyn tokio_postgres::types::ToSql + Sync),
                    &config.path as &(dyn tokio_postgres::types::ToSql + Sync),
                    &config.path_type.as_str() as &(dyn tokio_postgres::types::ToSql + Sync),
                    &config.format.as_str() as &(dyn tokio_postgres::types::ToSql + Sync),
                    &config.parser.as_str() as &(dyn tokio_postgres::types::ToSql + Sync),
                    &config.timestamp_pattern as &(dyn tokio_postgres::types::ToSql + Sync),
                    &config.timezone as &(dyn tokio_postgres::types::ToSql + Sync),
                    &error_patterns_json as &(dyn tokio_postgres::types::ToSql + Sync),
                    &warning_patterns_json as &(dyn tokio_postgres::types::ToSql + Sync),
                    &ignore_patterns_json as &(dyn tokio_postgres::types::ToSql + Sync),
                    &config.enabled as &(dyn tokio_postgres::types::ToSql + Sync),
                    &poll_ms as &(dyn tokio_postgres::types::ToSql + Sync),
                ],
            )
            .await
            .map_err(|e| format!("PG create_log_source: {}", e))?;

        let id: i64 = row.get(0);
        Ok(id)
    }

    /// Update an existing log source by ID.
    pub async fn update_log_source(&self, id: i64, config: &LogSourceConfig) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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
        let poll_ms = config.poll_interval_ms as i32;

        conn.execute(
            r#"
            UPDATE log_sources SET
                name = $1, description = $2, path = $3, path_type = $4,
                format = $5, parser = $6, timestamp_pattern = $7, timezone = $8,
                error_patterns = $9, warning_patterns = $10, ignore_patterns = $11,
                enabled = $12, poll_interval_ms = $13, updated_at = NOW()
            WHERE id = $14
            "#,
            &[
                &config.name as &(dyn tokio_postgres::types::ToSql + Sync),
                &config.description as &(dyn tokio_postgres::types::ToSql + Sync),
                &config.path as &(dyn tokio_postgres::types::ToSql + Sync),
                &config.path_type.as_str() as &(dyn tokio_postgres::types::ToSql + Sync),
                &config.format.as_str() as &(dyn tokio_postgres::types::ToSql + Sync),
                &config.parser.as_str() as &(dyn tokio_postgres::types::ToSql + Sync),
                &config.timestamp_pattern as &(dyn tokio_postgres::types::ToSql + Sync),
                &config.timezone as &(dyn tokio_postgres::types::ToSql + Sync),
                &error_patterns_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &warning_patterns_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &ignore_patterns_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &config.enabled as &(dyn tokio_postgres::types::ToSql + Sync),
                &poll_ms as &(dyn tokio_postgres::types::ToSql + Sync),
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG update_log_source: {}", e))?;

        Ok(())
    }

    /// Delete a log source by ID.
    pub async fn delete_log_source(&self, id: i64) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute("DELETE FROM log_sources WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_log_source: {}", e))?;

        Ok(affected > 0)
    }

    /// Get log sources filtered by enabled status.
    pub async fn get_log_sources_enabled(
        &self,
        enabled_only: bool,
    ) -> Result<Vec<LogSourceConfig>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = if enabled_only {
            conn.query(
                r#"
                SELECT id, name, description, path, path_type, format, parser,
                       timestamp_pattern, timezone, error_patterns, warning_patterns,
                       ignore_patterns, enabled, poll_interval_ms,
                       created_at::TEXT, updated_at::TEXT
                FROM log_sources
                WHERE enabled = true
                ORDER BY name
                "#,
                &[],
            )
            .await
        } else {
            conn.query(
                r#"
                SELECT id, name, description, path, path_type, format, parser,
                       timestamp_pattern, timezone, error_patterns, warning_patterns,
                       ignore_patterns, enabled, poll_interval_ms,
                       created_at::TEXT, updated_at::TEXT
                FROM log_sources
                ORDER BY name
                "#,
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG get_log_sources_enabled: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| Self::log_source_row_to_config(r))
            .collect())
    }

    /// Delete a log source by name.
    pub async fn delete_log_source_by_name(&self, name: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute("DELETE FROM log_sources WHERE name = $1", &[&name])
            .await
            .map_err(|e| format!("PG delete_log_source_by_name: {}", e))?;
        Ok(())
    }

    /// Sync log sources: clear all and re-insert from provided list.
    pub async fn sync_log_sources(&self, sources: &[LogSourceConfig]) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute("DELETE FROM log_sources", &[])
            .await
            .map_err(|e| format!("PG clear log_sources: {}", e))?;

        for source in sources {
            let error_patterns_json = source
                .error_patterns
                .as_ref()
                .map(|p| serde_json::to_string(p).unwrap_or_default());
            let warning_patterns_json = source
                .warning_patterns
                .as_ref()
                .map(|p| serde_json::to_string(p).unwrap_or_default());
            let ignore_patterns_json = source
                .ignore_patterns
                .as_ref()
                .map(|p| serde_json::to_string(p).unwrap_or_default());
            let poll_ms = source.poll_interval_ms as i32;

            conn.execute(
                r#"INSERT INTO log_sources (
                    name, description, path, path_type, format, parser,
                    timestamp_pattern, timezone, error_patterns, warning_patterns,
                    ignore_patterns, enabled, poll_interval_ms
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
                &[
                    &source.name as &(dyn tokio_postgres::types::ToSql + Sync),
                    &source.description as &(dyn tokio_postgres::types::ToSql + Sync),
                    &source.path as &(dyn tokio_postgres::types::ToSql + Sync),
                    &source.path_type.as_str() as &(dyn tokio_postgres::types::ToSql + Sync),
                    &source.format.as_str() as &(dyn tokio_postgres::types::ToSql + Sync),
                    &source.parser.as_str() as &(dyn tokio_postgres::types::ToSql + Sync),
                    &source.timestamp_pattern as &(dyn tokio_postgres::types::ToSql + Sync),
                    &source.timezone as &(dyn tokio_postgres::types::ToSql + Sync),
                    &error_patterns_json as &(dyn tokio_postgres::types::ToSql + Sync),
                    &warning_patterns_json as &(dyn tokio_postgres::types::ToSql + Sync),
                    &ignore_patterns_json as &(dyn tokio_postgres::types::ToSql + Sync),
                    &source.enabled as &(dyn tokio_postgres::types::ToSql + Sync),
                    &poll_ms as &(dyn tokio_postgres::types::ToSql + Sync),
                ],
            )
            .await
            .map_err(|e| format!("PG sync_log_sources insert: {}", e))?;
        }

        Ok(())
    }

    // -- helpers --

    fn log_source_row_to_config(row: &tokio_postgres::Row) -> LogSourceConfig {
        let path_type_str: String = row.get(4);
        let format_str: String = row.get(5);
        let parser_str: String = row.get(6);
        let error_patterns_json: Option<String> = row.get(9);
        let warning_patterns_json: Option<String> = row.get(10);
        let ignore_patterns_json: Option<String> = row.get(11);
        let poll_ms: i32 = row.get(13);

        LogSourceConfig {
            id: Some(row.get::<_, i64>(0)),
            name: row.get(1),
            description: row.get(2),
            path: row.get(3),
            path_type: PathType::from_str(&path_type_str).unwrap_or_default(),
            format: LogFormat::from_str(&format_str).unwrap_or_default(),
            parser: ParserType::from_str(&parser_str).unwrap_or_default(),
            timestamp_pattern: row.get(7),
            timezone: row
                .get::<_, Option<String>>(8)
                .unwrap_or_else(|| "local".to_string()),
            error_patterns: error_patterns_json.and_then(|s| serde_json::from_str(&s).ok()),
            warning_patterns: warning_patterns_json.and_then(|s| serde_json::from_str(&s).ok()),
            ignore_patterns: ignore_patterns_json.and_then(|s| serde_json::from_str(&s).ok()),
            enabled: row.get(12),
            poll_interval_ms: poll_ms as u32,
            created_at: row.get(14),
            updated_at: row.get(15),
        }
    }
}
