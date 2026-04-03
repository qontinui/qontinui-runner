//! PostgreSQL wrapper for prompt evolution operations.
//!
//! Async dual-write counterpart to meta_optimizer::prompt_evolution (SQLite).
//! Fire-and-forget from callers via tokio::spawn.

use tracing::info;

use super::PgDb;
use crate::meta_optimizer::prompt_evolution::PromptEvolutionEntry;

impl PgDb {
    /// Record a new prompt evolution entry.
    pub async fn record_prompt_evolution(
        &self,
        id: &str,
        agent_type: &str,
        parent_variant_id: Option<&str>,
        variant_id: &str,
        recommendation_id: Option<&str>,
        critique: Option<&str>,
        changes_summary: Option<&str>,
        score_before: Option<f64>,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"INSERT INTO prompt_evolution
               (id, agent_type, parent_variant_id, variant_id, recommendation_id,
                critique, changes_summary, canary_verdict, score_before, score_after)
               VALUES ($1, $2, $3, $4, $5, $6, $7, NULL, $8, NULL)
               ON CONFLICT (id) DO NOTHING"#,
            &[
                &id,
                &agent_type,
                &parent_variant_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &variant_id,
                &recommendation_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &critique as &(dyn tokio_postgres::types::ToSql + Sync),
                &changes_summary as &(dyn tokio_postgres::types::ToSql + Sync),
                &score_before as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("Failed to record prompt evolution: {}", e))?;

        info!(
            "PG: Recorded prompt evolution {} for agent {}",
            id, agent_type
        );
        Ok(())
    }

    /// Update the canary verdict and score for an evolution entry.
    pub async fn update_evolution_verdict(
        &self,
        evolution_id: &str,
        verdict: &str,
        score_after: Option<f64>,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            "UPDATE prompt_evolution SET canary_verdict = $1, score_after = $2 WHERE id = $3",
            &[
                &verdict,
                &score_after as &(dyn tokio_postgres::types::ToSql + Sync),
                &evolution_id,
            ],
        )
        .await
        .map_err(|e| format!("Failed to update evolution verdict: {}", e))?;

        Ok(())
    }

    /// Update verdict by recommendation_id (for canary feedback loop).
    pub async fn update_evolution_verdict_by_recommendation(
        &self,
        recommendation_id: &str,
        verdict: &str,
        score_after: Option<f64>,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            "UPDATE prompt_evolution SET canary_verdict = $1, score_after = $2 WHERE recommendation_id = $3 AND canary_verdict IS NULL",
            &[
                &verdict,
                &score_after as &(dyn tokio_postgres::types::ToSql + Sync),
                &recommendation_id,
            ],
        )
        .await
        .map_err(|e| format!("Failed to update evolution verdict by recommendation: {}", e))?;

        Ok(())
    }

    /// Get evolution history for an agent type.
    pub async fn get_evolution_history(
        &self,
        agent_type: Option<&str>,
        limit: i64,
    ) -> Result<Vec<PromptEvolutionEntry>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = if let Some(at) = agent_type {
            conn.query(
                r#"SELECT id, agent_type, parent_variant_id, variant_id, recommendation_id,
                          critique, changes_summary, canary_verdict, score_before, score_after,
                          baseline_prompt_hash, COALESCE(consecutive_rejections, 0), created_at::text
                   FROM prompt_evolution
                   WHERE agent_type = $1
                   ORDER BY created_at DESC
                   LIMIT $2"#,
                &[&at, &limit],
            )
            .await
        } else {
            conn.query(
                r#"SELECT id, agent_type, parent_variant_id, variant_id, recommendation_id,
                          critique, changes_summary, canary_verdict, score_before, score_after,
                          baseline_prompt_hash, COALESCE(consecutive_rejections, 0), created_at::text
                   FROM prompt_evolution
                   ORDER BY created_at DESC
                   LIMIT $1"#,
                &[&limit],
            )
            .await
        }
        .map_err(|e| format!("Failed to query evolution history: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| PromptEvolutionEntry {
                id: r.get(0),
                agent_type: r.get(1),
                parent_variant_id: r.get(2),
                variant_id: r.get(3),
                recommendation_id: r.get(4),
                critique: r.get(5),
                changes_summary: r.get(6),
                canary_verdict: r.get(7),
                score_before: r.get(8),
                score_after: r.get(9),
                baseline_prompt_hash: r.get(10),
                consecutive_rejections: r.get::<_, Option<i32>>(11).unwrap_or(0),
                created_at: r.get(12),
            })
            .collect())
    }
}
