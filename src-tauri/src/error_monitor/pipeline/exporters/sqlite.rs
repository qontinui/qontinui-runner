//! SQLite exporter for persisting error events to the database.
//! SQLite has been removed — this exporter is a no-op stub.

use crate::error_monitor::pipeline::traits::Exporter;
use crate::error_monitor::pipeline::types::LogRecord;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Exporter that previously wrote parsed error events to SQLite.
/// Now a no-op stub — all persistence goes through PostgreSQL.
pub struct SqliteExporter {
    _task_run_id: Arc<RwLock<Option<String>>>,
    _workflow_name: Arc<RwLock<Option<String>>>,
}

impl SqliteExporter {
    pub fn new(
        task_run_id: Arc<RwLock<Option<String>>>,
        workflow_name: Arc<RwLock<Option<String>>>,
    ) -> Self {
        Self {
            _task_run_id: task_run_id,
            _workflow_name: workflow_name,
        }
    }
}

#[async_trait]
impl Exporter for SqliteExporter {
    fn name(&self) -> &str {
        "sqlite"
    }

    async fn export(&self, _records: &[LogRecord]) -> Result<(), String> {
        // SQLite removed — no-op
        Ok(())
    }
}
