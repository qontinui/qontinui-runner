//! PostgreSQL cached app spec operations (raw SQL).

use super::PgDb;
use crate::database::types::CachedAppSpec;

impl PgDb {
    /// Upsert a cached spec for an external app.
    pub async fn upsert_cached_spec(
        &self,
        app_url: &str,
        app_name: &str,
        spec_id: &str,
        spec_json: &str,
        page_url: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let id = format!("{}:{}", app_url, spec_id);
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"INSERT INTO cached_app_specs (id, app_url, app_name, spec_id, spec_json, discovered_at, page_url)
               VALUES ($1, $2, $3, $4, $5, $6, $7)
               ON CONFLICT (id) DO UPDATE SET
                   app_name = EXCLUDED.app_name,
                   spec_json = EXCLUDED.spec_json,
                   discovered_at = EXCLUDED.discovered_at,
                   page_url = EXCLUDED.page_url"#,
            &[&id, &app_url, &app_name, &spec_id, &spec_json, &now, &page_url],
        )
        .await
        .map_err(|e| format!("PG upsert_cached_spec: {}", e))?;

        Ok(())
    }

    /// Get all cached specs for a specific app URL.
    pub async fn get_cached_specs_for_app(&self, app_url: &str) -> Result<Vec<CachedAppSpec>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, app_url, app_name, spec_id, spec_json, discovered_at, page_url
                   FROM cached_app_specs
                   WHERE app_url = $1
                   ORDER BY spec_id"#,
                &[&app_url],
            )
            .await
            .map_err(|e| format!("PG get_cached_specs_for_app: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| CachedAppSpec {
                id: r.get(0),
                app_url: r.get(1),
                app_name: r.get(2),
                spec_id: r.get(3),
                spec_json: r.get(4),
                discovered_at: r.get(5),
                page_url: r.get(6),
            })
            .collect())
    }

    /// Get all cached specs across all apps.
    pub async fn get_all_cached_specs(&self) -> Result<Vec<CachedAppSpec>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, app_url, app_name, spec_id, spec_json, discovered_at, page_url
                   FROM cached_app_specs
                   ORDER BY app_url, spec_id"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_all_cached_specs: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| CachedAppSpec {
                id: r.get(0),
                app_url: r.get(1),
                app_name: r.get(2),
                spec_id: r.get(3),
                spec_json: r.get(4),
                discovered_at: r.get(5),
                page_url: r.get(6),
            })
            .collect())
    }

    /// Delete all cached specs for a specific app URL.
    pub async fn delete_cached_specs_for_app(&self, app_url: &str) -> Result<u64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let deleted = conn
            .execute(
                "DELETE FROM cached_app_specs WHERE app_url = $1",
                &[&app_url],
            )
            .await
            .map_err(|e| format!("PG delete_cached_specs_for_app: {}", e))?;

        Ok(deleted)
    }
}
