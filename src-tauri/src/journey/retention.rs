//! Runner-side retention of the journey edge ledger.
//!
//! The runner writes edges into its OWN database (normally the embedded
//! Postgres), which qontinui-web's `journey_edge_retention` job can never
//! reach — so retention runs where the rows live (plan Phase 0 decision 2,
//! amended at pre-PR review). Same 90-day window as the web job, same
//! committed-chunk shape: an interrupted pass keeps the chunks it finished.
//!
//! Scheduling reuses the runner's existing periodic-maintenance mechanism —
//! a self-timed loop spawned once from `main.rs` setup beside
//! `session_touched_files` / process-log cleanup — rather than a new
//! scheduler: once shortly after startup, then hourly.
//!
//! Guarded by the same `journey_schema_supported` probe as the writer: an
//! absent table is a skipped pass, reported in `/journey/health` as
//! `lastPruneSkipped`, never as "deleted 0 rows".
//!
//! `project.journey_frontier` needs no retention: its primary key bounds it.
//!
//! ## The template scrub (once per database)
//!
//! Plan `2026-10-09-journey-ledger-stores-a-concrete-url-path-as-a-route-pattern`
//! Phase 1: before D1, `pathnameTemplate` stored whatever an app reported as
//! its route pattern — including a CONCRETE path carrying user input — and a
//! leaked frontier `node_key` never ages out (the primary key bounds the
//! table, nothing expires a row). The first tick of the loop below therefore
//! runs [`scrub_templates_once`], which, on the instance that owns the shared
//! root state (the primary), and once per database:
//! - invalidates (the existing `invalidated_*` columns, token
//!   [`TEMPLATE_SCRUB_VERSION`]) AND REDACTS every live edge observed before
//!   this process started whose from- or to-node carries a `pathnameTemplate`:
//!   the template is set to `null` inside the node, so the concrete path does
//!   not outlive the scrub. No stored row carries the router assertion D1 now
//!   requires, so none is trusted;
//! - deletes every frontier row of the leaked shape — `node_key` starting with
//!   `unmodelled:/` — and redacts the template inside every remaining frontier
//!   node. Label-keyed and modelled rows (`activation_failed`,
//!   `budget_exhausted`, …) are kept.
//!
//! The MARKER is a row in the database itself (`project.settings`, key
//! [`TEMPLATE_SCRUB_VERSION`], value = the cutoff as a JSON string), claimed in the SAME
//! transaction as the scrub, so it can never exist without its scrub and needs
//! no database identity. Only the owning instance scrubs and claims it: a temp
//! runner on this build must not mark a shared database while a pre-D1
//! primary is still writing leaked rows into it — the primary's own first
//! start on this build is what scrubs. The marker is what keeps a later start
//! from withdrawing rows a router-asserting build wrote legitimately; the
//! cutoff bounds the one run to rows older than the scrubbing process.
//!
//! The remaining limit, stated: once the owner has claimed the marker, a
//! pre-D1 writer still sharing the database — an older-build secondary, or a
//! last-known-good binary the supervisor falls back to — can still write
//! leaked rows, and nothing scrubs them; the marker says the scrub ran, not
//! that no old build is left. Rebuilding every runner that shares the
//! database is what closes it.
//! It runs on whatever database the runner's `PgDb` is — the embedded cluster
//! or `DATABASE_URL` alike.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use crate::database::pg::PgDb;

use super::capture::{journey_schema_supported, SchemaProbe};
use super::health;

/// Days an edge observation is kept (plan Phase 0 decision 2).
pub(crate) const RETENTION_DAYS: i64 = 90;

/// Rows deleted per statement.
pub(crate) const PRUNE_CHUNK_ROWS: i64 = 1000;

/// Upper bound on chunks per pass, so a pass always ends; the next hourly
/// pass continues where it stopped. 10,000 × 1,000 rows is far beyond any
/// measured volume (Phase 0 U5: 11 snapshot-equivalents in 30 days).
pub(crate) const MAX_CHUNKS_PER_PASS: u32 = 10_000;

/// Delay before the startup pass, keeping it off the boot path (the same
/// 30 s its sibling cleanup loops use).
const STARTUP_DELAY: Duration = Duration::from_secs(30);

/// Hourly, like the web job.
const INTERVAL: Duration = Duration::from_secs(3600);

/// The template scrub's version: the key of its marker row in
/// `project.settings` and the `invalidation_token` / `invalidated_by` of every
/// edge it withdraws. Bump it only for a NEW scrub — a changed name re-runs
/// against every database.
pub(crate) const TEMPLATE_SCRUB_VERSION: &str = "journey-template-scrub-v1";

