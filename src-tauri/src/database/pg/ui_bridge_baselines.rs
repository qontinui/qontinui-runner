//! PostgreSQL CRUD for ui_bridge_baselines (visual regression baseline store).

use super::PgDb;
use serde::{Deserialize, Serialize};

/// Full baseline record including PNG bytes.
#[derive(Debug, Serialize, Deserialize)]
pub struct Baseline {
    pub id: String,
    pub target_scope: String,
    pub fingerprint: Option<String>,
    pub png_bytes: Vec<u8>,
    pub width: i32,
    pub height: i32,
    pub created_at: String,
    pub updated_at: String,
    pub metadata_json: Option<String>,
    pub ttl_days: Option<i32>,
}

/// Baseline metadata returned by list (no png_bytes to keep payloads small).
#[derive(Debug, Serialize, Deserialize)]
pub struct BaselineMeta {
    pub id: String,
    pub target_scope: String,
    pub fingerprint: Option<String>,
    pub width: i32,
    pub height: i32,
    pub created_at: String,
    pub updated_at: String,
    pub metadata_json: Option<String>,
    pub ttl_days: Option<i32>,
}

impl PgDb {
    /// Save (upsert) a baseline image.
    ///
    /// The raw PNG bytes are decoded to authoritatively determine width and
    /// height — callers' claimed dimensions are ignored.
    pub async fn baseline_save(
        &self,
        id: &str,
        target_scope: &str,
        png_bytes: &[u8],
        fingerprint: Option<&str>,
        metadata_json: Option<&str>,
        ttl_days: Option<i32>,
    ) -> Result<Baseline, String> {
        // Authoritative decode: trust the actual image, not the caller.
        let (width, height) = match image::load_from_memory(png_bytes) {
            Ok(img) => (img.width() as i32, img.height() as i32),
            Err(e) => {
                return Err(format!(
                    "baseline_save: failed to decode PNG for id={}: {}",
                    id, e
                ));
            }
        };

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO ui_bridge_baselines
                (id, target_scope, fingerprint, png_bytes, width, height, metadata_json, ttl_days, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW(), NOW())
            ON CONFLICT (id) DO UPDATE SET
                target_scope  = EXCLUDED.target_scope,
                fingerprint   = EXCLUDED.fingerprint,
                png_bytes     = EXCLUDED.png_bytes,
                width         = EXCLUDED.width,
                height        = EXCLUDED.height,
                metadata_json = EXCLUDED.metadata_json,
                ttl_days      = EXCLUDED.ttl_days,
                updated_at    = NOW()
            "#,
            &[
                &id,
                &target_scope,
                &fingerprint,
                &png_bytes,
                &width,
                &height,
                &metadata_json,
                &ttl_days,
            ],
        )
        .await
        .map_err(|e| format!("PG baseline_save: {}", e))?;

        Ok(Baseline {
            id: id.to_string(),
            target_scope: target_scope.to_string(),
            fingerprint: fingerprint.map(|s| s.to_string()),
            png_bytes: png_bytes.to_vec(),
            width,
            height,
            created_at: String::new(), // filled by PG, not critical for return
            updated_at: String::new(),
            metadata_json: metadata_json.map(|s| s.to_string()),
            ttl_days,
        })
    }

    /// Get a single baseline by ID, including the full PNG bytes.
    pub async fn baseline_get(&self, id: &str) -> Result<Option<Baseline>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, target_scope, fingerprint, png_bytes, width, height,
                       created_at::TEXT, updated_at::TEXT, metadata_json, ttl_days
                FROM ui_bridge_baselines
                WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG baseline_get: {}", e))?;

        Ok(row.map(|r| Baseline {
            id: r.get(0),
            target_scope: r.get(1),
            fingerprint: r.get(2),
            png_bytes: r.get(3),
            width: r.get(4),
            height: r.get(5),
            created_at: r.get(6),
            updated_at: r.get(7),
            metadata_json: r.get(8),
            ttl_days: r.get(9),
        }))
    }

    /// List baseline metadata with optional target_scope filter.
    /// Does NOT return png_bytes to keep payloads small.
    pub async fn baseline_list(
        &self,
        target_scope_filter: Option<&str>,
    ) -> Result<Vec<BaselineMeta>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = match target_scope_filter {
            Some(scope) => {
                conn.query(
                    r#"
                    SELECT id, target_scope, fingerprint, width, height,
                           created_at::TEXT, updated_at::TEXT, metadata_json, ttl_days
                    FROM ui_bridge_baselines
                    WHERE target_scope = $1
                    ORDER BY updated_at DESC
                    "#,
                    &[&scope],
                )
                .await
            }
            None => {
                conn.query(
                    r#"
                    SELECT id, target_scope, fingerprint, width, height,
                           created_at::TEXT, updated_at::TEXT, metadata_json, ttl_days
                    FROM ui_bridge_baselines
                    ORDER BY updated_at DESC
                    "#,
                    &[],
                )
                .await
            }
        }
        .map_err(|e| format!("PG baseline_list: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| BaselineMeta {
                id: r.get(0),
                target_scope: r.get(1),
                fingerprint: r.get(2),
                width: r.get(3),
                height: r.get(4),
                created_at: r.get(5),
                updated_at: r.get(6),
                metadata_json: r.get(7),
                ttl_days: r.get(8),
            })
            .collect())
    }

    /// Delete a baseline by ID. Returns true if a row was actually deleted.
    pub async fn baseline_delete(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows_affected = conn
            .execute(
                "DELETE FROM ui_bridge_baselines WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("PG baseline_delete: {}", e))?;

        Ok(rows_affected > 0)
    }
}
