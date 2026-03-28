//! Prompt evolution tracking.
//!
//! Records the history of prompt rewrite attempts and their canary verdicts,
//! enabling the meta-prompt optimizer to learn from past failures and avoid
//! repeating rejected approaches.

use rusqlite::params;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::database::CheckpointDb;

/// A single entry in the prompt evolution history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptEvolutionEntry {
    pub id: String,
    pub agent_type: String,
    pub parent_variant_id: Option<String>,
    pub variant_id: String,
    pub recommendation_id: Option<String>,
    pub critique: Option<String>,
    pub changes_summary: Option<String>,
    pub canary_verdict: Option<String>,
    pub score_before: Option<f64>,
    pub score_after: Option<f64>,
    /// SHA256 hash of the baseline prompt at the time of rewrite.
    /// Detects when the baseline has drifted (e.g., code changes to hardcoded prompts),
    /// which would invalidate pending canary results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_prompt_hash: Option<String>,
    /// Number of consecutive rejections for this agent_type at time of creation.
    /// Used by the diminishing-returns circuit breaker.
    #[serde(default)]
    pub consecutive_rejections: i32,
    pub created_at: String,
}

/// Compute SHA256 hash of a prompt string for baseline drift detection.
pub fn compute_prompt_hash(prompt: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(prompt.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Record a new prompt evolution entry when a meta-prompt rewrite is created.
pub fn record_evolution(
    db: &CheckpointDb,
    agent_type: &str,
    parent_variant_id: Option<&str>,
    variant_id: &str,
    recommendation_id: Option<&str>,
    critique: Option<&str>,
    changes_summary: Option<&str>,
    score_before: Option<f64>,
) -> Result<String, String> {
    record_evolution_full(
        db,
        agent_type,
        parent_variant_id,
        variant_id,
        recommendation_id,
        critique,
        changes_summary,
        score_before,
        None,
    )
}

/// Record a new prompt evolution entry with baseline hash and rejection count.
pub fn record_evolution_full(
    db: &CheckpointDb,
    agent_type: &str,
    parent_variant_id: Option<&str>,
    variant_id: &str,
    recommendation_id: Option<&str>,
    critique: Option<&str>,
    changes_summary: Option<&str>,
    score_before: Option<f64>,
    baseline_prompt_hash: Option<&str>,
) -> Result<String, String> {
    let id = format!("pe-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();

    // Count consecutive rejections for this agent_type (for circuit breaker)
    let consecutive_rejections = count_consecutive_rejections(db, agent_type);

    let id_clone = id.clone();
    let agent_type = agent_type.to_string();
    let parent_variant_id = parent_variant_id.map(|s| s.to_string());
    let variant_id = variant_id.to_string();
    let recommendation_id = recommendation_id.map(|s| s.to_string());
    let critique = critique.map(|s| s.to_string());
    let changes_summary = changes_summary.map(|s| s.to_string());
    let baseline_hash = baseline_prompt_hash.map(|s| s.to_string());

    db.with_conn(move |conn| {
        conn.execute(
            r#"INSERT INTO prompt_evolution
               (id, agent_type, parent_variant_id, variant_id, recommendation_id,
                critique, changes_summary, canary_verdict, score_before, score_after,
                baseline_prompt_hash, consecutive_rejections, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, NULL, ?9, ?10, ?11)"#,
            params![
                id_clone,
                agent_type,
                parent_variant_id,
                variant_id,
                recommendation_id,
                critique,
                changes_summary,
                score_before,
                baseline_hash,
                consecutive_rejections,
                now,
            ],
        )
        .map_err(|e| format!("Failed to record prompt evolution: {}", e))?;

        info!(
            "Recorded prompt evolution {} for agent {} (variant {}, consecutive_rejections={})",
            id_clone, agent_type, variant_id, consecutive_rejections
        );
        Ok(())
    })?;

    Ok(id)
}

/// Record evolution with optional PG dual-write (fire-and-forget).
///
/// Like `record_evolution`, but also writes to PostgreSQL if a pool is provided.
pub fn record_evolution_with_pg(
    db: &CheckpointDb,
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    agent_type: &str,
    parent_variant_id: Option<&str>,
    variant_id: &str,
    recommendation_id: Option<&str>,
    critique: Option<&str>,
    changes_summary: Option<&str>,
    score_before: Option<f64>,
) -> Result<String, String> {
    let id = record_evolution(
        db,
        agent_type,
        parent_variant_id,
        variant_id,
        recommendation_id,
        critique,
        changes_summary,
        score_before,
    )?;

    // Fire-and-forget PG write
    let pg = pg_db.clone();
    let id_clone = id.clone();
    let agent_type = agent_type.to_string();
    let parent_variant_id = parent_variant_id.map(|s| s.to_string());
    let variant_id = variant_id.to_string();
    let recommendation_id = recommendation_id.map(|s| s.to_string());
    let critique = critique.map(|s| s.to_string());
    let changes_summary = changes_summary.map(|s| s.to_string());

    tokio::spawn(async move {
        if let Err(e) = pg
            .record_prompt_evolution(
                &id_clone,
                &agent_type,
                parent_variant_id.as_deref(),
                &variant_id,
                recommendation_id.as_deref(),
                critique.as_deref(),
                changes_summary.as_deref(),
                score_before,
            )
            .await
        {
            tracing::warn!("PG prompt_evolution write failed: {}", e);
        }
    });

    Ok(id)
}

/// Update the canary verdict and post-canary score for an evolution entry.
pub fn update_verdict(
    db: &CheckpointDb,
    evolution_id: &str,
    verdict: &str,
    score_after: Option<f64>,
) -> Result<(), String> {
    let evolution_id = evolution_id.to_string();
    let verdict = verdict.to_string();

    db.with_conn(move |conn| {
        conn.execute(
            "UPDATE prompt_evolution SET canary_verdict = ?1, score_after = ?2 WHERE id = ?3",
            params![verdict, score_after, evolution_id],
        )
        .map_err(|e| format!("Failed to update evolution verdict: {}", e))?;

        info!(
            "Updated prompt evolution {} verdict: {}",
            evolution_id, verdict
        );
        Ok(())
    })
}

/// Update verdict with PG dual-write (fire-and-forget).
pub fn update_verdict_with_pg(
    db: &CheckpointDb,
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    evolution_id: &str,
    verdict: &str,
    score_after: Option<f64>,
) -> Result<(), String> {
    update_verdict(db, evolution_id, verdict, score_after)?;

    let pg = pg_db.clone();
    let eid = evolution_id.to_string();
    let v = verdict.to_string();
    tokio::spawn(async move {
        if let Err(e) = pg.update_evolution_verdict(&eid, &v, score_after).await {
            tracing::warn!("PG update_evolution_verdict dual-write failed: {}", e);
        }
    });

    Ok(())
}

/// Update the canary verdict for an evolution entry identified by its variant_id.
pub fn update_verdict_by_variant(
    db: &CheckpointDb,
    variant_id: &str,
    verdict: &str,
    score_after: Option<f64>,
) -> Result<(), String> {
    let variant_id = variant_id.to_string();
    let verdict = verdict.to_string();

    db.with_conn(move |conn| {
        conn.execute(
            "UPDATE prompt_evolution SET canary_verdict = ?1, score_after = ?2 WHERE variant_id = ?3 AND canary_verdict IS NULL",
            params![verdict, score_after, variant_id],
        )
        .map_err(|e| format!("Failed to update evolution verdict by variant: {}", e))?;
        Ok(())
    })
}

/// Update verdict by variant with PG dual-write (fire-and-forget).
pub fn update_verdict_by_variant_with_pg(
    db: &CheckpointDb,
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    variant_id: &str,
    verdict: &str,
    score_after: Option<f64>,
) -> Result<(), String> {
    update_verdict_by_variant(db, variant_id, verdict, score_after)?;

    // PG doesn't have an exact `update_by_variant` but has `update_evolution_verdict_by_recommendation`.
    // For variant-based updates, we look up the recommendation_id first.
    let variant_id_owned = variant_id.to_string();
    let rec_id: Option<String> = db.with_conn({
        let vid = variant_id_owned.clone();
        move |conn| {
            conn.query_row(
                "SELECT recommendation_id FROM prompt_evolution WHERE variant_id = ?1 AND canary_verdict = ?2 ORDER BY created_at DESC LIMIT 1",
                params![vid, verdict],
                |row| row.get(0),
            )
            .ok()
            .ok_or_else(|| "not found".to_string())
        }
    }).ok();

    if let Some(rid) = rec_id {
        let pg = pg_db.clone();
        let v = verdict.to_string();
        tokio::spawn(async move {
            if let Err(e) = pg.update_evolution_verdict_by_recommendation(&rid, &v, score_after).await {
                tracing::warn!("PG update_evolution_verdict_by_recommendation dual-write failed: {}", e);
            }
        });
    }

    Ok(())
}

/// Get evolution history for an agent type (most recent first).
pub fn get_evolution_history(
    db: &CheckpointDb,
    agent_type: Option<&str>,
    limit: usize,
) -> Result<Vec<PromptEvolutionEntry>, String> {
    let agent_type = agent_type.map(|s| s.to_string());
    let limit = limit as i64;

    db.with_conn(move |conn| {
        let (sql, param_values): (String, Vec<Box<dyn rusqlite::types::ToSql>>) =
            if let Some(ref at) = agent_type {
                (
                    r#"SELECT id, agent_type, parent_variant_id, variant_id, recommendation_id,
                              critique, changes_summary, canary_verdict, score_before, score_after,
                              baseline_prompt_hash, COALESCE(consecutive_rejections, 0), created_at
                       FROM prompt_evolution
                       WHERE agent_type = ?1
                       ORDER BY created_at DESC
                       LIMIT ?2"#
                        .to_string(),
                    vec![Box::new(at.clone()), Box::new(limit)],
                )
            } else {
                (
                    r#"SELECT id, agent_type, parent_variant_id, variant_id, recommendation_id,
                              critique, changes_summary, canary_verdict, score_before, score_after,
                              baseline_prompt_hash, COALESCE(consecutive_rejections, 0), created_at
                       FROM prompt_evolution
                       ORDER BY created_at DESC
                       LIMIT ?1"#
                        .to_string(),
                    vec![Box::new(limit)],
                )
            };

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare evolution query: {}", e))?;

        let rows = stmt
            .query_map(rusqlite::params_from_iter(param_values.iter()), |row| {
                Ok(PromptEvolutionEntry {
                    id: row.get(0)?,
                    agent_type: row.get(1)?,
                    parent_variant_id: row.get(2)?,
                    variant_id: row.get(3)?,
                    recommendation_id: row.get(4)?,
                    critique: row.get(5)?,
                    changes_summary: row.get(6)?,
                    canary_verdict: row.get(7)?,
                    score_before: row.get(8)?,
                    score_after: row.get(9)?,
                    baseline_prompt_hash: row.get(10)?,
                    consecutive_rejections: row.get(11)?,
                    created_at: row.get(12)?,
                })
            })
            .map_err(|e| format!("Failed to query evolution history: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
    })
}

/// Check whether there is an active (no verdict yet) evolution entry for an agent type.
/// This indicates a canary is in progress and we should NOT create another rewrite.
pub fn has_active_evolution(db: &CheckpointDb, agent_type: &str) -> bool {
    let agent_type = agent_type.to_string();
    db.with_conn(move |conn| {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM prompt_evolution WHERE agent_type = ?1 AND canary_verdict IS NULL",
                params![agent_type],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(count > 0)
    })
    .unwrap_or(false)
}

/// Get the most recent evolution entry for an agent type that was rejected.
/// Used to feed failure context back into the next optimization round.
pub fn get_latest_rejected(
    db: &CheckpointDb,
    agent_type: &str,
) -> Result<Option<PromptEvolutionEntry>, String> {
    let agent_type = agent_type.to_string();

    db.with_conn(move |conn| {
        let result = conn.query_row(
            r#"SELECT id, agent_type, parent_variant_id, variant_id, recommendation_id,
                      critique, changes_summary, canary_verdict, score_before, score_after,
                      baseline_prompt_hash, COALESCE(consecutive_rejections, 0), created_at
               FROM prompt_evolution
               WHERE agent_type = ?1 AND canary_verdict = 'reject'
               ORDER BY created_at DESC
               LIMIT 1"#,
            params![agent_type],
            |row| {
                Ok(PromptEvolutionEntry {
                    id: row.get(0)?,
                    agent_type: row.get(1)?,
                    parent_variant_id: row.get(2)?,
                    variant_id: row.get(3)?,
                    recommendation_id: row.get(4)?,
                    critique: row.get(5)?,
                    changes_summary: row.get(6)?,
                    canary_verdict: row.get(7)?,
                    score_before: row.get(8)?,
                    score_after: row.get(9)?,
                    baseline_prompt_hash: row.get(10)?,
                    consecutive_rejections: row.get(11)?,
                    created_at: row.get(12)?,
                })
            },
        );

        match result {
            Ok(entry) => Ok(Some(entry)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get latest rejected evolution: {}", e)),
        }
    })
}

/// Check the cooldown: returns true if the last evolution entry for this agent_type
/// was created less than `cooldown_hours` ago.
pub fn is_in_cooldown(db: &CheckpointDb, agent_type: &str, cooldown_hours: i64) -> bool {
    let agent_type = agent_type.to_string();
    let cutoff = (chrono::Utc::now() - chrono::Duration::hours(cooldown_hours)).to_rfc3339();

    db.with_conn(move |conn| {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM prompt_evolution WHERE agent_type = ?1 AND created_at > ?2",
                params![agent_type, cutoff],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(count > 0)
    })
    .unwrap_or(false)
}

/// Count consecutive recent rejections for an agent_type.
///
/// Looks at the most recent evolution entries and counts how many consecutive
/// "reject" verdicts appear (stopping at the first non-reject). Used by the
/// diminishing-returns circuit breaker.
pub fn count_consecutive_rejections(db: &CheckpointDb, agent_type: &str) -> i32 {
    let agent_type = agent_type.to_string();
    db.with_conn(move |conn| {
        let mut stmt = conn
            .prepare(
                r#"SELECT canary_verdict FROM prompt_evolution
                   WHERE agent_type = ?1 AND canary_verdict IS NOT NULL
                   ORDER BY created_at DESC
                   LIMIT 10"#,
            )
            .map_err(|e| format!("Failed to count rejections: {}", e))?;

        let verdicts: Vec<String> = stmt
            .query_map(params![agent_type], |row| row.get(0))
            .map_err(|e| format!("Failed to query verdicts: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        let mut count = 0i32;
        for v in &verdicts {
            if v == "reject" {
                count += 1;
            } else {
                break; // Stop at first non-reject
            }
        }
        Ok(count)
    })
    .unwrap_or(0)
}

/// Compute the adaptive cooldown hours based on consecutive rejections.
///
/// Implements exponential backoff: 24h → 72h → 168h (1 week) → 336h (2 weeks).
/// After 4+ consecutive rejections, the optimizer should largely leave this
/// agent's prompt alone — it's likely near-optimal or has structural issues
/// that prompt rewriting can't fix.
pub fn adaptive_cooldown_hours(consecutive_rejections: i32) -> i64 {
    match consecutive_rejections {
        0 => 24,      // No rejections: standard 24h cooldown
        1 => 72,      // 1 rejection: 3 days
        2 => 168,     // 2 rejections: 1 week
        3 => 336,     // 3 rejections: 2 weeks
        _ => 672,     // 4+ rejections: 4 weeks
    }
}

/// Get all rejected prompt variant contents for an agent_type.
///
/// Returns (variant_id, prompt_content) pairs for similarity comparison.
/// Only fetches variants that were part of rejected evolution entries.
pub fn get_rejected_prompt_contents(
    db: &CheckpointDb,
    agent_type: &str,
) -> Result<Vec<(String, String)>, String> {
    let agent_type = agent_type.to_string();
    db.with_conn(move |conn| {
        let mut stmt = conn
            .prepare(
                r#"SELECT pe.variant_id, pr.prompt_content
                   FROM prompt_evolution pe
                   INNER JOIN prompt_registry pr ON pr.id = pe.variant_id
                   WHERE pe.agent_type = ?1 AND pe.canary_verdict = 'reject'
                   ORDER BY pe.created_at DESC
                   LIMIT 10"#,
            )
            .map_err(|e| format!("Failed to query rejected prompts: {}", e))?;

        let results: Vec<(String, String)> = stmt
            .query_map(params![agent_type], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("Failed to fetch rejected prompts: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    })
}

/// Check if the baseline prompt has drifted since a canary was started.
///
/// Compares the stored `baseline_prompt_hash` against the current prompt hash.
/// Returns true if the baseline has changed, indicating the canary results may
/// be invalid (the prompt being compared against is different from when the
/// canary started).
pub fn has_baseline_drifted(
    db: &CheckpointDb,
    agent_type: &str,
    current_prompt_hash: &str,
) -> bool {
    let agent_type = agent_type.to_string();
    let current_hash = current_prompt_hash.to_string();

    db.with_conn(move |conn| {
        let stored_hash: Option<String> = conn
            .query_row(
                r#"SELECT baseline_prompt_hash FROM prompt_evolution
                   WHERE agent_type = ?1 AND canary_verdict IS NULL
                   ORDER BY created_at DESC LIMIT 1"#,
                params![agent_type],
                |row| row.get(0),
            )
            .ok();

        match stored_hash {
            Some(h) if !h.is_empty() => Ok(h != current_hash),
            _ => Ok(false), // No stored hash or NULL — can't detect drift
        }
    })
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::CheckpointDb;

    fn setup_test_db() -> CheckpointDb {
        CheckpointDb::new_in_memory().unwrap()
    }

    #[test]
    fn test_record_and_get_evolution() {
        let db = setup_test_db();

        let id = record_evolution(
            &db,
            "implementer",
            None,
            "pv-123",
            Some("mor-456"),
            Some("The prompt lacks clear output format specification"),
            Some("Added JSON output format requirement"),
            Some(0.45),
        )
        .unwrap();

        assert!(id.starts_with("pe-"));

        let history = get_evolution_history(&db, Some("implementer"), 10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].agent_type, "implementer");
        assert_eq!(history[0].variant_id, "pv-123");
        assert!(history[0].canary_verdict.is_none());
    }

    #[test]
    fn test_update_verdict() {
        let db = setup_test_db();

        let id = record_evolution(
            &db,
            "verifier",
            None,
            "pv-789",
            None,
            None,
            None,
            Some(0.3),
        )
        .unwrap();

        update_verdict(&db, &id, "adopt", Some(0.75)).unwrap();

        let history = get_evolution_history(&db, Some("verifier"), 10).unwrap();
        assert_eq!(history[0].canary_verdict.as_deref(), Some("adopt"));
        assert_eq!(history[0].score_after, Some(0.75));
    }

    #[test]
    fn test_has_active_evolution() {
        let db = setup_test_db();

        assert!(!has_active_evolution(&db, "locator"));

        record_evolution(&db, "locator", None, "pv-1", None, None, None, None).unwrap();
        assert!(has_active_evolution(&db, "locator"));

        // Different agent should not be affected
        assert!(!has_active_evolution(&db, "implementer"));
    }

    #[test]
    fn test_get_latest_rejected() {
        let db = setup_test_db();

        let result = get_latest_rejected(&db, "verifier").unwrap();
        assert!(result.is_none());

        let id = record_evolution(
            &db,
            "verifier",
            None,
            "pv-1",
            None,
            Some("Bad rewrite"),
            Some("Removed role definition"),
            None,
        )
        .unwrap();

        update_verdict(&db, &id, "reject", Some(0.2)).unwrap();

        let rejected = get_latest_rejected(&db, "verifier").unwrap().unwrap();
        assert_eq!(rejected.changes_summary.as_deref(), Some("Removed role definition"));
    }

    #[test]
    fn test_cooldown() {
        let db = setup_test_db();

        assert!(!is_in_cooldown(&db, "spec_analyst", 24));

        record_evolution(&db, "spec_analyst", None, "pv-1", None, None, None, None).unwrap();
        assert!(is_in_cooldown(&db, "spec_analyst", 24));

        // Different agent should not be in cooldown
        assert!(!is_in_cooldown(&db, "locator", 24));
    }

    #[test]
    fn test_evolution_history_ordered_desc() {
        let db = setup_test_db();

        record_evolution(&db, "implementer", None, "pv-1", None, None, Some("first"), None)
            .unwrap();
        record_evolution(
            &db,
            "implementer",
            Some("pv-1"),
            "pv-2",
            None,
            None,
            Some("second"),
            None,
        )
        .unwrap();

        let history = get_evolution_history(&db, Some("implementer"), 10).unwrap();
        assert_eq!(history.len(), 2);
        // Most recent first
        assert_eq!(history[0].changes_summary.as_deref(), Some("second"));
        assert_eq!(history[1].changes_summary.as_deref(), Some("first"));
    }

    // ── Integration tests: recommendation linking and canary verdict flow ──

    #[test]
    fn test_recommendation_variant_linking() {
        let db = setup_test_db();

        // Simulate the parser flow: create evolution entry with recommendation_id
        let rec_id = "mor-test-rec-123";
        let variant_id = "pv-test-var-456";

        let evo_id = record_evolution(
            &db,
            "verifier",
            None,              // no parent (first rewrite)
            variant_id,
            Some(rec_id),      // linked to recommendation
            Some("The prompt lacks JSON output format spec"),
            Some("Added structured output requirements"),
            Some(0.35),        // score_before = 35%
        )
        .unwrap();

        // Verify the evolution entry links recommendation to variant
        let history = get_evolution_history(&db, Some("verifier"), 10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, evo_id);
        assert_eq!(history[0].recommendation_id.as_deref(), Some(rec_id));
        assert_eq!(history[0].variant_id, variant_id);
        assert!(history[0].canary_verdict.is_none()); // pending
        assert_eq!(history[0].score_before, Some(0.35));
        assert!(history[0].score_after.is_none());

        // There should be an active evolution (canary in progress)
        assert!(has_active_evolution(&db, "verifier"));
    }

    #[test]
    fn test_canary_verdict_feedback_loop_adopt() {
        let db = setup_test_db();

        // Step 1: Create evolution entry (simulating parser creating variant + starting canary)
        let rec_id = "mor-adopt-test";
        let evo_id = record_evolution(
            &db,
            "implementer",
            None,
            "pv-new-variant",
            Some(rec_id),
            Some("Missing error handling examples"),
            Some("Added error handling few-shot examples"),
            Some(0.40),
        )
        .unwrap();

        // Active evolution should exist
        assert!(has_active_evolution(&db, "implementer"));

        // Step 2: Canary completes — verdict: adopt (score improved to 0.72)
        update_verdict(&db, &evo_id, "adopt", Some(0.72)).unwrap();

        // Active evolution should be cleared (verdict is set)
        assert!(!has_active_evolution(&db, "implementer"));

        // Verify the evolution entry is updated
        let history = get_evolution_history(&db, Some("implementer"), 10).unwrap();
        assert_eq!(history[0].canary_verdict.as_deref(), Some("adopt"));
        assert_eq!(history[0].score_after, Some(0.72));
        assert!(history[0].score_after.unwrap() > history[0].score_before.unwrap());

        // No rejected entry should exist
        assert!(get_latest_rejected(&db, "implementer").unwrap().is_none());
    }

    #[test]
    fn test_canary_verdict_feedback_loop_reject() {
        let db = setup_test_db();

        // Step 1: Create evolution entry
        let rec_id = "mor-reject-test";
        let evo_id = record_evolution(
            &db,
            "spec_analyst",
            None,
            "pv-rejected-variant",
            Some(rec_id),
            Some("Prompt too verbose, agents confused by multi-step instructions"),
            Some("Simplified to single-step focus"),
            Some(0.45),
        )
        .unwrap();

        // Step 2: Canary completes — verdict: reject (score degraded to 0.30)
        update_verdict(&db, &evo_id, "reject", Some(0.30)).unwrap();

        // Active evolution cleared
        assert!(!has_active_evolution(&db, "spec_analyst"));

        // Step 3: Next optimizer round should see the rejected attempt
        let rejected = get_latest_rejected(&db, "spec_analyst").unwrap().unwrap();
        assert_eq!(rejected.changes_summary.as_deref(), Some("Simplified to single-step focus"));
        assert_eq!(rejected.score_after, Some(0.30));
        assert!(rejected.score_after.unwrap() < rejected.score_before.unwrap());

        // Step 4: Create a second attempt taking a different approach
        let evo_id2 = record_evolution(
            &db,
            "spec_analyst",
            Some("pv-rejected-variant"), // parent is the rejected variant
            "pv-second-attempt",
            Some("mor-reject-test-2"),
            Some("Previous simplification degraded quality; try adding examples instead"),
            Some("Added few-shot examples while preserving multi-step structure"),
            Some(0.45),
        )
        .unwrap();

        // Both entries should exist in history
        let history = get_evolution_history(&db, Some("spec_analyst"), 10).unwrap();
        assert_eq!(history.len(), 2);
        // Most recent first
        assert_eq!(history[0].id, evo_id2);
        assert_eq!(history[0].parent_variant_id.as_deref(), Some("pv-rejected-variant"));

        // Active evolution for second attempt
        assert!(has_active_evolution(&db, "spec_analyst"));

        // Step 5: Second attempt succeeds
        update_verdict(&db, &evo_id2, "adopt", Some(0.68)).unwrap();

        // Latest rejected is still the first attempt (adopt != reject)
        let rejected = get_latest_rejected(&db, "spec_analyst").unwrap().unwrap();
        assert_eq!(rejected.id, evo_id);
    }

    #[test]
    fn test_update_verdict_by_variant_closes_loop() {
        let db = setup_test_db();

        // Create evolution entry
        record_evolution(
            &db,
            "locator",
            None,
            "pv-canary-test",
            Some("mor-canary-rec"),
            None,
            None,
            Some(0.50),
        )
        .unwrap();

        assert!(has_active_evolution(&db, "locator"));

        // Update via variant_id (simulates canary.rs calling update_verdict_by_variant)
        update_verdict_by_variant(&db, "pv-canary-test", "reject", Some(0.40)).unwrap();

        assert!(!has_active_evolution(&db, "locator"));

        let history = get_evolution_history(&db, Some("locator"), 10).unwrap();
        assert_eq!(history[0].canary_verdict.as_deref(), Some("reject"));
        assert_eq!(history[0].score_after, Some(0.40));
    }
}