/// The `invalidated_reason` the scrub writes.
pub(crate) const TEMPLATE_SCRUB_REASON: &str = "pathnameTemplate was stored without the router \
     assertion (patternSource: \"router\") and may carry a concrete URL path; the template was \
     redacted; plan 2026-10-09-journey-ledger-stores-a-concrete-url-path-as-a-route-pattern";

/// Claim the marker row INSIDE the scrub's transaction: `$1` the version key,
/// `$2` the cutoff, stored as a JSON string ([`scrub_marker_value`]): every
/// `project.settings` value is JSON, and `get_all_settings` skips (and the
/// settings export nulls) a row that is not.
/// `ON CONFLICT DO NOTHING ... RETURNING`: zero rows back means another process
/// already claimed it, and the scrub rolls back without touching a row.
/// `project.settings` is the runner's existing key/value table
/// (`queries/settings.sql`), so the marker lives in the database it describes
/// and needs no identity of its own.
pub(crate) const SCRUB_CLAIM_MARKER_SQL: &str = r#"INSERT INTO project.settings (key, value, updated_at)
VALUES ($1, $2, now())
ON CONFLICT (key) DO NOTHING
RETURNING key"#;

/// The marker's `project.settings` value: the cutoff as a JSON string.
pub(crate) fn scrub_marker_value(cutoff: &str) -> String {
    serde_json::Value::String(cutoff.to_string()).to_string()
}

/// The cutoff a marker value records: its JSON string, or the raw text when
/// the value is not one (never treated as "no marker").
pub(crate) fn scrub_marker_cutoff(value: &str) -> String {
    serde_json::from_str::<String>(value).unwrap_or_else(|_| value.to_string())
}

/// Is the marker store present, and is the marker already in it?
pub(crate) const SCRUB_MARKER_STORE_SQL: &str =
    "SELECT to_regclass('project.settings') IS NOT NULL";
pub(crate) const SCRUB_MARKER_READ_SQL: &str = "SELECT value FROM project.settings WHERE key = $1";

/// Withdraw AND redact every live edge observed before `$4` whose from- or
/// to-node carries a `pathnameTemplate`: the template is set to JSON `null`
/// inside the node, so the concrete path does not survive the 90 days the
/// invalidated row is kept (privacy over history). `$1` reason, `$2`
/// invalidated_by, `$3` token. The node KEY is derived from the node, so a
/// redacted unmodelled node keys as its label or `unmodelled:unknown`; there is
/// no stored key column to rewrite. `jsonb_set` on a SQL-NULL `to_node` is NULL,
/// which keeps the `to_node_unobserved` iff rule intact.
pub(crate) const SCRUB_INVALIDATE_EDGES_SQL: &str = r#"UPDATE project.journey_edge_observations
   SET invalidated_at = now(),
       invalidated_reason = $1,
       invalidated_by = $2,
       invalidation_token = $3,
       from_node = CASE WHEN from_node ? 'pathnameTemplate'
                        THEN jsonb_set(from_node, '{pathnameTemplate}', 'null'::jsonb)
                        ELSE from_node END,
       to_node = CASE WHEN to_node ? 'pathnameTemplate'
                      THEN jsonb_set(to_node, '{pathnameTemplate}', 'null'::jsonb)
                      ELSE to_node END
 WHERE invalidated_at IS NULL
   AND observed_at < $4::text::timestamptz
   AND (from_node->>'pathnameTemplate' IS NOT NULL
        OR to_node->>'pathnameTemplate' IS NOT NULL)"#;

/// Delete the leaked frontier SHAPE: a template-keyed unmodelled node
/// (`unmodelled:/…`). `LIKE` with a literal prefix: neither `:` nor `/` is a
/// wildcard. A label-keyed or modelled row (`activation_failed`,
/// `budget_exhausted`, …) is kept — no snapshot necessarily re-derives it.
pub(crate) const SCRUB_DELETE_FRONTIER_SQL: &str = r#"DELETE FROM project.journey_frontier
 WHERE node_key LIKE 'unmodelled:/%'"#;

/// Redact the template inside every REMAINING frontier node (a modelled node's
/// key is `spec#states`, but its stored node still carries the template). The
/// row and its reason are kept.
pub(crate) const SCRUB_REDACT_FRONTIER_SQL: &str = r#"UPDATE project.journey_frontier
   SET node = jsonb_set(node, '{pathnameTemplate}', 'null'::jsonb)
 WHERE node->>'pathnameTemplate' IS NOT NULL"#;

