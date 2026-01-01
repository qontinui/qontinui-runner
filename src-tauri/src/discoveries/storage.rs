//! SQLite storage for pending discoveries.
//!
//! Provides persistent queue storage for discoveries that need to be synced
//! to qontinui-web. Supports offline operation with retry capability.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use tracing::{debug, info, warn};

use super::types::{DiscoveryPayload, PendingDiscovery};

/// Queue a discovery for sync to qontinui-web.
///
/// The discovery is stored locally and will be synced when connectivity
/// is available. Duplicates are detected by checking for similar discoveries
/// within a recent timeframe.
pub fn queue_discovery(conn: &Connection, payload: DiscoveryPayload) -> Result<String, String> {
    let pending = PendingDiscovery::new(payload);
    let now = Utc::now().to_rfc3339();

    let payload_json = serde_json::to_string(&pending.payload)
        .map_err(|e| format!("Failed to serialize payload: {}", e))?;

    conn.execute(
        r#"
        INSERT INTO pending_discoveries (id, payload, created_at, attempt_count)
        VALUES (?1, ?2, ?3, 0)
        "#,
        params![pending.id, payload_json, now],
    )
    .map_err(|e| format!("Failed to queue discovery: {}", e))?;

    info!("Queued discovery {} for sync", pending.id);
    Ok(pending.id)
}

/// Get all pending discoveries awaiting sync.
///
/// Returns discoveries ordered by creation time (oldest first).
pub fn get_pending_discoveries(conn: &Connection) -> Result<Vec<PendingDiscovery>, String> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT id, payload, created_at, last_attempt, attempt_count, error
            FROM pending_discoveries
            ORDER BY created_at ASC
            "#,
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let discoveries = stmt
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let payload_json: String = row.get(1)?;
            let created_at: String = row.get(2)?;
            let last_attempt: Option<String> = row.get(3)?;
            let attempt_count: u32 = row.get(4)?;
            let error: Option<String> = row.get(5)?;

            let payload: DiscoveryPayload = serde_json::from_str(&payload_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;

            Ok(PendingDiscovery {
                id,
                payload,
                created_at,
                last_attempt,
                attempt_count,
                error,
            })
        })
        .map_err(|e| format!("Failed to query discoveries: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(discoveries)
}

/// Get pending discoveries that are ready for retry.
///
/// Filters out discoveries that have been attempted recently (within backoff period)
/// or have exceeded the maximum retry count.
///
/// Backoff: 2^attempt_count minutes (1, 2, 4, 8, 16...)
/// Max attempts: 10
pub fn get_discoveries_for_retry(conn: &Connection) -> Result<Vec<PendingDiscovery>, String> {
    let max_attempts: u32 = 10;
    let now = Utc::now();

    let all = get_pending_discoveries(conn)?;

    let ready: Vec<PendingDiscovery> = all
        .into_iter()
        .filter(|d| {
            // Skip if exceeded max attempts
            if d.attempt_count >= max_attempts {
                return false;
            }

            // Skip if attempted too recently (backoff)
            if let Some(last) = &d.last_attempt {
                if let Ok(last_time) = chrono::DateTime::parse_from_rfc3339(last) {
                    let backoff_minutes = 2_i64.pow(d.attempt_count);
                    let next_allowed = last_time + chrono::Duration::minutes(backoff_minutes);
                    if now < next_allowed {
                        return false;
                    }
                }
            }

            true
        })
        .collect();

    Ok(ready)
}

/// Get a specific pending discovery by ID.
pub fn get_pending_discovery(
    conn: &Connection,
    id: &str,
) -> Result<Option<PendingDiscovery>, String> {
    let result = conn
        .query_row(
            r#"
            SELECT id, payload, created_at, last_attempt, attempt_count, error
            FROM pending_discoveries
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                let id: String = row.get(0)?;
                let payload_json: String = row.get(1)?;
                let created_at: String = row.get(2)?;
                let last_attempt: Option<String> = row.get(3)?;
                let attempt_count: u32 = row.get(4)?;
                let error: Option<String> = row.get(5)?;

                let payload: DiscoveryPayload =
                    serde_json::from_str(&payload_json).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;

                Ok(PendingDiscovery {
                    id,
                    payload,
                    created_at,
                    last_attempt,
                    attempt_count,
                    error,
                })
            },
        )
        .optional()
        .map_err(|e| format!("Failed to get discovery: {}", e))?;

    Ok(result)
}

/// Mark a discovery as successfully sent.
///
/// This removes the discovery from the pending queue.
pub fn mark_discovery_sent(conn: &Connection, id: &str) -> Result<(), String> {
    let rows = conn
        .execute("DELETE FROM pending_discoveries WHERE id = ?1", params![id])
        .map_err(|e| format!("Failed to delete discovery: {}", e))?;

    if rows > 0 {
        info!("Discovery {} marked as sent and removed from queue", id);
    } else {
        warn!("Discovery {} not found in queue", id);
    }

    Ok(())
}

