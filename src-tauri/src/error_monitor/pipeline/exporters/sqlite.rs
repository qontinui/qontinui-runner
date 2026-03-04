//! SQLite exporter for persisting error events to the database.

use crate::database::CheckpointDb;
use crate::error_monitor::pipeline::traits::Exporter;
use crate::error_monitor::pipeline::types::LogRecord;
use crate::error_monitor::storage::ErrorEventStorage;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Exporter that writes parsed error events to SQLite.
pub struct SqliteExporter {
    db: Arc<CheckpointDb>,
    /// Current workflow context for error association.
    task_run_id: Arc<RwLock<Option<String>>>,
    workflow_name: Arc<RwLock<Option<String>>>,
}

impl SqliteExporter {
    pub fn new(
        db: Arc<CheckpointDb>,
        task_run_id: Arc<RwLock<Option<String>>>,
        workflow_name: Arc<RwLock<Option<String>>>,
    ) -> Self {
        Self {
            db,
            task_run_id,
            workflow_name,
        }
    }
}

#[async_trait]
impl Exporter for SqliteExporter {
    fn name(&self) -> &str {
        "sqlite"
    }

    async fn export(&self, records: &[LogRecord]) -> Result<(), String> {
        let task_run_id = self.task_run_id.read().await.clone();
        let workflow_name = self.workflow_name.read().await.clone();
        let db = self.db.clone();

        let events: Vec<_> = records.iter().filter_map(|r| r.parsed.clone()).collect();

        if events.is_empty() {
            return Ok(());
        }

        tokio::task::spawn_blocking(move || {
            let conn = db
                .connection_string()
                .map_err(|e| format!("DB connection error: {}", e))?;
            for event in &events {
                if let Err(e) = ErrorEventStorage::insert(
                    &conn,
                    event,
                    task_run_id.as_deref(),
                    workflow_name.as_deref(),
                ) {
                    tracing::warn!(error = %e, "Failed to store error event");
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }
}
