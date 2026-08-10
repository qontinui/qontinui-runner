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
//! `~/.qontinui/runner/last-shutdown.json` (object `{clean, at}`),
//! INSTANCE-scoped exactly like the lifecycle store
//! ([`crate::session::session_lifecycle_store::store_path`]) so a secondary
//! does not read/write the primary's marker. Atomic temp-file + rename. A
//! missing / corrupt file is treated as "not clean" (the safe default — we
//! would rather over-report a crash banner than silently hide a real crash).

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

/// Resolve the marker path for this runner instance — INSTANCE-scoped via
/// [`crate::instance::scope_path`], the same helper (and the same fail-closed
/// `instance-unnamed-<port>` quarantine) the lifecycle store resolves through:
///
/// - Primary → the legacy UNSCOPED `~/.qontinui/runner/last-shutdown.json`,
///   byte-for-byte the pre-fix path, so its boot classification is unchanged.
/// - Secondary → `~/.qontinui/runner/instance-<name>/last-shutdown.json`.
///
/// This replaces API-PORT keying (`last-shutdown-<port>.json`). Temp-runner
/// ports 9877-9899 are RECYCLED across unrelated spawns, so a fresh runner on a
/// reused port read the PREVIOUS occupant's marker — and the marker is not
/// cosmetic: [`classify_boot`] turns it into [`BootClassification`], whose
/// `prior_marker_at` is `terminal_session_list_open`'s cohort anchor and whose
/// `crash_recovery` becomes its `boot_was_clean`. A foreign runner's last
/// moment of life therefore decided which of THIS runner's records were
/// restorable. Instance identity is unique per spawn and stable per named
/// runner, so no such inheritance is possible. Mirrors
/// [`crate::session::session_lifecycle_store::store_path`]: scope the
/// DIRECTORY, keep the filename plain.
///
/// ## One-time transition cost (2026-08-10)
///
/// The PRIMARY is unaffected — its path is byte-identical before and after, so
/// its boot classification carries over untouched.
///
/// An already-running NAMED secondary's marker moves from
/// `~/.qontinui/runner/last-shutdown-<port>.json` to
/// `~/.qontinui/runner/instance-<name>/last-shutdown.json`. Its first boot after
/// this change therefore finds NO marker, and [`classify_prior_marker`] maps an
/// absent marker to `crash_recovery: true` / `prior_marker_at: None`. So a named
/// secondary that shut down CLEANLY right before the upgrade gets ONE spurious
/// crash classification. That is the safe direction by this module's design
/// (over-report a crash rather than hide one) and it self-heals on the next
/// clean shutdown, but it is a real one-off, recorded here rather than left for
/// someone to rediscover from a confusing banner.
///
/// ## Orphaned files
///
/// The old `last-shutdown-<port>.json` files are now unreferenced, as are the
/// `session-snapshots-<port>.jsonl` files the same plan's Phase 4 stopped
/// reading. Nothing collects either — this extends the orphaned-`-<port>`-file
/// janitor that `2026-07-20-runner-port-keyed-state-inheritance` §7 already
/// deferred, from one file class to three. They are small, inert, and confined
/// to `~/.qontinui/runner/`; no code path can resurrect them.
pub fn marker_path() -> PathBuf {
    let runner_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".qontinui")
        .join("runner");
    crate::instance::scope_path(&runner_dir).join("last-shutdown.json")
}

/// Read the prior marker from disk. `None` when absent, unreadable, or
/// corrupt (a corrupt marker is logged).
fn read_marker(path: &Path) -> Option<ShutdownMarker> {
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
        // Absent (first ever boot, or the file was never written) — treat as
        // unclean is the SAFE default, but the very first boot legitimately
        // has no marker. We still report crash-recovery=true; the recovery
        // summary only surfaces a banner when there are sessions to resume,
        // so a clean first boot with zero sessions shows nothing.
        Err(_) => None,
    }
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
    classify_prior_marker(read_marker(path).as_ref()).crash_recovery
}

/// The one-shot classification of THIS boot, derived from the PRIOR
/// process's shutdown marker. Captured exactly once per process by
/// [`classify_boot`] — a later marker file read is USELESS for
/// classification because [`mark_running`] immediately rewrites the marker
/// `clean:false` for the now-running process (any command-time read would
/// permanently classify "crash").
#[derive(Debug, Clone, Copy)]
pub struct BootClassification {
    /// `true` iff the previous shutdown was NOT clean (marker absent,
    /// corrupt, or `clean:false`) ⇒ this boot is crash recovery.
    pub crash_recovery: bool,
    /// `at` of the PRIOR marker, if one was readable. On a clean shutdown
    /// this is the shutdown instant; on a crash it is the crashed process's
    /// boot instant. Used as one input to the restore path's anchored
    /// recency rule (the registry's "last moment of life").
    pub prior_marker_at: Option<i64>,
}

