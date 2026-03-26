//! PostgreSQL watcher operations via Clorinde-generated queries.
//!
//! Screenpipe-inspired scheduled reactive AI agents: each watcher queries the
//! activity timeline on a schedule, reasons with AI, and triggers an action.

use super::PgDb;
use crate::database::types::*;

/// Shared mapper: Clorinde watcher row -> app-level Watcher type.
/// Works for all three query result types (same shape, different Clorinde names).
macro_rules! map_watcher_row {
    ($r:expr) => {
        Watcher {
            id: $r.id,
            name: $r.name,
            schedule_json: $r.schedule_json,
            timeline_query: $r.timeline_query,
            app_name_filter: $r.app_name_filter,
            source_type_filter: $r.source_type_filter,
            lookback_window: $r.lookback_window,
            reasoning_prompt: $r.reasoning_prompt,
            action_json: $r.action_json,
            enabled: $r.enabled,
            last_run_at: $r.last_run_at.map(|dt| dt.to_rfc3339()),
            last_result_json: $r.last_result_json,
            created_at: $r.created_at.to_rfc3339(),
            updated_at: $r.updated_at.to_rfc3339(),
        }
    };
}

impl PgDb {
    /// Create a new watcher. Returns the watcher ID.
    pub async fn create_watcher(&self, input: &CreateWatcherInput) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();

        qontinui_db::queries::watchers::create_watcher()
            .bind(
                &conn,
                &id.as_str(),
                &input.name.as_str(),
                &input.schedule_json.as_str(),
                &input.timeline_query.as_str(),
                &input.app_name_filter,
                &input.source_type_filter,
                &input.lookback_window.as_str(),
                &input.reasoning_prompt.as_str(),
                &input.action_json.as_str(),
            )
            .one()
            .await
            .map_err(|e| format!("PG create_watcher: {}", e))?;

        Ok(id)
    }

    /// Get a single watcher by ID.
    pub async fn get_watcher(&self, id: &str) -> Result<Option<Watcher>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = qontinui_db::queries::watchers::get_watcher()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_watcher: {}", e))?;

        Ok(row.map(|r| map_watcher_row!(r)))
    }

    /// List all watchers (enabled and disabled).
    pub async fn list_watchers(&self) -> Result<Vec<Watcher>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::watchers::list_watchers()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG list_watchers: {}", e))?;

        Ok(rows.into_iter().map(|r| map_watcher_row!(r)).collect())
    }

    /// List only enabled watchers (for scheduler).
    pub async fn list_enabled_watchers(&self) -> Result<Vec<Watcher>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::watchers::list_enabled_watchers()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG list_enabled_watchers: {}", e))?;

        Ok(rows.into_iter().map(|r| map_watcher_row!(r)).collect())
    }

    /// Update a watcher's fields. Returns the ID if found.
    pub async fn update_watcher(&self, input: &UpdateWatcherInput) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = qontinui_db::queries::watchers::update_watcher()
            .bind(
                &conn,
                &input.name,
                &input.schedule_json,
                &input.timeline_query,
                &input.app_name_filter,
                &input.source_type_filter,
                &input.lookback_window,
                &input.reasoning_prompt,
                &input.action_json,
                &input.enabled,
                &input.id.as_str(),
            )
            .opt()
            .await
            .map_err(|e| format!("PG update_watcher: {}", e))?;

        Ok(id)
    }

    /// Record a watcher execution result.
    pub async fn record_watcher_run(
        &self,
        id: &str,
        result_json: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let result_opt = result_json.map(String::from);

        qontinui_db::queries::watchers::record_watcher_run()
            .bind(&conn, &result_opt, &id)
            .await
            .map_err(|e| format!("PG record_watcher_run: {}", e))?;

        Ok(())
    }

    /// Delete a watcher permanently. Returns true if found and deleted.
    pub async fn delete_watcher(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let deleted = qontinui_db::queries::watchers::delete_watcher()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG delete_watcher: {}", e))?;

        Ok(deleted.is_some())
    }

    /// Enable or disable a watcher.
    pub async fn set_watcher_enabled(&self, id: &str, enabled: bool) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let updated = qontinui_db::queries::watchers::set_watcher_enabled()
            .bind(&conn, &enabled, &id)
            .opt()
            .await
            .map_err(|e| format!("PG set_watcher_enabled: {}", e))?;

        Ok(updated.is_some())
    }
}
