//! Cached app spec operations.
//!
//! Contains all CheckpointDb methods related to cached app specs for UI Bridge.

use chrono::Utc;
use rusqlite::params;

use super::types::*;
use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Cached App Specs
    // ========================================================================

    /// Upsert a cached spec for an external app.
    pub fn upsert_cached_spec(
        &self,
        app_url: &str,
        app_name: &str,
        spec_id: &str,
        spec_json: &str,
        page_url: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let id = format!("{}:{}", app_url, spec_id);
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO cached_app_specs (id, app_url, app_name, spec_id, spec_json, discovered_at, page_url)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(id) DO UPDATE SET
                app_name = excluded.app_name,
                spec_json = excluded.spec_json,
                discovered_at = excluded.discovered_at,
                page_url = excluded.page_url
            "#,
            params![id, app_url, app_name, spec_id, spec_json, now, page_url],
        )
        .map_err(|e| format!("Failed to upsert cached spec: {}", e))?;

        Ok(())
    }

    /// Get all cached specs for a specific app URL.
    pub fn get_cached_specs_for_app(&self, app_url: &str) -> Result<Vec<CachedAppSpec>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, app_url, app_name, spec_id, spec_json, discovered_at, page_url
                FROM cached_app_specs
                WHERE app_url = ?1
                ORDER BY spec_id
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map(params![app_url], |row| {
                Ok(CachedAppSpec {
                    id: row.get(0)?,
                    app_url: row.get(1)?,
                    app_name: row.get(2)?,
                    spec_id: row.get(3)?,
                    spec_json: row.get(4)?,
                    discovered_at: row.get(5)?,
                    page_url: row.get(6)?,
                })
            })
            .map_err(|e| format!("Failed to query cached specs: {}", e))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
        }
        Ok(result)
    }

    /// Get all cached specs across all apps.
    pub fn get_all_cached_specs(&self) -> Result<Vec<CachedAppSpec>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, app_url, app_name, spec_id, spec_json, discovered_at, page_url
                FROM cached_app_specs
                ORDER BY app_url, spec_id
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(CachedAppSpec {
                    id: row.get(0)?,
                    app_url: row.get(1)?,
                    app_name: row.get(2)?,
                    spec_id: row.get(3)?,
                    spec_json: row.get(4)?,
                    discovered_at: row.get(5)?,
                    page_url: row.get(6)?,
                })
            })
            .map_err(|e| format!("Failed to query cached specs: {}", e))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
        }
        Ok(result)
    }

    /// Delete all cached specs for a specific app URL.
    pub fn delete_cached_specs_for_app(&self, app_url: &str) -> Result<u64, String> {
        let conn = self.get_conn()?;
        let deleted = conn
            .execute(
                "DELETE FROM cached_app_specs WHERE app_url = ?1",
                params![app_url],
            )
            .map_err(|e| format!("Failed to delete cached specs: {}", e))?;

        Ok(deleted as u64)
    }
}
