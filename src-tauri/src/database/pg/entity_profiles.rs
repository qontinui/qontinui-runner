//! PostgreSQL entity profile operations.
//!
//! Honcho-inspired evolving entity representations: typed profiles with
//! importance scoring, access tracking, FTS search, and upsert semantics.

use sha2::{Digest, Sha256};

use super::PgDb;
use crate::database::types::*;

/// Compute normalized SHA-256 hash for profile deduplication.
fn profile_content_hash(entity_kind: &str, entity_id: &str, summary: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(entity_kind.trim().to_lowercase().as_bytes());
    hasher.update(b"|");
    hasher.update(entity_id.trim().to_lowercase().as_bytes());
    hasher.update(b"|");
    hasher.update(summary.trim().to_lowercase().as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Derive a topic_key from entity kind and id.
fn derive_topic_key(entity_kind: &str, entity_id: &str) -> String {
    format!("entity:{}:{}", entity_kind, entity_id)
}

impl PgDb {
    /// Save or upsert an entity profile.
    ///
    /// On conflict (same entity_kind + entity_id, not deleted), updates the
    /// existing profile: bumps revision_count, updates summary/detail/sources,
    /// recomputes content_hash.
    ///
    /// Returns the profile ID (new or existing).
    pub async fn save_entity_profile(&self, input: &CreateEntityProfileInput) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let hash = profile_content_hash(&input.entity_kind, &input.entity_id, &input.profile_summary);
        let topic_key = derive_topic_key(&input.entity_kind, &input.entity_id);

        let obs_ids = input.source_observation_ids.clone().unwrap_or_default();
        let finding_ids = input.source_finding_ids.clone().unwrap_or_default();
        let fix_ids = input.source_fix_ids.clone().unwrap_or_default();
        let pattern_ids = input.source_cross_run_pattern_ids.clone().unwrap_or_default();

        let row = conn
            .query_one(
                "INSERT INTO entity_profiles (
                    entity_kind, entity_id, entity_label, profile_summary, profile_detail,
                    topic_key, content_hash, source_observation_ids, source_finding_ids,
                    source_fix_ids, source_cross_run_pattern_ids
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                ON CONFLICT (entity_kind, entity_id) WHERE NOT is_deleted
                DO UPDATE SET
                    entity_label = EXCLUDED.entity_label,
                    profile_summary = EXCLUDED.profile_summary,
                    profile_detail = EXCLUDED.profile_detail,
                    content_hash = EXCLUDED.content_hash,
                    source_observation_ids = EXCLUDED.source_observation_ids,
                    source_finding_ids = EXCLUDED.source_finding_ids,
                    source_fix_ids = EXCLUDED.source_fix_ids,
                    source_cross_run_pattern_ids = EXCLUDED.source_cross_run_pattern_ids,
                    revision_count = entity_profiles.revision_count + 1,
                    updated_at = NOW()
                RETURNING id",
                &[
                    &input.entity_kind,
                    &input.entity_id,
                    &input.entity_label,
                    &input.profile_summary,
                    &input.profile_detail,
                    &topic_key,
                    &hash,
                    &obs_ids,
                    &finding_ids,
                    &fix_ids,
                    &pattern_ids,
                ],
            )
            .await
            .map_err(|e| format!("PG save_entity_profile: {}", e))?;

        let id: i64 = row.get(0);
        Ok(id)
    }

    /// Get a single entity profile by kind + id.
    pub async fn get_entity_profile(
        &self,
        entity_kind: &str,
        entity_id: &str,
    ) -> Result<Option<EntityProfile>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                "SELECT id, entity_kind, entity_id, entity_label, profile_summary, profile_detail,
                        topic_key, content_hash, importance, decay_rate, access_count,
                        last_accessed_at::TEXT, revision_count,
                        source_observation_ids, source_finding_ids, source_fix_ids,
                        source_cross_run_pattern_ids,
                        valid_from::TEXT, valid_until::TEXT, superseded_by,
                        is_deleted, created_at::TEXT, updated_at::TEXT
                 FROM entity_profiles
                 WHERE entity_kind = $1 AND entity_id = $2 AND NOT is_deleted",
                &[&entity_kind, &entity_id],
            )
            .await
            .map_err(|e| format!("PG get_entity_profile: {}", e))?;

        Ok(row.map(|r| row_to_entity_profile(&r)))
    }

    /// Full-text search for entity profiles.
    pub async fn search_entity_profiles(
        &self,
        query: &str,
        max_results: i64,
    ) -> Result<Vec<EntityProfileSearchResult>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT id, entity_kind, entity_id, entity_label, profile_summary,
                        importance, revision_count, created_at::TEXT, updated_at::TEXT,
                        ts_rank(to_tsvector('english', entity_label || ' ' || profile_summary),
                                plainto_tsquery('english', $1)) AS rank
                 FROM entity_profiles
                 WHERE NOT is_deleted
                   AND to_tsvector('english', entity_label || ' ' || profile_summary)
                       @@ plainto_tsquery('english', $1)
                 ORDER BY rank DESC, importance DESC
                 LIMIT $2",
                &[&query, &max_results],
            )
            .await
            .map_err(|e| format!("PG search_entity_profiles: {}", e))?;

        Ok(rows.iter().map(|r| row_to_entity_profile_search(r)).collect())
    }

    /// Get profiles filtered by entity kind, ordered by importance.
    pub async fn get_profiles_by_kind(
        &self,
        entity_kind: &str,
        max_results: i64,
    ) -> Result<Vec<EntityProfileSearchResult>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT id, entity_kind, entity_id, entity_label, profile_summary,
                        importance, revision_count, created_at::TEXT, updated_at::TEXT
                 FROM entity_profiles
                 WHERE entity_kind = $1 AND NOT is_deleted
                 ORDER BY importance DESC, updated_at DESC
                 LIMIT $2",
                &[&entity_kind, &max_results],
            )
            .await
            .map_err(|e| format!("PG get_profiles_by_kind: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| EntityProfileSearchResult {
                id: r.get(0),
                entity_kind: r.get(1),
                entity_id: r.get(2),
                entity_label: r.get(3),
                profile_summary: r.get(4),
                importance: r.get(5),
                revision_count: r.get(6),
                created_at: r.get(7),
                updated_at: r.get(8),
                rank: None,
            })
            .collect())
    }

    /// Get all profiles regardless of kind, ordered by importance.
    pub async fn get_all_profiles(
        &self,
        max_results: i64,
    ) -> Result<Vec<EntityProfileSearchResult>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT id, entity_kind, entity_id, entity_label, profile_summary,
                        importance, revision_count, created_at::TEXT, updated_at::TEXT
                 FROM entity_profiles
                 WHERE NOT is_deleted
                 ORDER BY importance DESC, updated_at DESC
                 LIMIT $1",
                &[&max_results],
            )
            .await
            .map_err(|e| format!("PG get_all_profiles: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| EntityProfileSearchResult {
                id: r.get(0),
                entity_kind: r.get(1),
                entity_id: r.get(2),
                entity_label: r.get(3),
                profile_summary: r.get(4),
                importance: r.get(5),
                revision_count: r.get(6),
                created_at: r.get(7),
                updated_at: r.get(8),
                rank: None,
            })
            .collect())
    }

    /// Get stale profiles that haven't been updated in the given number of days.
    pub async fn get_stale_profiles(
        &self,
        stale_days: i32,
        max_profiles: i64,
    ) -> Result<Vec<EntityProfile>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let interval = format!("{} days", stale_days);
        let rows = conn
            .query(
                "SELECT id, entity_kind, entity_id, entity_label, profile_summary, profile_detail,
                        topic_key, content_hash, importance, decay_rate, access_count,
                        last_accessed_at::TEXT, revision_count,
                        source_observation_ids, source_finding_ids, source_fix_ids,
                        source_cross_run_pattern_ids,
                        valid_from::TEXT, valid_until::TEXT, superseded_by,
                        is_deleted, created_at::TEXT, updated_at::TEXT
                 FROM entity_profiles
                 WHERE NOT is_deleted AND updated_at < NOW() - $1::INTERVAL
                 ORDER BY updated_at ASC
                 LIMIT $2",
                &[&interval, &max_profiles],
            )
            .await
            .map_err(|e| format!("PG get_stale_profiles: {}", e))?;

        Ok(rows.iter().map(|r| row_to_entity_profile(r)).collect())
    }

    /// Batch get profiles for a list of (kind, id) pairs.
    pub async fn get_profiles_for_entities(
        &self,
        pairs: &[(String, String)],
    ) -> Result<Vec<EntityProfile>, String> {
        if pairs.is_empty() {
            return Ok(vec![]);
        }

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let kinds: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        let ids: Vec<&str> = pairs.iter().map(|(_, i)| i.as_str()).collect();

        let rows = conn
            .query(
                "SELECT id, entity_kind, entity_id, entity_label, profile_summary, profile_detail,
                        topic_key, content_hash, importance, decay_rate, access_count,
                        last_accessed_at::TEXT, revision_count,
                        source_observation_ids, source_finding_ids, source_fix_ids,
                        source_cross_run_pattern_ids,
                        valid_from::TEXT, valid_until::TEXT, superseded_by,
                        is_deleted, created_at::TEXT, updated_at::TEXT
                 FROM entity_profiles
                 WHERE NOT is_deleted
                   AND (entity_kind, entity_id) IN (
                       SELECT UNNEST($1::TEXT[]), UNNEST($2::TEXT[])
                   )",
                &[&kinds, &ids],
            )
            .await
            .map_err(|e| format!("PG get_profiles_for_entities: {}", e))?;

        Ok(rows.iter().map(|r| row_to_entity_profile(r)).collect())
    }

    /// Record an access event for a profile (bumps access_count and last_accessed_at).
    pub async fn record_profile_access(
        &self,
        entity_kind: &str,
        entity_id: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let result = conn
            .execute(
                "UPDATE entity_profiles
                 SET access_count = access_count + 1,
                     last_accessed_at = NOW()
                 WHERE entity_kind = $1 AND entity_id = $2 AND NOT is_deleted",
                &[&entity_kind, &entity_id],
            )
            .await
            .map_err(|e| format!("PG record_profile_access: {}", e))?;

        Ok(result > 0)
    }
}

