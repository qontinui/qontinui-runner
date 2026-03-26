//! PostgreSQL observation operations via Clorinde-generated queries.
//!
//! Engram-inspired persistent memory: typed, scoped, deduplicated observations
//! with full-text search and topic-key upsert semantics.

use sha2::{Digest, Sha256};

use super::PgDb;
use crate::database::types::*;

/// Strip `<private>...</private>` tags from content, replacing with `[REDACTED]`.
fn strip_private_tags(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find("<private>") {
        result.push_str(&rest[..start]);
        rest = &rest[start + 9..]; // skip "<private>"
        if let Some(end) = rest.find("</private>") {
            result.push_str("[REDACTED]");
            rest = &rest[end + 10..]; // skip "</private>"
        } else {
            // Unclosed tag — redact rest
            result.push_str("[REDACTED]");
            return result;
        }
    }
    result.push_str(rest);
    result
}

/// Compute normalized SHA-256 hash for deduplication.
fn content_hash(title: &str, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(title.trim().to_lowercase().as_bytes());
    hasher.update(b"|");
    hasher.update(content.trim().to_lowercase().as_bytes());
    format!("{:x}", hasher.finalize())
}

impl PgDb {
    /// Save a new observation. Handles privacy stripping, deduplication, and topic-key upsert.
    ///
    /// Returns the observation ID (new or existing).
    pub async fn save_observation(
        &self,
        input: &CreateObservationInput,
    ) -> Result<i64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        // Strip private content before hashing or storing
        let clean_content = strip_private_tags(&input.content);
        let clean_title = strip_private_tags(&input.title);
        let hash = content_hash(&clean_title, &clean_content);

        // Check for content-hash duplicate within 15-minute window
        let dup = qontinui_db::queries::observations::find_duplicate()
            .bind(&conn, &hash.as_str())
            .opt()
            .await
            .map_err(|e| format!("PG find_duplicate: {}", e))?;

        if let Some(existing) = dup {
            // Increment duplicate count and return existing ID
            qontinui_db::queries::observations::increment_duplicate_count()
                .bind(&conn, &existing.id)
                .await
                .map_err(|e| format!("PG increment_duplicate: {}", e))?;
            return Ok(existing.id);
        }

        // If topic_key is set, use upsert (updates existing, increments revision_count)
        let project_id = input.project_id.clone();
        let workflow_id = input.workflow_id.clone();
        let task_run_id = input.task_run_id.clone();
        let session_id = input.session_id.clone();

        if let Some(ref topic_key) = input.topic_key {
            let topic_key_str = topic_key.as_str();
            let id = qontinui_db::queries::observations::upsert_observation_by_topic_key()
                .bind(
                    &conn,
                    &clean_title.as_str(),
                    &clean_content.as_str(),
                    &input.observation_type.as_str(),
                    &input.scope.as_str(),
                    &topic_key_str,
                    &hash.as_str(),
                    &project_id,
                    &workflow_id,
                    &task_run_id,
                    &session_id,
                )
                .one()
                .await
                .map_err(|e| format!("PG upsert_observation: {}", e))?;
            return Ok(id);
        }

        // Plain insert
        let topic_key: Option<String> = None;
        let id = qontinui_db::queries::observations::save_observation()
            .bind(
                &conn,
                &clean_title.as_str(),
                &clean_content.as_str(),
                &input.observation_type.as_str(),
                &input.scope.as_str(),
                &topic_key,
                &hash.as_str(),
                &project_id,
                &workflow_id,
                &task_run_id,
                &session_id,
            )
            .one()
            .await
            .map_err(|e| format!("PG save_observation: {}", e))?;

