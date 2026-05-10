//! File Registry and Lock Module
//!
//! Two complementary systems for managing concurrent file access:
//!
//! 1. **FileRegistryManager** (advisory): Tracks which files are under active
//!    development by concurrent sessions. Non-blocking — multiple sessions CAN
//!    work on the same file, but new sessions are alerted to potential conflicts.
//!
//! 2. **FileLockManager** (exclusive): Provides per-file blocking locks. When
//!    a session edits a file, it acquires an exclusive lock. If another session
//!    tries to edit the same file, its stdout reader thread blocks until the
//!    lock is released, creating backpressure that pauses Claude Code.
//!
//! ## Worktree scoping
//!
//! The advisory `FileRegistryManager` keys registrations by `(worktree_id, path)`.
//! `worktree_id == None` means the main tree (the historical, default behavior).
//! Two sessions editing the same path in different worktrees do NOT conflict;
//! same path in the same worktree (or both in the main tree) still conflicts.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Notify, RwLock};
use tracing::{debug, info, warn};

/// Composite registry key. `worktree_id == None` means the main tree.
///
/// Two sessions touching the same `path` in different worktrees produce
/// distinct keys and therefore do not conflict.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RegistryKey {
    worktree_id: Option<String>,
    path: String,
}

/// A single file registration by a session.
#[derive(Debug, Clone)]
struct FileRegistryEntry {
    /// Task run ID of the session holding this registration.
    holder_task_run_id: String,
    /// Human-readable name (workflow name or session label).
    holder_name: String,
    /// Timestamp (epoch millis) when the file was registered.
    registered_at: u64,
}

/// Advisory file registry for tracking files under active development.
///
/// Sessions (workflows and AI terminal sessions) register files they are
/// actively working on. When a new session starts or wants to check for
/// conflicts, it queries the registry and receives a list of files that
/// other sessions are already touching.
///
/// Key differences from UrlLockManager:
/// - **Advisory, not exclusive**: multiple sessions can register the same file
/// - **Non-blocking**: registering a file that another session holds succeeds immediately
/// - **Conflict detection**: query endpoints return which other sessions hold the same files
/// - **Worktree-scoped**: registrations are keyed by `(worktree_id, path)`, so
///   the same path in different worktrees is treated as independent.
#[derive(Debug, Clone)]
pub struct FileRegistryManager {
    /// Maps `(worktree_id, normalized path)` → list of sessions working on it.
    state: Arc<RwLock<HashMap<RegistryKey, Vec<FileRegistryEntry>>>>,
}

impl FileRegistryManager {
    /// Create a new file registry manager.
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register one or more files as under active development by a session.
    ///
    /// `worktree_id == None` means the main tree (historical behavior).
    /// Returns a list of conflicts — files that are already registered by
    /// OTHER sessions in the SAME worktree. Registration always succeeds
    /// regardless.
    pub async fn register(
        &self,
        file_paths: &[String],
        task_run_id: &str,
        holder_name: &str,
        worktree_id: Option<String>,
    ) -> Vec<FileConflict> {
        let now = now_millis();
        let mut conflicts = Vec::new();
        let mut state = self.state.write().await;

        for raw_path in file_paths {
            let path = normalize_path(raw_path);
            let key = RegistryKey {
                worktree_id: worktree_id.clone(),
                path: path.clone(),
            };
            let entries = state.entry(key).or_default();

            // Check for conflicts from OTHER sessions in the same worktree
            let other_holders: Vec<ConflictHolder> = entries
                .iter()
                .filter(|e| e.holder_task_run_id != task_run_id)
                .map(|e| ConflictHolder {
                    task_run_id: e.holder_task_run_id.clone(),
                    holder_name: e.holder_name.clone(),
                    registered_at: e.registered_at,
                })
                .collect();

            if !other_holders.is_empty() {
                conflicts.push(FileConflict {
                    file_path: path.clone(),
                    worktree_id: worktree_id.clone(),
                    other_holders,
                });
            }

            // Add registration if this session hasn't already registered this file
            if !entries.iter().any(|e| e.holder_task_run_id == task_run_id) {
                entries.push(FileRegistryEntry {
                    holder_task_run_id: task_run_id.to_string(),
                    holder_name: holder_name.to_string(),
                    registered_at: now,
                });
                debug!(
                    "File '{}' (worktree {:?}) registered by session '{}' (task {})",
                    path, worktree_id, holder_name, task_run_id
                );
            }
        }

        if !conflicts.is_empty() {
            info!(
                "Session '{}' (task {}) registered {} file(s) with {} conflict(s) in worktree {:?}",
                holder_name,
                task_run_id,
                file_paths.len(),
                conflicts.len(),
                worktree_id,
            );
        }

        conflicts
    }

