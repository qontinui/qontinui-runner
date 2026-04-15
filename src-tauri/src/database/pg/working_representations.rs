//! PostgreSQL operations for working representations (pre-computed context snapshots).
//!
//! Working representations aggregate observations, cross-run patterns, entity profiles,
//! findings, fixes, and skills into a single snapshot that is built at task-run start
//! and injected into worker prompts.

use super::PgDb;
use crate::database::types::*;

impl PgDb {
    /// Save (upsert) a working representation for a task run.
    /// Returns the row ID.
    pub async fn save_working_representation(
        &self,
        wr: &WorkingRepresentation,
    ) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let obs_json = serde_json::to_string(&wr.observations).unwrap_or_else(|_| "[]".into());
        let patterns_json =
            serde_json::to_string(&wr.cross_run_patterns).unwrap_or_else(|_| "[]".into());
        let profiles_json =
            serde_json::to_string(&wr.entity_profiles).unwrap_or_else(|_| "[]".into());
        let findings_json =
            serde_json::to_string(&wr.recent_findings).unwrap_or_else(|_| "[]".into());
        let fixes_json = serde_json::to_string(&wr.recent_fixes).unwrap_or_else(|_| "[]".into());
        let skills_json =
            serde_json::to_string(&wr.applicable_skills).unwrap_or_else(|_| "[]".into());

        let row = conn
            .query_one(
                "INSERT INTO working_representations
                    (task_run_id, observations_json, cross_run_patterns_json, entity_profiles_json,
                     recent_findings_json, recent_fixes_json, applicable_skills_json,
                     workflow_id, workflow_name, total_items, build_duration_ms,
                     expires_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                         NOW() + INTERVAL '1 hour')
                 ON CONFLICT (task_run_id) DO UPDATE SET
                    observations_json = EXCLUDED.observations_json,
                    cross_run_patterns_json = EXCLUDED.cross_run_patterns_json,
                    entity_profiles_json = EXCLUDED.entity_profiles_json,
                    recent_findings_json = EXCLUDED.recent_findings_json,
                    recent_fixes_json = EXCLUDED.recent_fixes_json,
                    applicable_skills_json = EXCLUDED.applicable_skills_json,
                    total_items = EXCLUDED.total_items,
                    build_duration_ms = EXCLUDED.build_duration_ms,
                    built_at = NOW(),
                    expires_at = NOW() + INTERVAL '1 hour',
                    is_stale = false
                 RETURNING id",
                &[
                    &wr.task_run_id,
                    &obs_json,
                    &patterns_json,
                    &profiles_json,
                    &findings_json,
                    &fixes_json,
                    &skills_json,
                    &wr.workflow_id,
                    &wr.workflow_name,
                    &wr.total_items,
                    &wr.build_duration_ms,
                ],
            )
            .await
            .map_err(|e| format!("PG save_working_representation: {}", e))?;

