//! PostgreSQL CRUD for the `coord.coordinator_shadow_decisions` table
//! (alembic revision `sd01_coord_coordinator_shadow_decisions`).
//!
//! Backs the soak window comparing the in-process Rust coordinator
//! scheduler against the legacy `/coordinate` Claude-skill loop. When
//! `QONTINUI_COORDINATOR_SHADOW=1` the Rust scheduler still observes +
//! decides on every tick but does NOT acquire the lease and does NOT call
//! `coordinator/act::apply`; instead it inserts the would-be-action here.
//!
//! The diff endpoint (`mcp/coordinator.rs::shadow_diff`) joins shadow
//! rows against live `coord.coordinator_decisions` rows by
//! `observation_hash` to surface per-rule agreement / disagreement rates.
//!
//! UUIDs round-trip as TEXT for the same reason as `coordinator_decisions`
//! — tokio-postgres in this crate doesn't enable `with-uuid-1`.

use super::PgDb;
use serde::{Deserialize, Serialize};

/// One row from `coord.coordinator_shadow_decisions`. Mirrors the live
/// `CoordinatorDecisionRow` shape minus the resolution columns (shadow
/// rows are never resolved — they're audit-only).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinatorShadowDecisionRow {
    pub id: String,
    pub instance_id: String,
    pub iteration: i64,
    pub observation_hash: String,
    pub rule: String,
    pub action: String,
    pub target_id: Option<String>,
    pub reasoning: String,
    pub would_have_acted: bool,
    pub taken_at: String,
}

/// Input shape for [`PgDb::insert_coordinator_shadow_decision`]. The DB
/// allocates the UUID and stamps `taken_at`.
#[derive(Debug, Clone)]
pub struct InsertShadowDecisionInput<'a> {
    pub instance_id: &'a str,
    pub iteration: i64,
    pub observation_hash: &'a str,
    pub rule: &'a str,
    pub action: &'a str,
    pub target_id: Option<&'a str>,
    pub reasoning: &'a str,
    pub would_have_acted: bool,
}

const SELECT_COLS: &str = r#"
    id::text, instance_id, iteration, observation_hash, rule, action,
    target_id, reasoning, would_have_acted, taken_at::text
"#;

fn row_to_shadow(r: &tokio_postgres::Row) -> CoordinatorShadowDecisionRow {
    CoordinatorShadowDecisionRow {
        id: r.get(0),
        instance_id: r.get(1),
        iteration: r.get(2),
        observation_hash: r.get(3),
        rule: r.get(4),
        action: r.get(5),
        target_id: r.get(6),
        reasoning: r.get(7),
        would_have_acted: r.get(8),
        taken_at: r.get(9),
    }
}

/// One bucket from [`PgDb::shadow_diff_aggregate`]. Each row pairs the
/// shadow tally and live tally for a given `(rule, action)` cell over the
/// caller's time window. Either side may be zero — if shadow fired Rule
/// B/assign-task but the live skill never fired the same combo, the
/// `live_count` will be 0 and the cell highlights divergence.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShadowDiffBucket {
    pub rule: String,
    pub action: String,
    pub shadow_count: i64,
    pub live_count: i64,
    /// Rows where the shadow scheduler picked this `(rule, action)` AND a
    /// live decision row with the same `observation_hash` exists. Joins
    /// on `observation_hash`; rows with empty hash on either side don't
    /// contribute.
    pub matched_observations: i64,
    /// Of the matched observations, how many had identical `(rule,
    /// action)` on both sides. `disagreements_on_match = matched_observations - agreements_on_match`.
    pub agreements_on_match: i64,
}

