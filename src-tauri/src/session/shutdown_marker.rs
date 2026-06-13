//! Clean-vs-crash shutdown marker (Phase 4 of
//! `2026-06-06-runner-dev-loop-and-restart-resilience`).
//!
//! The runner cannot tell, at boot, whether the previous process exited
//! cleanly (a planned drain / `ExitRequested` seam) or died unexpectedly
//! (a crash, an OOM kill, a hard `taskkill /F`, a power loss). Phase 2's
//! drain flips an in-process `DRAINED` flag, but that flag does not survive
//! the process — the next boot has no memory of it.
//!
//! This module persists that signal to disk so the NEXT boot can classify
//! its own startup:
//!
//!   - On every boot we IMMEDIATELY (re)write the marker as `clean:false`
//!     ("a process is now running and has not yet shut down cleanly"). If
//!     THIS process then crashes, the marker stays `clean:false` and the
//!     following boot detects the crash.
//!   - On a clean drain (`drain::drain` completing) AND on the `main.rs`
//!     `ExitRequested` seam we flip the marker to `clean:true`.
//!   - At boot, BEFORE we overwrite it, we read the prior marker: a missing
//!     marker or `clean:false` ⇒ the previous shutdown was NOT clean ⇒ this
//!     boot is **crash recovery**.
//!
//! ## Storage
//!
//! A single JSON file co-located with the lifecycle store + pane store under
//! `~/.qontinui/runner/last-shutdown.json` (object `{clean, at}`), namespaced
//! by API port exactly like the lifecycle store so a temp runner (9877+) does
//! not read/write the primary's marker. Atomic temp-file + rename. A missing
//! / corrupt file is treated as "not clean" (the safe default — we would
//! rather over-report a crash banner than silently hide a real crash).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Persisted clean-shutdown marker. `at` is unix epoch millis of the last
/// write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownMarker {
    /// `true` iff the most recent write was a clean shutdown (drain / exit
    /// seam). A fresh boot writes `false`; a clean shutdown flips it `true`.
    pub clean: bool,
    /// Unix epoch millis when this marker was written.
    pub at: i64,
}

/// Resolve the marker path for this runner instance, namespaced by API port
/// (mirrors the lifecycle store: 9876 → base name, every other port →
/// `-<port>` suffix). Co-located under `~/.qontinui/runner/`.
pub fn marker_path(api_port: u16) -> PathBuf {
    let file_name = if api_port == 9876 {
        "last-shutdown.json".to_string()
    } else {
        format!("last-shutdown-{api_port}.json")
    };
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".qontinui")
        .join("runner")
        .join(file_name)
}

/// Read the prior marker and decide whether this boot is crash recovery.
///
/// Returns `true` (crash recovery) when the marker is absent, unreadable,
/// corrupt, or explicitly `clean:false`. Returns `false` only when a
/// well-formed marker says `clean:true`.
///
/// This must be called BEFORE [`mark_running`] overwrites the marker for the
/// current process.
pub fn was_unclean_shutdown(path: &Path) -> bool {
    match read_prior(path) {
        Some(m) => !m.clean,
        // Absent (first ever boot, or the file was never written) — treat as
        // unclean is the SAFE default, but the very first boot legitimately
        // has no marker. We still report crash-recovery=true; the recovery
        // summary only surfaces a banner when there are sessions to resume,
        // so a clean first boot with zero sessions shows nothing.
        None => true,
    }
}

/// Read the prior marker verbatim, if present and well-formed. Corrupt and
/// absent markers both read as `None` (and classify as unclean).
fn read_prior(path: &Path) -> Option<ShutdownMarker> {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<ShutdownMarker>(&bytes) {
            Ok(m) => Some(m),
            Err(e) => {
                warn!(
                    error = %e,
                    path = %path.display(),
                    "shutdown_marker: corrupt marker — treating as unclean (crash recovery)"
                );
                None
            }
        },
        Err(_) => None,
    }
}

/// The single boot-time shutdown-marker classification, captured exactly once
/// per process by [`classify_boot`].
#[derive(Debug, Clone, Copy)]
pub struct BootClassification {
    /// `true` iff the previous shutdown was NOT clean (marker absent, corrupt,
    /// or `clean:false`) — this boot is crash recovery.
    pub crash_recovery: bool,
    /// `at` of the PRIOR marker (before this boot overwrote it): the previous
    /// process's last recorded moment of life — its clean-shutdown time, or
    /// its own boot time if it crashed. `None` when the marker was absent or
    /// corrupt. Feeds the restore path's anchored-recency rule.
    pub prior_marker_at: Option<i64>,
}

static BOOT_CLASSIFICATION: OnceLock<BootClassification> = OnceLock::new();

