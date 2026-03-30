//! PostgreSQL persistence layer for the entailment cache.
//!
//! Provides write-through caching: the in-memory EntailmentCache is the hot
//! layer; this module backs it with PG so cache entries survive restarts.

use tracing::warn;

use super::PgDb;

/// A single cached entailment row from PG.
pub struct PgCachedEntailment {
    pub score: f64,
    pub explanation: Option<String>,
    pub tier: Option<String>,
}

impl PgDb {
    /// Look up a single entailment cache entry by (criterion_hash, step_hash).
    pub async fn get_entailment_cache(
        &self,
        criterion_hash: i64,
        step_hash: i64,
    ) -> Option<PgCachedEntailment> {
        let conn = self.pool.get().await.ok()?;
        let row = conn
            .query_opt(
                "SELECT score, explanation, tier FROM entailment_cache \
                 WHERE criterion_hash = $1 AND step_hash = $2",
                &[&criterion_hash, &step_hash],
            )
            .await
            .ok()
            .flatten()?;

        Some(PgCachedEntailment {
            score: row.get("score"),
            explanation: row.get("explanation"),
            tier: row.get("tier"),
        })
    }

    /// Upsert a single entailment cache entry.
    pub async fn set_entailment_cache(
        &self,
        criterion_hash: i64,
        step_hash: i64,
        score: f64,
        explanation: Option<&str>,
        tier: Option<&str>,
    ) {
        let conn = match self.pool.get().await {
            Ok(c) => c,
            Err(e) => {
                warn!("entailment_cache set: PG pool error: {}", e);
                return;
            }
        };

        if let Err(e) = conn
            .execute(
                "INSERT INTO entailment_cache (criterion_hash, step_hash, score, explanation, tier, cached_at) \
                 VALUES ($1, $2, $3, $4, $5, NOW()) \
                 ON CONFLICT (criterion_hash, step_hash) \
                 DO UPDATE SET score = EXCLUDED.score, \
                               explanation = EXCLUDED.explanation, \
                               tier = EXCLUDED.tier, \
                               cached_at = NOW()",
                &[&criterion_hash, &step_hash, &score, &explanation, &tier],
            )
            .await
        {
            warn!("entailment_cache set failed: {}", e);
        }
    }

    /// Batch lookup of entailment cache entries.
    /// Returns results in the same order as `pairs`; missing entries are None.
    pub async fn get_entailment_cache_batch(
        &self,
        pairs: &[(i64, i64)],
    ) -> Vec<Option<PgCachedEntailment>> {
        if pairs.is_empty() {
            return Vec::new();
        }

        let conn = match self.pool.get().await {
            Ok(c) => c,
            Err(e) => {
                warn!("entailment_cache batch: PG pool error: {}", e);
                return pairs.iter().map(|_| None).collect();
            }
        };

        // Build a VALUES list for efficient batch lookup
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync>> = Vec::new();
        let mut value_clauses = Vec::new();
        for (i, (ch, sh)) in pairs.iter().enumerate() {
            let p1 = i * 2 + 1;
            let p2 = i * 2 + 2;
            value_clauses.push(format!("(${}, ${})", p1, p2));
            params.push(Box::new(*ch));
            params.push(Box::new(*sh));
        }

        let sql = format!(
            "SELECT criterion_hash, step_hash, score, explanation, tier \
             FROM entailment_cache \
             WHERE (criterion_hash, step_hash) IN ({})",
            value_clauses.join(", ")
        );

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p.as_ref()).collect();

        let rows = match conn.query(&sql, &param_refs).await {
            Ok(r) => r,
            Err(e) => {
                warn!("entailment_cache batch query failed: {}", e);
                return pairs.iter().map(|_| None).collect();
            }
        };

        // Index results by (criterion_hash, step_hash) for fast lookup
        let mut map = std::collections::HashMap::new();
        for row in &rows {
            let ch: i64 = row.get("criterion_hash");
            let sh: i64 = row.get("step_hash");
            map.insert(
                (ch, sh),
                PgCachedEntailment {
                    score: row.get("score"),
                    explanation: row.get("explanation"),
                    tier: row.get("tier"),
                },
            );
        }

        pairs
            .iter()
            .map(|pair| map.remove(pair))
            .collect()
    }

    /// Truncate the entailment cache (full invalidation).
    pub async fn clear_entailment_cache(&self) {
        let conn = match self.pool.get().await {
            Ok(c) => c,
            Err(e) => {
                warn!("entailment_cache clear: PG pool error: {}", e);
                return;
            }
        };

        if let Err(e) = conn.execute("TRUNCATE entailment_cache", &[]).await {
            warn!("entailment_cache truncate failed: {}", e);
        }
    }

    /// Delete entailment cache entries older than `max_age_hours`.
    pub async fn trim_entailment_cache(&self, max_age_hours: i64) {
        let conn = match self.pool.get().await {
            Ok(c) => c,
            Err(e) => {
                warn!("entailment_cache trim: PG pool error: {}", e);
                return;
            }
        };

        if let Err(e) = conn
            .execute(
                "DELETE FROM entailment_cache WHERE cached_at < NOW() - ($1 || ' hours')::interval",
                &[&max_age_hours.to_string()],
            )
            .await
        {
            warn!("entailment_cache trim failed: {}", e);
        }
    }
}
