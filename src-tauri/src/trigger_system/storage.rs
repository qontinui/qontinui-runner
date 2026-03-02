//! SQLite storage for trigger definitions and history.

use rusqlite::params;
use std::sync::Arc;
use tracing::info;

use crate::database::CheckpointDb;

use super::types::{TriggerCondition, TriggerConfig, TriggerHistoryEntry, WorkflowTrigger};

// ============================================================================
// Trigger CRUD
// ============================================================================

/// Get all triggers.
pub fn get_all_triggers(db: &Arc<CheckpointDb>) -> Result<Vec<WorkflowTrigger>, String> {
    let conn = db.get_conn()?;

    let mut stmt = conn
        .prepare(
            r#"SELECT id, name, description, trigger_type, trigger_config,
                      workflow_id, workflow_overrides, conditions,
                      debounce_ms, cooldown_seconds, max_concurrent,
                      retry_count, retry_delay_seconds,
                      enabled, last_triggered_at, last_execution_id,
                      trigger_count, created_at, updated_at
               FROM workflow_triggers
               ORDER BY created_at DESC"#,
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let triggers = stmt
        .query_map([], |row| Ok(row_to_trigger(row)))
        .map_err(|e| format!("Failed to query triggers: {}", e))?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();

    Ok(triggers)
}

/// Get a single trigger by ID.
pub fn get_trigger(db: &Arc<CheckpointDb>, id: &str) -> Result<Option<WorkflowTrigger>, String> {
    let conn = db.get_conn()?;

    let result = conn
        .query_row(
            r#"SELECT id, name, description, trigger_type, trigger_config,
                      workflow_id, workflow_overrides, conditions,
                      debounce_ms, cooldown_seconds, max_concurrent,
                      retry_count, retry_delay_seconds,
                      enabled, last_triggered_at, last_execution_id,
                      trigger_count, created_at, updated_at
               FROM workflow_triggers WHERE id = ?1"#,
            params![id],
            |row| Ok(row_to_trigger(row)),
        )
        .optional()
        .map_err(|e| format!("Failed to get trigger: {}", e))?;

    Ok(result)
}

/// Get all enabled triggers.
pub fn get_enabled_triggers(db: &Arc<CheckpointDb>) -> Result<Vec<WorkflowTrigger>, String> {
    let conn = db.get_conn()?;

    let mut stmt = conn
        .prepare(
            r#"SELECT id, name, description, trigger_type, trigger_config,
                      workflow_id, workflow_overrides, conditions,
                      debounce_ms, cooldown_seconds, max_concurrent,
                      retry_count, retry_delay_seconds,
                      enabled, last_triggered_at, last_execution_id,
                      trigger_count, created_at, updated_at
               FROM workflow_triggers WHERE enabled = 1"#,
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let triggers = stmt
        .query_map([], |row| Ok(row_to_trigger(row)))
        .map_err(|e| format!("Failed to query enabled triggers: {}", e))?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();

    Ok(triggers)
}

/// Create a new trigger.
pub fn create_trigger(db: &Arc<CheckpointDb>, trigger: &WorkflowTrigger) -> Result<(), String> {
    let conn = db.get_conn()?;

    let config_json = serde_json::to_string(&trigger.trigger_config).map_err(|e| e.to_string())?;
    let conditions_json = serde_json::to_string(&trigger.conditions).map_err(|e| e.to_string())?;
    let overrides_json = trigger
        .workflow_overrides
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_default());

    conn.execute(
        r#"INSERT INTO workflow_triggers
           (id, name, description, trigger_type, trigger_config,
            workflow_id, workflow_overrides, conditions,
            debounce_ms, cooldown_seconds, max_concurrent,
            retry_count, retry_delay_seconds,
            enabled, last_triggered_at, last_execution_id,
            trigger_count, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)"#,
        params![
            trigger.id,
            trigger.name,
            trigger.description,
            trigger.trigger_type,
            config_json,
            trigger.workflow_id,
            overrides_json,
            conditions_json,
            trigger.debounce_ms,
            trigger.cooldown_seconds,
            trigger.max_concurrent,
            trigger.retry_count,
            trigger.retry_delay_seconds,
            trigger.enabled,
            trigger.last_triggered_at,
            trigger.last_execution_id,
            trigger.trigger_count,
            trigger.created_at,
            trigger.updated_at,
        ],
    )
    .map_err(|e| format!("Failed to create trigger: {}", e))?;

    info!("Created trigger '{}' ({})", trigger.name, trigger.id);
    Ok(())
}

/// Update an existing trigger.
pub fn update_trigger(db: &Arc<CheckpointDb>, trigger: &WorkflowTrigger) -> Result<(), String> {
    let conn = db.get_conn()?;

    let config_json = serde_json::to_string(&trigger.trigger_config).map_err(|e| e.to_string())?;
    let conditions_json = serde_json::to_string(&trigger.conditions).map_err(|e| e.to_string())?;
    let overrides_json = trigger
        .workflow_overrides
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_default());

    conn.execute(
        r#"UPDATE workflow_triggers SET
           name = ?1, description = ?2, trigger_type = ?3, trigger_config = ?4,
           workflow_id = ?5, workflow_overrides = ?6, conditions = ?7,
           debounce_ms = ?8, cooldown_seconds = ?9, max_concurrent = ?10,
           retry_count = ?11, retry_delay_seconds = ?12,
           enabled = ?13, updated_at = ?14
           WHERE id = ?15"#,
        params![
            trigger.name,
            trigger.description,
            trigger.trigger_type,
            config_json,
            trigger.workflow_id,
            overrides_json,
            conditions_json,
            trigger.debounce_ms,
            trigger.cooldown_seconds,
            trigger.max_concurrent,
            trigger.retry_count,
            trigger.retry_delay_seconds,
            trigger.enabled,
            trigger.updated_at,
            trigger.id,
        ],
    )
    .map_err(|e| format!("Failed to update trigger: {}", e))?;

    Ok(())
}

