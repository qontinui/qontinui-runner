//! URL Lock Module
//!
//! Provides per-URL exclusive access control for UI Bridge operations.
//! When a workflow reserves a URL, no other workflow can interact with
//! that URL until the reservation is released. This prevents concurrent
//! workflows from interfering with each other's UI state.
//!
//! Unlike the single `GuiLock`, this manages a `HashMap` of URL-to-holder
//! mappings, allowing workflows targeting different URLs to run concurrently.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Notify, RwLock};
use tracing::{debug, info, warn};

/// Entry tracking who holds a URL reservation.
#[derive(Debug, Clone)]
struct UrlLockEntry {
    /// Task run ID of the workflow holding the lock.
    holder_task_run_id: String,
    /// Human-readable name of the holding workflow.
    holder_name: String,
    /// Timestamp (epoch millis) when the lock was acquired.
    acquired_at: u64,
}

/// Per-URL lock manager for UI Bridge operations.
///
/// Ensures that only one workflow at a time can interact with a given
/// UI Bridge URL. Workflows targeting different URLs run concurrently.
/// When a URL is already reserved, callers wait (async) until it's released.
#[derive(Debug, Clone)]
pub struct UrlLockManager {
    state: Arc<RwLock<HashMap<String, UrlLockEntry>>>,
    notify: Arc<Notify>,
}

