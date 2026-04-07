//! PostgreSQL contradiction resolution operations.
//!
//! Detects contradictory observations (same topic_key, different content_hash)
//! and stores resolution records. Used by the memory contradiction engine.

use super::PgDb;
use crate::database::types::*;

impl PgDb {
    /// Detect pairs of observations that likely contradict each other.
    ///
    /// Two observations are candidates when:
    /// - Both are not deleted and not superseded
    /// - Both have the same `topic_key`
    /// - They have different `content_hash` values
    ///
    /// Results are ordered by topic_key for grouping, limited to `max_results`.
    pub async fn detect_contradictions(
        &self,
        max_results: i64,
    ) -> Result<Vec<ContradictionCandidate>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT
                    a.id AS obs_a_id,
                    a.title AS obs_a_title,
                    a.content AS obs_a_content,
                    a.topic_key AS obs_a_topic_key,
                    a.importance AS obs_a_importance,
                    a.valid_from AS obs_a_valid_from,
                    a.access_count AS obs_a_access_count,
                    b.id AS obs_b_id,
                    b.title AS obs_b_title,
                    b.content AS obs_b_content,
                    b.topic_key AS obs_b_topic_key,
                    b.importance AS obs_b_importance,
                    b.valid_from AS obs_b_valid_from,
                    b.access_count AS obs_b_access_count
                 FROM observations a
                 JOIN observations b ON a.id < b.id
                 WHERE a.is_deleted = false
                   AND b.is_deleted = false
                   AND a.superseded_by IS NULL
                   AND b.superseded_by IS NULL
                   AND a.topic_key IS NOT NULL
                   AND b.topic_key IS NOT NULL
                   AND a.topic_key = b.topic_key
                   AND a.content_hash <> b.content_hash
                 ORDER BY a.topic_key, a.id
                 LIMIT $1",
                &[&max_results],
            )
            .await
            .map_err(|e| format!("PG detect_contradictions: {}", e))?;

        let mut candidates = Vec::with_capacity(rows.len());
        for row in &rows {
            let obs_a_valid_from: chrono::DateTime<chrono::Utc> = row.get("obs_a_valid_from");
            let obs_b_valid_from: chrono::DateTime<chrono::Utc> = row.get("obs_b_valid_from");
            candidates.push(ContradictionCandidate {
                obs_a_id: row.get("obs_a_id"),
                obs_a_title: row.get("obs_a_title"),
                obs_a_content: row.get("obs_a_content"),
                obs_a_topic_key: row.get("obs_a_topic_key"),
                obs_a_importance: row.get("obs_a_importance"),
                obs_a_valid_from: obs_a_valid_from.to_rfc3339(),
                obs_a_access_count: row.get("obs_a_access_count"),
                obs_b_id: row.get("obs_b_id"),
                obs_b_title: row.get("obs_b_title"),
                obs_b_content: row.get("obs_b_content"),
                obs_b_topic_key: row.get("obs_b_topic_key"),
                obs_b_importance: row.get("obs_b_importance"),
                obs_b_valid_from: obs_b_valid_from.to_rfc3339(),
                obs_b_access_count: row.get("obs_b_access_count"),
            });
        }

        Ok(candidates)
    }

    /// Save a contradiction resolution record.
    ///
    /// Returns the newly created resolution ID.
    pub async fn save_contradiction_resolution(
        &self,
        obs_a_id: i64,
        obs_b_id: i64,
        resolution_type: &str,
        winner_id: Option<i64>,
        loser_id: Option<i64>,
        confidence: f64,
        rationale: &str,
        evidence_json: Option<&str>,
        resolved_by: &str,
    ) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                "INSERT INTO contradiction_resolutions
                    (observation_a_id, observation_b_id, resolution_type, winner_id, loser_id,
                     confidence, rationale, evidence_json, resolved_by)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 RETURNING id",
                &[
                    &obs_a_id,
                    &obs_b_id,
                    &resolution_type,
                    &winner_id,
                    &loser_id,
                    &confidence,
                    &rationale,
                    &evidence_json,
                    &resolved_by,
                ],
            )
            .await
            .map_err(|e| format!("PG save_contradiction_resolution: {}", e))?;

        Ok(row.get("id"))
    }

    /// Get recent contradiction resolutions, ordered by resolved_at descending.
    pub async fn get_recent_resolutions(
        &self,
        limit: i64,
    ) -> Result<Vec<ContradictionResolution>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT id, observation_a_id, observation_b_id, resolution_type,
                        winner_id, loser_id, confidence, rationale, evidence_json,
                        resolved_at, resolved_by, created_at
                 FROM contradiction_resolutions
                 ORDER BY resolved_at DESC
                 LIMIT $1",
                &[&limit],
            )
            .await
            .map_err(|e| format!("PG get_recent_resolutions: {}", e))?;

        let mut resolutions = Vec::with_capacity(rows.len());
        for row in &rows {
            let resolved_at: chrono::DateTime<chrono::Utc> = row.get("resolved_at");
            let created_at: chrono::DateTime<chrono::Utc> = row.get("created_at");
            resolutions.push(ContradictionResolution {
                id: row.get("id"),
                observation_a_id: row.get("observation_a_id"),
                observation_b_id: row.get("observation_b_id"),
                resolution_type: row.get("resolution_type"),
                winner_id: row.get("winner_id"),
                loser_id: row.get("loser_id"),
                confidence: row.get("confidence"),
                rationale: row.get("rationale"),
                evidence_json: row.get("evidence_json"),
                resolved_at: resolved_at.to_rfc3339(),
                resolved_by: row.get("resolved_by"),
                created_at: created_at.to_rfc3339(),
            });
        }

        Ok(resolutions)
    }
}