impl PgDb {
    /// Insert a shadow decision row. Used by the Rust scheduler when
    /// `QONTINUI_COORDINATOR_SHADOW=1` and the lease was NOT acquired (so
    /// the live `/coordinate` skill is in charge of acting).
    pub async fn insert_coordinator_shadow_decision(
        &self,
        input: &InsertShadowDecisionInput<'_>,
    ) -> Result<CoordinatorShadowDecisionRow, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                &format!(
                    r#"
                    INSERT INTO coord.coordinator_shadow_decisions
                        (instance_id, iteration, observation_hash, rule,
                         action, target_id, reasoning, would_have_acted)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                    RETURNING {}
                    "#,
                    SELECT_COLS
                ),
                &[
                    &input.instance_id,
                    &input.iteration,
                    &input.observation_hash,
                    &input.rule,
                    &input.action,
                    &input.target_id,
                    &input.reasoning,
                    &input.would_have_acted,
                ],
            )
            .await
            .map_err(|e| format!("Failed to insert shadow decision: {}", e))?;

        Ok(row_to_shadow(&row))
    }

    /// Self-heal the shadow table on PG instances where the alembic
    /// migration hasn't been applied yet. Idempotent. Mirrors the
    /// self-heal pattern coord tables already use elsewhere via
    /// `database/pg/mod.rs::PgDb::new`. Called from the scheduler boot
    /// path so the runner can write shadow rows without waiting for
    /// qontinui-web's alembic upgrade.
    pub async fn ensure_shadow_decisions_table(&self) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.batch_execute(
            r#"
            CREATE TABLE IF NOT EXISTS coord.coordinator_shadow_decisions (
                id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                instance_id       TEXT NOT NULL,
                iteration         BIGINT NOT NULL,
                observation_hash  TEXT NOT NULL,
                rule              TEXT NOT NULL,
                action            TEXT NOT NULL,
                target_id         TEXT,
                reasoning         TEXT NOT NULL,
                would_have_acted  BOOLEAN NOT NULL,
                taken_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );
            CREATE INDEX IF NOT EXISTS idx_csd_taken_at
                ON coord.coordinator_shadow_decisions(taken_at DESC);
            CREATE INDEX IF NOT EXISTS idx_csd_obs_hash
                ON coord.coordinator_shadow_decisions(observation_hash);
            CREATE INDEX IF NOT EXISTS idx_csd_instance
                ON coord.coordinator_shadow_decisions(instance_id, taken_at DESC);
            ALTER TABLE coord.coordinator_decisions
                ADD COLUMN IF NOT EXISTS observation_hash TEXT NOT NULL DEFAULT '';
            "#,
        )
        .await
        .map_err(|e| format!("Failed to ensure shadow_decisions table: {}", e))
    }

    /// List recent shadow rows, newest-first. Used by the soak dashboard.
    pub async fn list_recent_shadow_decisions(
        &self,
        limit: i64,
    ) -> Result<Vec<CoordinatorShadowDecisionRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                &format!(
                    r#"
                    SELECT {}
                    FROM coord.coordinator_shadow_decisions
                    ORDER BY taken_at DESC
                    LIMIT $1
                    "#,
                    SELECT_COLS
                ),
                &[&limit],
            )
            .await
            .map_err(|e| format!("Failed to list shadow decisions: {}", e))?;

        Ok(rows.iter().map(row_to_shadow).collect())
    }

    /// Aggregate shadow vs live decisions over the last `window_seconds`
    /// for the soak diff endpoint. Returns one bucket per `(rule, action)`
    /// pair seen on either side.
    ///
    /// `matched_observations` and `agreements_on_match` reduce the join
    /// to a single number per cell — the per-row payload would be huge
    /// over a 24h window. Callers wanting the raw join can use
    /// [`Self::shadow_diff_disagreements`] for the bounded sample.
    pub async fn shadow_diff_aggregate(
        &self,
        window_seconds: i64,
    ) -> Result<Vec<ShadowDiffBucket>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Build the union of (rule, action) pairs seen on either side over
        // the window, then count both sides per cell. The matched/agreed
        // counts join shadow ↔ live by observation_hash AFTER deduping each
        // side to one row per hash — without the dedupe, the prior
        // implementation produced a cartesian product (N shadow rows ×
        // M live rows for the same hash) and inflated matched_observations
        // by 100×–1000× plus computed an artificial 1.0 agreement_rate.
        //
        // The dedupe rule: pick the FIRST decision per (side, observation_hash)
        // by timestamp. The first decision is the one the rules emitted on
        // the original observation; subsequent ticks on the same hash are
        // re-fires of the same logical event, not new comparisons.
        let rows = conn
            .query(
                r#"
                WITH window_shadow AS (
                    SELECT DISTINCT ON (observation_hash)
                        observation_hash, rule, action
                    FROM coord.coordinator_shadow_decisions
                    WHERE taken_at > NOW() - make_interval(secs => $1::float)
                      AND observation_hash <> ''
                    ORDER BY observation_hash, taken_at ASC
                ),
                window_live AS (
                    SELECT DISTINCT ON (observation_hash)
                        observation_hash, rule, action
                    FROM coord.coordinator_decisions
                    WHERE created_at > NOW() - make_interval(secs => $1::float)
                      AND observation_hash <> ''
                    ORDER BY observation_hash, created_at ASC
                ),
                -- Raw counts use the un-deduped tables so the shadowCount /
                -- liveCount reflect every decision (idle ticks included),
                -- while the matched/agreement counts use the deduped CTEs.
                window_shadow_raw AS (
                    SELECT rule, action
                    FROM coord.coordinator_shadow_decisions
                    WHERE taken_at > NOW() - make_interval(secs => $1::float)
                ),
                window_live_raw AS (
                    SELECT rule, action
                    FROM coord.coordinator_decisions
                    WHERE created_at > NOW() - make_interval(secs => $1::float)
                ),
                shadow_counts AS (
                    SELECT rule, action, COUNT(*) AS n
                    FROM window_shadow_raw GROUP BY 1, 2
                ),
                live_counts AS (
                    SELECT rule, action, COUNT(*) AS n
                    FROM window_live_raw GROUP BY 1, 2
                ),
                cells AS (
                    SELECT rule, action FROM shadow_counts
                    UNION
                    SELECT rule, action FROM live_counts
                ),
                -- Now a true 1:1 join on observation_hash — each hash appears
                -- at most once on each side, so the join cardinality is the
                -- count of observation_hashes that appear on BOTH sides.
                matched AS (
                    SELECT
                        s.rule AS shadow_rule,
                        s.action AS shadow_action,
                        l.rule AS live_rule,
                        l.action AS live_action
                    FROM window_shadow s
                    INNER JOIN window_live l USING (observation_hash)
                ),
                match_per_shadow_cell AS (
                    SELECT shadow_rule AS rule, shadow_action AS action,
                           COUNT(*) AS matches,
                           SUM(CASE WHEN shadow_rule = live_rule
                                     AND shadow_action = live_action
                                    THEN 1 ELSE 0 END) AS agree
                    FROM matched
                    GROUP BY 1, 2
                )
                SELECT
                    c.rule,
                    c.action,
                    COALESCE(s.n, 0)::bigint AS shadow_count,
                    COALESCE(l.n, 0)::bigint AS live_count,
                    COALESCE(m.matches, 0)::bigint AS matched_observations,
                    COALESCE(m.agree, 0)::bigint AS agreements_on_match
                FROM cells c
                LEFT JOIN shadow_counts s USING (rule, action)
                LEFT JOIN live_counts l USING (rule, action)
                LEFT JOIN match_per_shadow_cell m USING (rule, action)
                ORDER BY
                    GREATEST(COALESCE(s.n, 0), COALESCE(l.n, 0)) DESC,
                    c.rule, c.action
                "#,
                &[&(window_seconds as f64)],
            )
            .await
            .map_err(|e| format!("Failed to aggregate shadow diff: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| ShadowDiffBucket {
                rule: r.get(0),
                action: r.get(1),
                shadow_count: r.get(2),
                live_count: r.get(3),
                matched_observations: r.get(4),
                agreements_on_match: r.get(5),
            })
            .collect())
    }

    /// Pull a small sample of disagreement rows for human inspection.
    /// Returned shape is `(shadow_row, live_action)` pairs where the
    /// shadow scheduler picked one action and the live `/coordinate`
    /// (matched by observation_hash) picked a different one.
    ///
    /// Caps at `limit` so a noisy soak window can still answer the
    /// diff endpoint cheaply.
    pub async fn shadow_diff_disagreements(
        &self,
        window_seconds: i64,
        limit: i64,
    ) -> Result<Vec<(CoordinatorShadowDecisionRow, String, String)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                &format!(
                    r#"
                    SELECT
                        {select_shadow},
                        l.rule AS live_rule,
                        l.action AS live_action
                    FROM coord.coordinator_shadow_decisions s
                    INNER JOIN coord.coordinator_decisions l
                        ON l.observation_hash = s.observation_hash
                    WHERE s.taken_at > NOW() - make_interval(secs => $1::float)
                      AND s.observation_hash <> ''
                      AND (s.rule <> l.rule OR s.action <> l.action)
                    ORDER BY s.taken_at DESC
                    LIMIT $2
                    "#,
                    select_shadow = SELECT_COLS
                        .replace("id::text,", "s.id::text,")
                        .replace("instance_id,", "s.instance_id,")
                        .replace("iteration,", "s.iteration,")
                        .replace("observation_hash,", "s.observation_hash,")
                        .replace("rule, action,", "s.rule, s.action,")
                        .replace(
                            "target_id, reasoning, would_have_acted,",
                            "s.target_id, s.reasoning, s.would_have_acted,"
                        )
                        .replace("taken_at::text", "s.taken_at::text"),
                ),
                &[&(window_seconds as f64), &limit],
            )
            .await
            .map_err(|e| format!("Failed to pull shadow disagreements: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    row_to_shadow(r),
                    r.get::<_, String>(10),
                    r.get::<_, String>(11),
                )
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_cols_contains_all_fields() {
        // Sanity check that the SELECT_COLS string and the row_to_shadow
        // function stay in sync — adding a column without updating both
        // produces a runtime panic on .get(idx) which is hard to debug.
        let columns: Vec<&str> = SELECT_COLS
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(columns.len(), 10, "SELECT_COLS must yield 10 columns");
    }
}
