#![allow(dead_code)] // Module contains planned features not yet fully integrated

//! Discovery Push mechanism for qontinui-runner.
//!
//! This module enables the runner to detect patterns from automation runs
//! and push them to qontinui-web for review. Discoveries help improve
//! state structures by identifying:
//!
//! - **New elements**: UI elements consistently detected across runs
//! - **New transitions**: State transitions not in the original config
//! - **Timing updates**: Stable timing patterns for transitions
//! - **Flaky detection**: Transitions or templates with unreliable behavior
//! - **Unexpected elements**: Elements appearing that weren't expected
//!
//! # Architecture
//!
//! ```text
//! Run completed
//!     ↓
//! detector.rs analyzes run + statistics
//!     ↓
//! Discoveries queued in storage.rs (SQLite)
//!     ↓
//! sync.rs pushes to qontinui-web API
//!     ↓
//! User reviews in web dashboard
//! ```
//!
//! # Offline Support
//!
//! Discoveries are stored locally in SQLite and synced when connectivity
//! is available. Failed syncs are retried with exponential backoff.

pub mod detector;
pub mod storage;
pub mod sync;
pub mod types;

// Re-export main types for convenience
pub use detector::{analyze_run_for_discoveries, DetectionContext, DetectionThresholds};
pub use storage::{
    cleanup_failed_discoveries, delete_discovery, get_pending_count, get_pending_discoveries,
    queue_discovery,
};
pub use sync::{
    apply_sync_results, extract_discoveries_for_sync, get_sync_status, sync_discoveries_batch,
    SyncStatus,
};
pub use types::PendingDiscovery;
