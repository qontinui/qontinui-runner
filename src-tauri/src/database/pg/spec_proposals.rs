//! PostgreSQL CRUD for `project.spec_proposals` — Stream E (Flywheel) queue.
//!
//! The table is authored declaratively in `atlas/schema.hcl`; this module
//! issues the queries directly via `tokio_postgres`, mirroring the
//! `regression.rs` / `merge_proposals.rs` style. There is no self-heal helper
//! here — Atlas owns the table shape and the runner expects it to be present
//! when `feature = "spec-authoring"` is enabled.
//!
//! Lifecycle (`status` column) is driven by Stream E:
//!   queued → inFlight → pendingValidation → pendingPromotion → promoted
//!                                                                    failed
//! See `spec_api/proposals.rs` for the execution handler that transitions
//! `queued → inFlight → pendingPromotion | failed` and the supervisor cron
//! (Step 10) for `pendingPromotion → promoted` after 2 consecutive greens.

use super::PgDb;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One row from `project.spec_proposals`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecProposalRow {
    pub id: String,
    pub kind: String,
    pub pathname: Option<String>,
    pub spec_id: Option<String>,
    pub status: String,
    pub created_at: String,
    pub last_attempt_at: Option<String>,
    pub consecutive_greens: i32,
    pub last_error: Option<String>,
    pub candidate_ir: Option<Value>,
    pub metadata: Value,
}

impl PgDb {
    // -------------------------------------------------------------------------
    // Writes
    // -------------------------------------------------------------------------

    /// Insert a new proposal row. Returns `Ok(true)` if a row was inserted,
    /// `Ok(false)` if the unique-on-(kind, COALESCE(pathname, spec_id))
    /// constraint suppressed the insert via `ON CONFLICT DO NOTHING`.
    ///
    /// `kind` must be `"fullPage"` or `"patch"`; the CHECK constraint enforces
    /// this server-side, but callers should validate upstream so the rejection
    /// surfaces as a structured `SpecError` rather than a raw PG error.
    pub async fn insert_proposal(
        &self,
        id: &str,
        kind: &str,
        pathname: Option<&str>,
        spec_id: Option<&str>,
        status: &str,
        metadata: &Value,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // ON CONFLICT DO NOTHING with no target catches both the PK and the
        // unique `spec_proposals_kind_target_uniq` index. The PK is a fresh
        // UUIDv7-shaped id from `mint_proposal_id` so PK collisions are
        // effectively zero — the only realistic conflict is the unique
        // dedup index, which is the intended swallow.
        let rows_affected = conn
            .execute(
                r#"
                INSERT INTO spec_proposals (id, kind, pathname, spec_id, status, metadata)
                VALUES ($1, $2, $3, $4, $5, $6::jsonb)
                ON CONFLICT DO NOTHING
                "#,
                &[&id, &kind, &pathname, &spec_id, &status, metadata],
            )
            .await
            .map_err(|e| format!("PG insert_proposal: {}", e))?;

        Ok(rows_affected == 1)
    }