/// What one scrub run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScrubOutcome {
    pub edges_invalidated: u64,
    pub frontier_deleted: u64,
    pub frontier_redacted: u64,
}

/// Claim the marker and run every scrub statement in ONE transaction, so a
/// database never holds a marker without its scrub (or a scrub without its
/// marker). `Ok(None)`: the marker was already claimed — nothing was touched.
pub(crate) async fn run_scrub_statements(
    conn: &mut deadpool_postgres::Object,
    cutoff: &str,
) -> Result<Option<ScrubOutcome>, String> {
    let err = |e: tokio_postgres::Error| crate::database::pg::pg_err("journey template scrub", &e);
    let tx = conn.transaction().await.map_err(err)?;
    let claimed = tx
        .query(
            SCRUB_CLAIM_MARKER_SQL,
            &[&TEMPLATE_SCRUB_VERSION, &scrub_marker_value(cutoff)],
        )
        .await
        .map_err(err)?;
    if claimed.is_empty() {
        tx.rollback().await.map_err(err)?;
        return Ok(None);
    }
    let edges_invalidated = tx
        .execute(
            SCRUB_INVALIDATE_EDGES_SQL,
            &[
                &TEMPLATE_SCRUB_REASON,
                &TEMPLATE_SCRUB_VERSION,
                &TEMPLATE_SCRUB_VERSION,
                &cutoff,
            ],
        )
        .await
        .map_err(err)?;
    let frontier_deleted = tx
        .execute(SCRUB_DELETE_FRONTIER_SQL, &[])
        .await
        .map_err(err)?;
    let frontier_redacted = tx
        .execute(SCRUB_REDACT_FRONTIER_SQL, &[])
        .await
        .map_err(err)?;
    tx.commit().await.map_err(err)?;
    Ok(Some(ScrubOutcome {
        edges_invalidated,
        frontier_deleted,
        frontier_redacted,
    }))
}

/// What the scrub should do, decided from what was observed. Pure, so every
/// arm is tested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScrubDecision {
    /// This runner does not own the shared root state: only the instance that
    /// does (the primary) scrubs and claims the marker, so its first start on
    /// this build always scrubs what a pre-D1 primary wrote. Settled.
    NotOwner,
    /// The journey tables do not exist: nothing can have leaked. Settled.
    SchemaAbsent(String),
    /// The database could not be asked; retried next tick.
    Retry(String),
    /// No marker store (`project.settings`) — running unmarked would re-run
    /// on every start and withdraw rows a router-asserting build wrote. Settled.
    NoMarkerStore,
    /// The marker is already claimed in this database. Settled.
    AlreadyApplied(String),
    /// Scrub now, claiming the marker in the same transaction.
    Run,
}

impl ScrubDecision {
    /// Whether this process should stop trying.
    pub(crate) fn settles(&self) -> bool {
        !matches!(self, ScrubDecision::Retry(_) | ScrubDecision::Run)
    }
}

/// The observations the decision is made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScrubObservation {
    pub owns_shared_root_state: bool,
    /// `None` = not asked (an earlier arm decided).
    pub schema: Option<SchemaProbe>,
    /// `None` = not asked; `Some(Err)` = the read failed.
    pub marker_store: Option<Result<bool, String>>,
    /// `None` = not asked; `Some(Ok(None))` = no marker row.
    pub marker: Option<Result<Option<String>, String>>,
}

pub(crate) fn scrub_decision(obs: &ScrubObservation) -> ScrubDecision {
    if !obs.owns_shared_root_state {
        return ScrubDecision::NotOwner;
    }
    match &obs.schema {
        None => return ScrubDecision::Retry("journey schema not probed".to_string()),
        Some(SchemaProbe::Present) => {}
        Some(SchemaProbe::Absent { missing }) => {
            return ScrubDecision::SchemaAbsent(missing.join(", "))
        }
        Some(SchemaProbe::Failed { error }) => {
            return ScrubDecision::Retry(format!("schema probe failed: {error}"))
        }
    }
    match &obs.marker_store {
        None => return ScrubDecision::Retry("marker store not probed".to_string()),
        Some(Err(e)) => return ScrubDecision::Retry(format!("marker store probe failed: {e}")),
        Some(Ok(false)) => return ScrubDecision::NoMarkerStore,
        Some(Ok(true)) => {}
    }
    match &obs.marker {
        None => ScrubDecision::Retry("marker not read".to_string()),
        Some(Err(e)) => ScrubDecision::Retry(format!("marker read failed: {e}")),
        Some(Ok(Some(cutoff))) => ScrubDecision::AlreadyApplied(cutoff.clone()),
        Some(Ok(None)) => ScrubDecision::Run,
    }
}