        Ok(row.get("id"))
    }

    /// Get the current (non-stale) working representation for a task run.
    pub async fn get_working_representation(
        &self,
        task_run_id: &str,
    ) -> Result<Option<WorkingRepresentation>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                "SELECT id, task_run_id, observations_json, cross_run_patterns_json,
                        entity_profiles_json, recent_findings_json, recent_fixes_json,
                        applicable_skills_json, workflow_id, workflow_name,
                        total_items, build_duration_ms,
                        built_at::TEXT, expires_at::TEXT, is_stale
                 FROM working_representations
                 WHERE task_run_id = $1 AND NOT is_stale AND expires_at > NOW()",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_working_representation: {}", e))?;

        match row {
            None => Ok(None),
            Some(r) => {
                let obs_json: String = r.get("observations_json");
                let patterns_json: String = r.get("cross_run_patterns_json");
                let profiles_json: String = r.get("entity_profiles_json");
                let findings_json: String = r.get("recent_findings_json");
                let fixes_json: String = r.get("recent_fixes_json");
                let skills_json: String = r.get("applicable_skills_json");

                Ok(Some(WorkingRepresentation {
                    id: r.get("id"),
                    task_run_id: r.get("task_run_id"),
                    observations: serde_json::from_str(&obs_json).unwrap_or_default(),
                    cross_run_patterns: serde_json::from_str(&patterns_json).unwrap_or_default(),
                    entity_profiles: serde_json::from_str(&profiles_json).unwrap_or_default(),
                    recent_findings: serde_json::from_str(&findings_json).unwrap_or_default(),
                    recent_fixes: serde_json::from_str(&fixes_json).unwrap_or_default(),
                    applicable_skills: serde_json::from_str(&skills_json).unwrap_or_default(),
                    workflow_id: r.get("workflow_id"),
                    workflow_name: r.get("workflow_name"),
                    total_items: r.get("total_items"),
                    build_duration_ms: r.get("build_duration_ms"),
                    built_at: r.get("built_at"),
                    expires_at: r.get("expires_at"),
                    is_stale: r.get("is_stale"),
                }))
            }
        }
    }

    /// Mark a working representation as stale. Returns true if a row was updated.
    pub async fn mark_working_representation_stale(
        &self,
        task_run_id: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                "UPDATE working_representations SET is_stale = true WHERE task_run_id = $1",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG mark_working_representation_stale: {}", e))?;

        Ok(n > 0)
    }

    /// Delete expired working representations. Returns the number of rows removed.
    pub async fn cleanup_expired_representations(&self) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                "DELETE FROM working_representations WHERE expires_at < NOW()",
                &[],
            )
            .await
            .map_err(|e| format!("PG cleanup_expired_representations: {}", e))?;

        Ok(n)
    }

    // ── Fetch helpers for building working representations ──

    /// Fetch top observations by importance, optionally filtered by workflow name.
    pub async fn fetch_observation_snapshots(
        &self,
        workflow_name: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ObservationSnapshot>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let rows = if let Some(wf) = workflow_name {
            conn.query(
                "SELECT id, title, LEFT(content, 200) as summary, observation_type, importance
                 FROM observations
                 WHERE NOT is_deleted AND superseded_by IS NULL
                   AND (title ILIKE '%' || $1 || '%'
                        OR to_tsvector('english', title || ' ' || content)
                           @@ plainto_tsquery('english', $1))
                 ORDER BY importance DESC, created_at DESC
                 LIMIT $2",
                &[&wf, &limit],
            )
            .await
        } else {
            conn.query(
                "SELECT id, title, LEFT(content, 200) as summary, observation_type, importance
                 FROM observations
                 WHERE NOT is_deleted AND superseded_by IS NULL
                 ORDER BY importance DESC, created_at DESC
                 LIMIT $1",
                &[&limit],
            )
            .await
        };

        let rows = rows.map_err(|e| format!("fetch_observation_snapshots: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| ObservationSnapshot {
                id: r.get("id"),
                title: r.get("title"),
                summary: r.get("summary"),
                observation_type: r.get("observation_type"),
                importance: r.get("importance"),
            })
            .collect())
    }

    /// Fetch active cross-run patterns, optionally filtered by workflow name.
    pub async fn fetch_active_pattern_snapshots(
        &self,
        workflow_name: Option<&str>,
        limit: i64,
    ) -> Result<Vec<CrossRunPatternSnapshot>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let rows = if let Some(wf) = workflow_name {
            conn.query(
                "SELECT id, pattern_type, COALESCE(pattern_data, '') as description,
                        occurrence_count, status
                 FROM cross_run_patterns
                 WHERE status = 'active' AND (workflow_name = $1 OR workflow_name IS NULL)
                 ORDER BY occurrence_count DESC
                 LIMIT $2",
                &[&wf, &limit],
            )
            .await
        } else {
            conn.query(
                "SELECT id, pattern_type, COALESCE(pattern_data, '') as description,
                        occurrence_count, status
                 FROM cross_run_patterns
                 WHERE status = 'active'
                 ORDER BY occurrence_count DESC
                 LIMIT $1",
                &[&limit],
            )
            .await
        };

        let rows = rows.map_err(|e| format!("fetch_active_pattern_snapshots: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| CrossRunPatternSnapshot {
                id: r.get("id"),
                pattern_type: r.get("pattern_type"),
                description: r.get("description"),
                occurrence_count: r.get("occurrence_count"),
                status: r.get("status"),
            })
            .collect())
    }

    /// Fetch top entity profiles by importance.
    pub async fn fetch_entity_profile_snapshots(
        &self,
        limit: i64,
    ) -> Result<Vec<EntityProfileSnapshot>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let rows = conn
            .query(
                "SELECT entity_kind, entity_id, entity_label, profile_summary
                 FROM entity_profiles
                 WHERE NOT is_deleted
                 ORDER BY importance DESC, access_count DESC
                 LIMIT $1",
                &[&limit],
            )
            .await
            .map_err(|e| format!("fetch_entity_profile_snapshots: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| EntityProfileSnapshot {
                entity_kind: r.get("entity_kind"),
                entity_id: r.get("entity_id"),
                entity_label: r.get("entity_label"),
                profile_summary: r.get("profile_summary"),
            })
            .collect())
    }

    /// Fetch recent findings, optionally filtered by workflow name.
    pub async fn fetch_finding_snapshots(
        &self,
        workflow_name: Option<&str>,
        limit: i64,
    ) -> Result<Vec<FindingSnapshot>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let rows = if let Some(wf) = workflow_name {
            conn.query(
                "SELECT f.id, f.title, COALESCE(f.severity, 'unknown') as severity, f.status
                 FROM task_run_findings f
                 JOIN task_runs tr ON f.task_run_id = tr.id
                 WHERE tr.workflow_name = $1
                 ORDER BY f.detected_at DESC
                 LIMIT $2",
                &[&wf, &limit],
            )
            .await
        } else {
            conn.query(
                "SELECT id, title, COALESCE(severity, 'unknown') as severity, status
                 FROM task_run_findings
                 ORDER BY detected_at DESC
                 LIMIT $1",
                &[&limit],
            )
            .await
        };

        let rows = rows.map_err(|e| format!("fetch_finding_snapshots: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| FindingSnapshot {
                id: r.get("id"),
                title: r.get("title"),
                severity: r.get("severity"),
                status: r.get("status"),
            })
            .collect())
    }

    /// Fetch recent reflection fixes, optionally filtered by workflow name.
    pub async fn fetch_fix_snapshots(
        &self,
        workflow_name: Option<&str>,
        limit: i64,
    ) -> Result<Vec<FixSnapshot>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let rows = if let Some(wf) = workflow_name {
            conn.query(
                "SELECT rf.id, rf.fix_description as title, rf.status
                 FROM reflection_fixes rf
                 JOIN task_runs tr ON rf.source_task_run_id = tr.id
                 WHERE tr.workflow_name = $1
                 ORDER BY rf.created_at DESC
                 LIMIT $2",
                &[&wf, &limit],
            )
            .await
        } else {
            conn.query(
                "SELECT id, fix_description as title, status
                 FROM reflection_fixes
                 ORDER BY created_at DESC
                 LIMIT $1",
                &[&limit],
            )
            .await
        };

        let rows = rows.map_err(|e| format!("fetch_fix_snapshots: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| FixSnapshot {
                id: r.get("id"),
                title: r.get("title"),
                status: r.get("status"),
            })
            .collect())
    }

    /// Fetch approved/user skills ordered by usage count.
    pub async fn fetch_skill_snapshots(&self, limit: i64) -> Result<Vec<SkillSnapshot>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let rows = conn
            .query(
                "SELECT name, COALESCE(description, '') as description,
                        COALESCE(category, 'custom') as category,
                        COALESCE(usage_count, 0) as usage_count
                 FROM user_skills
                 WHERE approval_status = 'approved' OR source = 'user'
                 ORDER BY usage_count DESC
                 LIMIT $1",
                &[&limit],
            )
            .await
            .map_err(|e| format!("fetch_skill_snapshots: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| SkillSnapshot {
                name: r.get("name"),
                description: r.get("description"),
                category: r.get("category"),
                usage_count: r.get("usage_count"),
            })
            .collect())
    }
}
