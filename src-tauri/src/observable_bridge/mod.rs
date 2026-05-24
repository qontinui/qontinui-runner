//! Federated observable categories — runner-mediated coord bridges.
//!
//! Plan: `2026-05-22-memories-on-coord-cross-machine.md`.
//!
//! ## Pattern
//!
//! Every federated agent observable (memories first, git-ops next, more
//! later) follows the same shape:
//!
//!   Agent (Claude session, ...) writes/reads via local convention
//!     → Runner bridges on session boundary + during session
//!     → Coord HTTP API (per-category routes)
//!     → Single tenant-scoped store, fleet-wide
//!     → Fan-out: pulls into other runners' next session prep
//!
//! The [`RunnerObservableBridge`] trait abstracts this lifecycle.
//! [`MemoryBridge`] is the reference implementation. A second category
//! (`GitOpBridge`) implements the same trait — the trait is
//! category-generic: it carries no memory-specific types. Bridge-internal
//! concerns (the memory watcher's `push` + its `ObservableChange` change
//! type) live inside [`memory`], not on the shared interface, because a
//! git-op change has nothing structurally in common with a memory upsert.

use anyhow::Result;
use async_trait::async_trait;
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

pub mod git_ops;
pub mod git_ops_client;
pub mod memory;
pub mod memory_client;

/// Process-wide registry of every enabled observable bridge, set once at
/// runner init by `main.rs::setup`. Spawn-site call paths
/// (`claude_session::runner`, `claude_session::session`) reach it via
/// [`global_registry`] because they run in the binary tree without
/// access to Tauri `State`, and iterate every bridge for pull / watch /
/// reconcile.
///
/// Empty (or unset) if init failed (e.g., no profile, no reqwest
/// builder) — every caller iterates zero bridges and the spawn proceeds
/// without federation.
static GLOBAL_REGISTRY: OnceCell<Vec<Arc<dyn RunnerObservableBridge>>> = OnceCell::new();

/// Install the process-wide observable-bridge registry. Idempotent —
/// subsequent calls are silently ignored so accidental double-init
/// (e.g., from two Tauri `.setup` closures during testing) does not
/// panic.
pub fn init_registry(bridges: Vec<Arc<dyn RunnerObservableBridge>>) {
    let _ = GLOBAL_REGISTRY.set(bridges);
}

/// Read the process-wide observable-bridge registry. Returns an empty
/// slice until [`init_registry`] runs successfully — federation-aware
/// call sites iterate it and a zero-length registry is the
/// "feature disabled this session" signal.
pub fn global_registry() -> &'static [Arc<dyn RunnerObservableBridge>] {
    GLOBAL_REGISTRY.get().map(Vec::as_slice).unwrap_or(&[])
}

/// Per-session context every bridge receives.
///
/// `tenant_id` is carried for local bookkeeping + UI surfacing only —
/// the coord-side `TenantId` extractor resolves it from the device-token
/// JWT, so HTTP request bodies do not include it.
#[derive(Debug, Clone)]
pub struct SessionContext {
    pub tenant_id: Uuid,
    pub device_id: Uuid,
    pub account_name: String,
    pub memory_dir: PathBuf,
    /// The working directory the session spawned in. `MemoryBridge`
    /// derives `memory_dir` from it upstream and does not read this
    /// field; `GitOpBridge` reads it to locate `<working_dir>/.git/`.
    pub working_dir: PathBuf,
    pub session_id: Uuid,
    /// Snapshot of `memory_dir` taken immediately after `pull` completed.
    /// Used by `reconcile` to diff against final state at session end.
    /// `None` until `pull` runs.
    pub post_pull_snapshot: Option<DirSnapshot>,
    /// Count of files pulled from coord during the `pull` phase.
    /// Populated by `MemoryBridge::pull`; read by `reconcile` to fill
    /// the `ReconcileReport.pulled` field.
    pub pulled_count: u32,
}

/// A single file's identity in a memory directory snapshot.
/// `name` is the file basename without `.md`; `sha256` is hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFingerprint {
    pub name: String,
    pub sha256: String,
}

