//! The OBSERVATION EPOCH — the last instant the liveness poll could actually
//! SEE the process table (plan
//! `2026-10-28-force-exited-runner-strands-open-sessions-out-of-the-restore-set`,
//! Phase 2).
//!
//! ## Why this exists
//!
//! Restore admission keeps an `open` row only when it was seen alive within
//! [`RESTORABLE_OPEN_ANCHOR_GRACE_MS`](crate::session::session_lifecycle_store)
//! of an anchor. A row's `last_seen_at` only advances when the poll TOUCHES it,
//! and the poll only touches a row on a tick whose process snapshot succeeded
//! (a failed snapshot makes `classify` return `Skip` for every matched row). So
//! a stretch of failed snapshots freezes every row's stamp while later stamps —
//! a spawn, the clean-shutdown marker — keep moving the anchor. On 2026-10-27
//! that stranded 64 of 65 open sessions: no touch landed for 77 minutes before
//! the marker, so every row read as 38+ minutes stale.
//!
//! A row is stale only if the runner LOOKED, could see, and did not find it
//! alive. This module records when it last could see: `observed_ok_at`, the
//! capture instant of the last poll tick whose snapshot succeeded AND whose
//! per-record classify pass ran to completion (so every live row it saw was
//! touched no earlier than this instant). Admission then measures an open row
//! against `min(anchor, observed_ok_at)` — an unobservable stretch is UNKNOWN,
//! never evidence of death.
//!
//! ## Two values, two processes
//!
//! - [`ObservationEpoch::prior_observed`] — the last epoch any EARLIER process
//!   of this instance wrote, read from disk once at [`ObservationEpoch::open`],
//!   BEFORE this process writes. Normally that is the prior process's; a prior
//!   process that never completed a successful tick leaves an older one in
//!   place, and then every row it wrote postdates that epoch and is admitted
//!   (the never-looked rule), bounded by the `OPEN_STALE_MS` prune. The value
//!   never moves afterwards, so nothing this process does (a restore's
//!   re-asserts, its own successful ticks) can move the window the prior
//!   cohort is judged against. The record's `bootMs` names the writer, and
//!   [`ObservationEpoch::open`] logs it.
//! - [`ObservationEpoch::current_observed`] — THIS process's last successful
//!   tick, in memory. It anchors rows this process wrote: if this process never
//!   observed successfully, nothing it wrote is stale.
//!
//! ## Storage
//!
//! `observation-epoch.json` (`{observedOkAt, bootMs}`) beside the shutdown
//! marker, INSTANCE-scoped through [`crate::instance::scope_path`] exactly as
//! [`crate::session::shutdown_marker::marker_path`] is, so a temp runner never
//! reads the primary's epoch. Atomic temp-file + rename. Absent or corrupt
//! reads as `None` — the first boot after the upgrade anchors open rows to the
//! prior process's stamps alone, and says so once.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// The sidecar's file name (the directory is instance-scoped).
pub const EPOCH_FILE_NAME: &str = "observation-epoch.json";

/// Logged once at boot when there is no prior epoch to anchor against.
pub const NO_EPOCH_NOTICE: &str = "restore admission: no observation epoch — anchoring open rows to the prior process's stamps (plus the clean marker when its teardown finished)";

/// The persisted epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpochRecord {
    /// Unix millis — the capture instant of the last fully-classified poll
    /// tick whose process snapshot succeeded.
    pub observed_ok_at: i64,
    /// Unix millis at which the process that wrote it booted (provenance).
    pub boot_ms: i64,
}

/// The two values restore admission reads (see
/// `session_lifecycle_store::RestoreAnchors`). `Default` = no epoch at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Observed {
    /// [`ObservationEpoch::prior_observed`].
    pub prior: Option<i64>,
    /// [`ObservationEpoch::current_observed`].
    pub current: Option<i64>,
}

/// Resolve this instance's epoch path — the same directory as the shutdown
/// marker.
pub fn epoch_path() -> PathBuf {
    let runner_dir = qontinui_runner_lib::ambient::runner_dir_or_cwd();
    crate::instance::scope_path(&runner_dir).join(EPOCH_FILE_NAME)
}