impl UrlLockManager {
    /// Create a new URL lock manager.
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(HashMap::new())),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Acquire exclusive access to a URL, waiting if another workflow holds it.
    ///
    /// If the same `task_run_id` already holds this URL, returns immediately
    /// (idempotent). If another workflow holds it, blocks until the URL is
    /// released, then acquires it.
    pub async fn acquire(&self, url: &str, task_run_id: &str, holder_name: &str) {
        let normalized = normalize_url(url);

        loop {
            {
                let mut state = self.state.write().await;

                if let Some(entry) = state.get(&normalized) {
                    if entry.holder_task_run_id == task_run_id {
                        // Already held by this task — idempotent
                        debug!(
                            "URL lock for {} already held by task {} (idempotent)",
                            normalized, task_run_id
                        );
                        return;
                    }
                    // Held by someone else — drop the write lock and wait
                    info!(
                        "UI Bridge URL {} is reserved by '{}' (task {}), waiting...",
                        normalized, entry.holder_name, entry.holder_task_run_id
                    );
                } else {
                    // URL is free — acquire it
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);

                    state.insert(
                        normalized.clone(),
                        UrlLockEntry {
                            holder_task_run_id: task_run_id.to_string(),
                            holder_name: holder_name.to_string(),
                            acquired_at: now,
                        },
                    );

                    info!(
                        "UI Bridge URL {} acquired by task {} ('{}')",
                        normalized, task_run_id, holder_name
                    );
                    return;
                }
            }
            // Write lock dropped here — wait for notification, then retry
            self.notify.notified().await;
        }
    }

    /// Try to acquire a URL lock without waiting.
    ///
    /// Returns `Ok(())` if acquired (or already held by this task).
    /// Returns `Err(message)` describing who holds the lock if unavailable.
    pub async fn try_acquire(
        &self,
        url: &str,
        task_run_id: &str,
        holder_name: &str,
    ) -> Result<(), String> {
        let normalized = normalize_url(url);
        let mut state = self.state.write().await;

        if let Some(entry) = state.get(&normalized) {
            if entry.holder_task_run_id == task_run_id {
                return Ok(());
            }
            return Err(format!(
                "URL {} is reserved by workflow '{}' (task_run_id: {}). Wait for it to complete or stop it.",
                normalized, entry.holder_name, entry.holder_task_run_id
            ));
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        state.insert(
            normalized.clone(),
            UrlLockEntry {
                holder_task_run_id: task_run_id.to_string(),
                holder_name: holder_name.to_string(),
                acquired_at: now,
            },
        );

        Ok(())
    }

    /// Release a specific URL lock held by the given task.
    ///
    /// Returns `true` if the lock was released, `false` if the task didn't hold it.
    pub async fn release(&self, url: &str, task_run_id: &str) -> bool {
        let normalized = normalize_url(url);
        let mut state = self.state.write().await;

        if let Some(entry) = state.get(&normalized) {
            if entry.holder_task_run_id != task_run_id {
                warn!(
                    "URL lock release failed: {} held by task {} but task {} tried to release",
                    normalized, entry.holder_task_run_id, task_run_id
                );
                return false;
            }

            state.remove(&normalized);
            info!(
                "UI Bridge URL {} released by task {}",
                normalized, task_run_id
            );
            // Drop write lock before notifying
            drop(state);
            self.notify.notify_waiters();
            true
        } else {
            debug!("URL lock release: {} not locked", normalized);
            true
        }
    }

    /// Release all URL locks held by a specific task.
    ///
    /// Called when a workflow completes, fails, or is cancelled.
    pub async fn release_all(&self, task_run_id: &str) {
        let mut state = self.state.write().await;
        let before = state.len();

        state.retain(|url, entry| {
            if entry.holder_task_run_id == task_run_id {
                info!(
                    "UI Bridge URL {} released (workflow '{}' ended)",
                    url, entry.holder_name
                );
                false
            } else {
                true
            }
        });

        let released = before - state.len();
        if released > 0 {
            info!("Released {} URL lock(s) for task {}", released, task_run_id);
            drop(state);
            self.notify.notify_waiters();
        }
    }

    /// Synchronous version of `release_all` for use in `Drop` impls.
    ///
    /// Uses `try_write()` which may fail if the lock is contended.
    /// In that case, logs a warning — the stale lock will be cleaned up
    /// on next acquire attempt or by the next workflow release.
    pub fn release_all_sync(&self, task_run_id: &str) {
        match self.state.try_write() {
            Ok(mut state) => {
                let before = state.len();

                state.retain(|url, entry| {
                    if entry.holder_task_run_id == task_run_id {
                        info!(
                            "UI Bridge URL {} released (sync cleanup for task {})",
                            url, task_run_id
                        );
                        false
                    } else {
                        true
                    }
                });

                let released = before - state.len();
                if released > 0 {
                    info!(
                        "Sync-released {} URL lock(s) for task {}",
                        released, task_run_id
                    );
                    drop(state);
                    self.notify.notify_waiters();
                }
            }
            Err(_) => {
                warn!(
                    "Could not acquire URL lock state for sync release (task {}). \
                     Lock will be cleaned up on next access.",
                    task_run_id
                );
            }
        }
    }

    /// Clean up stale locks whose holding task runs are no longer running.
    ///
    /// Reads the lock state, checks each holder's task run status in the DB,
    /// and removes entries whose task run is no longer "running".
    pub async fn cleanup_stale_locks(&self, db: &crate::database::CheckpointDb) {
        // First pass: identify stale entries under a read lock
        let stale_ids: Vec<(String, String)> = {
            let state = self.state.read().await;
            let mut stale = Vec::new();
            for (url, entry) in state.iter() {
                match db.get_task_run(&entry.holder_task_run_id) {
                    Ok(Some(task_run)) if task_run.status == "running" => {
                        // Still running — not stale
                    }
                    Ok(_) => {
                        // Not running (completed, failed, stopped, or not found)
                        stale.push((url.clone(), entry.holder_task_run_id.clone()));
                    }
                    Err(e) => {
                        warn!(
                            "Failed to check task run status for lock cleanup ({}): {}",
                            entry.holder_task_run_id, e
                        );
                    }
                }
            }
            stale
        };

        if stale_ids.is_empty() {
            return;
        }

        // Second pass: remove stale entries under a write lock
        {
            let mut state = self.state.write().await;
            for (url, task_run_id) in &stale_ids {
                // Re-check that the entry still belongs to the same task (avoid race)
                if let Some(entry) = state.get(url) {
                    if entry.holder_task_run_id == *task_run_id {
                        info!(
                            "Cleaning up stale URL lock: {} (task {} no longer running)",
                            url, task_run_id
                        );
                        state.remove(url);
                    }
                }
            }
        }

        // Notify waiters so they can re-acquire
        self.notify.notify_waiters();
        info!("Cleaned up {} stale URL lock(s)", stale_ids.len());
    }

    /// Check if a specific URL is currently locked.
    pub async fn is_locked(&self, url: &str) -> bool {
        let normalized = normalize_url(url);
        let state = self.state.read().await;
        state.contains_key(&normalized)
    }

    /// Get information about all current URL locks.
    pub async fn info(&self) -> Vec<UrlLockInfo> {
        let state = self.state.read().await;
        state
            .iter()
            .map(|(url, entry)| UrlLockInfo {
                url: url.clone(),
                holder_task_run_id: entry.holder_task_run_id.clone(),
                holder_name: entry.holder_name.clone(),
                acquired_at: entry.acquired_at,
            })
            .collect()
    }
}

