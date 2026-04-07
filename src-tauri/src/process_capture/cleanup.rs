//! Background cleanup loop for old process_sessions and their output rows.
//!
//! Runs once per `interval_secs`, deleting sessions older than the configured
//! retention period. CASCADE handles `process_session_output` cleanup.

use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use crate::database::pg::PgDb;

/// Default retention: 7 days. Override with env var `QONTINUI_PROCESS_LOG_RETENTION_DAYS`.
const DEFAULT_RETENTION_DAYS: u32 = 7;

/// Default interval: once per day.
const DEFAULT_INTERVAL_SECS: u64 = 86_400;

fn get_retention_days() -> u32 {
    std::env::var("QONTINUI_PROCESS_LOG_RETENTION_DAYS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_RETENTION_DAYS)
}

/// Background loop: delete process sessions older than the retention period.
pub async fn run_process_log_cleanup_loop(pg_db: Arc<PgDb>) {
    let interval_secs = DEFAULT_INTERVAL_SECS;
    let retention_days = get_retention_days();

    info!(
        "process_log_cleanup_loop_started: interval_secs={}, retention_days={}",
        interval_secs, retention_days
    );

    // Run once a few seconds after startup, then periodically
    tokio::time::sleep(Duration::from_secs(30)).await;

    loop {
        match pg_db.cleanup_old_process_sessions(retention_days).await {
            Ok(deleted) if deleted > 0 => {
                info!(
                    "process_log_cleanup: deleted {} sessions older than {} days",
                    deleted, retention_days
                );
            }
            Ok(_) => {}
            Err(e) => {
                warn!("process_log_cleanup failed: {}", e);
            }
        }

        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
    }
}
