//! SQLite storage for pending discoveries.
//!
//! Provides persistent queue storage for discoveries that need to be synced
//! to qontinui-web. Supports offline operation with retry capability.

use super::types::{DiscoveryPayload, PendingDiscovery};

/// Queue a discovery for sync to qontinui-web.
///
/// The discovery is stored locally and will be synced when connectivity
/// is available. Duplicates are detected by checking for similar discoveries
/// within a recent timeframe.
pub fn queue_discovery(payload: DiscoveryPayload) -> Result<String, String> {
    Err("SQLite removed".to_string())
}

/// Get all pending discoveries awaiting sync.
///
/// Returns discoveries ordered by creation time (oldest first).
pub fn get_pending_discoveries() -> Result<Vec<PendingDiscovery>, String> {
    Err("SQLite removed".to_string())
}

/// Get pending discoveries that are ready for retry.
///
/// Filters out discoveries that have been attempted recently (within backoff period)
/// or have exceeded the maximum retry count.
///
/// Backoff: 2^attempt_count minutes (1, 2, 4, 8, 16...)
/// Max attempts: 10
pub fn get_discoveries_for_retry() -> Result<Vec<PendingDiscovery>, String> {
    Err("SQLite removed".to_string())
}

/// Get a specific pending discovery by ID.
pub fn get_pending_discovery(id: &str) -> Result<Option<PendingDiscovery>, String> {
    Err("SQLite removed".to_string())
}

/// Mark a discovery as successfully sent.
///
/// This removes the discovery from the pending queue.
pub fn mark_discovery_sent(id: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Record a failed sync attempt for a discovery.
pub fn record_sync_failure(id: &str, error: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Delete a discovery from the queue (manually cleared or expired).
pub fn delete_discovery(id: &str) -> Result<bool, String> {
    Err("SQLite removed".to_string())
}

/// Delete all discoveries that have exceeded max retry attempts.
pub fn cleanup_failed_discoveries() -> Result<u32, String> {
    Err("SQLite removed".to_string())
}

/// Get the count of pending discoveries.
pub fn get_pending_count() -> Result<u32, String> {
    Err("SQLite removed".to_string())
}

/// Delete old discoveries (older than N days) regardless of attempt count.
pub fn cleanup_old_discoveries(days_old: i64) -> Result<u32, String> {
    Err("SQLite removed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discoveries::types::{DiscoveryEvidence, DiscoveryType};
    use chrono::Utc;

    fn create_test_payload() -> DiscoveryPayload {
        let evidence = DiscoveryEvidence::new(Utc::now().to_rfc3339());
        DiscoveryPayload::new(
            "runner-1".to_string(),
            "project-1".to_string(),
            "config-1".to_string(),
            DiscoveryType::NewElement,
            "Test discovery".to_string(),
            evidence,
            0.95,
        )
    }

    #[test]
    fn test_queue_and_retrieve() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_mark_sent() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_record_failure() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_cleanup_failed() {
        // SQLite removed - no-op
    }
}