/// Classify this boot from the on-disk marker, exactly once per process.
///
/// The marker is single-read by construction: the first call reads the prior
/// marker, immediately overwrites it `clean:false` for the NOW-running
/// process ([`mark_running`]), and stashes the result; every later call (any
/// consumer, any thread) returns the stashed classification without touching
/// the file. A command-time re-read would always see the `clean:false` we
/// just wrote and misclassify every boot as a crash.
///
/// Call this from a synchronous boot site (before the API server / frontend
/// can race it); consumers read the stash via this same function.
pub fn classify_boot(path: &Path) -> BootClassification {
    *BOOT_CLASSIFICATION.get_or_init(|| {
        let prior = read_prior(path);
        let classification = BootClassification {
            crash_recovery: prior.as_ref().map(|m| !m.clean).unwrap_or(true),
            prior_marker_at: prior.map(|m| m.at),
        };
        mark_running(path);
        classification
    })
}

/// Write the marker for the NOW-running process as `clean:false`. Call once,
/// early at boot, AFTER [`was_unclean_shutdown`] has read the prior value.
/// If this process later crashes, the `clean:false` marker is what the next
/// boot reads → crash recovery.
pub fn mark_running(path: &Path) {
    write_marker(path, false);
}

/// Flip the marker to `clean:true`. Call from the clean-shutdown paths (drain
/// completion + the `main.rs` exit seam). Idempotent.
pub fn mark_clean_shutdown(path: &Path) {
    write_marker(path, true);
}

/// Best-effort atomic write. Failures are logged, not propagated — a marker
/// we fail to persist simply degrades to "treated as unclean next boot",
/// which is the safe direction.
fn write_marker(path: &Path, clean: bool) {
    let marker = ShutdownMarker {
        clean,
        at: chrono::Utc::now().timestamp_millis(),
    };
    if let Err(e) = write_atomic(path, &marker) {
        warn!(
            error = %e,
            path = %path.display(),
            clean,
            "shutdown_marker: write failed — marker not persisted"
        );
    }
}

fn write_atomic(path: &Path, marker: &ShutdownMarker) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(marker).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn absent_marker_is_crash_recovery() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("last-shutdown.json");
        assert!(
            was_unclean_shutdown(&path),
            "a missing marker must classify as unclean (crash recovery)"
        );
    }

    #[test]
    fn clean_marker_is_planned_restart() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("last-shutdown.json");
        mark_clean_shutdown(&path);
        assert!(
            !was_unclean_shutdown(&path),
            "a clean:true marker must classify as a planned restart"
        );
    }

    #[test]
    fn running_marker_is_crash_recovery() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("last-shutdown.json");
        // Simulate a process that started (clean:false) and then died without
        // ever flipping the marker clean.
        mark_running(&path);
        assert!(
            was_unclean_shutdown(&path),
            "a clean:false (running) marker left behind by a crash must classify as unclean"
        );
    }

    #[test]
    fn corrupt_marker_is_crash_recovery() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("last-shutdown.json");
        std::fs::write(&path, b"{not valid json").unwrap();
        assert!(
            was_unclean_shutdown(&path),
            "a corrupt marker must classify as unclean (safe default)"
        );
    }

    #[test]
    fn lifecycle_round_trip_running_then_clean() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("last-shutdown.json");

        // Boot 1: nothing on disk → crash recovery (first boot).
        assert!(was_unclean_shutdown(&path));
        // Boot 1 marks itself running.
        mark_running(&path);
        // Boot 1 drains cleanly.
        mark_clean_shutdown(&path);

        // Boot 2: prior shutdown was clean → planned restart.
        assert!(!was_unclean_shutdown(&path));
        mark_running(&path);
        // Boot 2 CRASHES (no mark_clean_shutdown).

        // Boot 3: prior marker is still clean:false → crash recovery.
        assert!(was_unclean_shutdown(&path));
    }

    /// The single test allowed to touch the process-wide `BOOT_CLASSIFICATION`
    /// OnceLock — a second such test would read this one's stash.
    #[test]
    fn classify_boot_single_read_semantics() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("last-shutdown.json");
        mark_clean_shutdown(&path);

        let first = classify_boot(&path);
        assert!(
            !first.crash_recovery,
            "prior clean marker → planned restart"
        );
        assert!(first.prior_marker_at.is_some(), "prior at captured");

        // classify_boot immediately re-marked the file `clean:false` for the
        // now-running process — a naive re-READ would misclassify as crash...
        assert!(was_unclean_shutdown(&path));
        // ...but a second consumer gets the boot-time STASH, not the file.
        let second = classify_boot(&path);
        assert!(
            !second.crash_recovery,
            "stash survives the marker overwrite"
        );
        assert_eq!(second.prior_marker_at, first.prior_marker_at);
    }

    #[test]
    fn marker_path_namespaces_by_port() {
        let primary = marker_path(9876);
        let temp = marker_path(9877);
        assert!(primary.ends_with("last-shutdown.json"));
        assert!(temp.ends_with("last-shutdown-9877.json"));
        assert_ne!(primary, temp);
    }
}