    /// Unregister specific files for a session within a specific worktree.
    pub async fn unregister(
        &self,
        file_paths: &[String],
        task_run_id: &str,
        worktree_id: Option<String>,
    ) {
        let mut state = self.state.write().await;

        for raw_path in file_paths {
            let path = normalize_path(raw_path);
            let key = RegistryKey {
                worktree_id: worktree_id.clone(),
                path: path.clone(),
            };
            if let Some(entries) = state.get_mut(&key) {
                entries.retain(|e| e.holder_task_run_id != task_run_id);
                if entries.is_empty() {
                    state.remove(&key);
                }
                debug!(
                    "File '{}' (worktree {:?}) unregistered by task {}",
                    path, worktree_id, task_run_id
                );
            }
        }
    }

    /// Release all file registrations for a session, across ALL worktrees.
    ///
    /// Called when a workflow completes, fails, or is cancelled,
    /// or when an AI session ends.
    pub async fn release_all(&self, task_run_id: &str) {
        let mut state = self.state.write().await;
        let before: usize = state.values().map(|v| v.len()).sum();

        for entries in state.values_mut() {
            entries.retain(|e| e.holder_task_run_id != task_run_id);
        }

        // Remove empty keys
        state.retain(|_, entries| !entries.is_empty());

        let after: usize = state.values().map(|v| v.len()).sum();
        let released = before - after;
        if released > 0 {
            info!(
                "Released {} file registration(s) for task {}",
                released, task_run_id
            );
        }
    }