/// The health line for a decision that did not run the scrub.
fn decision_summary(d: &ScrubDecision) -> String {
    match d {
        ScrubDecision::NotOwner => "not run: this runner does not own the shared root state; \
                                    the owning (primary) instance scrubs this database"
            .to_string(),
        ScrubDecision::SchemaAbsent(m) => {
            format!("not needed: journey schema absent (missing: {m})")
        }
        ScrubDecision::Retry(why) => format!("not run (retried next tick): {why}"),
        ScrubDecision::NoMarkerStore => {
            "not run: project.settings is absent, so the scrub's marker cannot be kept".to_string()
        }
        ScrubDecision::AlreadyApplied(cutoff) => {
            format!("already applied to this database (edges before {cutoff})")
        }
        ScrubDecision::Run => "running".to_string(),
    }
}

/// Set once the scrub has settled in this process.
static TEMPLATE_SCRUB_SETTLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Gather what [`scrub_decision`] needs, asking the database only as far as
/// the decision requires.
async fn observe_for_scrub(conn: &deadpool_postgres::Object, owns: bool) -> ScrubObservation {
    let mut obs = ScrubObservation {
        owns_shared_root_state: owns,
        schema: None,
        marker_store: None,
        marker: None,
    };
    if !owns {
        return obs;
    }
    let probe = journey_schema_supported(conn).await;
    let present = probe == SchemaProbe::Present;
    obs.schema = Some(probe);
    if !present {
        return obs;
    }
    let store = conn
        .query_one(SCRUB_MARKER_STORE_SQL, &[])
        .await
        .map(|row| row.try_get::<_, bool>(0).unwrap_or(false))
        .map_err(|e| crate::database::pg::pg_err("scrub marker store", &e));
    let has_store = matches!(store, Ok(true));
    obs.marker_store = Some(store);
    if !has_store {
        return obs;
    }
    obs.marker = Some(
        conn.query_opt(SCRUB_MARKER_READ_SQL, &[&TEMPLATE_SCRUB_VERSION])
            .await
            .map(|row| {
                row.and_then(|r| r.try_get::<_, String>(0).ok())
                    .map(|v| scrub_marker_cutoff(&v))
            })
            .map_err(|e| crate::database::pg::pg_err("scrub marker", &e)),
    );
    obs
}

/// The template scrub, at most once per database (see the module doc).
/// `cutoff` is when this process's retention loop started: no edge this
/// build wrote is older. Every outcome is reported to `/journey/health`; a
/// pass that could not run is retried on the next tick.
pub(crate) async fn scrub_templates_once(pg: &PgDb, cutoff: &str) {
    use std::sync::atomic::Ordering;
    if TEMPLATE_SCRUB_SETTLED.load(Ordering::Acquire) {
        return;
    }
    let owns = crate::instance::owns_shared_root_state();
    let mut conn = match pg.pool().get().await {
        Ok(c) => c,
        Err(e) => {
            health::record_template_scrub(format!(
                "not run (retried next tick): PG pool error: {e}"
            ));
            return;
        }
    };
    let decision = scrub_decision(&observe_for_scrub(&conn, owns).await);
    if decision != ScrubDecision::Run {
        health::record_template_scrub(decision_summary(&decision));
        if decision.settles() {
            TEMPLATE_SCRUB_SETTLED.store(true, Ordering::Release);
        }
        return;
    }
    match run_scrub_statements(&mut conn, cutoff).await {
        Ok(Some(out)) => {
            info!(
                "journey template scrub: invalidated+redacted {} edge(s), deleted {} and redacted \
                 {} frontier row(s)",
                out.edges_invalidated, out.frontier_deleted, out.frontier_redacted
            );
            health::record_template_scrub(format!(
                "applied: invalidated+redacted {} edge(s) observed before {cutoff}, deleted {} \
                 unmodelled:/ frontier row(s), redacted {} more",
                out.edges_invalidated, out.frontier_deleted, out.frontier_redacted
            ));
            TEMPLATE_SCRUB_SETTLED.store(true, Ordering::Release);
        }
        Ok(None) => {
            health::record_template_scrub(
                "already applied to this database (claimed concurrently)".to_string(),
            );
            TEMPLATE_SCRUB_SETTLED.store(true, Ordering::Release);
        }
        Err(e) => {
            warn!("journey template scrub failed (retried next tick): {e}");
            health::record_template_scrub(format!("failed (retried next tick): {e}"));
        }
    }
}

