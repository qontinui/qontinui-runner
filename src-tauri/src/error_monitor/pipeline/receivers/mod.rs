//! Pipeline receivers — sources of log records.

pub mod file_watch;
pub mod stream;

use super::traits::Receiver;
use super::types::LogRecord;
use async_trait::async_trait;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

/// No-op receiver used as a placeholder during pipeline construction.
pub(crate) struct NoopReceiver;

#[async_trait]
impl Receiver for NoopReceiver {
    fn name(&self) -> &str {
        "noop"
    }

    async fn start(&mut self, _tx: Sender<Vec<LogRecord>>, cancel: CancellationToken) {
        cancel.cancelled().await;
    }
}
