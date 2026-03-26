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
    pub created_at: String,
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
    let id = format!("pe-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();

    let id_clone = id.clone();
    let agent_type = agent_type.to_string();
    let parent_variant_id = parent_variant_id.map(|s| s.to_string());
    let variant_id = variant_id.to_string();
    let recommendation_id = recommendation_id.map(|s| s.to_string());
    let critique = critique.map(|s| s.to_string());
    let changes_summary = changes_summary.map(|s| s.to_string());

    db.with_conn(move |conn| {
        conn.execute(
            r#"INSERT INTO prompt_evolution
               (id, agent_type, parent_variant_id, variant_id, recommendation_id,
                critique, changes_summary, canary_verdict, score_before, score_after, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, NULL, ?9)"#,
            params![
                id_clone,
                agent_type,
                parent_variant_id,
                variant_id,
                recommendation_id,
                critique,
                changes_summary,
                score_before,
                now,
            ],
        )
        .map_err(|e| format!("Failed to record prompt evolution: {}", e))?;

        info!(
            "Recorded prompt evolution {} for agent {} (variant {})",
            id_clone, agent_type, variant_id
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
    pg_db: Option<&std::sync::Arc<crate::database::pg::PgDb>>,
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
    if let Some(pg) = pg_db {
        let pg = pg.clone();
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
    }

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
                              created_at
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
                              created_at
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
                    created_at: row.get(10)?,
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
                      created_at
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
                    created_at: row.get(10)?,
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
}