/// One bounded chunk of expired rows. The cutoff is bound once per pass, so
/// rows that expire while a pass runs wait for the next one.
pub(crate) const PRUNE_CHUNK_SQL: &str = r#"DELETE FROM project.journey_edge_observations
 WHERE id IN (
    SELECT id FROM project.journey_edge_observations
     WHERE observed_at < $1::text::timestamptz
     LIMIT $2
 )"#;

/// Drive `delete_chunk` until a chunk comes back short, an error occurs, or
/// [`MAX_CHUNKS_PER_PASS`] chunks have run. Returns the rows deleted and the
/// error that stopped the pass, if any — rows deleted before an error are
/// still reported (each chunk commits on its own).
pub(crate) async fn prune_in_chunks<F, Fut>(
    chunk_rows: u64,
    mut delete_chunk: F,
) -> (u64, Option<String>)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<u64, String>>,
{
    let mut deleted = 0u64;
    for _ in 0..MAX_CHUNKS_PER_PASS {
        match delete_chunk().await {
            Ok(n) => {
                deleted += n;
                if n < chunk_rows {
                    return (deleted, None);
                }
            }
            Err(e) => return (deleted, Some(e)),
        }
    }
    (deleted, None)
}

/// One retention pass against the runner's database.
pub(crate) async fn prune_once(pg: &PgDb) {
    let conn = match pg.pool().get().await {
        Ok(c) => c,
        Err(e) => {
            health::record_prune_skipped(format!("PG pool error: {e}"), 0);
            return;
        }
    };
    let probe = journey_schema_supported(&conn).await;
    match probe {
        SchemaProbe::Present => {}
        SchemaProbe::Absent { missing } => {
            health::record_prune_skipped(
                format!("schema absent (missing: {})", missing.join(", ")),
                0,
            );
            return;
        }
        SchemaProbe::Failed { error } => {
            health::record_prune_skipped(format!("schema probe failed: {error}"), 0);
            return;
        }
    }
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(RETENTION_DAYS))
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let conn = &conn;
    let cutoff = &cutoff;
    let (deleted, error) = prune_in_chunks(PRUNE_CHUNK_ROWS as u64, || async move {
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 2] = [cutoff, &PRUNE_CHUNK_ROWS];
        conn.execute(PRUNE_CHUNK_SQL, &params)
            .await
            .map_err(|e| crate::database::pg::pg_err("journey edge retention", &e))
    })
    .await;
    match error {
        None => {
            if deleted > 0 {
                info!(
                    "journey_edge_retention: deleted {} edge observation(s) older than {} days",
                    deleted, RETENTION_DAYS
                );
            }
            health::record_prune(deleted);
        }
        Some(e) => {
            warn!(
                "journey_edge_retention: pass stopped after deleting {} row(s): {}",
                deleted, e
            );
            health::record_prune_skipped(format!("pass failed: {e}"), deleted);
        }
    }
}

