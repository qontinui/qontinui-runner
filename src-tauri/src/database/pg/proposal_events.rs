//! PostgreSQL CRUD for `project.proposal_events` — Plan 06 Step 6 (G.6)
//! flywheel observability.
//!
//! The table is authored declaratively in `atlas/schema.hcl`; this module
//! issues the queries directly via `tokio_postgres`, mirroring the
//! `spec_proposals.rs` style. The runtime self-heal in `pg/mod.rs::PgDb::new`
//! backfills the `app_id` column on first boot after spec-multi-app Stream E.1.
//!
//! Append-only log of state transitions on `spec_proposals` rows. Each row is
//! written alongside the corresponding `SpecApiEvent` broadcast (Plan 06
//! Step 2). Persistence is decoupled from broadcast — a subscriber that drops
//! events still gets full history from this table.
//!
//! `event_type` is constrained server-side to one of
//! `('scanned','executed','promoted','demoted','failed')`.
//!
//! `app_id` (spec-multi-app Stream E.1) scopes every event to a registered
//! app. Reads are app-scoped via `list_recent_proposal_events_for_app` /
//! `count_proposal_events_in_window_for_app`; the legacy bare variants stay
//! around for the cross-app aggregate path used by the supervisor dashboard.

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
    /// spec-multi-app Stream E.1 — owning app.
    pub app_id: String,
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
    ///
    /// `app_id` is required (spec-multi-app Stream E.1) — every event is
    /// scoped to a registered app.
    pub async fn insert_proposal_event(
        &self,
        app_id: &str,
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
                (id, app_id, proposal_id, event_type, snapshot_id, failing_assertion_id)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
            &[
                &id,
                &app_id,
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

    /// Most-recent events across ALL apps, ordered by `at DESC`. Backs the
    /// `qontinui-specs flywheel` CLI cross-app "Recent activity" block (no
    /// `--app` filter).
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
                    at::TEXT,
                    app_id
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

    /// Most-recent events for ONE app, ordered by `at DESC`. Backs the
    /// `qontinui-specs flywheel --app <id>` CLI surface.
    pub async fn list_recent_proposal_events_for_app(
        &self,
        app_id: &str,
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
                    at::TEXT,
                    app_id
                FROM proposal_events
                WHERE app_id = $1
                ORDER BY at DESC, id
                LIMIT $2
                "#,
                &[&app_id, &lim],
            )
            .await
            .map_err(|e| format!("PG list_recent_proposal_events_for_app: {}", e))?;

        Ok(rows.iter().map(row_to_event).collect())
    }

    /// Count of events of a given `event_type` in the trailing `days`-day
    /// window across ALL apps. Backs the cross-app aggregate CLI surface.
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

    /// Count of events of a given `event_type` in the trailing `days`-day
    /// window for ONE app. Backs the per-app CLI tiles.
    pub async fn count_proposal_events_in_window_for_app(
        &self,
        app_id: &str,
        days: i32,
        event_type: &str,
    ) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                r#"
                SELECT count(*)::bigint
                FROM proposal_events
                WHERE app_id = $1
                  AND event_type = $2
                  AND at > now() - make_interval(days => $3)
                "#,
                &[&app_id, &event_type, &days],
            )
            .await
            .map_err(|e| format!("PG count_proposal_events_in_window_for_app: {}", e))?;

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
        app_id: r.get(6),
    }
}
