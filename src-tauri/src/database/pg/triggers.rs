//! PostgreSQL trigger system CRUD operations (raw SQL).
//!
//! Mirrors the SQLite trigger_system/storage.rs operations.

use super::PgDb;
use crate::trigger_system::types::{TriggerCondition, TriggerConfig, TriggerHistoryEntry, WorkflowTrigger};

impl PgDb {
    // ========================================================================
    // Trigger CRUD
    // ========================================================================

    /// Get all triggers.
    pub async fn get_all_triggers(&self) -> Result<Vec<WorkflowTrigger>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, name, description, trigger_type, trigger_config,
                       workflow_id, workflow_overrides, conditions,
                       debounce_ms, cooldown_seconds, max_concurrent,
                       retry_count, retry_delay_seconds,
                       enabled, last_triggered_at, last_execution_id,
                       trigger_count, created_at::TEXT, updated_at::TEXT
                FROM workflow_triggers
                ORDER BY created_at DESC
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_all_triggers: {}", e))?;

        Ok(rows.iter().map(|r| Self::trigger_row_to_workflow_trigger(r)).collect())
    }

    /// Get a single trigger by ID.
    pub async fn get_trigger(&self, id: &str) -> Result<Option<WorkflowTrigger>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, name, description, trigger_type, trigger_config,
                       workflow_id, workflow_overrides, conditions,
                       debounce_ms, cooldown_seconds, max_concurrent,
                       retry_count, retry_delay_seconds,
                       enabled, last_triggered_at, last_execution_id,
                       trigger_count, created_at::TEXT, updated_at::TEXT
                FROM workflow_triggers WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_trigger: {}", e))?;

        Ok(row.map(|r| Self::trigger_row_to_workflow_trigger(&r)))
    }

    /// Get all enabled triggers.
    pub async fn get_enabled_triggers(&self) -> Result<Vec<WorkflowTrigger>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, name, description, trigger_type, trigger_config,
                       workflow_id, workflow_overrides, conditions,
                       debounce_ms, cooldown_seconds, max_concurrent,
                       retry_count, retry_delay_seconds,
                       enabled, last_triggered_at, last_execution_id,
                       trigger_count, created_at::TEXT, updated_at::TEXT
                FROM workflow_triggers WHERE enabled = TRUE
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_enabled_triggers: {}", e))?;

        Ok(rows.iter().map(|r| Self::trigger_row_to_workflow_trigger(r)).collect())
    }

    /// Create a new trigger.
    pub async fn create_trigger(&self, trigger: &WorkflowTrigger) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let config_json = serde_json::to_string(&trigger.trigger_config).map_err(|e| e.to_string())?;
        let conditions_json = serde_json::to_string(&trigger.conditions).map_err(|e| e.to_string())?;
        let overrides_json = trigger
            .workflow_overrides
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default());
        let debounce_ms = trigger.debounce_ms as i64;
        let cooldown_seconds = trigger.cooldown_seconds as i64;
        let max_concurrent = trigger.max_concurrent as i32;
        let retry_count = trigger.retry_count as i32;
        let retry_delay_seconds = trigger.retry_delay_seconds as i64;
        let trigger_count = trigger.trigger_count as i64;

        conn.execute(
            r#"
            INSERT INTO workflow_triggers
            (id, name, description, trigger_type, trigger_config,
             workflow_id, workflow_overrides, conditions,
             debounce_ms, cooldown_seconds, max_concurrent,
             retry_count, retry_delay_seconds,
             enabled, last_triggered_at, last_execution_id,
             trigger_count, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                    $15::TIMESTAMPTZ, $16, $17, $18::TIMESTAMPTZ, $19::TIMESTAMPTZ)
            "#,
            &[
                &trigger.id as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger.name as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger.description as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger.trigger_type as &(dyn tokio_postgres::types::ToSql + Sync),
                &config_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger.workflow_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &overrides_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &conditions_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &debounce_ms as &(dyn tokio_postgres::types::ToSql + Sync),
                &cooldown_seconds as &(dyn tokio_postgres::types::ToSql + Sync),
                &max_concurrent as &(dyn tokio_postgres::types::ToSql + Sync),
                &retry_count as &(dyn tokio_postgres::types::ToSql + Sync),
                &retry_delay_seconds as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger.enabled as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger.last_triggered_at as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger.last_execution_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger_count as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger.created_at as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger.updated_at as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG create_trigger: {}", e))?;

        tracing::info!("Created trigger '{}' ({})", trigger.name, trigger.id);
        Ok(())
    }

    /// Update an existing trigger.
    pub async fn update_trigger(&self, trigger: &WorkflowTrigger) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let config_json = serde_json::to_string(&trigger.trigger_config).map_err(|e| e.to_string())?;
        let conditions_json = serde_json::to_string(&trigger.conditions).map_err(|e| e.to_string())?;
        let overrides_json = trigger
            .workflow_overrides
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default());
        let debounce_ms = trigger.debounce_ms as i64;
        let cooldown_seconds = trigger.cooldown_seconds as i64;
        let max_concurrent = trigger.max_concurrent as i32;
        let retry_count = trigger.retry_count as i32;
        let retry_delay_seconds = trigger.retry_delay_seconds as i64;

        conn.execute(
            r#"
            UPDATE workflow_triggers SET
                name = $1, description = $2, trigger_type = $3, trigger_config = $4,
                workflow_id = $5, workflow_overrides = $6, conditions = $7,
                debounce_ms = $8, cooldown_seconds = $9, max_concurrent = $10,
                retry_count = $11, retry_delay_seconds = $12,
                enabled = $13, updated_at = $14::TIMESTAMPTZ
            WHERE id = $15
            "#,
            &[
                &trigger.name as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger.description as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger.trigger_type as &(dyn tokio_postgres::types::ToSql + Sync),
                &config_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger.workflow_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &overrides_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &conditions_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &debounce_ms as &(dyn tokio_postgres::types::ToSql + Sync),
                &cooldown_seconds as &(dyn tokio_postgres::types::ToSql + Sync),
                &max_concurrent as &(dyn tokio_postgres::types::ToSql + Sync),
                &retry_count as &(dyn tokio_postgres::types::ToSql + Sync),
                &retry_delay_seconds as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger.enabled as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger.updated_at as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger.id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG update_trigger: {}", e))?;

        Ok(())
    }

    /// Delete a trigger.
    pub async fn delete_trigger(&self, id: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute("DELETE FROM workflow_triggers WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_trigger: {}", e))?;

        tracing::info!("Deleted trigger {}", id);
        Ok(())
    }

    /// Set trigger enabled status.
    pub async fn set_trigger_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "UPDATE workflow_triggers SET enabled = $1, updated_at = $2::TIMESTAMPTZ WHERE id = $3",
            &[
                &enabled as &(dyn tokio_postgres::types::ToSql + Sync),
                &now as &(dyn tokio_postgres::types::ToSql + Sync),
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG set_trigger_enabled: {}", e))?;

        Ok(())
    }

    /// Record that a trigger was fired.
    pub async fn record_trigger_fired(
        &self,
        id: &str,
        execution_id: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"UPDATE workflow_triggers SET
               last_triggered_at = $1::TIMESTAMPTZ, last_execution_id = $2,
               trigger_count = trigger_count + 1, updated_at = $1::TIMESTAMPTZ
               WHERE id = $3"#,
            &[
                &now as &(dyn tokio_postgres::types::ToSql + Sync),
                &execution_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG record_trigger_fired: {}", e))?;

        Ok(())
    }

    // ========================================================================
    // Trigger History
    // ========================================================================

    /// Record a trigger history entry.
    pub async fn record_trigger_history(&self, entry: &TriggerHistoryEntry) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let event_data_json = serde_json::to_string(&entry.event_data).map_err(|e| e.to_string())?;

        conn.execute(
            r#"INSERT INTO trigger_history
               (id, trigger_id, event_type, event_data, action, task_run_id, error_message, triggered_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8::TIMESTAMPTZ)"#,
            &[
                &entry.id as &(dyn tokio_postgres::types::ToSql + Sync),
                &entry.trigger_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &entry.event_type as &(dyn tokio_postgres::types::ToSql + Sync),
                &event_data_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &entry.action as &(dyn tokio_postgres::types::ToSql + Sync),
                &entry.task_run_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &entry.error_message as &(dyn tokio_postgres::types::ToSql + Sync),
                &entry.triggered_at as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG record_trigger_history: {}", e))?;

        Ok(())
    }

    /// Get trigger history entries with optional filtering.
    pub async fn get_trigger_history_filtered(
        &self,
        trigger_id: &str,
        limit: u32,
        action_filter: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<TriggerHistoryEntry>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        // Build dynamic WHERE clause with $N params
        let mut conditions = vec!["trigger_id = $1".to_string()];
        let mut param_values: Vec<String> = vec![trigger_id.to_string()];
        let mut param_index = 2u32;

        if let Some(action) = action_filter {
            conditions.push(format!("action = ${}", param_index));
            param_values.push(action.to_string());
            param_index += 1;
        }
        if let Some(since_val) = since {
            conditions.push(format!("triggered_at >= ${}", param_index));
            param_values.push(since_val.to_string());
            param_index += 1;
        }
        if let Some(until_val) = until {
            conditions.push(format!("triggered_at <= ${}", param_index));
            param_values.push(until_val.to_string());
            param_index += 1;
        }

        let limit_i64 = limit as i64;

        let query = format!(
            r#"SELECT id, trigger_id, event_type, event_data, action,
                      task_run_id, error_message, triggered_at::TEXT
               FROM trigger_history
               WHERE {}
               ORDER BY triggered_at DESC
               LIMIT ${}"#,
            conditions.join(" AND "),
            param_index
        );

        // Build params: all strings + the limit as i64
        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
        for v in &param_values {
            params.push(v as &(dyn tokio_postgres::types::ToSql + Sync));
        }
        params.push(&limit_i64 as &(dyn tokio_postgres::types::ToSql + Sync));

        let rows = conn
            .query(&query, &params)
            .await
            .map_err(|e| format!("PG get_trigger_history_filtered: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| {
                let event_data_str: String = row.get::<_, Option<String>>(3).unwrap_or_default();
                TriggerHistoryEntry {
                    id: row.get(0),
                    trigger_id: row.get(1),
                    event_type: row.get(2),
                    event_data: serde_json::from_str(&event_data_str).unwrap_or_default(),
                    action: row.get(4),
                    task_run_id: row.get(5),
                    error_message: row.get(6),
                    triggered_at: row.get(7),
                }
            })
            .collect())
    }

    /// Get trigger counts (total, enabled).
    pub async fn get_trigger_stats(&self) -> Result<(u64, u64), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let total: i64 = conn
            .query_one("SELECT COUNT(*) FROM workflow_triggers", &[])
            .await
            .map_err(|e| format!("PG count triggers: {}", e))?
            .get(0);

        let enabled: i64 = conn
            .query_one(
                "SELECT COUNT(*) FROM workflow_triggers WHERE enabled = TRUE",
                &[],
            )
            .await
            .map_err(|e| format!("PG count enabled triggers: {}", e))?
            .get(0);

        Ok((total as u64, enabled as u64))
    }

    // -- helpers --

    fn trigger_row_to_workflow_trigger(row: &tokio_postgres::Row) -> WorkflowTrigger {
        let config_str: String = row.get::<_, Option<String>>(4).unwrap_or_default();
        let conditions_str: String = row.get::<_, Option<String>>(7).unwrap_or_default();
        let overrides_str: Option<String> = row.get(6);

        let trigger_config: TriggerConfig = serde_json::from_str(&config_str).unwrap_or_else(|e| {
            tracing::error!(
                "Failed to deserialize trigger_config (falling back to empty Webhook): {}",
                e
            );
            TriggerConfig::Webhook {
                secret: None,
                payload_filter: None,
                variable_mapping: std::collections::HashMap::new(),
            }
        });

        let conditions: Vec<TriggerCondition> =
            serde_json::from_str(&conditions_str).unwrap_or_default();

        let workflow_overrides: Option<serde_json::Value> =
            overrides_str.and_then(|s| serde_json::from_str(&s).ok());

        WorkflowTrigger {
            id: row.get(0),
            name: row.get(1),
            description: row.get(2),
            trigger_type: row.get(3),
            trigger_config,
            workflow_id: row.get(5),
            workflow_overrides,
            conditions,
            debounce_ms: row.get::<_, i64>(8) as u64,
            cooldown_seconds: row.get::<_, i64>(9) as u64,
            max_concurrent: row.get::<_, i32>(10) as u32,
            retry_count: row.get::<_, i32>(11) as u32,
            retry_delay_seconds: row.get::<_, i64>(12) as u64,
            enabled: row.get(13),
            last_triggered_at: row.get(14),
            last_execution_id: row.get(15),
            trigger_count: row.get::<_, i64>(16) as u64,
            created_at: row.get(17),
            updated_at: row.get(18),
        }
    }
}
