//! Trait definitions for pipeline stages.

use super::types::LogRecord;
use async_trait::async_trait;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

/// A source of log records.
///
/// Receivers run in their own task and push batches of LogRecords
/// into the pipeline channel. Multiple receivers run in parallel.
#[async_trait]
pub trait Receiver: Send + Sync {
    /// Human-readable name for logging/debugging.
    fn name(&self) -> &str;

    /// Start producing records. Runs until cancelled.
    async fn start(&mut self, tx: Sender<Vec<LogRecord>>, cancel: CancellationToken);
}

/// Transforms or filters log records.
///
/// Processors run sequentially in the order they were added to the
/// pipeline. Each processor receives the output of the previous one.
#[async_trait]
pub trait Processor: Send + Sync {
    /// Human-readable name for logging/debugging.
    fn name(&self) -> &str;

    /// Process a batch of records, returning the (possibly filtered/transformed) result.
    async fn process(&self, records: Vec<LogRecord>) -> Vec<LogRecord>;
}

/// Consumes processed log records.
///
/// Exporters receive the final output after all processors. Multiple
/// exporters receive the same records (fan-out).
#[async_trait]
pub trait Exporter: Send + Sync {
    /// Human-readable name for logging/debugging.
    fn name(&self) -> &str;

    /// Export a batch of processed records.
    async fn export(&self, records: &[LogRecord]) -> Result<(), String>;
}