/// Pure classification core for [`classify_boot`] (testable without the
/// process-wide [`OnceLock`]).
fn classify_prior_marker(prior: Option<&ShutdownMarker>) -> BootClassification {
    BootClassification {
        crash_recovery: prior.map(|m| !m.clean).unwrap_or(true),
        prior_marker_at: prior.map(|m| m.at),
    }
}

static BOOT_CLASSIFICATION: OnceLock<BootClassification> = OnceLock::new();

/// Classify this boot EXACTLY ONCE: read the prior marker, stash the
/// classification process-wide, and immediately (re)write the marker
/// `clean:false` for the now-running process. Idempotent — every call after
/// the first returns the stashed classification without touching disk, so
/// call-site ordering (synchronous boot in `main.rs` setup vs the delayed
/// AI-session resume spawn) cannot double-classify or double-overwrite.
pub fn classify_boot(path: &Path) -> BootClassification {
    *BOOT_CLASSIFICATION.get_or_init(|| {
        let classification = classify_prior_marker(read_marker(path).as_ref());
        mark_running(path);
        classification
    })
}

/// The stashed boot classification, if [`classify_boot`] has run. Consumers
/// that must never trigger a marker write (e.g. `terminal_session_list_open`)
/// read this instead of calling [`classify_boot`].
pub fn boot_classification() -> Option<BootClassification> {
    BOOT_CLASSIFICATION.get().copied()
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
    fn classify_prior_marker_covers_all_marker_shapes() {
        // Absent marker → crash recovery, no anchor.
        let absent = classify_prior_marker(None);
        assert!(absent.crash_recovery);
        assert!(absent.prior_marker_at.is_none());

        // Clean marker → planned restart, anchor = shutdown instant.
        let clean = classify_prior_marker(Some(&ShutdownMarker {
            clean: true,
            at: 1_000,
        }));
        assert!(!clean.crash_recovery);
        assert_eq!(clean.prior_marker_at, Some(1_000));

        // Running (clean:false) marker left by a crash → crash recovery, but
        // the prior `at` (the crashed process's boot instant) still anchors.
        let crashed = classify_prior_marker(Some(&ShutdownMarker {
            clean: false,
            at: 2_000,
        }));
        assert!(crashed.crash_recovery);
        assert_eq!(crashed.prior_marker_at, Some(2_000));
    }

    /// Replaces `marker_path_namespaces_by_port`, which pinned the OLD
    /// API-PORT keying (`last-shutdown.json` for 9876, `last-shutdown-<port>.json`
    /// otherwise). That contract was the defect: temp-runner ports 9877-9899 are
    /// RECYCLED, so two unrelated temp runners shared one marker and the second
    /// inherited the first's boot classification. The contract is now INSTANCE
    /// scoping — see [`marker_path`] and the sibling regression test
    /// `instance::tests::shutdown_marker_path_cannot_be_inherited_across_instances`,
    /// which additionally pins the primary's unscoped path through the pure
    /// `resolve_data_subdir` core.
    ///
    /// Only `QONTINUI_INSTANCE_NAME` is set here: `resolve_data_subdir` answers
    /// on its FIRST branch from the name alone, so this cannot flake on the
    /// `QONTINUI_PORT` that `scheduler_service`'s tests mutate concurrently.
    #[test]
    fn marker_path_is_instance_scoped_not_port_keyed() {
        let _env = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&["QONTINUI_INSTANCE_NAME"]);

        std::env::set_var("QONTINUI_INSTANCE_NAME", "test-19f6faa3bf8-0");
        let first = marker_path();
        std::env::set_var("QONTINUI_INSTANCE_NAME", "test-19f6fd50c26-2");
        let recycled = marker_path();

        assert_ne!(
            first, recycled,
            "two spawns on the same recycled port must not share one marker"
        );
        for path in [&first, &recycled] {
            // The FILENAME stays plain — the instance lives in the DIRECTORY,
            // exactly like `session_lifecycle_store::store_path`.
            assert!(
                path.ends_with("last-shutdown.json"),
                "filename must stay plain, got {}",
                path.display()
            );
            let shown = path.to_string_lossy().replace('\\', "/");
            assert!(
                shown.contains("/instance-test-19f6f"),
                "a secondary's marker must sit under its instance dir, got {shown}"
            );
            assert!(
                !shown.contains("last-shutdown-"),
                "the port suffix must be gone, got {shown}"
            );
        }
    }
}