impl Default for UrlLockManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Information about a single URL lock for API introspection.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UrlLockInfo {
    /// The locked URL.
    pub url: String,
    /// Task run ID of the holder.
    pub holder_task_run_id: String,
    /// Human-readable workflow name.
    pub holder_name: String,
    /// Timestamp when the lock was acquired.
    pub acquired_at: u64,
}

/// Normalize a URL to its base form for lock key purposes.
///
/// Strips trailing slashes and path segments after the UI Bridge base.
/// For example, `http://localhost:3001/api/ui-bridge/sdk/execute` becomes
/// `http://localhost:3001/api/ui-bridge`.
fn normalize_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');

    // If URL contains known SDK/control path segments, strip them
    for segment in &["/sdk/", "/control/"] {
        if let Some(pos) = trimmed.find(segment) {
            return trimmed[..pos].to_string();
        }
    }

    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_acquire_and_release() {
        let mgr = UrlLockManager::new();

        mgr.acquire(
            "http://localhost:3001/api/ui-bridge",
            "task-1",
            "Workflow A",
        )
        .await;

        assert!(mgr.is_locked("http://localhost:3001/api/ui-bridge").await);

        assert!(
            mgr.release("http://localhost:3001/api/ui-bridge", "task-1")
                .await
        );
        assert!(!mgr.is_locked("http://localhost:3001/api/ui-bridge").await);
    }

    #[tokio::test]
    async fn test_idempotent_acquire() {
        let mgr = UrlLockManager::new();

        mgr.acquire(
            "http://localhost:3001/api/ui-bridge",
            "task-1",
            "Workflow A",
        )
        .await;

        // Same task acquiring again should succeed immediately (no deadlock)
        mgr.acquire(
            "http://localhost:3001/api/ui-bridge",
            "task-1",
            "Workflow A",
        )
        .await;

        let locks = mgr.info().await;
        assert_eq!(locks.len(), 1);
    }

    #[tokio::test]
    async fn test_exclusive_access_waits() {
        let mgr = UrlLockManager::new();

        // Task 1 acquires
        mgr.acquire(
            "http://localhost:3001/api/ui-bridge",
            "task-1",
            "Workflow A",
        )
        .await;

        // Task 2 tries to acquire in background — should block
        let mgr_clone = mgr.clone();
        let acquired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let acquired_clone = acquired.clone();

        let handle = tokio::spawn(async move {
            mgr_clone
                .acquire(
                    "http://localhost:3001/api/ui-bridge",
                    "task-2",
                    "Workflow B",
                )
                .await;
            acquired_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        // Give task 2 a moment — it should still be waiting
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !acquired.load(std::sync::atomic::Ordering::SeqCst),
            "Task 2 should still be waiting"
        );

        // Release task 1 — task 2 should proceed
        mgr.release("http://localhost:3001/api/ui-bridge", "task-1")
            .await;

        // Wait for task 2 to finish
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("Task 2 should complete within timeout")
            .expect("Task 2 should not panic");

        assert!(
            acquired.load(std::sync::atomic::Ordering::SeqCst),
            "Task 2 should have acquired the lock"
        );

        // Verify task 2 now holds the lock
        let locks = mgr.info().await;
        assert_eq!(locks.len(), 1);
        assert_eq!(locks[0].holder_task_run_id, "task-2");
    }

    #[tokio::test]
    async fn test_different_urls() {
        let mgr = UrlLockManager::new();

        mgr.acquire(
            "http://localhost:3001/api/ui-bridge",
            "task-1",
            "Workflow A",
        )
        .await;
        mgr.acquire("http://localhost:9876/ui-bridge", "task-2", "Workflow B")
            .await;

        let locks = mgr.info().await;
        assert_eq!(locks.len(), 2);
    }

    #[tokio::test]
    async fn test_release_all() {
        let mgr = UrlLockManager::new();

        // Task 1 holds two URLs
        mgr.acquire(
            "http://localhost:3001/api/ui-bridge",
            "task-1",
            "Workflow A",
        )
        .await;
        mgr.acquire("http://localhost:9876/ui-bridge", "task-1", "Workflow A")
            .await;

        // Task 2 holds one URL
        mgr.acquire("http://example.com/ui-bridge", "task-2", "Workflow B")
            .await;

        assert_eq!(mgr.info().await.len(), 3);

        // Release all for task 1
        mgr.release_all("task-1").await;

        let locks = mgr.info().await;
        assert_eq!(locks.len(), 1);
        assert_eq!(locks[0].holder_task_run_id, "task-2");
    }

    #[tokio::test]
    async fn test_release_all_notifies_waiters() {
        let mgr = UrlLockManager::new();

        mgr.acquire(
            "http://localhost:3001/api/ui-bridge",
            "task-1",
            "Workflow A",
        )
        .await;

        let mgr_clone = mgr.clone();
        let acquired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let acquired_clone = acquired.clone();

        let handle = tokio::spawn(async move {
            mgr_clone
                .acquire(
                    "http://localhost:3001/api/ui-bridge",
                    "task-2",
                    "Workflow B",
                )
                .await;
            acquired_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!acquired.load(std::sync::atomic::Ordering::SeqCst));

        // release_all should notify waiters
        mgr.release_all("task-1").await;

        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("Task 2 should complete after release_all")
            .expect("Task 2 should not panic");

        assert!(acquired.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_try_acquire_fails_when_held() {
        let mgr = UrlLockManager::new();

        mgr.acquire(
            "http://localhost:3001/api/ui-bridge",
            "task-1",
            "Workflow A",
        )
        .await;

        let result = mgr
            .try_acquire(
                "http://localhost:3001/api/ui-bridge",
                "task-2",
                "Workflow B",
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Workflow A"));
        assert!(err.contains("task-1"));
    }

    #[tokio::test]
    async fn test_try_acquire_succeeds_same_holder() {
        let mgr = UrlLockManager::new();

        mgr.acquire(
            "http://localhost:3001/api/ui-bridge",
            "task-1",
            "Workflow A",
        )
        .await;

        let result = mgr
            .try_acquire(
                "http://localhost:3001/api/ui-bridge",
                "task-1",
                "Workflow A",
            )
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_normalize_url_strips_sdk_paths() {
        let mgr = UrlLockManager::new();

        mgr.acquire(
            "http://localhost:3001/api/ui-bridge/sdk/execute",
            "task-1",
            "Workflow A",
        )
        .await;

        // Should match the base URL
        assert!(mgr.is_locked("http://localhost:3001/api/ui-bridge").await);
        assert!(
            mgr.is_locked("http://localhost:3001/api/ui-bridge/sdk/navigate")
                .await
        );
        assert!(
            mgr.is_locked("http://localhost:3001/api/ui-bridge/control/snapshot")
                .await
        );
    }

    #[tokio::test]
    async fn test_release_wrong_holder() {
        let mgr = UrlLockManager::new();

        mgr.acquire(
            "http://localhost:3001/api/ui-bridge",
            "task-1",
            "Workflow A",
        )
        .await;

        // task-2 shouldn't be able to release task-1's lock
        assert!(
            !mgr.release("http://localhost:3001/api/ui-bridge", "task-2")
                .await
        );

        // Lock should still be held
        assert!(mgr.is_locked("http://localhost:3001/api/ui-bridge").await);
    }

    #[test]
    fn test_release_all_sync() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mgr = UrlLockManager::new();

        rt.block_on(async {
            mgr.acquire(
                "http://localhost:3001/api/ui-bridge",
                "task-1",
                "Workflow A",
            )
            .await;
        });

        // Sync release should work outside async context
        mgr.release_all_sync("task-1");

        rt.block_on(async {
            assert!(!mgr.is_locked("http://localhost:3001/api/ui-bridge").await);
        });
    }

    #[test]
    fn test_normalize_url_function() {
        assert_eq!(
            normalize_url("http://localhost:3001/api/ui-bridge/sdk/execute"),
            "http://localhost:3001/api/ui-bridge"
        );
        assert_eq!(
            normalize_url("http://localhost:3001/api/ui-bridge/control/snapshot"),
            "http://localhost:3001/api/ui-bridge"
        );
        assert_eq!(
            normalize_url("http://localhost:9876/ui-bridge/"),
            "http://localhost:9876/ui-bridge"
        );
        assert_eq!(
            normalize_url("http://localhost:9876/ui-bridge"),
            "http://localhost:9876/ui-bridge"
        );
    }
}
