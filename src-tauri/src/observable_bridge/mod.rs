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
//! [`MemoryBridge`] is the reference implementation; subsequent
//! categories (e.g. `GitOpBridge`) reuse the trait without modification.

use anyhow::Result;
use async_trait::async_trait;
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

pub mod memory;
pub mod memory_client;

/// Process-wide singleton `MemoryBridge` set once at runner init by
/// `main.rs::setup`. Spawn-site call paths (`claude_session::runner`,
/// `claude_session::session`) reach it via [`global`] because they run
/// in the binary tree without access to Tauri `State`.
///
/// `None` if init failed (e.g., no profile, no reqwest builder) — every
/// caller short-circuits on that and the spawn proceeds without
/// federation.
static GLOBAL_BRIDGE: OnceCell<Arc<memory::MemoryBridge>> = OnceCell::new();

/// Install the process-wide `MemoryBridge`. Idempotent — subsequent
/// calls are silently ignored so accidental double-init (e.g., from
/// two Tauri `.setup` closures during testing) does not panic.
pub fn init_global(bridge: Arc<memory::MemoryBridge>) {
    let _ = GLOBAL_BRIDGE.set(bridge);
}

/// Read the process-wide `MemoryBridge`. Returns `None` until
/// [`init_global`] runs successfully — federation-aware call sites
/// must treat this as the "feature disabled this session" signal.
pub fn global() -> Option<&'static Arc<memory::MemoryBridge>> {
    GLOBAL_BRIDGE.get()
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
    pub session_id: Uuid,
    /// Snapshot of `memory_dir` taken immediately after `pull` completed.
    /// Used by `reconcile` to diff against final state at session end.
    /// `None` until `pull` runs.
    pub post_pull_snapshot: Option<DirSnapshot>,
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

/// Local-change event surfaced by a bridge's file watcher to `push`.
#[derive(Debug, Clone)]
pub enum ObservableChange {
    Upserted { name: String, content: String },
    Deleted { name: String },
}

/// Aggregate counts from a `reconcile` pass — useful for UI banner.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconcileReport {
    pub pushed: u32,
    pub pulled: u32,
    pub unchanged: u32,
    pub failed: u32,
}

/// The trait every federated observable category implements.
#[async_trait]
pub trait RunnerObservableBridge: Send + Sync {
    /// Stable category name — used as the `coord.<category>` table name
    /// and the dashboard route segment. Memories: `"memory"`. Git-ops
    /// (future): `"git_op"`.
    fn category(&self) -> &'static str;

    /// Spawn-time: pull the tenant pool from coord, materialize locally.
    /// Populates `session_ctx.post_pull_snapshot`.
    async fn pull(&self, session_ctx: &mut SessionContext) -> Result<()>;

    /// During-session: invoked on a debounced local change event.
    async fn push(
        &self,
        change: ObservableChange,
        session_ctx: &SessionContext,
    ) -> Result<()>;

    /// Session-end: re-snapshot, diff against `post_pull_snapshot`,
    /// push anything the watcher missed, return aggregate counts.
    async fn reconcile(&self, session_ctx: &SessionContext) -> Result<ReconcileReport>;
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
