//! Event bus exporter for waking the frontend when new errors arrive.

use crate::error_monitor::pipeline::processors::dedup::DedupProcessor;
use crate::error_monitor::pipeline::traits::Exporter;
use crate::error_monitor::pipeline::types::LogRecord;
use crate::error_monitor::service::ErrorMonitorEvent;
use async_trait::async_trait;
use tokio::sync::mpsc;

/// How many distinct signatures the wake-up suppressor remembers before it
/// resets. Same bound the shared chain used when it (wrongly) ran the
/// suppressor for everyone.
const WAKEUP_DEDUP_CAPACITY: usize = 10_000;

/// Exporter that sends a wake-up through an mpsc channel for consumption by
/// the frontend (Tauri event system).
///
/// # Why the dedup lives HERE and not in the shared chain
///
/// This exporter's payload is deliberately EMPTY: it signals presence, and
/// subscribers then re-query the PG-backed API. A log line repeating in a tight
/// loop would therefore turn into a re-query storm, which is what the signature
/// dedup is genuinely for.
///
/// It used to sit in `ErrorMonitorService::process_records`, in front of BOTH
/// exporters — and that starved `PostgresExporter` of every repeat sighting, so
/// `occurrence_count` never left 1 and `new` never became `recurring` within a
/// runner's lifetime. The two consumers want opposite things, so they now get
/// different behaviour: persistence sees every record and lets the SQL upsert
/// arbitrate; the wake-up is suppressed per signature here. See
/// `processors/dedup.rs`.
pub struct EventBusExporter {
    tx: mpsc::Sender<ErrorMonitorEvent>,
    /// Suppresses a wake-up for a signature already signalled in this process.
    wakeup_dedup: DedupProcessor,
}

impl EventBusExporter {
    pub fn new(tx: mpsc::Sender<ErrorMonitorEvent>) -> Self {
        Self {
            tx,
            wakeup_dedup: DedupProcessor::new(WAKEUP_DEDUP_CAPACITY),
        }
    }
}

#[async_trait]
impl Exporter for EventBusExporter {
    fn name(&self) -> &str {
        "event_bus"
    }

    async fn export(&self, records: &[LogRecord]) -> Result<(), String> {
        // Only parsed records are errors, and only a signature not yet
        // signalled in this process is worth another wake-up. `observe` is
        // called for every parsed record (not short-circuited) so the whole
        // batch is registered, not just the prefix up to the first fresh one.
        let fresh = records
            .iter()
            .filter(|r| r.parsed.is_some())
            .filter(|r| self.wakeup_dedup.observe(r))
            .count();

        if fresh > 0 {
            // Emit a NewErrors notification to wake up the UI. The payload is
            // empty because this exporter only signals presence — subscribers
            // fetch the actual records via the PG-backed query API.
            //
            // That contract is only honest because `PostgresExporter` runs
            // immediately BEFORE this one and has already written the rows
            // (see `ErrorMonitorService::process_records`). For as long as it
            // did not exist, this empty payload pointed subscribers at a table
            // nothing ever populated. Do not reorder the two exporters.
            let _ = self.tx.send(ErrorMonitorEvent::NewErrors(Vec::new())).await;
            let source = records
                .first()
                .map(|r| r.source_name.clone())
                .unwrap_or_default();
            tracing::debug!(
                source = %source,
                count = fresh,
                "Emitted error events to frontend"
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error_monitor::pipeline::processors::dedup::tests::error_event;
    use crate::error_monitor::pipeline::types::{LogRecord, SourceMeta};
    use crate::error_monitor::types::{LogFormat, ParserType};

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

    /// A repeating log line must not become a UI re-query storm — but a
    /// genuinely different error must still wake the UI.
    #[tokio::test]
    async fn repeats_are_suppressed_but_a_new_error_still_wakes_the_ui() {
        let (tx, mut rx) = mpsc::channel(16);
        let exporter = EventBusExporter::new(tx);

        exporter.export(&[record("boom")]).await.unwrap();
        exporter.export(&[record("boom")]).await.unwrap();
        exporter
            .export(&[record("a different error")])
            .await
            .unwrap();

        let mut wakeups = 0;
        while rx.try_recv().is_ok() {
            wakeups += 1;
        }
        assert_eq!(
            wakeups, 2,
            "expected one wake-up for 'boom' and one for the different error; the second \
             'boom' must be suppressed here — and ONLY here, never upstream of persistence"
        );
    }

    #[tokio::test]
    async fn an_unparsed_batch_emits_nothing() {
        let (tx, mut rx) = mpsc::channel(16);
        let exporter = EventBusExporter::new(tx);
        let meta = SourceMeta {
            parser_type: ParserType::Generic,
            format: LogFormat::Plaintext,
            path: None,
        };
        let plain = LogRecord::new("just a log line".to_string(), "s".to_string(), meta);

        exporter.export(&[plain]).await.unwrap();

        assert!(
            rx.try_recv().is_err(),
            "a batch with no parsed error is not a wake-up"
        );
    }
}