/// All `.md` files in a memory directory at a point in time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DirSnapshot {
    pub files: Vec<FileFingerprint>,
}

impl DirSnapshot {
    pub fn get(&self, name: &str) -> Option<&FileFingerprint> {
        self.files.iter().find(|f| f.name == name)
    }
}

/// Aggregate counts from a `reconcile` pass — useful for UI banner.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconcileReport {
    pub pushed: u32,
    pub pulled: u32,
    pub unchanged: u32,
    pub failed: u32,
    /// Names of files that failed during reconcile (for debugging).
    #[serde(default)]
    pub failed_names: Vec<String>,
}

/// The trait every federated observable category implements.
///
/// The interface is category-generic: it carries no memory-specific
/// types. How a bridge detects local changes during a session (a file
/// watcher, a git hook, …) and how it represents those changes are
/// bridge-internal — they never cross this trait. `MemoryBridge`'s
/// `push` + `ObservableChange` live inside [`memory`] for exactly this
/// reason.
#[async_trait]
pub trait RunnerObservableBridge: Send + Sync {
    /// Stable category name — used as the `coord.<category>` table name
    /// and the dashboard route segment. Memories: `"memory"`. Git-ops:
    /// `"git_op"`.
    fn category(&self) -> &'static str;

    /// Spawn-time: pull the tenant pool from coord, materialize locally.
    /// May record per-session baseline state on `ctx` (e.g.
    /// `post_pull_snapshot`) for `reconcile` to diff against.
    async fn pull(&self, ctx: &mut SessionContext) -> Result<()>;

    /// During-session: start watching for local changes and push them to
    /// coord best-effort.
    ///
    /// The receiver is `self: Arc<Self>` (NOT `&self`): an
    /// implementation may `Arc::clone(&self)` into a detached watch task
    /// so the watcher can keep calling its own internal push after this
    /// returns. `&self` cannot produce that owned `Arc`, and `&Arc<Self>`
    /// is not object-safe. `Arc<Self>` IS object-safe — callable on
    /// `Arc<dyn RunnerObservableBridge>` — so the dispatch site passes
    /// `Arc::clone(b)`. `ctx` is borrowed; the impl clones it internally
    /// before moving the clone into any spawned task.
    async fn start_watching(self: Arc<Self>, ctx: &SessionContext) -> Result<()>;

    /// Stop the watcher for `session_id` if one is running. Idempotent —
    /// a no-op when no watcher is registered for the id. Safe to call
    /// before `reconcile` (which may also stop internally).
    async fn stop_watching(&self, session_id: Uuid);

    /// Session-end: stop watching, diff final state against the post-pull
    /// baseline, push anything the watcher missed, return aggregate
    /// counts.
    async fn reconcile(&self, ctx: &SessionContext) -> Result<ReconcileReport>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_fingerprint_equality() {
        let a = FileFingerprint {
            name: "feedback_demo".to_string(),
            sha256: "abc123".to_string(),
        };
        let b = FileFingerprint {
            name: "feedback_demo".to_string(),
            sha256: "abc123".to_string(),
        };
        let c = FileFingerprint {
            name: "feedback_demo".to_string(),
            sha256: "def456".to_string(),
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn dir_snapshot_get_finds_existing() {
        let snap = DirSnapshot {
            files: vec![
                FileFingerprint {
                    name: "alpha".to_string(),
                    sha256: "h1".to_string(),
                },
                FileFingerprint {
                    name: "beta".to_string(),
                    sha256: "h2".to_string(),
                },
            ],
        };
        let found = snap.get("beta").expect("beta should be present");
        assert_eq!(found.sha256, "h2");
    }

    #[test]
    fn dir_snapshot_get_returns_none_for_missing() {
        let snap = DirSnapshot {
            files: vec![FileFingerprint {
                name: "alpha".to_string(),
                sha256: "h1".to_string(),
            }],
        };
        assert!(snap.get("missing").is_none());
    }

    #[test]
    fn dir_snapshot_default_is_empty() {
        let snap = DirSnapshot::default();
        assert!(snap.files.is_empty());
        assert!(snap.get("anything").is_none());
    }
}
