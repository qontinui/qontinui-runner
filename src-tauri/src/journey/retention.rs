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
//! runs [`scrub_templates_once`], which on its first run against a database:
//! - invalidates (the existing `invalidated_*` columns, token
//!   [`TEMPLATE_SCRUB_VERSION`]) every live edge observed before this process
//!   started whose from- or to-node carries a `pathnameTemplate` — no stored
//!   row carries the router assertion D1 now requires, so none is trusted;
//! - deletes every frontier row whose `node_key` starts with `unmodelled:/`
//!   (a template-keyed node) or whose node carries a `pathnameTemplate`. A
//!   frontier row is re-derived from the next snapshot of its page, so the
//!   delete loses nothing a live page cannot restore.
//!
//! It is idempotent (an invalidated edge is never re-touched; a deleted key is
//! gone) and guarded by a MARKER: a file under the runner's data dir named for
//! the database's identity (`current_database()`, server address, port and
//! database oid), written after the scrub commits. The marker lives outside the
//! instance-scoped dirs because the database it describes may be shared by
//! several runners. Without it a later start would invalidate rows a router-
//! asserting build wrote legitimately. It runs on whatever database the
//! runner's `PgDb` is — the embedded cluster or `DATABASE_URL` alike.

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

/// The template scrub's version: its marker-file prefix and the
/// `invalidation_token` / `invalidated_by` of every edge it withdraws. Bump it
/// only for a NEW scrub — a changed name re-runs against every database.
pub(crate) const TEMPLATE_SCRUB_VERSION: &str = "journey-template-scrub-v1";

/// The `invalidated_reason` the scrub writes.
pub(crate) const TEMPLATE_SCRUB_REASON: &str = "pathnameTemplate was stored without the router \
     assertion (patternSource: \"router\") and may carry a concrete URL path; plan \
     2026-10-09-journey-ledger-stores-a-concrete-url-path-as-a-route-pattern";

/// Withdraw every live edge observed before `$4` whose from- or to-node
/// carries a `pathnameTemplate`. `$1` reason, `$2` invalidated_by, `$3` token.
/// `->>` on a SQL-NULL `to_node` is NULL, so an unobserved edge is judged by
/// its from-node alone.
pub(crate) const SCRUB_INVALIDATE_EDGES_SQL: &str = r#"UPDATE project.journey_edge_observations
   SET invalidated_at = now(),
       invalidated_reason = $1,
       invalidated_by = $2,
       invalidation_token = $3
 WHERE invalidated_at IS NULL
   AND observed_at < $4::text::timestamptz
   AND (from_node->>'pathnameTemplate' IS NOT NULL
        OR to_node->>'pathnameTemplate' IS NOT NULL)"#;

/// Delete every template-keyed (`unmodelled:/…`) or template-carrying frontier
/// row. `LIKE` with a literal prefix: neither `:` nor `/` is a wildcard.
pub(crate) const SCRUB_DELETE_FRONTIER_SQL: &str = r#"DELETE FROM project.journey_frontier
 WHERE node_key LIKE 'unmodelled:/%'
    OR node->>'pathnameTemplate' IS NOT NULL"#;

/// The database's identity, for the marker. Readable by any role: no
/// superuser-only function, and a unix-socket connection reads `local`.
pub(crate) const DB_IDENTITY_SQL: &str = r#"SELECT current_database()::text,
       COALESCE(host(inet_server_addr()), 'local'),
       current_setting('port'),
       (SELECT oid::text FROM pg_database WHERE datname = current_database())"#;

/// The marker file for a database identity: `<version>-<sha256(identity)[..16]>.done`.
pub(crate) fn scrub_marker_name(identity: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(identity.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("{TEMPLATE_SCRUB_VERSION}-{hex}.done")
}

/// Where scrub markers live: `~/.qontinui/runner/journey/`, deliberately NOT
/// instance-scoped (the database may be shared by several runners).
fn scrub_marker_dir() -> Option<std::path::PathBuf> {
    qontinui_runner_lib::ambient::runner_dir().map(|d| d.join("journey"))
}

/// What one scrub run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScrubOutcome {
    pub edges_invalidated: u64,
    pub frontier_deleted: u64,
}

/// Run both scrub statements in ONE transaction, so a database never holds an
/// invalidated ledger beside a still-leaking frontier (or the reverse).
pub(crate) async fn run_scrub_statements(
    conn: &mut deadpool_postgres::Object,
    cutoff: &str,
) -> Result<ScrubOutcome, String> {
    let err = |e: tokio_postgres::Error| crate::database::pg::pg_err("journey template scrub", &e);
    let tx = conn.transaction().await.map_err(err)?;
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
    tx.commit().await.map_err(err)?;
    Ok(ScrubOutcome {
        edges_invalidated,
        frontier_deleted,
    })
}

