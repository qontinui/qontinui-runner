//! PostgreSQL trigger system CRUD operations (raw SQL).

use super::PgDb;
use crate::trigger_system::types::{
    TriggerCondition, TriggerConfig, TriggerHistoryEntry, WorkflowTrigger,
};

impl PgDb {
    // ========================================================================
    // Trigger CRUD
    // ========================================================================

    /// Get all triggers.
    pub async fn get_all_triggers(&self) -> Result<Vec<WorkflowTrigger>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, name, description, trigger_type, trigger_config,
                       workflow_id, workflow_overrides, conditions,
                       debounce_ms, cooldown_seconds, max_concurrent,
                       retry_count, retry_delay_seconds,
                       enabled, last_triggered_at, last_execution_id,
                       trigger_count, created_at, updated_at
                FROM workflow_triggers
                ORDER BY created_at DESC
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_all_triggers: {}", e))?;

        Ok(rows
            .iter()
            .filter_map(Self::tolerant_trigger_from_row)
            .collect())
    }

    /// Get a single trigger by ID.
    pub async fn get_trigger(&self, id: &str) -> Result<Option<WorkflowTrigger>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, name, description, trigger_type, trigger_config,
                       workflow_id, workflow_overrides, conditions,
                       debounce_ms, cooldown_seconds, max_concurrent,
                       retry_count, retry_delay_seconds,
                       enabled, last_triggered_at, last_execution_id,
                       trigger_count, created_at, updated_at
                FROM workflow_triggers WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_trigger: {}", e))?;

        Ok(row.and_then(|r| Self::tolerant_trigger_from_row(&r)))
    }

    /// Get all enabled triggers.
    pub async fn get_enabled_triggers(&self) -> Result<Vec<WorkflowTrigger>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, name, description, trigger_type, trigger_config,
                       workflow_id, workflow_overrides, conditions,
                       debounce_ms, cooldown_seconds, max_concurrent,
                       retry_count, retry_delay_seconds,
                       enabled, last_triggered_at, last_execution_id,
                       trigger_count, created_at, updated_at
                FROM workflow_triggers WHERE enabled = TRUE
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_enabled_triggers: {}", e))?;

        Ok(rows
            .iter()
            .filter_map(Self::tolerant_trigger_from_row)
            .collect())
    }

    /// Create a new trigger.
    pub async fn create_trigger(&self, trigger: &WorkflowTrigger) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let config_json =
            serde_json::to_string(&trigger.trigger_config).map_err(|e| e.to_string())?;
        let conditions_json =
            serde_json::to_string(&trigger.conditions).map_err(|e| e.to_string())?;
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
                    $15, $16, $17, $18, $19)
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let config_json =
            serde_json::to_string(&trigger.trigger_config).map_err(|e| e.to_string())?;
        let conditions_json =
            serde_json::to_string(&trigger.conditions).map_err(|e| e.to_string())?;
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
                enabled = $13, updated_at = $14
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute("DELETE FROM workflow_triggers WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_trigger: {}", e))?;

        tracing::info!("Deleted trigger {}", id);
        Ok(())
    }

    /// Set trigger enabled status.
    pub async fn set_trigger_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now();

        conn.execute(
            "UPDATE workflow_triggers SET enabled = $1, updated_at = $2 WHERE id = $3",
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now();

        conn.execute(
            r#"UPDATE workflow_triggers SET
               last_triggered_at = $1, last_execution_id = $2,
               trigger_count = trigger_count + 1, updated_at = $1
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let event_data_json =
            serde_json::to_string(&entry.event_data).map_err(|e| e.to_string())?;

        conn.execute(
            r#"INSERT INTO trigger_history
               (id, trigger_id, event_type, event_data, action, task_run_id, error_message, triggered_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
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
    ///
    /// `since` / `until` are bound as `DateTime<Utc>` so they match the
    /// `trigger_history.triggered_at` TIMESTAMPTZ column with zero casts.
    /// Callers parse incoming RFC3339 strings before invoking.
    pub async fn get_trigger_history_filtered(
        &self,
        trigger_id: &str,
        limit: u32,
        action_filter: Option<&str>,
        since: Option<chrono::DateTime<chrono::Utc>>,
        until: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<TriggerHistoryEntry>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Build dynamic WHERE clause with $N params
        let mut conditions = vec!["trigger_id = $1".to_string()];
        let mut param_index = 2u32;

        // Heterogeneous params: trigger_id is a &str, action is a String
        // (owned for lifetime), since/until are DateTime<Utc>. We hold
        // owned action separately so the borrow can live in `params`.
        let trigger_id_owned = trigger_id.to_string();
        let action_owned: Option<String> = action_filter.map(|s| s.to_string());

        if action_owned.is_some() {
            conditions.push(format!("action = ${}", param_index));
            param_index += 1;
        }
        if since.is_some() {
            conditions.push(format!("triggered_at >= ${}", param_index));
            param_index += 1;
        }
        if until.is_some() {
            conditions.push(format!("triggered_at <= ${}", param_index));
            param_index += 1;
        }

        let limit_i64 = limit as i64;

        let query = format!(
            r#"SELECT id, trigger_id, event_type, event_data, action,
                      task_run_id, error_message, triggered_at
               FROM trigger_history
               WHERE {}
               ORDER BY triggered_at DESC
               LIMIT ${}"#,
            conditions.join(" AND "),
            param_index
        );

        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
        params.push(&trigger_id_owned as &(dyn tokio_postgres::types::ToSql + Sync));
        if let Some(ref action) = action_owned {
            params.push(action as &(dyn tokio_postgres::types::ToSql + Sync));
        }
        if let Some(ref since_dt) = since {
            params.push(since_dt as &(dyn tokio_postgres::types::ToSql + Sync));
        }
        if let Some(ref until_dt) = until {
            params.push(until_dt as &(dyn tokio_postgres::types::ToSql + Sync));
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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

    /// Decode a trigger row, tolerating malformed rows.
    ///
    /// Converts a decode failure from [`Self::trigger_row_to_workflow_trigger`]
    /// into a logged skip (`None`) so one bad row can never take down a loader
    /// — or the runner. Live incident 2026-07-03: the primary crash-looped at
    /// startup because the old panicking `row.get(5)` hit a `workflow_id`
    /// value that does not decode as TEXT (the canonical column is `uuid`;
    /// this mapper reads it as `Option<String>`).
    fn tolerant_trigger_from_row(row: &tokio_postgres::Row) -> Option<WorkflowTrigger> {
        match Self::trigger_row_to_workflow_trigger(row) {
            Ok(trigger) => Some(trigger),
            Err(e) => {
                let row_id = row
                    .try_get::<_, String>(0)
                    .unwrap_or_else(|_| "<unreadable id>".to_string());
                // tokio-postgres 0.7's `Display` impl prints only a generic
                // "error deserializing column N" / "db error" — the `{:?}`
                // debug format carries the failing column and source cause,
                // so log that.
                tracing::error!("skipping malformed trigger row id={}: {:?}", row_id, e);
                None
            }
        }
    }

    /// Decode one `workflow_triggers` row.
    ///
    /// Every column goes through `try_get` so a malformed / unexpected-type
    /// value surfaces as an `Err` for the caller to skip instead of a panic
    /// that aborts the process. Nullable numeric/bool columns fall back to
    /// the same defaults `WorkflowTrigger`'s serde derives use.
    fn trigger_row_to_workflow_trigger(
        row: &tokio_postgres::Row,
    ) -> Result<WorkflowTrigger, tokio_postgres::Error> {
        let config_str: String = row.try_get::<_, Option<String>>(4)?.unwrap_or_default();
        let conditions_str: String = row.try_get::<_, Option<String>>(7)?.unwrap_or_default();
        let overrides_str: Option<String> = row.try_get(6)?;

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

        Ok(WorkflowTrigger {
            id: row.try_get(0)?,
            name: row.try_get(1)?,
            description: row.try_get(2)?,
            trigger_type: row.try_get(3)?,
            trigger_config,
            workflow_id: row.try_get(5)?,
            workflow_overrides,
            conditions,
            debounce_ms: row.try_get::<_, Option<i64>>(8)?.unwrap_or(1000) as u64,
            cooldown_seconds: row.try_get::<_, Option<i64>>(9)?.unwrap_or(60) as u64,
            max_concurrent: row.try_get::<_, Option<i32>>(10)?.unwrap_or(1) as u32,
            retry_count: row.try_get::<_, Option<i32>>(11)?.unwrap_or(0) as u32,
            retry_delay_seconds: row.try_get::<_, Option<i64>>(12)?.unwrap_or(30) as u64,
            enabled: row.try_get::<_, Option<bool>>(13)?.unwrap_or(false),
            last_triggered_at: row.try_get(14)?,
            last_execution_id: row.try_get(15)?,
            trigger_count: row.try_get::<_, Option<i64>>(16)?.unwrap_or(0) as u64,
            created_at: row.try_get(17)?,
            updated_at: row.try_get(18)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trigger_system::types::TriggerConfig;
    use std::collections::HashMap;

    /// Build a unique trigger id per test so concurrent test runs don't
    /// collide on the same PG instance. Uses nanos-since-epoch + a thread
    /// id — collision-free for any realistic test cadence.
    fn unique_trigger_id(label: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!(
            "test-trig-{}-{}-{:?}",
            label,
            nanos,
            std::thread::current().id()
        )
    }

    /// Build a baseline `WorkflowTrigger` with `last_triggered_at: None` —
    /// the exact shape that triggered the original serialization bug.
    fn make_baseline_trigger(id: String) -> WorkflowTrigger {
        let now = chrono::Utc::now();
        WorkflowTrigger {
            id,
            name: "regression-test-trigger".to_string(),
            description: Some("regression test for serialization bug".to_string()),
            trigger_type: "webhook".to_string(),
            trigger_config: TriggerConfig::Webhook {
                secret: None,
                payload_filter: None,
                variable_mapping: HashMap::new(),
            },
            // Tests exercise trigger CRUD; no workflow_id is set so we
            // don't have to seed an `unified_workflows` row to satisfy
            // the FK. Phase 5b made this column nullable.
            workflow_id: None,
            workflow_overrides: None,
            conditions: Vec::new(),
            debounce_ms: 1000,
            cooldown_seconds: 60,
            max_concurrent: 1,
            retry_count: 0,
            retry_delay_seconds: 30,
            enabled: true,
            last_triggered_at: None,
            last_execution_id: None,
            trigger_count: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// Regression: prior bug serialized `Option<String>` to a TIMESTAMPTZ
    /// slot and failed with `error serializing parameter 14`. This exact
    /// shape (last_triggered_at = None) must now round-trip cleanly.
    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn create_then_get_round_trips_with_none_last_triggered() {
        let db = PgDb::new_blocking_for_test();
        let id = unique_trigger_id("create-get-none");
        let trigger = make_baseline_trigger(id.clone());

        db.create_trigger(&trigger)
            .await
            .expect("create_trigger should succeed with last_triggered_at: None");

        let fetched = db
            .get_trigger(&id)
            .await
            .expect("get_trigger pool/query ok")
            .expect("trigger row must exist after create");

        assert_eq!(fetched.id, trigger.id);
        assert_eq!(fetched.name, trigger.name);
        assert_eq!(fetched.description, trigger.description);
        assert_eq!(fetched.trigger_type, trigger.trigger_type);
        assert_eq!(fetched.workflow_id, trigger.workflow_id);
        assert_eq!(fetched.debounce_ms, trigger.debounce_ms);
        assert_eq!(fetched.cooldown_seconds, trigger.cooldown_seconds);
        assert_eq!(fetched.max_concurrent, trigger.max_concurrent);
        assert_eq!(fetched.retry_count, trigger.retry_count);
        assert_eq!(fetched.retry_delay_seconds, trigger.retry_delay_seconds);
        assert_eq!(fetched.enabled, trigger.enabled);
        assert!(
            fetched.last_triggered_at.is_none(),
            "last_triggered_at must round-trip as None, got {:?}",
            fetched.last_triggered_at
        );
        assert_eq!(fetched.trigger_count, 0);

        // Cleanup
        let _ = db.delete_trigger(&id).await;
    }

    /// `update_trigger` must persist a fresh `updated_at` value bound as
    /// a TIMESTAMPTZ. Asserts the new timestamp advances past the original.
    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn update_round_trips_updated_at() {
        let db = PgDb::new_blocking_for_test();
        let id = unique_trigger_id("update-updated-at");
        let trigger = make_baseline_trigger(id.clone());
        let original_updated_at = trigger.updated_at;

        db.create_trigger(&trigger).await.expect("create_trigger");

        // Sleep so the new updated_at is strictly greater than the original.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        let mut mutated = trigger.clone();
        mutated.name = "regression-test-trigger-updated".to_string();
        mutated.updated_at = chrono::Utc::now();

        db.update_trigger(&mutated).await.expect("update_trigger");

        let fetched = db
            .get_trigger(&id)
            .await
            .expect("get_trigger pool/query ok")
            .expect("trigger row must exist after update");

        assert_eq!(fetched.name, "regression-test-trigger-updated");
        assert!(
            fetched.updated_at > original_updated_at,
            "updated_at must advance after update: orig={:?} new={:?}",
            original_updated_at,
            fetched.updated_at
        );

        let _ = db.delete_trigger(&id).await;
    }

    /// `set_trigger_enabled` flips the `enabled` flag and bumps
    /// `updated_at`. Exercises the post-rewrite UPDATE path that bound
    /// `now: String` to `$2::TIMESTAMPTZ`.
    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn set_trigger_enabled_toggles_and_advances_updated_at() {
        let db = PgDb::new_blocking_for_test();
        let id = unique_trigger_id("set-enabled");
        let trigger = make_baseline_trigger(id.clone());
        let original_updated_at = trigger.updated_at;
        assert!(trigger.enabled, "test precondition: starts enabled=true");

        db.create_trigger(&trigger).await.expect("create_trigger");

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        db.set_trigger_enabled(&id, false)
            .await
            .expect("set_trigger_enabled(false)");

        let fetched = db
            .get_trigger(&id)
            .await
            .expect("get_trigger pool/query ok")
            .expect("trigger row must exist after toggle");

        assert!(!fetched.enabled, "enabled flip must land");
        assert!(
            fetched.updated_at > original_updated_at,
            "updated_at must advance after set_trigger_enabled: orig={:?} new={:?}",
            original_updated_at,
            fetched.updated_at
        );

        let _ = db.delete_trigger(&id).await;
    }

    /// `record_trigger_fired` stamps `last_triggered_at`, increments
    /// `trigger_count`, and bumps `updated_at` — all in a single UPDATE.
    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn record_trigger_fired_increments_and_stamps() {
        let db = PgDb::new_blocking_for_test();
        let id = unique_trigger_id("record-fired");
        let trigger = make_baseline_trigger(id.clone());
        let original_updated_at = trigger.updated_at;

        db.create_trigger(&trigger).await.expect("create_trigger");

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        db.record_trigger_fired(&id, Some("exec-regression-1"))
            .await
            .expect("record_trigger_fired");

        let fetched = db
            .get_trigger(&id)
            .await
            .expect("get_trigger pool/query ok")
            .expect("trigger row must exist after fire");

        assert!(
            fetched.last_triggered_at.is_some(),
            "last_triggered_at must be set after record_trigger_fired"
        );
        assert_eq!(fetched.trigger_count, 1, "trigger_count must increment");
        assert_eq!(
            fetched.last_execution_id.as_deref(),
            Some("exec-regression-1")
        );
        assert!(
            fetched.updated_at > original_updated_at,
            "updated_at must advance after record_trigger_fired: orig={:?} new={:?}",
            original_updated_at,
            fetched.updated_at
        );

        let _ = db.delete_trigger(&id).await;
    }

    /// `record_trigger_history` + `get_trigger_history_filtered` must
    /// round-trip `triggered_at` as a `DateTime<Utc>` with no string
    /// reparse step.
    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn record_then_get_history_round_trips_triggered_at() {
        let db = PgDb::new_blocking_for_test();
        let trigger_id = unique_trigger_id("hist-roundtrip");
        let trigger = make_baseline_trigger(trigger_id.clone());

        // Need a trigger row to satisfy any FK constraint on trigger_history.
        db.create_trigger(&trigger).await.expect("create_trigger");

        let entry_id = unique_trigger_id("hist-entry");
        let triggered_at = chrono::Utc::now();
        let entry = TriggerHistoryEntry {
            id: entry_id.clone(),
            trigger_id: trigger_id.clone(),
            event_type: "test".to_string(),
            event_data: serde_json::json!({"hello": "world"}),
            action: "executed".to_string(),
            task_run_id: Some("tr-1".to_string()),
            error_message: None,
            triggered_at,
        };

        db.record_trigger_history(&entry)
            .await
            .expect("record_trigger_history");

        let history = db
            .get_trigger_history_filtered(&trigger_id, 10, None, None, None)
            .await
            .expect("get_trigger_history_filtered");

        let found = history
            .iter()
            .find(|h| h.id == entry_id)
            .expect("history entry must round-trip back");

        // Same wall-clock moment to microsecond precision — PG TIMESTAMPTZ
        // truncates nanos to microseconds, so allow ≤1µs drift.
        let drift = (found.triggered_at - triggered_at).num_microseconds();
        assert!(
            drift.map(|d| d.abs() <= 1).unwrap_or(false),
            "triggered_at must round-trip with ≤1µs drift: orig={:?} fetched={:?}",
            triggered_at,
            found.triggered_at
        );
        assert_eq!(found.action, "executed");
        assert_eq!(found.task_run_id.as_deref(), Some("tr-1"));

        let _ = db.delete_trigger(&trigger_id).await;
    }
}
