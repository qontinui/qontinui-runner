//! PostgreSQL CRUD for `project.proposal_events` — Plan 06 Step 6 (G.6)
//! flywheel observability.
//!
//! The table is authored declaratively in `atlas/schema.hcl`; this module
//! issues the queries directly via `tokio_postgres`, mirroring the
//! `spec_proposals.rs` style. There is no self-heal helper here — Atlas owns
//! the table shape.
//!
//! Append-only log of state transitions on `spec_proposals` rows. Each row is
//! written alongside the corresponding `SpecApiEvent` broadcast (Plan 06
//! Step 2). Persistence is decoupled from broadcast — a subscriber that drops
//! events still gets full history from this table.
//!
//! `event_type` is constrained server-side to one of
//! `('scanned','executed','promoted','demoted','failed')`.

use super::PgDb;
use serde::{Deserialize, Serialize};

/// One row from `project.proposal_events`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalEventRow {
    pub id: String,
    pub proposal_id: String,
    pub event_type: String,
    pub snapshot_id: Option<String>,
    pub failing_assertion_id: Option<String>,
    pub at: String,
}

impl PgDb {
    // -------------------------------------------------------------------------
    // Writes
    // -------------------------------------------------------------------------

    /// Insert a single proposal event row. Generates a UUIDv7 id, returns it
    /// for caller-side correlation. `event_type` must satisfy the
    /// `proposal_events_type_chk` CHECK constraint (server-enforced); callers
    /// should validate upstream so a bad variant surfaces as a structured
    /// error rather than a raw PG `check_violation`.
    pub async fn insert_proposal_event(
        &self,
        proposal_id: &str,
        event_type: &str,
        snapshot_id: Option<&str>,
        failing_assertion_id: Option<&str>,
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::now_v7().to_string();
        conn.execute(
            r#"
            INSERT INTO proposal_events
                (id, proposal_id, event_type, snapshot_id, failing_assertion_id)
            VALUES ($1, $2, $3, $4, $5)
            "#,
            &[
                &id,
                &proposal_id,
                &event_type,
                &snapshot_id,
                &failing_assertion_id,
            ],
        )
        .await
        .map_err(|e| format!("PG insert_proposal_event: {}", e))?;

        Ok(id)
    }

    // -------------------------------------------------------------------------
    // Reads
    // -------------------------------------------------------------------------

    /// Most-recent events across all proposals, ordered by `at DESC`. Backs
    /// the `qontinui-specs flywheel` CLI "Recent activity" block (Plan 06
    /// Step 6).
    pub async fn list_recent_proposal_events(
        &self,
        limit: i64,
    ) -> Result<Vec<ProposalEventRow>, String> {
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
                    proposal_id,
                    event_type,
                    snapshot_id,
                    failing_assertion_id,
                    at::TEXT
                FROM proposal_events
                ORDER BY at DESC, id
                LIMIT $1
                "#,
                &[&lim],
            )
            .await
            .map_err(|e| format!("PG list_recent_proposal_events: {}", e))?;

        Ok(rows.iter().map(row_to_event).collect())
    }

    /// Count of events of a given `event_type` in the trailing `days`-day
    /// window. Backs the CLI's "promotions / demotions last week" tiles.
    /// Caller passes the type literal — no client-side enum-shape coupling.
    pub async fn count_proposal_events_in_window(
        &self,
        days: i32,
        event_type: &str,
    ) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // `make_interval(days := $1)` keeps the cast off the SQL side and
        // avoids the `'$1 days'::interval` string-concat shape that some
        // drivers refuse to bind across.
        let row = conn
            .query_one(
                r#"
                SELECT count(*)::bigint
                FROM proposal_events
                WHERE event_type = $2
                  AND at > now() - make_interval(days => $1)
                "#,
                &[&days, &event_type],
            )
            .await
            .map_err(|e| format!("PG count_proposal_events_in_window: {}", e))?;

        Ok(row.get::<usize, i64>(0))
    }
}

fn row_to_event(r: &tokio_postgres::Row) -> ProposalEventRow {
    ProposalEventRow {
        id: r.get(0),
        proposal_id: r.get(1),
        event_type: r.get(2),
        snapshot_id: r.get(3),
        failing_assertion_id: r.get(4),
        at: r.get(5),
    }
}