/// Set once the scrub has run (or found its marker) in this process.
static TEMPLATE_SCRUB_SETTLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The template scrub, at most once per database (see the module doc).
/// `cutoff` is when this process's retention loop started: no edge this
/// build wrote is older. Every outcome is reported to `/journey/health`; a
/// pass that could not run is retried on the next tick.
pub(crate) async fn scrub_templates_once(pg: &PgDb, cutoff: &str) {
    use std::sync::atomic::Ordering;
    if TEMPLATE_SCRUB_SETTLED.load(Ordering::Acquire) {
        return;
    }
    let mut conn = match pg.pool().get().await {
        Ok(c) => c,
        Err(e) => {
            health::record_template_scrub(format!("not run: PG pool error: {e}"));
            return;
        }
    };
    match journey_schema_supported(&conn).await {
        SchemaProbe::Present => {}
        SchemaProbe::Absent { missing } => {
            // Nothing can have leaked into tables that do not exist.
            health::record_template_scrub(format!(
                "not needed: journey schema absent (missing: {})",
                missing.join(", ")
            ));
            TEMPLATE_SCRUB_SETTLED.store(true, Ordering::Release);
            return;
        }
        SchemaProbe::Failed { error } => {
            health::record_template_scrub(format!("not run: schema probe failed: {error}"));
            return;
        }
    }
    let identity = match conn.query_one(DB_IDENTITY_SQL, &[]).await {
        Ok(row) => (0..4)
            .map(|i| row.try_get::<_, Option<String>>(i).ok().flatten().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("|"),
        Err(e) => {
            health::record_template_scrub(format!(
                "not run: {}",
                crate::database::pg::pg_err("database identity", &e)
            ));
            return;
        }
    };
    let Some(marker_dir) = scrub_marker_dir() else {
        // No place to remember it ran: running anyway would re-run on every
        // start and withdraw rows a router-asserting build wrote.
        health::record_template_scrub(
            "not run: no runner data dir to hold its marker".to_string(),
        );
        TEMPLATE_SCRUB_SETTLED.store(true, Ordering::Release);
        return;
    };
    let marker = marker_dir.join(scrub_marker_name(&identity));
    if marker.exists() {
        health::record_template_scrub(format!(
            "already applied to this database (marker {})",
            marker.display()
        ));
        TEMPLATE_SCRUB_SETTLED.store(true, Ordering::Release);
        return;
    }
    match run_scrub_statements(&mut conn, cutoff).await {
        Ok(out) => {
            info!(
                "journey template scrub: invalidated {} edge(s), deleted {} frontier row(s)",
                out.edges_invalidated, out.frontier_deleted
            );
            let written = std::fs::create_dir_all(&marker_dir).and_then(|()| {
                std::fs::write(
                    &marker,
                    format!(
                        "{TEMPLATE_SCRUB_VERSION}\nidentity={identity}\ncutoff={cutoff}\n\
                         edges_invalidated={}\nfrontier_deleted={}\n",
                        out.edges_invalidated, out.frontier_deleted
                    ),
                )
            });
            let marker_note = match written {
                Ok(()) => String::new(),
                Err(e) => {
                    warn!(
                        "journey template scrub: marker {} not written ({e}); the scrub will \
                         re-run on the next start",
                        marker.display()
                    );
                    format!("; marker NOT written ({e}) — it re-runs on the next start")
                }
            };
            health::record_template_scrub(format!(
                "applied: invalidated {} edge(s), deleted {} frontier row(s){marker_note}",
                out.edges_invalidated, out.frontier_deleted
            ));
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
    let scrub_cutoff =
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
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
    fn the_scrub_invalidates_template_carrying_edges_before_the_cutoff_only() {
        let sql = SCRUB_INVALIDATE_EDGES_SQL;
        assert!(sql.starts_with("UPDATE project.journey_edge_observations"));
        assert!(!sql.contains("DELETE"), "edges are withdrawn, never deleted");
        for needle in [
            "invalidated_at = now()",
            "invalidated_reason = $1",
            "invalidated_by = $2",
            "invalidation_token = $3",
            "WHERE invalidated_at IS NULL",
            "observed_at < $4::text::timestamptz",
            "from_node->>'pathnameTemplate' IS NOT NULL",
            "OR to_node->>'pathnameTemplate' IS NOT NULL",
        ] {
            assert!(sql.contains(needle), "missing {needle:?}");
        }
    }

    #[test]
    fn the_scrub_deletes_every_unmodelled_path_key() {
        let sql = SCRUB_DELETE_FRONTIER_SQL;
        assert!(sql.starts_with("DELETE FROM project.journey_frontier"));
        assert!(sql.contains("node_key LIKE 'unmodelled:/%'"));
        assert!(sql.contains("OR node->>'pathnameTemplate' IS NOT NULL"));
        assert!(!sql.contains('$'), "the delete binds nothing");
        // The LIKE prefix carries no wildcard of its own, so it matches
        // exactly the keys a template produced — never `unmodelled:unknown`
        // or a label key.
        let like = |key: &str| key.starts_with("unmodelled:/");
        assert!(like("unmodelled:/search/JP2SENTINELw4r8mv"));
        assert!(!like("unmodelled:unknown"));
        assert!(!like("unmodelled:settings"));
        assert!(!"unmodelled:/".contains(['%', '_']));
    }

    #[test]
    fn the_marker_is_per_database_and_per_version() {
        let a = scrub_marker_name("qontinui_db|local|5432|16384");
        let b = scrub_marker_name("qontinui_db|local|5433|16384");
        assert_ne!(a, b, "two clusters are two markers");
        assert_eq!(a, scrub_marker_name("qontinui_db|local|5432|16384"));
        assert!(a.starts_with(TEMPLATE_SCRUB_VERSION) && a.ends_with(".done"));
        assert!(!a.contains('/') && !a.contains('|'), "{a}");
        assert!(DB_IDENTITY_SQL.contains("current_database()"));
        assert!(!DB_IDENTITY_SQL.contains("pg_control"), "superuser-only");
    }

    /// The scrub against a live, seeded ledger: zero `unmodelled:/` frontier
    /// keys after it, the leaked edge withdrawn, a clean edge untouched, and a
    /// second run a no-op. `#[ignore]` per the `database/pg/*` convention —
    /// needs DATABASE_URL pointing at a database with the journey tables. The
    /// scrub is database-wide, so point it at a throwaway database: it
    /// withdraws/deletes every template-carrying row there, not only its own.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL) with qontinui-web migration journey_01_edge_ledger"]
    async fn the_scrub_clears_a_seeded_ledger() {
        let pg = PgDb::new_for_test().await;
        let mut conn = pg.pool().get().await.expect("pooled connection");
        let app = format!("scrub-test-{}", uuid::Uuid::new_v4());
        let leaked = serde_json::json!({
            "specId": null, "stateIds": [], "modelled": false,
            "pathnameTemplate": "/search/JP2SENTINELw4r8mv", "pageLabel": null
        });
        let clean = serde_json::json!({
            "specId": null, "stateIds": [], "modelled": false,
            "pathnameTemplate": null, "pageLabel": "settings"
        });
        let trigger = serde_json::json!({
            "actionType": "navigate", "navigationTrigger": "push", "chokePoint": "navigation"
        });
        for (from, to) in [(&leaked, &clean), (&clean, &clean)] {
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
        for (key, node) in [
            ("unmodelled:/search/JP2SENTINELw4r8mv", &leaked),
            ("unmodelled:settings", &clean),
        ] {
            conn.execute(
                "INSERT INTO project.journey_frontier \
                 (app_id, node_key, node, affordance_fingerprint, reason) \
                 VALUES ($1, $2, $3, 'fp', 'not_yet_activated')",
                &[&app, &key, node],
            )
            .await
            .expect("seed frontier");
        }
        let cutoff = (chrono::Utc::now() + chrono::Duration::seconds(5))
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        let first = run_scrub_statements(&mut conn, &cutoff).await.expect("scrub");
        assert!(first.edges_invalidated >= 1 && first.frontier_deleted >= 1);
        let left: i64 = conn
            .query_one(
                "SELECT count(*) FROM project.journey_frontier \
                 WHERE app_id = $1 AND node_key LIKE 'unmodelled:/%'",
                &[&app],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(left, 0, "no unmodelled:/ key survives the scrub");
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
        let second = run_scrub_statements(&mut conn, &cutoff).await.expect("re-run");
        assert_eq!(second.edges_invalidated, 0, "idempotent");
        conn.execute(
            "DELETE FROM project.journey_edge_observations WHERE app_id = $1",
            &[&app],
        )
        .await
        .unwrap();
        conn.execute(
            "DELETE FROM project.journey_frontier WHERE app_id = $1",
            &[&app],
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