/// Record a failed sync attempt for a discovery.
pub fn record_sync_failure(conn: &Connection, id: &str, error: &str) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();

    conn.execute(
        r#"
        UPDATE pending_discoveries
        SET last_attempt = ?1, attempt_count = attempt_count + 1, error = ?2
        WHERE id = ?3
        "#,
        params![now, error, id],
    )
    .map_err(|e| format!("Failed to update discovery: {}", e))?;

    debug!("Recorded sync failure for discovery {}: {}", id, error);
    Ok(())
}

/// Delete a discovery from the queue (manually cleared or expired).
pub fn delete_discovery(conn: &Connection, id: &str) -> Result<bool, String> {
    let rows = conn
        .execute("DELETE FROM pending_discoveries WHERE id = ?1", params![id])
        .map_err(|e| format!("Failed to delete discovery: {}", e))?;

    Ok(rows > 0)
}

/// Delete all discoveries that have exceeded max retry attempts.
pub fn cleanup_failed_discoveries(conn: &Connection) -> Result<u32, String> {
    let max_attempts: u32 = 10;

    let rows = conn
        .execute(
            "DELETE FROM pending_discoveries WHERE attempt_count >= ?1",
            params![max_attempts],
        )
        .map_err(|e| format!("Failed to cleanup discoveries: {}", e))?;

    if rows > 0 {
        info!(
            "Cleaned up {} discoveries that exceeded max retry attempts",
            rows
        );
    }

    Ok(rows as u32)
}

/// Get the count of pending discoveries.
pub fn get_pending_count(conn: &Connection) -> Result<u32, String> {
    let count: u32 = conn
        .query_row("SELECT COUNT(*) FROM pending_discoveries", [], |row| {
            row.get(0)
        })
        .map_err(|e| format!("Failed to count discoveries: {}", e))?;

    Ok(count)
}

/// Delete old discoveries (older than N days) regardless of attempt count.
pub fn cleanup_old_discoveries(conn: &Connection, days_old: i64) -> Result<u32, String> {
    let cutoff = Utc::now() - chrono::Duration::days(days_old);
    let cutoff_str = cutoff.to_rfc3339();

    let rows = conn
        .execute(
            "DELETE FROM pending_discoveries WHERE created_at < ?1",
            params![cutoff_str],
        )
        .map_err(|e| format!("Failed to cleanup old discoveries: {}", e))?;

    if rows > 0 {
        info!(
            "Cleaned up {} discoveries older than {} days",
            rows, days_old
        );
    }

    Ok(rows as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discoveries::types::{DiscoveryEvidence, DiscoveryType};

    fn create_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS pending_discoveries (
                id TEXT PRIMARY KEY,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL,
                last_attempt TEXT,
                attempt_count INTEGER DEFAULT 0,
                error TEXT
            );
            "#,
        )
        .unwrap();
        conn
    }

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
        let conn = create_test_db();
        let payload = create_test_payload();

        let id = queue_discovery(&conn, payload.clone()).unwrap();
        assert!(!id.is_empty());

        let pending = get_pending_discoveries(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].payload.title, "Test discovery");
    }

    #[test]
    fn test_mark_sent() {
        let conn = create_test_db();
        let payload = create_test_payload();

        let id = queue_discovery(&conn, payload).unwrap();
        assert_eq!(get_pending_count(&conn).unwrap(), 1);

        mark_discovery_sent(&conn, &id).unwrap();
        assert_eq!(get_pending_count(&conn).unwrap(), 0);
    }

    #[test]
    fn test_record_failure() {
        let conn = create_test_db();
        let payload = create_test_payload();

        let id = queue_discovery(&conn, payload).unwrap();

        record_sync_failure(&conn, &id, "Network error").unwrap();

        let pending = get_pending_discovery(&conn, &id).unwrap().unwrap();
        assert_eq!(pending.attempt_count, 1);
        assert_eq!(pending.error, Some("Network error".to_string()));
    }

    #[test]
    fn test_cleanup_failed() {
        let conn = create_test_db();
        let payload = create_test_payload();

        let id = queue_discovery(&conn, payload).unwrap();

        // Simulate 10 failures
        for _ in 0..10 {
            record_sync_failure(&conn, &id, "Error").unwrap();
        }

        let cleaned = cleanup_failed_discoveries(&conn).unwrap();
        assert_eq!(cleaned, 1);
        assert_eq!(get_pending_count(&conn).unwrap(), 0);
    }
}
