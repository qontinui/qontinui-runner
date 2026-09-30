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
    match journey_schema_supported(&conn).await {
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
    tokio::time::sleep(STARTUP_DELAY).await;
    loop {
        prune_once(&pg).await;
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