    /// Update only the status of a proposal. Does NOT touch
    /// `last_attempt_at` / `last_error` — use the more specific helpers when
    /// recording attempts or failures.
    pub async fn update_proposal_status(
        &self,
        id: &str,
        status: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE spec_proposals SET status = $2 WHERE id = $1",
            &[&id, &status],
        )
        .await
        .map_err(|e| format!("PG update_proposal_status: {}", e))?;
        Ok(())
    }

    /// Mark an attempt without changing status — used to record the start of
    /// an execute call before authoring kicks off.
    pub async fn update_proposal_attempt_only(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE spec_proposals SET last_attempt_at = now() WHERE id = $1",
            &[&id],
        )
        .await
        .map_err(|e| format!("PG update_proposal_attempt_only: {}", e))?;
        Ok(())
    }

    /// Terminal-failure update: status='failed' + last_error populated +
    /// last_attempt_at bumped. Idempotent.
    pub async fn update_proposal_status_with_error(
        &self,
        id: &str,
        status: &str,
        last_error: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            r#"UPDATE spec_proposals
               SET status = $2,
                   last_error = $3,
                   last_attempt_at = now()
               WHERE id = $1"#,
            &[&id, &status, &last_error],
        )
        .await
        .map_err(|e| format!("PG update_proposal_status_with_error: {}", e))?;
        Ok(())
    }

    /// Set `consecutive_greens` to an absolute value (e.g. 0 on red, +1 on
    /// green). The promotion sweep (Step 10) decides when to use this.
    pub async fn update_proposal_greens(
        &self,
        id: &str,
        consecutive_greens: i32,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE spec_proposals SET consecutive_greens = $2 WHERE id = $1",
            &[&id, &consecutive_greens],
        )
        .await
        .map_err(|e| format!("PG update_proposal_greens: {}", e))?;
        Ok(())
    }

    /// Persist the authored candidate IR + transition to a new status
    /// (typically `pendingValidation` or `pendingPromotion`).
    pub async fn update_proposal_candidate_ir(
        &self,
        id: &str,
        status: &str,
        candidate_ir: &Value,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            r#"UPDATE spec_proposals
               SET status = $2,
                   candidate_ir = $3::jsonb,
                   last_attempt_at = now(),
                   last_error = NULL
               WHERE id = $1"#,
            &[&id, &status, candidate_ir],
        )
        .await
        .map_err(|e| format!("PG update_proposal_candidate_ir: {}", e))?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Reads
    // -------------------------------------------------------------------------

    /// Fetch a single proposal by id. Returns `Ok(None)` if missing.
    pub async fn find_proposal_by_id(
        &self,
        id: &str,
    ) -> Result<Option<SpecProposalRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"
                SELECT
                    id,
                    kind,
                    pathname,
                    spec_id,
                    status,
                    created_at::TEXT,
                    last_attempt_at::TEXT,
                    consecutive_greens,
                    last_error,
                    candidate_ir,
                    metadata
                FROM spec_proposals
                WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG find_proposal_by_id: {}", e))?;

        Ok(rows.first().map(row_to_proposal))
    }

    /// Paginated list of proposals, ordered by `created_at DESC, id`.
    pub async fn list_proposals(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SpecProposalRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let lim = limit.clamp(1, 1000);
        let off = offset.max(0);
        let rows = conn
            .query(
                r#"
                SELECT
                    id,
                    kind,
                    pathname,
                    spec_id,
                    status,
                    created_at::TEXT,
                    last_attempt_at::TEXT,
                    consecutive_greens,
                    last_error,
                    candidate_ir,
                    metadata
                FROM spec_proposals
                ORDER BY created_at DESC, id
                LIMIT $1 OFFSET $2
                "#,
                &[&lim, &off],
            )
            .await
            .map_err(|e| format!("PG list_proposals: {}", e))?;

        Ok(rows.iter().map(row_to_proposal).collect())
    }

    /// Proposals filtered by a single status value, ordered by
    /// `created_at DESC`. Used by the supervisor cron to find
    /// `pendingPromotion` candidates.
    pub async fn list_proposals_by_status(
        &self,
        status: &str,
        limit: i64,
    ) -> Result<Vec<SpecProposalRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let lim = limit.clamp(1, 1000);
        let rows = conn
            .query(
                r#"
                SELECT
                    id,
                    kind,
                    pathname,
                    spec_id,
                    status,
                    created_at::TEXT,
                    last_attempt_at::TEXT,
                    consecutive_greens,
                    last_error,
                    candidate_ir,
                    metadata
                FROM spec_proposals
                WHERE status = $1
                ORDER BY created_at DESC, id
                LIMIT $2
                "#,
                &[&status, &lim],
            )
            .await
            .map_err(|e| format!("PG list_proposals_by_status: {}", e))?;

        Ok(rows.iter().map(row_to_proposal).collect())
    }
}

fn row_to_proposal(r: &tokio_postgres::Row) -> SpecProposalRow {
    SpecProposalRow {
        id: r.get(0),
        kind: r.get(1),
        pathname: r.get(2),
        spec_id: r.get(3),
        status: r.get(4),
        created_at: r.get(5),
        last_attempt_at: r.get(6),
        consecutive_greens: r.get(7),
        last_error: r.get(8),
        candidate_ir: r.get(9),
        metadata: r.get(10),
    }
}
