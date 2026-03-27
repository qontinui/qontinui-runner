//! PostgreSQL observation operations via Clorinde-generated queries.
//!
//! Engram-inspired persistent memory: typed, scoped, deduplicated observations
//! with full-text search, topic-key upsert semantics, and temporal validity tracking.

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use tracing::warn;

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

/// Compute temporal relevance score for an observation.
///
/// Combines recency decay (~23-day half-life), currency boost (still-valid observations),
/// and supersession penalty. Used to rank observations in retrieval.
pub fn temporal_score(valid_from: &DateTime<Utc>, valid_until: Option<&DateTime<Utc>>, superseded_by: Option<i64>) -> f64 {
    let now = Utc::now();
    let age_days = (now - *valid_from).num_days() as f64;
    let recency_score = (-0.03 * age_days).exp(); // half-life ~23 days
    let currency_score = if valid_until.is_none() { 1.0 } else { 0.3 };
    let supersession_penalty = if superseded_by.is_some() { 0.2 } else { 1.0 };
    recency_score * currency_score * supersession_penalty
}

impl PgDb {
    /// Save a new observation. Handles privacy stripping, deduplication, and topic-key upsert.
    ///
    /// When upserting by topic_key, the previous version is automatically snapshotted
    /// into observation_history before being overwritten.
    ///
    /// Returns the observation ID (new or existing).
    pub async fn save_observation(
        &self,
        input: &CreateObservationInput,
    ) -> Result<i64, String> {
        let mut conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

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
            let topic_key_opt: Option<&str> = Some(topic_key_str);

            // Use a transaction to atomically snapshot + upsert, preventing
            // concurrent upserts from losing history entries.
            let txn = conn.transaction().await
                .map_err(|e| format!("PG begin transaction: {}", e))?;

            // Snapshot the current version into history before overwriting
            if let Err(e) = qontinui_db::queries::observations::snapshot_before_upsert()
                .bind(&txn, &topic_key_opt)
                .await
            {
                warn!("snapshot_before_upsert failed (topic_key={topic_key_str}): {e}");
                // Non-fatal: the observation may not exist yet (first insert via ON CONFLICT),
                // in which case the INSERT...SELECT produces 0 rows — not an error.
            }

            let id = qontinui_db::queries::observations::upsert_observation_by_topic_key()
                .bind(
                    &txn,
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

            txn.commit().await
                .map_err(|e| format!("PG commit transaction: {}", e))?;
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
            valid_from: r.valid_from.to_rfc3339(),
            valid_until: r.valid_until.map(|t| t.to_rfc3339()),
            superseded_by: r.superseded_by,
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
                valid_from: r.valid_from.to_rfc3339(),
                valid_until: r.valid_until.map(|t| t.to_rfc3339()),
                superseded_by: r.superseded_by,
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
                valid_from: r.valid_from.to_rfc3339(),
                valid_until: r.valid_until.map(|t| t.to_rfc3339()),
                superseded_by: r.superseded_by,
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
            valid_from: r.valid_from.to_rfc3339(),
            valid_until: r.valid_until.map(|t| t.to_rfc3339()),
            superseded_by: r.superseded_by,
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
            valid_from: r.valid_from.to_rfc3339(),
            valid_until: r.valid_until.map(|t| t.to_rfc3339()),
            superseded_by: r.superseded_by,
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
            valid_from: r.valid_from.to_rfc3339(),
            valid_until: r.valid_until.map(|t| t.to_rfc3339()),
            superseded_by: r.superseded_by,
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

    // ========================================================================
    // Memory Consolidation methods
    // ========================================================================

    /// Get observations suitable for consolidation (non-mental-model, non-deleted).
    pub async fn get_observations_for_consolidation(
        &self,
        max_results: i64,
    ) -> Result<Vec<crate::memory::consolidation::ConsolidationObservation>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::observations::get_observations_for_consolidation()
            .bind(&conn, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG get_observations_for_consolidation: {}", e))?;

        Ok(rows.into_iter().map(|r| crate::memory::consolidation::ConsolidationObservation {
            id: r.id,
            title: r.title,
            content: r.content,
            observation_type: r.observation_type,
            scope: r.scope,
            topic_key: r.topic_key,
            content_hash: r.content_hash,
            revision_count: r.revision_count,
            duplicate_count: r.duplicate_count,
            importance: r.importance,
            access_count: r.access_count,
            decay_rate: r.decay_rate,
            is_mental_model: r.is_mental_model,
            consolidated_from: r.consolidated_from,
            project_id: r.project_id,
            workflow_id: r.workflow_id,
            task_run_id: r.task_run_id,
            session_id: r.session_id,
            last_accessed_at: r.last_accessed_at.map(|t| t.with_timezone(&Utc)),
            created_at: r.created_at.with_timezone(&Utc),
            updated_at: r.updated_at.with_timezone(&Utc),
        }).collect())
    }

    /// Get all mental models.
    pub async fn get_mental_models(
        &self,
        max_results: i64,
    ) -> Result<Vec<crate::memory::consolidation::ConsolidationObservation>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::observations::get_mental_models()
            .bind(&conn, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG get_mental_models: {}", e))?;

        Ok(rows.into_iter().map(|r| crate::memory::consolidation::ConsolidationObservation {
            id: r.id,
            title: r.title,
            content: r.content,
            observation_type: r.observation_type,
            scope: r.scope,
            topic_key: r.topic_key,
            content_hash: r.content_hash,
            revision_count: r.revision_count,
            duplicate_count: r.duplicate_count,
            importance: r.importance,
            access_count: r.access_count,
            decay_rate: r.decay_rate,
            is_mental_model: r.is_mental_model,
            consolidated_from: r.consolidated_from,
            project_id: r.project_id,
            workflow_id: r.workflow_id,
            task_run_id: r.task_run_id,
            session_id: r.session_id,
            last_accessed_at: r.last_accessed_at.map(|t| t.with_timezone(&Utc)),
            created_at: r.created_at.with_timezone(&Utc),
            updated_at: r.updated_at.with_timezone(&Utc),
        }).collect())
    }

    /// Update an observation's importance and decay rate.
    pub async fn update_observation_importance(
        &self,
        id: i64,
        importance: f64,
        decay_rate: f64,
    ) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let result = qontinui_db::queries::observations::update_observation_importance()
            .bind(&conn, &importance, &decay_rate, &id)
            .opt()
            .await
            .map_err(|e| format!("PG update_observation_importance: {}", e))?;

        Ok(result.is_some())
    }

    /// Record an access to an observation (updates last_accessed_at and access_count).
    pub async fn record_observation_access(&self, id: i64) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let result = qontinui_db::queries::observations::record_observation_access()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG record_observation_access: {}", e))?;

        Ok(result.is_some())
    }

