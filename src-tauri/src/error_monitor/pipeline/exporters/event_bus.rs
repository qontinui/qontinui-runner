//! Event bus exporter for sending error events to the frontend.

use crate::error_monitor::pipeline::traits::Exporter;
use crate::error_monitor::pipeline::types::LogRecord;
use crate::error_monitor::service::ErrorMonitorEvent;
use async_trait::async_trait;
use tokio::sync::mpsc;

/// Exporter that sends error events through an mpsc channel
/// for consumption by the frontend (Tauri event system).
pub struct EventBusExporter {
    tx: mpsc::Sender<ErrorMonitorEvent>,
}

impl EventBusExporter {
    pub fn new(tx: mpsc::Sender<ErrorMonitorEvent>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl Exporter for EventBusExporter {
    fn name(&self) -> &str {
        "event_bus"
    }

    async fn export(&self, records: &[LogRecord]) -> Result<(), String> {
        // Only emit events for records with parsed errors
        let error_count = records.iter().filter(|r| r.parsed.is_some()).count();

        if error_count > 0 {
            // Emit a NewErrors event as a notification (the actual errors
            // are stored by the SqliteExporter — this just notifies the UI)
            let _ = self.tx.send(ErrorMonitorEvent::NewErrors(Vec::new())).await;
            let source = records
                .first()
                .map(|r| r.source_name.clone())
                .unwrap_or_default();
            tracing::debug!(
                source = %source,
                count = error_count,
                "Emitted error events to frontend"
            );
        }

        Ok(())
    }
}
