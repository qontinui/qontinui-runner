//! Postgres exporter — the pipeline's PERSISTENCE stage.
//!
//! # The defect this closes
//!
//! Until this exporter existed, the error-monitor pipeline persisted NOTHING.
//! Records flowed receiver -> jsonl-preprocess -> parse -> dedup -> the event-bus
//! exporter, and the event-bus exporter's entire job was to send an **empty**
//! `NewErrors(Vec::new())` "presence" signal whose comment said subscribers
//! would fetch the real records from the PG-backed query API. Nothing ever
//! wrote them there: an exhaustive search of `src-tauri/src/` found no
//! `INSERT INTO error_events` at all, only SELECTs and UPDATEs. So
//! `query_error_events` and `get_error_summary` read a table this application
//! never populated, and the store stayed empty machine-wide regardless of how
//! log sources were configured.
//!
//! Ingestion is **not** external. `error_events` is owned by qontinui-web's
//! alembic migration, but the runner is the only component that parses runner
//! logs, and the surrounding code says so plainly: `link_error_events_to_fix`,
//! `resolve_errors_by_task_run` and `promote to finding` all mutate rows the
//! runner expects to be there. Those UPDATEs are the evidence — they are
//! written against rows only this pipeline can produce.
//!
//! # Ordering
//!
//! This exporter runs BEFORE the event-bus exporter, so by the time the UI is
//! woken the rows it is told to fetch already exist. The reverse order is a
//! race the empty payload cannot paper over.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::error_monitor::pipeline::traits::Exporter;
use crate::error_monitor::pipeline::types::LogRecord;

/// Exporter that persists parsed errors into `error_events`.
pub struct PostgresExporter {
    pg_db: Arc<crate::database::pg::PgDb>,
    /// Shared with `ErrorMonitorService` — the workflow run in flight, so a row
    /// captured during a run is attributable to it. Read per export rather than
    /// captured at construction: the service rewrites it on every
    /// `SetWorkflowContext` command.
    current_task_run_id: Arc<RwLock<Option<String>>>,
}

impl PostgresExporter {
    pub fn new(
        pg_db: Arc<crate::database::pg::PgDb>,
        current_task_run_id: Arc<RwLock<Option<String>>>,
    ) -> Self {
        Self {
            pg_db,
            current_task_run_id,
        }
    }
}

#[async_trait]
impl Exporter for PostgresExporter {
    fn name(&self) -> &str {
        "postgres"
    }

    async fn export(&self, records: &[LogRecord]) -> Result<(), String> {
        // Only PARSED records are errors. An unparsed record is a log line the
        // parsers did not recognise as an error at all, and writing it would
        // fill the store with noise the UI cannot classify.
        let events: Vec<_> = records.iter().filter_map(|r| r.parsed.clone()).collect();
        if events.is_empty() {
            return Ok(());
        }

        let task_run_id = self.current_task_run_id.read().await.clone();

        // ONE pool checkout for the whole batch (see `upsert_error_events`).
        let (inserted, bumped, failed) = self
            .pg_db
            .upsert_error_events(&events, task_run_id.as_deref())
            .await?;

        if inserted > 0 || bumped > 0 {
            tracing::debug!(
                inserted,
                bumped,
                task_run_id = ?task_run_id,
                "Persisted error events"
            );
        }

        match failed {
            Some(e) => Err(format!(
                "persisted {inserted} new / {bumped} recurring error(s); at least one failed: {e}"
            )),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    /// The pipeline MUST persist before it wakes the UI.
    ///
    /// `EventBusExporter` sends an intentionally EMPTY `NewErrors(Vec::new())`
    /// whose whole contract is "go read the store". If the wake-up ran first,
    /// a subscriber that obeyed it would read a store that does not yet contain
    /// the rows — the exact shape of the defect this exporter was added to fix,
    /// reintroduced as a race. A source-level assertion is the honest way to
    /// pin an ORDERING between two hard-coded struct-field calls; the repo
    /// already uses this idiom for the cfg-gate canaries in
    /// `mcp/test_fixtures.rs`.
    #[test]
    fn persistence_runs_before_the_event_bus_wakeup() {
        let src = include_str!("../../service.rs");
        let persist = src
            .find("self.postgres_exporter.export(")
            .expect("service must call the postgres exporter — nothing else writes error_events");
        let wake = src
            .find("self.event_bus_exporter.export(")
            .expect("service must call the event bus exporter");
        assert!(
            persist < wake,
            "the postgres exporter must run BEFORE the event bus exporter: the bus payload \
             is empty and tells subscribers to read the store, so waking them first is a \
             race against the rows existing"
        );
    }

    /// Nothing may filter repeat SIGNATURES out of the chain before this
    /// exporter sees them.
    ///
    /// `upsert_error_events` implements dedup by bumping `occurrence_count`,
    /// advancing `last_seen_at` and promoting `new` -> `recurring`. That is the
    /// dedup authority for persistence — and it is reachable only if it is
    /// handed the repeat. A `DedupProcessor` in the shared chain (where one
    /// used to be) pins `occurrence_count` at 1 for a whole runner lifetime and
    /// makes `recurring` fire only across restarts, which is how the defect
    /// hid: it looked like working dedup as long as you restarted between
    /// samples. The suppressor belongs to `EventBusExporter` alone.
    #[test]
    fn no_signature_dedup_runs_ahead_of_persistence() {
        let src = include_str!("../../service.rs");
        let chain_start = src
            .find("async fn process_records")
            .expect("process_records must exist");
        let persist = src[chain_start..]
            .find("self.postgres_exporter.export(")
            .expect("process_records must call the postgres exporter");
        let chain = &src[chain_start..chain_start + persist];
        assert!(
            !chain.contains("dedup"),
            "a signature-dedup stage must not run between the parser and the postgres \
             exporter: it drops exactly the repeat sightings that make occurrence_count \
             and the new -> recurring promotion reachable within one process"
        );
    }

    /// The exporter must be reachable from the live service, not just compiled.
    /// The dead `PipelineBuilder` in `pipeline/mod.rs` has no call sites at
    /// all, and an exporter registered only there would never run.
    #[test]
    fn the_exporter_is_wired_into_the_running_service() {
        let src = include_str!("../../service.rs");
        assert!(
            src.contains("postgres_exporter: PostgresExporter"),
            "the service must own the exporter as a field"
        );
        assert!(
            src.contains("PostgresExporter::new("),
            "the service must construct the exporter"
        );
    }
}