/// Delete a trigger.
pub fn delete_trigger(db: &Arc<CheckpointDb>, id: &str) -> Result<(), String> {
    let conn = db.get_conn()?;

    conn.execute("DELETE FROM workflow_triggers WHERE id = ?1", params![id])
        .map_err(|e| format!("Failed to delete trigger: {}", e))?;

    info!("Deleted trigger {}", id);
    Ok(())
}

/// Set trigger enabled status.
pub fn set_trigger_enabled(db: &Arc<CheckpointDb>, id: &str, enabled: bool) -> Result<(), String> {
    let conn = db.get_conn()?;
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "UPDATE workflow_triggers SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
        params![enabled, now, id],
    )
    .map_err(|e| format!("Failed to set trigger enabled: {}", e))?;

    Ok(())
}

/// Record that a trigger was fired.
pub fn record_trigger_fired(
    db: &Arc<CheckpointDb>,
    id: &str,
    execution_id: Option<&str>,
) -> Result<(), String> {
    let conn = db.get_conn()?;
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        r#"UPDATE workflow_triggers SET
           last_triggered_at = ?1, last_execution_id = ?2,
           trigger_count = trigger_count + 1, updated_at = ?1
           WHERE id = ?3"#,
        params![now, execution_id, id],
    )
    .map_err(|e| format!("Failed to record trigger fired: {}", e))?;

    Ok(())
}

// ============================================================================
// Trigger History
// ============================================================================

