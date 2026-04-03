//! File Registry Module
//!
//! Advisory registry that tracks which files are under active development
//! by concurrent sessions (workflows and AI sessions). Unlike the URL lock
//! system, this is non-blocking — multiple sessions CAN work on the same
//! file, but new sessions are alerted to potential conflicts.
//!
//! This helps avoid merge conflicts when multiple Claude Code sessions
//! run simultaneously on the same branch.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

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
#[derive(Debug, Clone)]
pub struct FileRegistryManager {
    /// Maps normalized file path → list of sessions working on it.
    state: Arc<RwLock<HashMap<String, Vec<FileRegistryEntry>>>>,
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
    /// Returns a list of conflicts — files that are already registered by
    /// OTHER sessions. The registration always succeeds regardless.
    pub async fn register(
        &self,
        file_paths: &[String],
        task_run_id: &str,
        holder_name: &str,
    ) -> Vec<FileConflict> {
        let now = now_millis();
        let mut conflicts = Vec::new();
        let mut state = self.state.write().await;

        for raw_path in file_paths {
            let path = normalize_path(raw_path);
            let entries = state.entry(path.clone()).or_default();

            // Check for conflicts from OTHER sessions
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
                    "File '{}' registered by session '{}' (task {})",
                    path, holder_name, task_run_id
                );
            }
        }

        if !conflicts.is_empty() {
            info!(
                "Session '{}' (task {}) registered {} file(s) with {} conflict(s)",
                holder_name,
                task_run_id,
                file_paths.len(),
                conflicts.len()
            );
        }

        conflicts
    }

    /// Unregister specific files for a session.
    pub async fn unregister(&self, file_paths: &[String], task_run_id: &str) {
        let mut state = self.state.write().await;

        for raw_path in file_paths {
            let path = normalize_path(raw_path);
            if let Some(entries) = state.get_mut(&path) {
                entries.retain(|e| e.holder_task_run_id != task_run_id);
                if entries.is_empty() {
                    state.remove(&path);
                }
                debug!("File '{}' unregistered by task {}", path, task_run_id);
            }
        }
    }

    /// Release all file registrations for a session.
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
    /// Returns files registered by OTHER sessions (not the querying session).
    /// Useful for alerting a new session about files already under development.
    pub async fn check_conflicts(&self, task_run_id: &str) -> Vec<FileConflict> {
        let state = self.state.read().await;
        let mut conflicts = Vec::new();

        for (path, entries) in state.iter() {
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
                    other_holders,
                });
            }
        }

        conflicts
    }

    /// Query conflicts for a specific set of files.
    ///
    /// Returns only conflicts for the given file paths, excluding the
    /// querying session's own registrations.
    pub async fn check_conflicts_for_files(
        &self,
        file_paths: &[String],
        task_run_id: &str,
    ) -> Vec<FileConflict> {
        let state = self.state.read().await;
        let mut conflicts = Vec::new();

        for raw_path in file_paths {
            let path = normalize_path(raw_path);
            if let Some(entries) = state.get(&path) {
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
                        other_holders,
                    });
                }
            }
        }

        conflicts
    }

    /// Get a snapshot of all current file registrations.
    pub async fn info(&self) -> Vec<FileRegistryInfo> {
        let state = self.state.read().await;
        state
            .iter()
            .flat_map(|(path, entries)| {
                entries.iter().map(move |e| FileRegistryInfo {
                    file_path: path.clone(),
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
fn normalize_path(path: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_info() {
        let mgr = FileRegistryManager::new();

        let conflicts = mgr
            .register(&["src/main.rs".into()], "task-1", "Workflow A")
            .await;
        assert!(conflicts.is_empty());

        let info = mgr.info().await;
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].file_path, "src/main.rs");
        assert_eq!(info[0].holder_task_run_id, "task-1");
    }

    #[tokio::test]
    async fn test_idempotent_register() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["src/main.rs".into()], "task-1", "Workflow A")
            .await;
        mgr.register(&["src/main.rs".into()], "task-1", "Workflow A")
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
        mgr.register(&["src/lib.rs".into()], "task-1", "Workflow A")
            .await;

        // Task 2 registers the same file — should get a conflict
        let conflicts = mgr
            .register(&["src/lib.rs".into()], "task-2", "Workflow B")
            .await;

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].file_path, "src/lib.rs");
        assert_eq!(conflicts[0].other_holders.len(), 1);
        assert_eq!(conflicts[0].other_holders[0].task_run_id, "task-1");
    }

    #[tokio::test]
    async fn test_no_self_conflict() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["src/lib.rs".into()], "task-1", "Workflow A")
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
        )
        .await;
        mgr.register(&["src/b.rs".into()], "task-2", "Workflow B")
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
        )
        .await;

        mgr.unregister(&["src/a.rs".into()], "task-1").await;

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
        )
        .await;

        // Task 2 checks only specific files
        let conflicts = mgr
            .check_conflicts_for_files(&["src/b.rs".into(), "src/d.rs".into()], "task-2")
            .await;

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].file_path, "src/b.rs");
    }

    #[tokio::test]
    async fn test_multiple_sessions_same_file() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["src/shared.rs".into()], "task-1", "Workflow A")
            .await;
        mgr.register(&["src/shared.rs".into()], "task-2", "Workflow B")
            .await;

        // Task 3 registers — should see both task-1 and task-2 as conflicts
        let conflicts = mgr
            .register(&["src/shared.rs".into()], "task-3", "Workflow C")
            .await;

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].other_holders.len(), 2);
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn test_windows_path_normalization() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["src\\main.rs".into()], "task-1", "Workflow A")
            .await;

        // Forward-slash version should match
        let conflicts = mgr
            .check_conflicts_for_files(&["src/main.rs".into()], "task-2")
            .await;

        assert_eq!(conflicts.len(), 1);
    }

    #[test]
    fn test_release_all_sync() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mgr = FileRegistryManager::new();

        rt.block_on(async {
            mgr.register(&["src/main.rs".into()], "task-1", "Workflow A")
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

        let conflicts = mgr.register(&[], "task-1", "Workflow A").await;
        assert!(conflicts.is_empty());
        assert!(mgr.info().await.is_empty());
    }

    #[tokio::test]
    async fn test_unregister_nonexistent_file() {
        let mgr = FileRegistryManager::new();

        // Unregistering a file that was never registered should not panic
        mgr.unregister(&["nonexistent.rs".into()], "task-1").await;
        assert!(mgr.info().await.is_empty());
    }

    #[tokio::test]
    async fn test_release_all_nonexistent_task() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["src/a.rs".into()], "task-1", "Workflow A")
            .await;

        // Releasing a different task should not affect existing registrations
        mgr.release_all("task-99").await;
        assert_eq!(mgr.info().await.len(), 1);
    }

    #[tokio::test]
    async fn test_conflict_holder_names_correct() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["shared.rs".into()], "task-1", "Session Alpha")
            .await;
        mgr.register(&["shared.rs".into()], "task-2", "Session Beta")
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

        mgr.register(&["a.rs".into(), "b.rs".into()], "task-1", "Session A")
            .await;
        mgr.register(&["b.rs".into(), "c.rs".into()], "task-2", "Session B")
            .await;

        // Task 1 checks for conflicts on b.rs — should see task-2 but not itself
        let conflicts = mgr
            .check_conflicts_for_files(&["b.rs".into()], "task-1")
            .await;
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].other_holders.len(), 1);
        assert_eq!(conflicts[0].other_holders[0].task_run_id, "task-2");
    }

    #[tokio::test]
    async fn test_info_returns_all_entries() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["a.rs".into(), "b.rs".into()], "task-1", "Session A")
            .await;
        mgr.register(&["b.rs".into(), "c.rs".into()], "task-2", "Session B")
            .await;

        let info = mgr.info().await;
        // a.rs(task-1), b.rs(task-1), b.rs(task-2), c.rs(task-2)
        assert_eq!(info.len(), 4);
    }

    #[tokio::test]
    async fn test_register_returns_conflicts_on_registration() {
        let mgr = FileRegistryManager::new();

        mgr.register(&["x.rs".into(), "y.rs".into()], "task-1", "A")
            .await;

        // Task 2 registers x.rs (conflict) and z.rs (no conflict)
        let conflicts = mgr
            .register(&["x.rs".into(), "z.rs".into()], "task-2", "B")
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
            mgr.register(&["shared.rs".into()], &task_id, &format!("Session {}", i))
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
            other_holders: vec![ConflictHolder {
                task_run_id: "task-1".to_string(),
                holder_name: "Session A".to_string(),
                registered_at: 1234567890,
            }],
        };

        let json = serde_json::to_value(&conflict).unwrap();
        assert_eq!(json["file_path"], "src/main.rs");
        assert_eq!(json["other_holders"][0]["task_run_id"], "task-1");
        assert_eq!(json["other_holders"][0]["holder_name"], "Session A");
        assert_eq!(json["other_holders"][0]["registered_at"], 1234567890);

        let info = FileRegistryInfo {
            file_path: "src/lib.rs".to_string(),
            holder_task_run_id: "task-2".to_string(),
            holder_name: "Workflow B".to_string(),
            registered_at: 9876543210,
        };

        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["file_path"], "src/lib.rs");
        assert_eq!(json["holder_task_run_id"], "task-2");
        assert_eq!(json["holder_name"], "Workflow B");
        assert_eq!(json["registered_at"], 9876543210u64);
    }
}