/// One process's view of the observation epoch. Open it once at boot.
#[derive(Debug)]
pub struct ObservationEpoch {
    path: PathBuf,
    boot_ms: i64,
    /// Latched at [`Self::open`], before this process's first write.
    prior: Option<EpochRecord>,
    /// This process's last successful, fully-classified tick.
    current: Mutex<Option<i64>>,
}

impl ObservationEpoch {
    /// Latch the prior process's epoch from `path` and log once when there is
    /// none. `boot_ms` is THIS process's boot instant, stamped into every
    /// write.
    pub fn open(path: &Path, boot_ms: i64) -> Self {
        let epoch = Self {
            path: path.to_path_buf(),
            boot_ms,
            prior: read_epoch(path),
            current: Mutex::new(None),
        };
        match epoch.prior {
            None => info!(path = %path.display(), "{NO_EPOCH_NOTICE}"),
            Some(prior) => info!(
                path = %path.display(),
                observed_ok_at = prior.observed_ok_at,
                writer_boot_ms = prior.boot_ms,
                "restore admission: latched the prior observation epoch"
            ),
        }
        epoch
    }

    /// The line [`Self::open`] logs at boot, if any.
    pub fn boot_notice(&self) -> Option<&'static str> {
        self.prior.is_none().then_some(NO_EPOCH_NOTICE)
    }

    /// The PRIOR process's last observation instant (never this process's).
    pub fn prior_observed(&self) -> Option<i64> {
        self.prior.map(|p| p.observed_ok_at)
    }

    /// THIS process's last successful, fully-classified tick.
    pub fn current_observed(&self) -> Option<i64> {
        self.current
            .lock()
            .map(|c| *c)
            .unwrap_or_else(|e| *e.into_inner())
    }

    /// Both admission inputs at once.
    pub fn observed(&self) -> Observed {
        Observed {
            prior: self.prior_observed(),
            current: self.current_observed(),
        }
    }

    /// The liveness poll's one call, made once per tick AFTER the per-record
    /// classify loop has finished: advance the epoch to the snapshot's
    /// `captured_at_ms` iff that snapshot succeeded. A failed snapshot makes
    /// classify `Skip` every matched row, so nothing was touched and nothing
    /// was observed — the epoch must not move. Returns whether it advanced.
    pub fn record_tick(&self, snapshot_ok: bool, captured_at_ms: i64) -> bool {
        if !snapshot_ok {
            return false;
        }
        self.record_ok(captured_at_ms);
        true
    }

    /// Record a successful observation at `at` — in memory for this process,
    /// and on disk for the next one. A write failure is logged and leaves the
    /// prior file in place (the next boot then anchors to an older epoch,
    /// which only admits more).
    pub fn record_ok(&self, at: i64) {
        match self.current.lock() {
            Ok(mut c) => *c = Some(at),
            Err(e) => *e.into_inner() = Some(at),
        }
        let rec = EpochRecord {
            observed_ok_at: at,
            boot_ms: self.boot_ms,
        };
        if let Err(e) = write_atomic(&self.path, &rec) {
            warn!(
                error = %e,
                path = %self.path.display(),
                "observation_epoch: write failed — epoch not persisted"
            );
        }
    }
}

fn read_epoch(path: &Path) -> Option<EpochRecord> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice::<EpochRecord>(&bytes) {
        Ok(r) => Some(r),
        Err(e) => {
            warn!(
                error = %e,
                path = %path.display(),
                "observation_epoch: corrupt epoch — treating as absent"
            );
            None
        }
    }
}

