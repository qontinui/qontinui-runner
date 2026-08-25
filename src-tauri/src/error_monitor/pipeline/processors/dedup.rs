//! Deduplication by error signature hash.
//!
//! # This is a UI wake-up suppressor, NOT the persistence dedup
//!
//! It used to sit in the shared processor chain, between the parser and the
//! exporters (`ErrorMonitorService::process_records`). That placement made the
//! whole point of persistence unreachable: `upsert_error_events` deduplicates
//! by bumping `occurrence_count`, advancing `last_seen_at` and promoting
//! `new` -> `recurring`, and it can only do that if it is HANDED the repeat.
//! A signature filter in front of it means the repeat is dropped before the
//! store ever hears about it, so within one runner lifetime `occurrence_count`
//! never left 1 and `recurring` never fired. It only appeared to work across a
//! restart, because that clears this in-process `seen` set.
//!
//! Measured on this branch before the fix: a collected `ERROR` line gave
//! `id=5 status=new occ=1`; the same line again **in the same process** changed
//! nothing; only after a restart did a third copy give `status=recurring
//! occ=2`. One row, correct promote/bump SQL, simply unreachable.
//!
//! So the two consumers of a parsed record want opposite things and now get
//! them separately:
//!
//! * **Persistence** (`PostgresExporter`) must see EVERY parsed record. The
//!   `UPDATE ... occurrence_count + 1` in `upsert_error_events` is the dedup
//!   authority there, and it is the only one that can distinguish a repeat
//!   from a first sighting across restarts, because it reads the store.
//! * **The event bus** (`EventBusExporter`) sends an empty `NewErrors` wake-up
//!   that makes the UI re-query. A log line repeating in a tight loop would
//!   turn into a re-query storm, so that exporter — and only that exporter —
//!   owns an instance of this processor and consults it before emitting.
//!
//! Narrowing this to "drop only byte-identical lines" was the other candidate
//! and is wrong for the same reason: the repeat we must persist IS a
//! byte-identical line. Anything that drops it upstream of the exporter
//! re-creates the defect.
//!
//! **Do not re-add this to `process_records`.** `postgres.rs`'s
//! `no_signature_dedup_runs_ahead_of_persistence` test fails if you do.

use crate::error_monitor::pipeline::traits::Processor;
use crate::error_monitor::pipeline::types::LogRecord;
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Mutex;

/// Tracks recently seen error signatures in a bounded set.
pub struct DedupProcessor {
    seen: Mutex<HashSet<String>>,
    capacity: usize,
}

impl DedupProcessor {
    pub fn new(capacity: usize) -> Self {
        Self {
            seen: Mutex::new(HashSet::new()),
            capacity,
        }
    }

    /// Record `record`'s signature and report whether this is the FIRST time
    /// this instance has seen it.
    ///
    /// An unparsed record carries no signature to compare, so it is always
    /// reported fresh — as is every record if the lock is poisoned, because
    /// failing open is the only safe direction for something whose job is to
    /// suppress.
    pub fn observe(&self, record: &LogRecord) -> bool {
        let Some(ref event) = record.parsed else {
            return true;
        };
        let mut seen = match self.seen.lock() {
            Ok(s) => s,
            Err(_) => return true,
        };
        if seen.len() > self.capacity {
            seen.clear();
        }
        seen.insert(event.compute_signature_hash())
    }
}

#[async_trait]
impl Processor for DedupProcessor {
    fn name(&self) -> &str {
        "dedup"
    }

    async fn process(&self, records: Vec<LogRecord>) -> Vec<LogRecord> {
        records
            .into_iter()
            .filter(|record| self.observe(record))
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::error_monitor::pipeline::types::{LogRecord, SourceMeta};
    use crate::error_monitor::types::{ErrorEvent, ErrorSeverity, LogFormat, ParserType};

    pub(crate) fn error_event(message: &str) -> ErrorEvent {
        ErrorEvent {
            log_source_name: "test-source".to_string(),
            severity: ErrorSeverity::Error,
            error_type: None,
            error_code: None,
            message: message.to_string(),
            stack_trace: None,
            location: None,
            context_lines: None,
            raw_entry: message.to_string(),
            log_timestamp: None,
            trace_id: None,
        }
    }

    fn record(message: &str) -> LogRecord {
        let meta = SourceMeta {
            parser_type: ParserType::Generic,
            format: LogFormat::Plaintext,
            path: None,
        };
        let mut rec = LogRecord::new(message.to_string(), "test-source".to_string(), meta);
        rec.parsed = Some(error_event(message));
        rec
    }

    #[test]
    fn a_repeat_signature_is_reported_stale_but_a_new_one_is_fresh() {
        let dedup = DedupProcessor::new(10);
        let first = record("boom");

        assert!(dedup.observe(&first), "first sighting must be fresh");
        assert!(
            !dedup.observe(&record("boom")),
            "the same signature again must be reported stale — this is what keeps a \
             looping log line from becoming a UI re-query storm"
        );
        assert!(
            dedup.observe(&record("a genuinely different error")),
            "a DIFFERENT error must still be fresh — a suppressor that swallows \
             everything is a worse bug than the one it fixes"
        );
    }

    #[test]
    fn an_unparsed_record_is_always_fresh() {
        let dedup = DedupProcessor::new(10);
        let meta = SourceMeta {
            parser_type: ParserType::Generic,
            format: LogFormat::Plaintext,
            path: None,
        };
        let plain = LogRecord::new("not an error".to_string(), "s".to_string(), meta);
        assert!(dedup.observe(&plain));
        assert!(dedup.observe(&plain));
    }

    /// The service must NOT run this processor in the shared chain: doing so
    /// starves `PostgresExporter` of the repeats it needs to bump
    /// `occurrence_count` and promote `new` -> `recurring`.
    #[test]
    fn the_service_does_not_run_this_in_the_shared_chain() {
        let src = include_str!("../../service.rs");
        assert!(
            !src.contains("dedup_processor"),
            "DedupProcessor must not be a stage of ErrorMonitorService::process_records — \
             it filters out exactly the repeat sightings that make persistence's \
             occurrence_count / recurring promotion reachable. It belongs to \
             EventBusExporter alone."
        );
    }
}