    /// Synchronous version of `release_all` for use in `Drop` impls.
    pub fn release_all_sync(&self, task_run_id: &str) {
        for attempt in 0..10 {
            match self.state.try_write() {
                Ok(mut state) => {
                    for entries in state.values_mut() {
                        entries.retain(|e| e.holder_task_run_id != task_run_id);
                    }
                    state.retain(|_, entries| !entries.is_empty());
                    return;
                }
                Err(_) => {
                    if attempt < 9 {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
            }
        }
        warn!(
            "Could not acquire file registry state after 10 retries for sync release (task {})",
            task_run_id
        );
    }

    /// Query all files that would conflict with a given session.
    ///
    /// Returns files registered by OTHER sessions (not the querying session)
    /// across all worktrees. Useful for alerting a new session about files
    /// already under development. The returned `FileConflict` entries carry
    /// the `worktree_id` so callers can group/scope as needed.
    pub async fn check_conflicts(&self, task_run_id: &str) -> Vec<FileConflict> {
        let state = self.state.read().await;
        let mut conflicts = Vec::new();

        for (key, entries) in state.iter() {
            let other_holders: Vec<ConflictHolder> = entries
                .iter()
                .filter(|e| e.holder_task_run_id != task_run_id)
                .map(|e| ConflictHolder {
                    task_run_id: e.holder_task_run_id.clone(),
                    holder_name: e.holder_name.clone(),
                    registered_at: e.registered_at,
                })
                .collect();

            if !other_holders.is_empty() {
                conflicts.push(FileConflict {
                    file_path: key.path.clone(),
                    worktree_id: key.worktree_id.clone(),
                    other_holders,
                });
            }
        }

        conflicts
    }

    /// Query conflicts for a specific set of files within a specific worktree.
    ///
    /// Returns only conflicts for the given file paths in the given worktree,
    /// excluding the querying session's own registrations.
    pub async fn check_conflicts_for_files(
        &self,
        file_paths: &[String],
        task_run_id: &str,
        worktree_id: Option<String>,
    ) -> Vec<FileConflict> {
        let state = self.state.read().await;
        let mut conflicts = Vec::new();

        for raw_path in file_paths {
            let path = normalize_path(raw_path);
            let key = RegistryKey {
                worktree_id: worktree_id.clone(),
                path: path.clone(),
            };
            if let Some(entries) = state.get(&key) {
                let other_holders: Vec<ConflictHolder> = entries
                    .iter()
                    .filter(|e| e.holder_task_run_id != task_run_id)
                    .map(|e| ConflictHolder {
                        task_run_id: e.holder_task_run_id.clone(),
                        holder_name: e.holder_name.clone(),
                        registered_at: e.registered_at,
                    })
                    .collect();

                if !other_holders.is_empty() {
                    conflicts.push(FileConflict {
                        file_path: path,
                        worktree_id: worktree_id.clone(),
                        other_holders,
                    });
                }
            }
        }

        conflicts
    }

    /// Get a snapshot of all current file registrations.
    ///
    /// Each entry includes its `worktree_id` so the UI can group/display
    /// per-worktree.
    pub async fn info(&self) -> Vec<FileRegistryInfo> {
        let state = self.state.read().await;
        state
            .iter()
            .flat_map(|(key, entries)| {
                let path = key.path.clone();
                let worktree_id = key.worktree_id.clone();
                entries.iter().map(move |e| FileRegistryInfo {
                    file_path: path.clone(),
                    worktree_id: worktree_id.clone(),
                    holder_task_run_id: e.holder_task_run_id.clone(),
                    holder_name: e.holder_name.clone(),
                    registered_at: e.registered_at,
                })
            })
            .collect()
    }

    /// Clean up stale registrations whose holding task runs are no longer running.
    ///
    /// Note: This uses a two-phase read-then-write pattern. Between phases, a task
    /// could theoretically re-register. In practice, task run IDs are UUIDs and are
    /// never reused, so this TOCTOU gap is safe.
    pub async fn cleanup_stale(&self, db: &crate::database::pg::PgDb) {
        // First pass: identify stale task_run_ids under a read lock
        let stale_task_ids: Vec<String> = {
            let state = self.state.read().await;
            let mut unique_ids: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for entries in state.values() {
                for entry in entries {
                    unique_ids.insert(entry.holder_task_run_id.clone());
                }
            }

            let mut stale = Vec::new();
            for task_id in unique_ids {
                match db.get_task_run(&task_id).await {
                    Ok(Some(task_run)) if task_run.status == "running" => {
                        // Still running — not stale
                    }
                    Ok(_) => {
                        stale.push(task_id);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to check task run status for file registry cleanup ({}): {}",
                            task_id, e
                        );
                    }
                }
            }
            stale
        };

        if stale_task_ids.is_empty() {
            return;
        }

        // Second pass: remove stale entries under a write lock
        {
            let mut state = self.state.write().await;
            for task_id in &stale_task_ids {
                for entries in state.values_mut() {
                    entries.retain(|e| e.holder_task_run_id != *task_id);
                }
            }
            state.retain(|_, entries| !entries.is_empty());
        }

        info!(
            "Cleaned up file registrations for {} stale task(s)",
            stale_task_ids.len()
        );
    }
}

impl Default for FileRegistryManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Information about a conflict on a specific file.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileConflict {
    /// The file path with a conflict.
    pub file_path: String,
    /// Worktree the conflict belongs to. `None` means the main tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    /// Other sessions working on this file.
    pub other_holders: Vec<ConflictHolder>,
}

/// A session that holds a conflicting file registration.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConflictHolder {
    /// Task run ID of the other session.
    pub task_run_id: String,
    /// Human-readable name of the other session.
    pub holder_name: String,
    /// When the other session registered this file.
    pub registered_at: u64,
}

/// Information about a single file registration for API introspection.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileRegistryInfo {
    /// The registered file path.
    pub file_path: String,
    /// Worktree the registration belongs to. `None` means the main tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    /// Task run ID of the holder.
    pub holder_task_run_id: String,
    /// Human-readable session/workflow name.
    pub holder_name: String,
    /// Timestamp when the file was registered.
    pub registered_at: u64,
}

/// Normalize a file path for consistent registry keys.
///
/// Converts backslashes to forward slashes and lowercases on Windows
/// for case-insensitive matching.
pub(crate) fn normalize_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    // Windows paths are case-insensitive
    #[cfg(target_os = "windows")]
    let normalized = normalized.to_lowercase();
    normalized
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// =============================================================================
// FileLockManager — exclusive per-file locks with blocking
// =============================================================================

/// Entry tracking who holds a file lock.
#[derive(Debug, Clone)]
struct FileLockEntry {
    holder_task_run_id: String,
    holder_name: String,
    acquired_at: u64,
}

