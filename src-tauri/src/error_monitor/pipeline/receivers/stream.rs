//! Stream receiver for ingesting errors from managed process output.

use crate::error_monitor::pipeline::traits::Receiver;
use crate::error_monitor::pipeline::types::LogRecord;
use crate::error_monitor::types::ErrorEvent;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Receiver that accepts pre-parsed errors from managed process streams.
///
/// Instead of watching files, this receiver accepts ErrorEvents pushed
/// via a channel (e.g., from IngestStreamErrors command).
pub struct StreamReceiver {
    rx: mpsc::Receiver<Vec<ErrorEvent>>,
}

impl StreamReceiver {
    /// Create a new stream receiver and its sender handle.
    pub fn new() -> (Self, mpsc::Sender<Vec<ErrorEvent>>) {
        let (tx, rx) = mpsc::channel(100);
        (Self { rx }, tx)
    }
}

#[async_trait]
impl Receiver for StreamReceiver {
    fn name(&self) -> &str {
        "stream"
    }

    async fn start(&mut self, tx: mpsc::Sender<Vec<LogRecord>>, cancel: CancellationToken) {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                Some(errors) = self.rx.recv() => {
                    let records: Vec<LogRecord> = errors.into_iter().map(LogRecord::from).collect();
                    if !records.is_empty() && tx.send(records).await.is_err() {
                        return; // Channel closed
                    }
                }
                else => break, // Sender dropped
            }
        }
    }
}