fn write_atomic(path: &Path, rec: &EpochRecord) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(rec).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const T: i64 = 1_700_000_000_000;

    #[test]
    fn absent_epoch_is_none_and_says_so_once() {
        let dir = tempdir().unwrap();
        let e = ObservationEpoch::open(&dir.path().join(EPOCH_FILE_NAME), T);
        assert_eq!(e.prior_observed(), None);
        assert_eq!(e.current_observed(), None);
        assert_eq!(e.boot_notice(), Some(NO_EPOCH_NOTICE));
    }

    #[test]
    fn corrupt_epoch_is_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(EPOCH_FILE_NAME);
        std::fs::write(&path, b"{not json").unwrap();
        let e = ObservationEpoch::open(&path, T);
        assert_eq!(e.prior_observed(), None);
        assert_eq!(e.boot_notice(), Some(NO_EPOCH_NOTICE));
    }

    #[test]
    fn a_second_boot_reads_the_first_boots_epoch() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(EPOCH_FILE_NAME);
        let first = ObservationEpoch::open(&path, T);
        assert!(first.record_tick(true, T + 60_000));
        assert!(first.record_tick(true, T + 105_000));

        let second = ObservationEpoch::open(&path, T + 3_600_000);
        assert_eq!(second.prior_observed(), Some(T + 105_000));
        assert_eq!(second.boot_notice(), None);
        assert_eq!(second.current_observed(), None);
        let on_disk: EpochRecord = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            on_disk,
            EpochRecord {
                observed_ok_at: T + 105_000,
                boot_ms: T
            }
        );
    }

    /// This process's own ticks move `current_observed` only: the prior value
    /// was latched before its first write.
    #[test]
    fn this_process_epoch_does_not_feed_prior() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(EPOCH_FILE_NAME);
        ObservationEpoch::open(&path, T).record_ok(T + 1_000);

        let e = ObservationEpoch::open(&path, T + 3_600_000);
        e.record_ok(T + 5 * 3_600_000);
        assert_eq!(e.prior_observed(), Some(T + 1_000));
        assert_eq!(e.current_observed(), Some(T + 5 * 3_600_000));
    }

    /// Mutation (b): a failed snapshot observed nothing — neither the
    /// in-memory nor the on-disk epoch may move.
    #[test]
    fn epoch_is_not_advanced_by_a_failed_snapshot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(EPOCH_FILE_NAME);
        let e = ObservationEpoch::open(&path, T);
        assert!(e.record_tick(true, T + 45_000));
        assert!(!e.record_tick(false, T + 90_000));
        assert_eq!(e.current_observed(), Some(T + 45_000));
        assert_eq!(
            ObservationEpoch::open(&path, T + 1).prior_observed(),
            Some(T + 45_000)
        );
    }

    /// Mutation (d): the poll must advance the epoch only AFTER its per-record
    /// classify loop — the loop is where the KeepAlive touches land, so an
    /// epoch stamped before it (a tick that stalls between snapshot and
    /// touches) would claim an observation no row reflects and recreate the
    /// strand. The poll loop lives in `main.rs` (an async closure over the
    /// whole runtime), so this pins the ORDER at the call site: exactly one
    /// `record_tick` call, OUTSIDE the per-record classify loop (after the
    /// brace that closes `for rec in &open`, so no arm can host it) and before
    /// the tick's held-records log that follows the loop.
    #[test]
    fn epoch_not_advanced_by_a_tick_that_never_classified() {
        let main_rs = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("main.rs"),
        )
        .unwrap();
        let calls: Vec<usize> = main_rs
            .match_indices(".record_tick(")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            calls.len(),
            1,
            "exactly one record_tick call site in main.rs"
        );
        let touch = main_rs
            .find("poll_lifecycle_store.touch(&rec.claude_session_id)")
            .expect("the poll's KeepAlive touch");
        let header = main_rs[..touch]
            .rfind("for rec in &open {")
            .expect("the classify loop that hosts the touch");
        let loop_end = closing_brace(&main_rs, header + "for rec in &open ".len());
        assert!(touch < loop_end, "the touch sits inside the classify loop");
        let held_log = main_rs
            .find("if held_for_restore > 0 {")
            .expect("the tick's held-records log after the loop");
        assert!(
            calls[0] > loop_end && calls[0] < held_log,
            "record_tick must run once per tick, after the per-record classify loop"
        );
    }

    /// Byte offset of the `}` matching the `{` at `open`, skipping braces in
    /// string literals and `//` comments.
    fn closing_brace(src: &str, open: usize) -> usize {
        let bytes = src.as_bytes();
        assert_eq!(
            bytes[open], b'{',
            "the offset must point at an opening brace"
        );
        let (mut depth, mut i) = (0_i32, open);
        let (mut in_str, mut in_comment) = (false, false);
        while i < bytes.len() {
            let b = bytes[i];
            if in_comment {
                in_comment = b != b'\n';
            } else if in_str {
                match b {
                    b'\\' => i += 1,
                    b'"' => in_str = false,
                    _ => {}
                }
            } else if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
                in_comment = true;
            } else if b == b'"' {
                in_str = true;
            } else if b == b'{' {
                depth += 1;
            } else if b == b'}' {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            i += 1;
        }
        panic!("unbalanced braces after offset {open}");
    }
}