// ============================================================================
// Row mapping helpers
// ============================================================================

fn row_to_entity_profile(r: &tokio_postgres::Row) -> EntityProfile {
    EntityProfile {
        id: r.get(0),
        entity_kind: r.get(1),
        entity_id: r.get(2),
        entity_label: r.get(3),
        profile_summary: r.get(4),
        profile_detail: r.get(5),
        topic_key: r.get(6),
        content_hash: r.get(7),
        importance: r.get(8),
        decay_rate: r.get(9),
        access_count: r.get(10),
        last_accessed_at: r.get(11),
        revision_count: r.get(12),
        source_observation_ids: r.get(13),
        source_finding_ids: r.get(14),
        source_fix_ids: r.get(15),
        source_cross_run_pattern_ids: r.get(16),
        valid_from: r.get(17),
        valid_until: r.get(18),
        superseded_by: r.get(19),
        is_deleted: r.get(20),
        created_at: r.get(21),
        updated_at: r.get(22),
    }
}

fn row_to_entity_profile_search(r: &tokio_postgres::Row) -> EntityProfileSearchResult {
    EntityProfileSearchResult {
        id: r.get(0),
        entity_kind: r.get(1),
        entity_id: r.get(2),
        entity_label: r.get(3),
        profile_summary: r.get(4),
        importance: r.get(5),
        revision_count: r.get(6),
        created_at: r.get(7),
        updated_at: r.get(8),
        rank: r.get::<_, Option<f32>>(9),
    }
}
