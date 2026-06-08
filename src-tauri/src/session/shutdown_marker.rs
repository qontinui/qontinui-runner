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
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<ShutdownMarker>(&bytes) {
            Ok(m) => !m.clean,
            Err(e) => {
                warn!(
                    error = %e,
                    path = %path.display(),
                    "shutdown_marker: corrupt marker — treating as unclean (crash recovery)"
                );
                true
            }
        },
        // Absent (first ever boot, or the file was never written) — treat as
        // unclean is the SAFE default, but the very first boot legitimately
        // has no marker. We still report crash-recovery=true; the recovery
        // summary only surfaces a banner when there are sessions to resume,
        // so a clean first boot with zero sessions shows nothing.
        Err(_) => true,
    }
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

    #[test]
    fn marker_path_namespaces_by_port() {
        let primary = marker_path(9876);
        let temp = marker_path(9877);
        assert!(primary.ends_with("last-shutdown.json"));
        assert!(temp.ends_with("last-shutdown-9877.json"));
        assert_ne!(primary, temp);
    }
}