        Ok(id)
    }

    /// Get a single observation by ID (full content).
    pub async fn get_observation(&self, id: i64) -> Result<Option<Observation>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = qontinui_db::queries::observations::get_observation()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_observation: {}", e))?;

        Ok(row.map(|r| Observation {
            id: r.id,
            title: r.title,
            content: r.content,
            observation_type: r.observation_type,
            scope: r.scope,
            topic_key: r.topic_key,
            content_hash: r.content_hash,
            revision_count: r.revision_count,
            duplicate_count: r.duplicate_count,
            project_id: r.project_id,
            workflow_id: r.workflow_id,
            task_run_id: r.task_run_id,
            session_id: r.session_id,
            is_deleted: r.is_deleted,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }))
    }

    /// Full-text search over observations. Returns 300-char previews with relevance ranking.
    pub async fn search_observations(
        &self,
        query: &str,
        project_id: Option<&str>,
        max_results: i64,
    ) -> Result<Vec<ObservationSearchResult>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        if let Some(pid) = project_id {
            let rows = qontinui_db::queries::observations::search_observations_by_project()
                .bind(&conn, &query, &pid, &max_results)
                .all()
                .await
                .map_err(|e| format!("PG search_observations: {}", e))?;

            Ok(rows.into_iter().map(|r| ObservationSearchResult {
                id: r.id,
                title: r.title,
                content_preview: r.content_preview,
                observation_type: r.observation_type,
                scope: r.scope,
                topic_key: r.topic_key,
                revision_count: r.revision_count,
                project_id: r.project_id,
                created_at: r.created_at.to_rfc3339(),
                updated_at: r.updated_at.to_rfc3339(),
                rank: Some(r.rank),
            }).collect())
        } else {
            let rows = qontinui_db::queries::observations::search_observations()
                .bind(&conn, &query, &max_results)
                .all()
                .await
                .map_err(|e| format!("PG search_observations: {}", e))?;

            Ok(rows.into_iter().map(|r| ObservationSearchResult {
                id: r.id,
                title: r.title,
                content_preview: r.content_preview,
                observation_type: r.observation_type,
                scope: r.scope,
                topic_key: r.topic_key,
                revision_count: r.revision_count,
                project_id: r.project_id,
                created_at: r.created_at.to_rfc3339(),
                updated_at: r.updated_at.to_rfc3339(),
                rank: Some(r.rank),
            }).collect())
        }
    }

    /// Get project context: recent observations for a project (or global scope).
    pub async fn get_project_context(
        &self,
        project_id: &str,
        observation_type: Option<&str>,
        max_results: i64,
    ) -> Result<Vec<ObservationSearchResult>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::observations::get_project_context()
            .bind(&conn, &project_id, &observation_type, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG get_project_context: {}", e))?;

        Ok(rows.into_iter().map(|r| ObservationSearchResult {
            id: r.id,
            title: r.title,
            content_preview: r.content_preview,
            observation_type: r.observation_type,
            scope: r.scope,
            topic_key: r.topic_key,
            revision_count: r.revision_count,
            project_id: r.project_id,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
            rank: None,
        }).collect())
    }

    /// Update an observation's title, content, or type.
    pub async fn update_observation(
        &self,
        input: &UpdateObservationInput,
    ) -> Result<Option<i64>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        // Recompute hash when title or content changes.
        // If only one is provided, fetch the current value for the other.
        let new_hash = if input.title.is_some() || input.content.is_some() {
            let current = qontinui_db::queries::observations::get_observation()
                .bind(&conn, &input.id)
                .opt()
                .await
                .map_err(|e| format!("PG get_observation for hash: {}", e))?;
            match current {
                Some(obs) => {
                    let t = input.title.as_deref().unwrap_or(&obs.title);
                    let c = input.content.as_deref().unwrap_or(&obs.content);
                    Some(content_hash(&strip_private_tags(t), &strip_private_tags(c)))
                }
                None => None, // observation not found — update will return None
            }
        } else {
            None
        };
        let clean_title = input.title.as_deref().map(strip_private_tags);
        let clean_content = input.content.as_deref().map(strip_private_tags);
        let title_ref = clean_title.as_deref();
        let content_ref = clean_content.as_deref();
        let type_ref = input.observation_type.as_deref();
        let hash_ref = new_hash.as_deref();

        let id = qontinui_db::queries::observations::update_observation()
            .bind(
                &conn,
                &title_ref,
                &content_ref,
                &type_ref,
                &hash_ref,
                &input.id,
            )
            .opt()
            .await
            .map_err(|e| format!("PG update_observation: {}", e))?;

        Ok(id)
    }

    /// Soft-delete an observation.
    pub async fn delete_observation(&self, id: i64) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let deleted = qontinui_db::queries::observations::soft_delete_observation()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG soft_delete_observation: {}", e))?;

        Ok(deleted.is_some())
    }

    /// Get observations linked to a specific task run.
    pub async fn get_observations_by_task_run(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<ObservationSearchResult>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::observations::get_observations_by_task_run()
            .bind(&conn, &task_run_id)
            .all()
            .await
            .map_err(|e| format!("PG get_observations_by_task_run: {}", e))?;

        Ok(rows.into_iter().map(|r| ObservationSearchResult {
            id: r.id,
            title: r.title,
            content_preview: r.content_preview,
            observation_type: r.observation_type,
            scope: r.scope,
            topic_key: r.topic_key,
            revision_count: r.revision_count,
            project_id: r.project_id,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
            rank: None,
        }).collect())
    }

    /// Get all observations with full content (for graph loading).
    /// Returns complete Observation structs, not truncated previews.
    pub async fn get_all_observations_full(
        &self,
        max_results: i64,
    ) -> Result<Vec<Observation>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::observations::get_all_observations_full()
            .bind(&conn, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG get_all_observations_full: {}", e))?;

        Ok(rows.into_iter().map(|r| Observation {
            id: r.id,
            title: r.title,
            content: r.content,
            observation_type: r.observation_type,
            scope: r.scope,
            topic_key: r.topic_key,
            content_hash: r.content_hash,
            revision_count: r.revision_count,
            duplicate_count: r.duplicate_count,
            project_id: r.project_id,
            workflow_id: r.workflow_id,
            task_run_id: r.task_run_id,
            session_id: r.session_id,
            is_deleted: r.is_deleted,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }).collect())
    }

    /// Soft-delete stale observations based on retention policy.
    ///
    /// Removes observations that are:
    /// - Older than `retention_days`
    /// - Have low revision count (not actively maintained knowledge)
    /// - Have zero duplicates (never re-encountered)
    ///
    /// Returns the number of observations archived.
    pub async fn cleanup_stale_observations(
        &self,
        retention_days: i32,
        max_revision_count: i32,
    ) -> Result<u64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let retention_str = retention_days.to_string();
        let ids = qontinui_db::queries::observations::cleanup_stale_observations()
            .bind(&conn, &max_revision_count, &retention_str.as_str())
            .all()
            .await
            .map_err(|e| format!("PG cleanup_stale_observations: {}", e))?;

        Ok(ids.len() as u64)
    }

    /// Get observation type statistics.
    pub async fn get_observation_stats(&self) -> Result<Vec<ObservationTypeStat>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::observations::get_observation_stats()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG get_observation_stats: {}", e))?;

        Ok(rows.into_iter().map(|r| ObservationTypeStat {
            observation_type: r.observation_type,
            count: r.count,
            latest_updated: r.latest_updated.to_rfc3339(),
        }).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_private_tags() {
        assert_eq!(
            strip_private_tags("before <private>secret</private> after"),
            "before [REDACTED] after"
        );
        assert_eq!(
            strip_private_tags("no tags here"),
            "no tags here"
        );
        assert_eq!(
            strip_private_tags("<private>a</private> mid <private>b</private>"),
            "[REDACTED] mid [REDACTED]"
        );
        // Unclosed tag redacts rest
        assert_eq!(
            strip_private_tags("before <private>leaked"),
            "before [REDACTED]"
        );
    }

    #[test]
    fn test_content_hash_deterministic() {
        let h1 = content_hash("Title", "Content body");
        let h2 = content_hash("Title", "Content body");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_content_hash_normalized() {
        // Trimming and lowercasing should produce same hash
        let h1 = content_hash("  Title  ", "  Content  ");
        let h2 = content_hash("title", "content");
        assert_eq!(h1, h2);
    }
}