    /// Save a new mental model observation.
    pub async fn save_mental_model(
        &self,
        title: &str,
        content: &str,
        observation_type: &str,
        scope: &str,
        importance: f64,
        source_ids: &[i64],
    ) -> Result<i64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let hash = content_hash(title, content);
        let decay_rate = crate::memory::importance::compute_decay_rate(importance, true);
        let topic_key: Option<String> = None;
        let project_id: Option<String> = None;
        let workflow_id: Option<String> = None;
        let task_run_id: Option<String> = None;
        let session_id: Option<String> = None;
        let consolidated_from: Option<Vec<i64>> = if source_ids.is_empty() {
            None
        } else {
            Some(source_ids.to_vec())
        };

        let id = qontinui_db::queries::observations::save_mental_model()
            .bind(
                &conn,
                &title,
                &content,
                &observation_type,
                &scope,
                &topic_key,
                &hash.as_str(),
                &importance,
                &decay_rate,
                &consolidated_from,
                &project_id,
                &workflow_id,
                &task_run_id,
                &session_id,
            )
            .one()
            .await
            .map_err(|e| format!("PG save_mental_model: {}", e))?;

        Ok(id)
    }

    /// Reduce an observation's importance by a multiplicative factor.
    pub async fn reduce_observation_importance(&self, id: i64, factor: f64) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let result = qontinui_db::queries::observations::reduce_observation_importance()
            .bind(&conn, &factor, &id)
            .opt()
            .await
            .map_err(|e| format!("PG reduce_observation_importance: {}", e))?;

        Ok(result.is_some())
    }

    /// Archive observations that have decayed below the retention threshold.
    pub async fn decay_and_archive_observations(&self, threshold: f64) -> Result<u64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let ids = qontinui_db::queries::observations::decay_and_archive_observations()
            .bind(&conn, &threshold)
            .all()
            .await
            .map_err(|e| format!("PG decay_and_archive_observations: {}", e))?;

        Ok(ids.len() as u64)
    }

    /// Archive mental models that have decayed below the retention threshold.
    pub async fn decay_and_archive_mental_models(&self, threshold: f64) -> Result<u64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let ids = qontinui_db::queries::observations::decay_and_archive_mental_models()
            .bind(&conn, &threshold)
            .all()
            .await
            .map_err(|e| format!("PG decay_and_archive_mental_models: {}", e))?;

        Ok(ids.len() as u64)
    }

    /// Insert a new consolidation log entry. Returns the log ID.
    pub async fn insert_consolidation_log(&self) -> Result<i64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let id = qontinui_db::queries::observations::insert_consolidation_log()
            .bind(&conn)
            .one()
            .await
            .map_err(|e| format!("PG insert_consolidation_log: {}", e))?;

        Ok(id)
    }

    /// Complete a consolidation log entry with stats.
    pub async fn complete_consolidation_log(
        &self,
        id: i64,
        stats: &crate::memory::consolidation::ConsolidationStats,
        error: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        qontinui_db::queries::observations::complete_consolidation_log()
            .bind(
                &conn,
                &stats.observations_scanned,
                &stats.groups_found,
                &stats.models_created,
                &stats.observations_merged,
                &stats.observations_decayed,
                &stats.observations_archived,
                &error,
                &id,
            )
            .await
            .map_err(|e| format!("PG complete_consolidation_log: {}", e))?;

        Ok(())
    }

    /// Get the most recent successful consolidation timestamp.
    pub async fn get_last_consolidation_time(&self) -> Result<Option<chrono::DateTime<chrono::Utc>>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        // MAX() aggregate always returns exactly one row; last_run is NULL when no successful runs exist.
        let row = qontinui_db::queries::observations::get_last_consolidation_time()
            .bind(&conn)
            .one()
            .await
            .map_err(|e| format!("PG get_last_consolidation_time: {}", e))?;

        Ok(row.last_run.map(|t| t.with_timezone(&chrono::Utc)))
    }

    /// Get recent consolidation log entries.
    pub async fn get_consolidation_log(
        &self,
        max_results: i64,
    ) -> Result<Vec<ConsolidationLogEntry>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::observations::get_consolidation_log()
            .bind(&conn, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG get_consolidation_log: {}", e))?;

        Ok(rows.into_iter().map(|r| ConsolidationLogEntry {
            id: r.id,
            started_at: r.started_at.to_rfc3339(),
            completed_at: r.completed_at.map(|t| t.to_rfc3339()),
            observations_scanned: r.observations_scanned,
            groups_found: r.groups_found,
            models_created: r.models_created,
            observations_merged: r.observations_merged,
            observations_decayed: r.observations_decayed,
            observations_archived: r.observations_archived,
            error: r.error,
        }).collect())
    }

    /// Get memory health statistics.
    pub async fn get_memory_health_stats(&self) -> Result<MemoryHealthStats, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = qontinui_db::queries::observations::get_memory_health_stats()
            .bind(&conn)
            .opt()
            .await
            .map_err(|e| format!("PG get_memory_health_stats: {}", e))?;

        match row {
            Some(r) => Ok(MemoryHealthStats {
                total_observations: r.total_observations,
                total_mental_models: r.total_mental_models,
                decay_queue_size: r.decay_queue_size,
                avg_importance: r.avg_importance,
                avg_access_count: r.avg_access_count,
            }),
            None => Ok(MemoryHealthStats::default()),
        }
    }

    /// Get decay preview (which observations would be archived).
    pub async fn get_decay_preview(
        &self,
        max_results: i64,
    ) -> Result<Vec<DecayPreviewEntry>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::observations::get_decay_preview()
            .bind(&conn, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG get_decay_preview: {}", e))?;

        Ok(rows.into_iter().map(|r| DecayPreviewEntry {
            id: r.id,
            title: r.title,
            observation_type: r.observation_type,
            importance: r.importance,
            decay_rate: r.decay_rate,
            is_mental_model: r.is_mental_model,
            last_activity: r.last_activity.to_rfc3339(),
            current_retention: r.current_retention,
        }).collect())
    }

    // ========================================================================
    // Temporal retrieval methods
    // ========================================================================

    /// Mark an observation as superseded by another.
    /// Sets valid_until = NOW() and superseded_by on the old observation.
    pub async fn supersede_observation(
        &self,
        observation_id: i64,
        new_observation_id: i64,
    ) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let result = qontinui_db::queries::observations::supersede_observation()
            .bind(&conn, &new_observation_id, &observation_id)
            .opt()
            .await
            .map_err(|e| format!("PG supersede_observation: {}", e))?;

        Ok(result.is_some())
    }

    /// Temporal-filtered search with optional FTS, date range, and point-in-time query.
    ///
    /// - `query`: FTS query string (optional — if None, returns all matching temporal filter)
    /// - `from`/`to`: validity range filter
    /// - `as_of`: point-in-time snapshot (returns only observations valid at that moment)
    pub async fn search_observations_temporal(
        &self,
        query: Option<&str>,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        as_of: Option<DateTime<Utc>>,
        max_results: i64,
    ) -> Result<Vec<ObservationSearchResult>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let from_fixed = from.map(|t| t.fixed_offset());
        let to_fixed = to.map(|t| t.fixed_offset());
        let as_of_fixed = as_of.map(|t| t.fixed_offset());

        let rows = qontinui_db::queries::observations::search_observations_temporal()
            .bind(
                &conn,
                &query,
                &from_fixed,
                &to_fixed,
                &as_of_fixed,
                &max_results,
            )
            .all()
            .await
            .map_err(|e| format!("PG search_observations_temporal: {}", e))?;

        Ok(rows.into_iter().map(|r| ObservationSearchResult {
            id: r.id,
            title: r.title,
            content_preview: r.content_preview,
            observation_type: r.observation_type,
            scope: r.scope,
            topic_key: r.topic_key,
            revision_count: r.revision_count,
            project_id: r.project_id,
            valid_from: r.valid_from.to_rfc3339(),
            valid_until: r.valid_until.map(|t| t.to_rfc3339()),
            superseded_by: r.superseded_by,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
            rank: Some(r.rank),
        }).collect())
    }

    /// Get weekly observation trend (count per type per week).
    pub async fn observation_trend(&self, weeks: i32) -> Result<Vec<WeeklyTrend>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let weeks_str = weeks.to_string();
        let rows = qontinui_db::queries::observations::observation_weekly_trend()
            .bind(&conn, &weeks_str.as_str())
            .all()
            .await
            .map_err(|e| format!("PG observation_weekly_trend: {}", e))?;

        Ok(rows.into_iter().map(|r| WeeklyTrend {
            observation_type: r.observation_type,
            week_start: r.week_start.to_rfc3339(),
            count: r.count,
        }).collect())
    }

    /// Get topics with the most revisions in a time period.
    pub async fn most_revised_topics(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        max_results: i64,
    ) -> Result<Vec<TopicRevisionCount>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let from_fixed = from.fixed_offset();
        let to_fixed = to.fixed_offset();

        let rows = qontinui_db::queries::observations::most_revised_topics()
            .bind(&conn, &from_fixed, &to_fixed, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG most_revised_topics: {}", e))?;

        Ok(rows.into_iter().map(|r| TopicRevisionCount {
            topic_key: r.topic_key,
            title: r.title,
            revisions: r.revisions,
        }).collect())
    }

    /// Get the full revision history of an observation.
    pub async fn get_observation_history(
        &self,
        observation_id: i64,
    ) -> Result<Vec<ObservationHistoryEntry>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::observations::get_observation_history()
            .bind(&conn, &observation_id)
            .all()
            .await
            .map_err(|e| format!("PG get_observation_history: {}", e))?;

        Ok(rows.into_iter().map(|r| ObservationHistoryEntry {
            id: r.id,
            observation_id: r.observation_id,
            title: r.title,
            content_preview: r.content_preview,
            content_hash: r.content_hash,
            valid_from: r.valid_from.to_rfc3339(),
            valid_until: r.valid_until.to_rfc3339(),
            revision_number: r.revision_number,
            created_at: r.created_at.to_rfc3339(),
        }).collect())
    }

    /// Get a point-in-time snapshot of all observations as they were at `as_of`.
    pub async fn snapshot_at(
        &self,
        as_of: DateTime<Utc>,
        max_results: i64,
    ) -> Result<Vec<Observation>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let as_of_fixed = as_of.fixed_offset();

        let rows = qontinui_db::queries::observations::snapshot_observations_at()
            .bind(&conn, &as_of_fixed, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG snapshot_observations_at: {}", e))?;

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
            valid_from: r.valid_from.to_rfc3339(),
            valid_until: r.valid_until.map(|t| t.to_rfc3339()),
            superseded_by: r.superseded_by,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
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

    #[test]
    fn test_temporal_score_current_recent() {
        let now = Utc::now();
        let score = temporal_score(&now, None, None);
        // Recent + current + not superseded => close to 1.0
        assert!(score > 0.95, "Expected >0.95, got {}", score);
    }

    #[test]
    fn test_temporal_score_decays_with_age() {
        let now = Utc::now();
        let old = now - chrono::Duration::days(30);
        let score_new = temporal_score(&now, None, None);
        let score_old = temporal_score(&old, None, None);
        assert!(score_new > score_old, "Newer should score higher: {} vs {}", score_new, score_old);
    }

    #[test]
    fn test_temporal_score_expired_penalty() {
        let now = Utc::now();
        let valid_until = now - chrono::Duration::days(1);
        let score_current = temporal_score(&now, None, None);
        let score_expired = temporal_score(&now, Some(&valid_until), None);
        assert!(score_current > score_expired, "Current should score higher than expired");
    }

    #[test]
    fn test_temporal_score_superseded_penalty() {
        let now = Utc::now();
        let score_active = temporal_score(&now, None, None);
        let score_superseded = temporal_score(&now, None, Some(42));
        assert!(score_active > score_superseded, "Active should score higher than superseded");
    }
}