/// The retention loop: once shortly after startup, then hourly, for the life
/// of the process. Errors are recorded and the cadence kept.
pub async fn run_journey_edge_retention_loop(pg: Arc<PgDb>) {
    info!(
        "journey_edge_retention_loop_started: interval_secs={}, retention_days={}",
        INTERVAL.as_secs(),
        RETENTION_DAYS
    );
    // No edge this process writes can be older than this.
    let scrub_cutoff = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    tokio::time::sleep(STARTUP_DELAY).await;
    loop {
        scrub_templates_once(&pg, &scrub_cutoff).await;
        prune_once(&pg).await;
        // The same tick sweeps pending edges older than the TTL, so an
        // action no snapshot ever followed still lands as to_node_unobserved.
        super::capture::enqueue_edge_observation(
            Arc::clone(&pg),
            super::capture::JourneyEvent::Sweep,
        );
        tokio::time::sleep(INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    async fn drive(results: Vec<Result<u64, String>>, chunk: u64) -> (u64, Option<String>, usize) {
        let queue = Mutex::new(std::collections::VecDeque::from(results));
        let calls = Mutex::new(0usize);
        let (deleted, err) = prune_in_chunks(chunk, || {
            *calls.lock().unwrap() += 1;
            let next = queue.lock().unwrap().pop_front().unwrap_or(Ok(0));
            async move { next }
        })
        .await;
        let n = *calls.lock().unwrap();
        (deleted, err, n)
    }

    #[tokio::test]
    async fn full_chunks_continue_until_a_short_one() {
        let (deleted, err, calls) = drive(vec![Ok(1000), Ok(1000), Ok(3)], 1000).await;
        assert_eq!((deleted, err, calls), (2003, None, 3));
    }

    #[tokio::test]
    async fn an_empty_table_is_one_statement() {
        let (deleted, err, calls) = drive(vec![Ok(0)], 1000).await;
        assert_eq!((deleted, err, calls), (0, None, 1));
    }

    #[tokio::test]
    async fn an_exact_multiple_ends_on_the_empty_chunk() {
        let (deleted, err, calls) = drive(vec![Ok(1000), Ok(0)], 1000).await;
        assert_eq!((deleted, err, calls), (1000, None, 2));
    }

    #[tokio::test]
    async fn an_error_stops_the_pass_and_keeps_the_count() {
        let (deleted, err, calls) = drive(vec![Ok(1000), Err("boom".into()), Ok(5)], 1000).await;
        assert_eq!(
            deleted, 1000,
            "committed chunks before the error still count"
        );
        assert_eq!(err.as_deref(), Some("boom"));
        assert_eq!(calls, 2);
    }

    #[tokio::test]
    async fn a_pass_is_bounded() {
        let calls = Mutex::new(0u32);
        let (deleted, err) = prune_in_chunks(1, || {
            *calls.lock().unwrap() += 1;
            async { Ok(1) }
        })
        .await;
        assert_eq!(*calls.lock().unwrap(), MAX_CHUNKS_PER_PASS);
        assert_eq!(deleted, u64::from(MAX_CHUNKS_PER_PASS));
        assert_eq!(err, None);
    }

    // ---- the template scrub ---------------------------------------------------

    #[test]
    fn the_scrub_invalidates_and_redacts_template_carrying_edges_before_the_cutoff_only() {
        let sql = SCRUB_INVALIDATE_EDGES_SQL;
        assert!(sql.starts_with("UPDATE project.journey_edge_observations"));
        assert!(
            !sql.contains("DELETE"),
            "edges are withdrawn, never deleted"
        );
        for needle in [
            "invalidated_at = now()",
            "invalidated_reason = $1",
            "invalidated_by = $2",
            "invalidation_token = $3",
            "jsonb_set(from_node, '{pathnameTemplate}', 'null'::jsonb)",
            "jsonb_set(to_node, '{pathnameTemplate}', 'null'::jsonb)",
            "WHERE invalidated_at IS NULL",
            "observed_at < $4::text::timestamptz",
            "from_node->>'pathnameTemplate' IS NOT NULL",
            "OR to_node->>'pathnameTemplate' IS NOT NULL",
        ] {
            assert!(sql.contains(needle), "missing {needle:?}");
        }
    }

    #[test]
    fn the_frontier_delete_takes_only_the_leaked_key_shape() {
        let sql = SCRUB_DELETE_FRONTIER_SQL;
        assert!(sql.starts_with("DELETE FROM project.journey_frontier"));
        assert!(sql.contains("node_key LIKE 'unmodelled:/%'"));
        assert!(
            !sql.contains("pathnameTemplate") && !sql.contains(" OR "),
            "a modelled / label-keyed row is redacted, never deleted"
        );
        assert!(!sql.contains('$'), "the delete binds nothing");
        assert!(SCRUB_REDACT_FRONTIER_SQL.starts_with("UPDATE project.journey_frontier"));
        assert!(SCRUB_REDACT_FRONTIER_SQL.contains("jsonb_set(node, '{pathnameTemplate}'"));
        // The LIKE prefix carries no wildcard of its own.
        assert!(!"unmodelled:/".contains(['%', '_']));
    }

    #[test]
    fn the_marker_value_is_json_like_every_settings_value() {
        let v = scrub_marker_value("2026-10-10T00:00:00.000000Z");
        assert_eq!(v, "\"2026-10-10T00:00:00.000000Z\"");
        assert!(serde_json::from_str::<serde_json::Value>(&v).is_ok());
        assert_eq!(scrub_marker_cutoff(&v), "2026-10-10T00:00:00.000000Z");
        assert_eq!(scrub_marker_cutoff("not-json"), "not-json");
    }

    #[test]
    fn the_marker_is_claimed_in_the_database_without_overwriting() {
        assert!(SCRUB_CLAIM_MARKER_SQL.starts_with("INSERT INTO project.settings"));
        assert!(SCRUB_CLAIM_MARKER_SQL.contains("ON CONFLICT (key) DO NOTHING"));
        assert!(SCRUB_CLAIM_MARKER_SQL.contains("RETURNING key"));
        assert!(SCRUB_MARKER_STORE_SQL.contains("to_regclass('project.settings')"));
        assert!(SCRUB_MARKER_READ_SQL.contains("WHERE key = $1"));
    }

    fn observed(
        owns: bool,
        schema: Option<SchemaProbe>,
        store: Option<Result<bool, String>>,
        marker: Option<Result<Option<String>, String>>,
    ) -> ScrubObservation {
        ScrubObservation {
            owns_shared_root_state: owns,
            schema,
            marker_store: store,
            marker,
        }
    }

    #[test]
    fn only_the_owning_instance_scrubs_or_claims_the_marker() {
        // A temp runner on a shared database, marker absent: it must not
        // scrub (and so cannot claim the marker) while a pre-D1 primary may
        // still be writing leaked rows.
        let d = scrub_decision(&observed(
            false,
            Some(SchemaProbe::Present),
            Some(Ok(true)),
            Some(Ok(None)),
        ));
        assert_eq!(d, ScrubDecision::NotOwner);
        assert!(d.settles());
    }

    #[test]
    fn the_owner_scrubs_when_the_marker_is_absent_and_not_when_present() {
        let run = scrub_decision(&observed(
            true,
            Some(SchemaProbe::Present),
            Some(Ok(true)),
            Some(Ok(None)),
        ));
        assert_eq!(run, ScrubDecision::Run);
        assert!(!run.settles(), "Run settles only once the scrub commits");
        let done = scrub_decision(&observed(
            true,
            Some(SchemaProbe::Present),
            Some(Ok(true)),
            Some(Ok(Some("2026-10-10T00:00:00Z".into()))),
        ));
        assert_eq!(
            done,
            ScrubDecision::AlreadyApplied("2026-10-10T00:00:00Z".into())
        );
        assert!(done.settles(), "an applied marker is never re-run");
    }

    #[test]
    fn an_absent_schema_settles_and_a_failed_read_retries() {
        let absent = scrub_decision(&observed(
            true,
            Some(SchemaProbe::Absent {
                missing: vec!["project.journey_frontier"],
            }),
            None,
            None,
        ));
        assert_eq!(
            absent,
            ScrubDecision::SchemaAbsent("project.journey_frontier".into())
        );
        assert!(absent.settles());
        for d in [
            scrub_decision(&observed(
                true,
                Some(SchemaProbe::Failed {
                    error: "timeout".into(),
                }),
                None,
                None,
            )),
            scrub_decision(&observed(
                true,
                Some(SchemaProbe::Present),
                Some(Err("boom".into())),
                None,
            )),
            scrub_decision(&observed(
                true,
                Some(SchemaProbe::Present),
                Some(Ok(true)),
                Some(Err("boom".into())),
            )),
        ] {
            assert!(matches!(d, ScrubDecision::Retry(_)), "{d:?}");
            assert!(!d.settles());
        }
    }

    #[test]
    fn no_marker_store_never_runs_unmarked() {
        let d = scrub_decision(&observed(
            true,
            Some(SchemaProbe::Present),
            Some(Ok(false)),
            None,
        ));
        assert_eq!(d, ScrubDecision::NoMarkerStore);
        assert!(d.settles());
    }

    /// The scrub against a live, seeded ledger: no sentinel byte left in
    /// either table, the leaked edge withdrawn and redacted, a clean edge and a
    /// label-keyed `activation_failed` frontier row untouched, the marker
    /// claimed, and a second run a no-op. `#[ignore]` per the `database/pg/*`
    /// convention — needs DATABASE_URL pointing at a database with the journey
    /// tables and `project.settings`. The scrub is database-wide, so point it at
    /// a throwaway database: it withdraws/redacts every template-carrying row
    /// there, not only its own, and claims the database's marker.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL) with qontinui-web migration journey_01_edge_ledger"]
    async fn the_scrub_clears_a_seeded_ledger() {
        const SENTINEL: &str = "JP2SENTINELw4r8mv";
        let pg = PgDb::new_for_test().await;
        let mut conn = pg.pool().get().await.expect("pooled connection");
        conn.execute(
            "DELETE FROM project.settings WHERE key = $1",
            &[&TEMPLATE_SCRUB_VERSION],
        )
        .await
        .expect("clear a marker an earlier run left");
        let app = format!("scrub-test-{}", uuid::Uuid::new_v4());
        let leaked = serde_json::json!({
            "specId": null, "stateIds": [], "modelled": false,
            "pathnameTemplate": format!("/search/{SENTINEL}"), "pageLabel": null
        });
        let leaked_modelled = serde_json::json!({
            "specId": "search", "stateIds": ["open"], "modelled": true,
            "pathnameTemplate": format!("/search/{SENTINEL}"), "pageLabel": "search"
        });
        let clean = serde_json::json!({
            "specId": null, "stateIds": [], "modelled": false,
            "pathnameTemplate": null, "pageLabel": "settings"
        });
        let trigger = serde_json::json!({
            "actionType": "navigate", "navigationTrigger": "push", "chokePoint": "navigation"
        });
        for (from, to) in [(&leaked, &leaked_modelled), (&clean, &clean)] {
            conn.execute(
                "INSERT INTO project.journey_edge_observations \
                 (app_id, runner_build_id, runner_instance, run_kind, from_node, to_node, \
                  trigger, outcome) VALUES ($1, 'test', 'test', 'agent_action', $2, $3, $4, \
                  'changed')",
                &[&app, from, to, &trigger],
            )
            .await
            .expect("seed edge");
        }
        for (key, node, reason) in [
            (
                format!("unmodelled:/search/{SENTINEL}"),
                &leaked,
                "not_yet_activated",
            ),
            (
                "search#open".to_string(),
                &leaked_modelled,
                "budget_exhausted",
            ),
            (
                "unmodelled:settings".to_string(),
                &clean,
                "activation_failed",
            ),
        ] {
            conn.execute(
                "INSERT INTO project.journey_frontier \
                 (app_id, node_key, node, affordance_fingerprint, reason) \
                 VALUES ($1, $2, $3, 'fp', $4)",
                &[&app, &key, node, &reason],
            )
            .await
            .expect("seed frontier");
        }
        let cutoff = (chrono::Utc::now() + chrono::Duration::seconds(5))
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        let first = run_scrub_statements(&mut conn, &cutoff)
            .await
            .expect("scrub")
            .expect("the marker was free, so the scrub ran");
        assert!(first.edges_invalidated >= 1 && first.frontier_deleted >= 1);
        let count = |sql: &'static str| {
            let conn = &conn;
            let app = app.clone();
            async move {
                conn.query_one(sql, &[&app, &format!("%{SENTINEL}%")])
                    .await
                    .unwrap()
                    .get::<_, i64>(0)
            }
        };
        assert_eq!(
            count(
                "SELECT count(*) FROM project.journey_edge_observations e \
                 WHERE e.app_id = $1 AND e::text ILIKE $2"
            )
            .await,
            0,
            "no sentinel byte survives in the edge ledger"
        );
        assert_eq!(
            count(
                "SELECT count(*) FROM project.journey_frontier f \
                 WHERE f.app_id = $1 AND f::text ILIKE $2"
            )
            .await,
            0,
            "no sentinel byte survives in the frontier"
        );
        let live: i64 = conn
            .query_one(
                "SELECT count(*) FROM project.journey_edge_observations \
                 WHERE app_id = $1 AND invalidated_at IS NULL",
                &[&app],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(live, 1, "only the clean edge stays live");
        let kept: Vec<String> = conn
            .query(
                "SELECT reason FROM project.journey_frontier WHERE app_id = $1 ORDER BY reason",
                &[&app],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert_eq!(
            kept,
            vec!["activation_failed", "budget_exhausted"],
            "only the unmodelled:/ row is deleted"
        );
        let marker: Option<String> = conn
            .query_opt(SCRUB_MARKER_READ_SQL, &[&TEMPLATE_SCRUB_VERSION])
            .await
            .unwrap()
            .map(|r| r.get(0));
        assert_eq!(
            marker
                .as_deref()
                .map(|v| serde_json::from_str::<String>(v).unwrap()),
            Some(cutoff.clone()),
            "the marker is claimed, its value a JSON string"
        );
        let second = run_scrub_statements(&mut conn, &cutoff)
            .await
            .expect("re-run");
        assert_eq!(
            second, None,
            "a claimed marker makes a second run touch nothing"
        );
        for sql in [
            "DELETE FROM project.journey_edge_observations WHERE app_id = $1",
            "DELETE FROM project.journey_frontier WHERE app_id = $1",
        ] {
            conn.execute(sql, &[&app]).await.unwrap();
        }
        conn.execute(
            "DELETE FROM project.settings WHERE key = $1",
            &[&TEMPLATE_SCRUB_VERSION],
        )
        .await
        .unwrap();
    }

    #[test]
    fn the_chunk_statement_is_bounded_and_keyed_on_observed_at() {
        assert!(PRUNE_CHUNK_SQL.contains("observed_at < $1::text::timestamptz"));
        assert!(PRUNE_CHUNK_SQL.contains("LIMIT $2"));
        assert!(PRUNE_CHUNK_SQL.contains("DELETE FROM project.journey_edge_observations"));
        assert_eq!(
            RETENTION_DAYS, 90,
            "the window matches the web job's default"
        );
    }
}