/// Record a trigger history entry.
pub fn record_history(db: &Arc<CheckpointDb>, entry: &TriggerHistoryEntry) -> Result<(), String> {
    let conn = db.get_conn()?;

    let event_data_json = serde_json::to_string(&entry.event_data).map_err(|e| e.to_string())?;

    conn.execute(
        r#"INSERT INTO trigger_history
           (id, trigger_id, event_type, event_data, action, task_run_id, error_message, triggered_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"#,
        params![
            entry.id,
            entry.trigger_id,
            entry.event_type,
            event_data_json,
            entry.action,
            entry.task_run_id,
            entry.error_message,
            entry.triggered_at,
        ],
    )
    .map_err(|e| format!("Failed to record trigger history: {}", e))?;

    Ok(())
}

/// Get trigger history entries.
pub fn get_trigger_history(
    db: &Arc<CheckpointDb>,
    trigger_id: &str,
    limit: u32,
) -> Result<Vec<TriggerHistoryEntry>, String> {
    get_trigger_history_filtered(db, trigger_id, limit, None, None, None)
}

/// Get trigger history entries with optional filtering.
pub fn get_trigger_history_filtered(
    db: &Arc<CheckpointDb>,
    trigger_id: &str,
    limit: u32,
    action_filter: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<Vec<TriggerHistoryEntry>, String> {
    let conn = db.get_conn()?;

    // Build dynamic WHERE clause
    let mut conditions = vec!["trigger_id = ?1".to_string()];
    let mut param_index = 2u32;

    if action_filter.is_some() {
        conditions.push(format!("action = ?{}", param_index));
        param_index += 1;
    }
    if since.is_some() {
        conditions.push(format!("triggered_at >= ?{}", param_index));
        param_index += 1;
    }
    if until.is_some() {
        conditions.push(format!("triggered_at <= ?{}", param_index));
        param_index += 1;
    }

    let query = format!(
        r#"SELECT id, trigger_id, event_type, event_data, action,
                  task_run_id, error_message, triggered_at
           FROM trigger_history
           WHERE {}
           ORDER BY triggered_at DESC
           LIMIT ?{}"#,
        conditions.join(" AND "),
        param_index
    );

    let mut stmt = conn
        .prepare(&query)
        .map_err(|e| format!("Failed to prepare history query: {}", e))?;

    // Build params dynamically
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(trigger_id.to_string()));
    if let Some(action) = action_filter {
        param_values.push(Box::new(action.to_string()));
    }
    if let Some(since_val) = since {
        param_values.push(Box::new(since_val.to_string()));
    }
    if let Some(until_val) = until {
        param_values.push(Box::new(until_val.to_string()));
    }
    param_values.push(Box::new(limit));

    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();

    let entries = stmt
        .query_map(param_refs.as_slice(), |row| {
            let event_data_str: String = row.get(3)?;
            Ok(TriggerHistoryEntry {
                id: row.get(0)?,
                trigger_id: row.get(1)?,
                event_type: row.get(2)?,
                event_data: serde_json::from_str(&event_data_str).unwrap_or_default(),
                action: row.get(4)?,
                task_run_id: row.get(5)?,
                error_message: row.get(6)?,
                triggered_at: row.get(7)?,
            })
        })
        .map_err(|e| format!("Failed to query trigger history: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(entries)
}

/// Get trigger counts by type.
pub fn get_trigger_stats(db: &Arc<CheckpointDb>) -> Result<(u64, u64), String> {
    let conn = db.get_conn()?;

    let total: u64 = conn
        .query_row("SELECT COUNT(*) FROM workflow_triggers", [], |row| {
            row.get(0)
        })
        .map_err(|e| format!("Failed to count triggers: {}", e))?;

    let enabled: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM workflow_triggers WHERE enabled = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to count enabled triggers: {}", e))?;

    Ok((total, enabled))
}

// ============================================================================
// Helper: Row to Trigger
// ============================================================================

use rusqlite::{OptionalExtension, Row};

fn row_to_trigger(row: &Row) -> WorkflowTrigger {
    let config_str: String = row.get(4).unwrap_or_default();
    let conditions_str: String = row.get(7).unwrap_or_default();
    let overrides_str: Option<String> = row.get(6).unwrap_or(None);

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
        id: row.get(0).unwrap_or_default(),
        name: row.get(1).unwrap_or_default(),
        description: row.get(2).unwrap_or(None),
        trigger_type: row.get(3).unwrap_or_default(),
        trigger_config,
        workflow_id: row.get(5).unwrap_or_default(),
        workflow_overrides,
        conditions,
        debounce_ms: row.get::<_, i64>(8).unwrap_or(1000) as u64,
        cooldown_seconds: row.get::<_, i64>(9).unwrap_or(60) as u64,
        max_concurrent: row.get::<_, i32>(10).unwrap_or(1) as u32,
        retry_count: row.get::<_, i32>(11).unwrap_or(0) as u32,
        retry_delay_seconds: row.get::<_, i64>(12).unwrap_or(30) as u64,
        enabled: row.get(13).unwrap_or(true),
        last_triggered_at: row.get(14).unwrap_or(None),
        last_execution_id: row.get(15).unwrap_or(None),
        trigger_count: row.get::<_, i64>(16).unwrap_or(0) as u64,
        created_at: row.get(17).unwrap_or_default(),
        updated_at: row.get(18).unwrap_or_default(),
    }
}
