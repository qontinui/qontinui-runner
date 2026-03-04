//! Composable pipeline architecture for error monitoring.
//!
//! Inspired by OpenTelemetry's collector pipeline, this module provides
//! Receiver -> Processor -> Exporter stages for log processing.
//!
//! ```text
//! Receivers (parallel) -> mpsc channel -> Processors (sequential) -> Exporters (fan-out)
//! ```

pub mod exporters;
pub mod processors;
pub mod receivers;
pub mod traits;
pub mod types;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use types::LogRecord;

/// Composable log processing pipeline.
pub struct Pipeline {
    receivers: Vec<Box<dyn traits::Receiver>>,
    processors: Vec<Box<dyn traits::Processor>>,
    exporters: Vec<Box<dyn traits::Exporter>>,
    channel_size: usize,
}

impl Pipeline {
    /// Create a new pipeline builder.
    pub fn builder() -> PipelineBuilder {
        PipelineBuilder::new()
    }

    /// Run the pipeline until cancelled.
    pub async fn run(mut self, cancel: CancellationToken) {
        let (tx, mut rx) = mpsc::channel::<Vec<LogRecord>>(self.channel_size);

        // Start all receivers
        for receiver in &mut self.receivers {
            let name = receiver.name().to_string();
            info!(receiver = %name, "Starting pipeline receiver");
            let tx_clone = tx.clone();
            let cancel_clone = cancel.clone();
            let mut recv = std::mem::replace(receiver, Box::new(receivers::NoopReceiver));
            tokio::spawn(async move {
                recv.start(tx_clone, cancel_clone).await;
            });
        }

        // Drop original tx so channel closes when all receivers stop
        drop(tx);

        // Process records as they arrive
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("Pipeline cancelled");
                    break;
                }
                Some(mut records) = rx.recv() => {
                    // Run through processor chain sequentially
                    for processor in &self.processors {
                        if records.is_empty() {
                            break;
                        }
                        records = processor.process(records).await;
                    }

                    // Fan out to all exporters
                    if !records.is_empty() {
                        for exporter in &self.exporters {
                            if let Err(e) = exporter.export(&records).await {
                                warn!(
                                    exporter = %exporter.name(),
                                    error = %e,
                                    "Exporter failed"
                                );
                            }
                        }
                    }
                }
                else => {
                    info!("All receivers closed, pipeline shutting down");
                    break;
                }
            }
        }
    }
}

/// Builder for constructing a Pipeline.
pub struct PipelineBuilder {
    receivers: Vec<Box<dyn traits::Receiver>>,
    processors: Vec<Box<dyn traits::Processor>>,
    exporters: Vec<Box<dyn traits::Exporter>>,
    channel_size: usize,
}

impl PipelineBuilder {
    pub fn new() -> Self {
        Self {
            receivers: Vec::new(),
            processors: Vec::new(),
            exporters: Vec::new(),
            channel_size: 256,
        }
    }

    /// Add a receiver to the pipeline.
    pub fn receiver(mut self, r: impl traits::Receiver + 'static) -> Self {
        self.receivers.push(Box::new(r));
        self
    }

    /// Add a processor to the pipeline.
    pub fn processor(mut self, p: impl traits::Processor + 'static) -> Self {
        self.processors.push(Box::new(p));
        self
    }

    /// Add an exporter to the pipeline.
    pub fn exporter(mut self, e: impl traits::Exporter + 'static) -> Self {
        self.exporters.push(Box::new(e));
        self
    }

    /// Set the channel buffer size.
    pub fn channel_size(mut self, size: usize) -> Self {
        self.channel_size = size;
        self
    }

    /// Build the pipeline.
    pub fn build(self) -> Pipeline {
        Pipeline {
            receivers: self.receivers,
            processors: self.processors,
            exporters: self.exporters,
            channel_size: self.channel_size,
        }
    }
}
