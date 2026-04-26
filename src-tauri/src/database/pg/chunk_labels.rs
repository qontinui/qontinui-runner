//! PostgreSQL chunk_labels CRUD operations.
//!
//! Per-(config_id, chunk_id) user-chosen label overrides for the chunked
//! state-machine graph view. chunk_id is the stable djb2 hash produced by
//! `chunkStateMachine` in `@qontinui/workflow-utils`.

use super::PgDb;

/// A single chunk label row returned to callers.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChunkLabel {
    #[serde(rename = "chunkId")]
    pub chunk_id: String,
    pub label: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

impl PgDb {
    /// List all chunk labels for a given config_id.
    pub async fn list_chunk_labels(&self, config_id: &str) -> Result<Vec<ChunkLabel>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT chunk_id, label, updated_at::TEXT
                   FROM chunk_labels
                   WHERE config_id = $1"#,
                &[&config_id],
            )
            .await
            .map_err(|e| format!("PG list_chunk_labels: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| ChunkLabel {
                chunk_id: r.get(0),
                label: r.get(1),
                updated_at: r.try_get(2).unwrap_or_default(),
            })
            .collect())
    }

    /// Upsert a chunk label. Empty labels should be deleted via
    /// `delete_chunk_label`; callers enforce that trimming.
    pub async fn upsert_chunk_label(
        &self,
        config_id: &str,
        chunk_id: &str,
        label: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"INSERT INTO chunk_labels (config_id, chunk_id, label, updated_at)
               VALUES ($1, $2, $3, NOW())
               ON CONFLICT (config_id, chunk_id) DO UPDATE
               SET label = EXCLUDED.label,
                   updated_at = NOW()"#,
            &[&config_id, &chunk_id, &label],
        )
        .await
        .map_err(|e| format!("PG upsert_chunk_label: {}", e))?;

        Ok(())
    }

    /// Delete a chunk label (revert to auto-derived name).
    pub async fn delete_chunk_label(&self, config_id: &str, chunk_id: &str) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"DELETE FROM chunk_labels WHERE config_id = $1 AND chunk_id = $2"#,
            &[&config_id, &chunk_id],
        )
        .await
        .map_err(|e| format!("PG delete_chunk_label: {}", e))?;

        Ok(())
    }
}