/// Exclusive per-file lock manager that blocks concurrent access.
///
/// When a session edits a file (Edit/Write tool), it acquires an exclusive lock.
/// If another session tries to edit the same file, the `acquire` call blocks
/// until the lock is released. This creates backpressure on the Claude Code
/// stdout reader thread, which pauses the AI session deterministically.
///
/// Unlike the advisory `FileRegistryManager`, this is a hard blocking mechanism.
#[derive(Debug, Clone)]
pub struct FileLockManager {
    state: Arc<RwLock<HashMap<String, FileLockEntry>>>,
    notify: Arc<Notify>,
}

impl FileLockManager {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(HashMap::new())),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Acquire exclusive access to a file, blocking if another session holds it.
    ///
    /// If the same `task_run_id` already holds this file, returns immediately
    /// (idempotent). If another session holds it, blocks until released.
    ///
    /// Returns the name of the session that was blocking (if any), for logging.
    pub async fn acquire(
        &self,
        file_path: &str,
        task_run_id: &str,
        holder_name: &str,
    ) -> Option<String> {
        let normalized = normalize_path(file_path);
        let mut waited_for: Option<String> = None;

        loop {
            {
                let mut state = self.state.write().await;

                // Check if already held by this session (idempotent)
                if let Some(entry) = state.get(&normalized) {
                    if entry.holder_task_run_id == task_run_id {
                        return waited_for;
                    }
                    // Held by another session — record who we're waiting for, then wait
                    waited_for = Some(entry.holder_name.clone());
                } else {
                    // Free — acquire it
                    state.insert(
                        normalized,
                        FileLockEntry {
                            holder_task_run_id: task_run_id.to_string(),
                            holder_name: holder_name.to_string(),
                            acquired_at: now_millis(),
                        },
                    );
                    return waited_for;
                }
            }
            // Drop the write lock before waiting
            // Wait for any release notification, then retry
            tokio::time::timeout(Duration::from_secs(5), self.notify.notified())
                .await
                .ok();
        }
    }

    /// Release a specific file lock.
    pub async fn release(&self, file_path: &str, task_run_id: &str) {
        let normalized = normalize_path(file_path);
        let mut state = self.state.write().await;

        if let Some(entry) = state.get(&normalized) {
            if entry.holder_task_run_id == task_run_id {
                state.remove(&normalized);
                drop(state);
                self.notify.notify_waiters();
            }
        }
    }

    /// Release all file locks held by a session.
    ///
    /// Returns the normalized paths whose locks were released. Callers
    /// with access to a `tauri::AppHandle` use this to emit a
    /// `file-lock-released` event per path so frontends can clear "X is
    /// blocked on …" indicators without waiting on the next poll.
    pub async fn release_all(&self, task_run_id: &str) -> Vec<String> {
        let mut state = self.state.write().await;
        let mut released_paths: Vec<String> = Vec::new();
        state.retain(|path, entry| {
            if entry.holder_task_run_id == task_run_id {
                released_paths.push(path.clone());
                false
            } else {
                true
            }
        });
        if !released_paths.is_empty() {
            info!(
                "Released {} file lock(s) for task {}",
                released_paths.len(),
                task_run_id
            );
            drop(state);
            self.notify.notify_waiters();
        }
        released_paths
    }

    /// Synchronous version for Drop impls.
    ///
    /// Returns the normalized paths whose locks were released (same shape
    /// as the async `release_all`). Callers without access to a runtime
    /// can still emit per-path events from the returned vec; sites that
    /// don't have a `tauri::AppHandle` (e.g. `WorkflowDropGuard::drop`)
    /// can drop the result — the event is best-effort.
    pub fn release_all_sync(&self, task_run_id: &str) -> Vec<String> {
        for attempt in 0..10 {
            match self.state.try_write() {
                Ok(mut state) => {
                    let mut released_paths: Vec<String> = Vec::new();
                    state.retain(|path, entry| {
                        if entry.holder_task_run_id == task_run_id {
                            released_paths.push(path.clone());
                            false
                        } else {
                            true
                        }
                    });
                    if !released_paths.is_empty() {
                        drop(state);
                        self.notify.notify_waiters();
                    }
                    return released_paths;
                }
                Err(_) => {
                    if attempt < 9 {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            }
        }
        warn!(
            "Could not acquire file lock state after 10 retries for sync release (task {})",
            task_run_id
        );
        Vec::new()
    }

    /// Check if a file is held by a different session. Returns the holder name if so.
    pub async fn is_held_by_other(&self, file_path: &str, task_run_id: &str) -> Option<String> {
        let normalized = normalize_path(file_path);
        let state = self.state.read().await;
        state
            .get(&normalized)
            .filter(|e| e.holder_task_run_id != task_run_id)
            .map(|e| e.holder_name.clone())
    }

    /// Get info about all currently held locks (for debugging/UI).
    pub async fn info(&self) -> Vec<FileLockInfo> {
        let state = self.state.read().await;
        state
            .iter()
            .map(|(path, entry)| FileLockInfo {
                file_path: path.clone(),
                holder_task_run_id: entry.holder_task_run_id.clone(),
                holder_name: entry.holder_name.clone(),
                acquired_at: entry.acquired_at,
            })
            .collect()
    }
}

impl Default for FileLockManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Information about a held file lock.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileLockInfo {
    pub file_path: String,
    pub holder_task_run_id: String,
    pub holder_name: String,
    pub acquired_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_info() {
        let mgr = FileRegistryManager::new();

        let conflicts = mgr
            .register(&["src/main.rs".into()], "task-1", "Workflow A", None)
            .await;
        assert!(conflicts.is_empty());

        let info = mgr.info().await;
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].file_path, "src/main.rs");
        assert_eq!(info[0].holder_task_run_id, "task-1");
        assert_eq!(info[0].worktree_id, None);
    }

    #[tokio::test]
    async fn test_idempotent_register() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["src/main.rs".into()], "task-1", "Workflow A", None)
            .await;
        mgr.register(&["src/main.rs".into()], "task-1", "Workflow A", None)
            .await;

        let info = mgr.info().await;
        assert_eq!(
            info.len(),
            1,
            "Same session registering twice should not duplicate"
        );
    }

    #[tokio::test]
    async fn test_conflict_detection() {
        let mgr = FileRegistryManager::new();

        // Task 1 registers a file
        mgr.register(&["src/lib.rs".into()], "task-1", "Workflow A", None)
            .await;

        // Task 2 registers the same file — should get a conflict
        let conflicts = mgr
            .register(&["src/lib.rs".into()], "task-2", "Workflow B", None)
            .await;

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].file_path, "src/lib.rs");
        assert_eq!(conflicts[0].other_holders.len(), 1);
        assert_eq!(conflicts[0].other_holders[0].task_run_id, "task-1");
    }

    #[tokio::test]
    async fn test_no_self_conflict() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["src/lib.rs".into()], "task-1", "Workflow A", None)
            .await;

        // Same session should not conflict with itself
        let conflicts = mgr.check_conflicts("task-1").await;
        assert!(conflicts.is_empty());
    }

    #[tokio::test]
    async fn test_release_all() {
        let mgr = FileRegistryManager::new();

        mgr.register(
            &["src/a.rs".into(), "src/b.rs".into()],
            "task-1",
            "Workflow A",
            None,
        )
        .await;
        mgr.register(&["src/b.rs".into()], "task-2", "Workflow B", None)
            .await;

        assert_eq!(mgr.info().await.len(), 3);

        mgr.release_all("task-1").await;

        let info = mgr.info().await;
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].holder_task_run_id, "task-2");
    }

    #[tokio::test]
    async fn test_unregister_specific() {
        let mgr = FileRegistryManager::new();

        mgr.register(
            &["src/a.rs".into(), "src/b.rs".into()],
            "task-1",
            "Workflow A",
            None,
        )
        .await;

        mgr.unregister(&["src/a.rs".into()], "task-1", None).await;

        let info = mgr.info().await;
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].file_path, "src/b.rs");
    }

    #[tokio::test]
    async fn test_check_conflicts_for_files() {
        let mgr = FileRegistryManager::new();

        mgr.register(
            &["src/a.rs".into(), "src/b.rs".into(), "src/c.rs".into()],
            "task-1",
            "Workflow A",
            None,
        )
        .await;

        // Task 2 checks only specific files
        let conflicts = mgr
            .check_conflicts_for_files(&["src/b.rs".into(), "src/d.rs".into()], "task-2", None)
            .await;

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].file_path, "src/b.rs");
    }

    #[tokio::test]
    async fn test_multiple_sessions_same_file() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["src/shared.rs".into()], "task-1", "Workflow A", None)
            .await;
        mgr.register(&["src/shared.rs".into()], "task-2", "Workflow B", None)
            .await;

        // Task 3 registers — should see both task-1 and task-2 as conflicts
        let conflicts = mgr
            .register(&["src/shared.rs".into()], "task-3", "Workflow C", None)
            .await;

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].other_holders.len(), 2);
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn test_windows_path_normalization() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["src\\main.rs".into()], "task-1", "Workflow A", None)
            .await;

        // Forward-slash version should match
        let conflicts = mgr
            .check_conflicts_for_files(&["src/main.rs".into()], "task-2", None)
            .await;

        assert_eq!(conflicts.len(), 1);
    }

    #[test]
    fn test_release_all_sync() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mgr = FileRegistryManager::new();

        rt.block_on(async {
            mgr.register(&["src/main.rs".into()], "task-1", "Workflow A", None)
                .await;
        });

        mgr.release_all_sync("task-1");

        rt.block_on(async {
            assert!(mgr.info().await.is_empty());
        });
    }

    #[tokio::test]
    async fn test_empty_file_paths() {
        let mgr = FileRegistryManager::new();

        let conflicts = mgr.register(&[], "task-1", "Workflow A", None).await;
        assert!(conflicts.is_empty());
        assert!(mgr.info().await.is_empty());
    }

    #[tokio::test]
    async fn test_unregister_nonexistent_file() {
        let mgr = FileRegistryManager::new();

        // Unregistering a file that was never registered should not panic
        mgr.unregister(&["nonexistent.rs".into()], "task-1", None)
            .await;
        assert!(mgr.info().await.is_empty());
    }

    #[tokio::test]
    async fn test_release_all_nonexistent_task() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["src/a.rs".into()], "task-1", "Workflow A", None)
            .await;

        // Releasing a different task should not affect existing registrations
        mgr.release_all("task-99").await;
        assert_eq!(mgr.info().await.len(), 1);
    }

    #[tokio::test]
    async fn test_conflict_holder_names_correct() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["shared.rs".into()], "task-1", "Session Alpha", None)
            .await;
        mgr.register(&["shared.rs".into()], "task-2", "Session Beta", None)
            .await;

        let conflicts = mgr.check_conflicts("task-1").await;
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].other_holders.len(), 1);
        assert_eq!(conflicts[0].other_holders[0].task_run_id, "task-2");
        assert_eq!(conflicts[0].other_holders[0].holder_name, "Session Beta");
    }

    #[tokio::test]
    async fn test_check_conflicts_for_files_excludes_self() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["a.rs".into(), "b.rs".into()], "task-1", "Session A", None)
            .await;
        mgr.register(&["b.rs".into(), "c.rs".into()], "task-2", "Session B", None)
            .await;

        // Task 1 checks for conflicts on b.rs — should see task-2 but not itself
        let conflicts = mgr
            .check_conflicts_for_files(&["b.rs".into()], "task-1", None)
            .await;
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].other_holders.len(), 1);
        assert_eq!(conflicts[0].other_holders[0].task_run_id, "task-2");
    }

    #[tokio::test]
    async fn test_info_returns_all_entries() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["a.rs".into(), "b.rs".into()], "task-1", "Session A", None)
            .await;
        mgr.register(&["b.rs".into(), "c.rs".into()], "task-2", "Session B", None)
            .await;

        let info = mgr.info().await;
        // a.rs(task-1), b.rs(task-1), b.rs(task-2), c.rs(task-2)
        assert_eq!(info.len(), 4);
    }

    #[tokio::test]
    async fn test_register_returns_conflicts_on_registration() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["x.rs".into(), "y.rs".into()], "task-1", "A", None)
            .await;

        // Task 2 registers x.rs (conflict) and z.rs (no conflict)
        let conflicts = mgr
            .register(&["x.rs".into(), "z.rs".into()], "task-2", "B", None)
            .await;

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].file_path, "x.rs");
    }

    #[tokio::test]
    async fn test_concurrent_register_and_release() {
        let mgr = FileRegistryManager::new();

        // Simulate rapid register/release cycles
        for i in 0..50 {
            let task_id = format!("task-{}", i);
            mgr.register(
                &["shared.rs".into()],
                &task_id,
                &format!("Session {}", i),
                None,
            )
            .await;
        }

        assert_eq!(mgr.info().await.len(), 50);

        // Release all at once
        for i in 0..50 {
            mgr.release_all(&format!("task-{}", i)).await;
        }

        assert!(mgr.info().await.is_empty());
    }

    #[tokio::test]
    async fn test_serialization_shapes() {
        // Verify serde serialization produces expected JSON shapes
        let conflict = FileConflict {
            file_path: "src/main.rs".to_string(),
            worktree_id: None,
            other_holders: vec![ConflictHolder {
                task_run_id: "task-1".to_string(),
                holder_name: "Session A".to_string(),
                registered_at: 1234567890,
            }],
        };

        let json = serde_json::to_value(&conflict).unwrap();
        assert_eq!(json["file_path"], "src/main.rs");
        assert!(
            json.get("worktree_id").is_none(),
            "None worktree_id should be skipped in serialization"
        );
        assert_eq!(json["other_holders"][0]["task_run_id"], "task-1");
        assert_eq!(json["other_holders"][0]["holder_name"], "Session A");
        assert_eq!(json["other_holders"][0]["registered_at"], 1234567890);

        let info = FileRegistryInfo {
            file_path: "src/lib.rs".to_string(),
            worktree_id: Some("wt-7".to_string()),
            holder_task_run_id: "task-2".to_string(),
            holder_name: "Workflow B".to_string(),
            registered_at: 9876543210,
        };

        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["file_path"], "src/lib.rs");
        assert_eq!(json["worktree_id"], "wt-7");
        assert_eq!(json["holder_task_run_id"], "task-2");
        assert_eq!(json["holder_name"], "Workflow B");
        assert_eq!(json["registered_at"], 9876543210u64);
    }

    // =========================================================================
    // Phase 1 worktree-scoping tests
    // =========================================================================

    #[tokio::test]
    async fn test_no_conflict_across_different_worktrees() {
        let mgr = FileRegistryManager::new();

        mgr.register(
            &["src/lib.rs".into()],
            "task-1",
            "Worktree 1 session",
            Some("wt-1".to_string()),
        )
        .await;

        // Same path in a different worktree — must NOT conflict
        let conflicts = mgr
            .register(
                &["src/lib.rs".into()],
                "task-2",
                "Worktree 2 session",
                Some("wt-2".to_string()),
            )
            .await;

        assert!(
            conflicts.is_empty(),
            "Same path in different worktrees must not conflict"
        );

        // Both registrations should be present
        let info = mgr.info().await;
        assert_eq!(info.len(), 2);
        let mut ids: Vec<Option<String>> = info.iter().map(|i| i.worktree_id.clone()).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec![Some("wt-1".to_string()), Some("wt-2".to_string())]
        );
    }

    #[tokio::test]
    async fn test_no_conflict_main_tree_vs_worktree() {
        let mgr = FileRegistryManager::new();

        // Main tree (None)
        mgr.register(&["src/lib.rs".into()], "task-main", "Main session", None)
            .await;

        // Worktree 1 — different scope, must NOT conflict
        let conflicts = mgr
            .register(
                &["src/lib.rs".into()],
                "task-wt1",
                "Worktree 1 session",
                Some("wt-1".to_string()),
            )
            .await;

        assert!(
            conflicts.is_empty(),
            "Main tree and worktree must be independent scopes"
        );

        // check_conflicts_for_files for main tree should not see the worktree entry
        let main_conflicts = mgr
            .check_conflicts_for_files(&["src/lib.rs".into()], "task-other", None)
            .await;
        assert_eq!(main_conflicts.len(), 1);
        assert_eq!(main_conflicts[0].other_holders[0].task_run_id, "task-main");

        // check_conflicts_for_files for worktree 1 should not see the main entry
        let wt_conflicts = mgr
            .check_conflicts_for_files(
                &["src/lib.rs".into()],
                "task-other",
                Some("wt-1".to_string()),
            )
            .await;
        assert_eq!(wt_conflicts.len(), 1);
        assert_eq!(wt_conflicts[0].other_holders[0].task_run_id, "task-wt1");
    }

    #[tokio::test]
    async fn test_conflict_within_same_worktree() {
        let mgr = FileRegistryManager::new();

        mgr.register(
            &["src/lib.rs".into()],
            "task-1",
            "Session A",
            Some("wt-1".to_string()),
        )
        .await;

        // Same worktree, same path, different session — must conflict
        let conflicts = mgr
            .register(
                &["src/lib.rs".into()],
                "task-2",
                "Session B",
                Some("wt-1".to_string()),
            )
            .await;

        assert_eq!(
            conflicts.len(),
            1,
            "Same path in same worktree must conflict"
        );
        assert_eq!(conflicts[0].file_path, "src/lib.rs");
        assert_eq!(conflicts[0].worktree_id, Some("wt-1".to_string()));
        assert_eq!(conflicts[0].other_holders.len(), 1);
        assert_eq!(conflicts[0].other_holders[0].task_run_id, "task-1");
    }

    #[tokio::test]
    async fn test_release_all_clears_across_worktrees() {
        let mgr = FileRegistryManager::new();

        // Same task registers in main tree, worktree 1, and worktree 2
        mgr.register(&["a.rs".into()], "task-1", "Multi", None)
            .await;
        mgr.register(
            &["b.rs".into()],
            "task-1",
            "Multi",
            Some("wt-1".to_string()),
        )
        .await;
        mgr.register(
            &["c.rs".into()],
            "task-1",
            "Multi",
            Some("wt-2".to_string()),
        )
        .await;

        assert_eq!(mgr.info().await.len(), 3);

        mgr.release_all("task-1").await;

        assert!(
            mgr.info().await.is_empty(),
            "release_all must clear registrations across all worktrees for the session"
        );
    }

    #[tokio::test]
    async fn test_unregister_is_worktree_scoped() {
        let mgr = FileRegistryManager::new();

        // Same task registers same path in two worktrees
        mgr.register(&["lib.rs".into()], "task-1", "S", Some("wt-1".to_string()))
            .await;
        mgr.register(&["lib.rs".into()], "task-1", "S", Some("wt-2".to_string()))
            .await;

        // Unregister only in worktree 1
        mgr.unregister(&["lib.rs".into()], "task-1", Some("wt-1".to_string()))
            .await;

        let info = mgr.info().await;
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].worktree_id, Some("wt-2".to_string()));
    }

    // =========================================================================
    // FileLockManager: release_all returns released paths
    //
    // The async `release_all` and sync `release_all_sync` return the
    // normalized paths whose locks were released, so callers with access
    // to a `tauri::AppHandle` can emit a `file-lock-released` event per
    // path (best-effort; sites without a handle drop the result).
    // =========================================================================

    #[tokio::test]
    async fn test_lock_release_all_returns_released_paths() {
        let mgr = FileLockManager::new();

        // Same task acquires three files
        mgr.acquire("src/a.rs", "task-1", "Session A").await;
        mgr.acquire("src/b.rs", "task-1", "Session A").await;
        mgr.acquire("src/c.rs", "task-1", "Session A").await;

        // A second task holds an unrelated file — must NOT be returned.
        mgr.acquire("src/other.rs", "task-2", "Session B").await;

        let mut released = mgr.release_all("task-1").await;
        released.sort();

        // Paths come back normalized (forward-slash, lowercased on Windows).
        let mut expected: Vec<String> = vec!["src/a.rs", "src/b.rs", "src/c.rs"]
            .into_iter()
            .map(normalize_path)
            .collect();
        expected.sort();

        assert_eq!(released, expected);

        // Sanity: task-2's lock survives.
        let info = mgr.info().await;
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].holder_task_run_id, "task-2");
    }

    #[tokio::test]
    async fn test_lock_release_all_returns_empty_when_no_holdings() {
        let mgr = FileLockManager::new();

        // No locks held at all — empty Vec, no panic.
        let released = mgr.release_all("never-held").await;
        assert!(released.is_empty());

        // Some other task holds a lock — still empty for the queried task.
        mgr.acquire("src/x.rs", "task-other", "Other").await;
        let released = mgr.release_all("never-held").await;
        assert!(released.is_empty());
    }

    #[test]
    fn test_lock_release_all_sync_returns_released_paths() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mgr = FileLockManager::new();

        rt.block_on(async {
            mgr.acquire("src/a.rs", "task-1", "Session A").await;
            mgr.acquire("src/b.rs", "task-1", "Session A").await;
        });

        let mut released = mgr.release_all_sync("task-1");
        released.sort();

        let mut expected: Vec<String> = vec!["src/a.rs", "src/b.rs"]
            .into_iter()
            .map(normalize_path)
            .collect();
        expected.sort();

        assert_eq!(released, expected);

        rt.block_on(async {
            assert!(mgr.info().await.is_empty());
        });
    }
}
