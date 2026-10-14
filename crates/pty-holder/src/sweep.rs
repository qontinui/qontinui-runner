//! The boot sweep: what a starting runner does with the panes its pane
//! directory already holds.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 3 (D8, as amended under D13). There is no separate orphan ledger: **the
//! ledger is the per-pane lock directory**, and every pane a holder ever served
//! has a `<pane-id>.lock` there whose record names the holder, its child and
//! the child's start token (`crate::lock::LockRecord`). The sweep is one walk
//! of it — the same walk as the census (`crate::client::census`), so a pane is
//! judged by the same two observables (an answered handshake, then the lock):
//!
//! | census verdict | sweep outcome | what the sweep does |
//! |---|---|---|
//! | healthy | [`SweepOutcome::Live`] | nothing — the CALLER adopts the pane |
//! | no common version | [`SweepOutcome::Incompatible`] | nothing: listed, never killed (D15) |
//! | lock held, handshake unanswered | [`SweepOutcome::Unknown`] | nothing: counted, never touched |
//! | lock free (holder dead) | [`SweepOutcome::Reaped`] | under that lock: reap what the record PROVES is the pane's tree (`crate::reap`), remove the endpoint/spec/report, then the lock file |
//!
//! Then the files no lock accounts for: an endpoint, spec or report whose pane
//! has no lock file and that is older than [`STRAY_MIN_AGE`] (a spawn in
//! flight writes its spec before its holder creates the lock, so a young one
//! is left alone), and a dead pane's diagnostic log once it is older than
//! [`DEAD_LOG_RETENTION`] — a log outlives its pane on purpose, since it is
//! the only account of why a holder died.
//!
//! **This is not a Windows stub** (the dinotty defect D8 names). Every step
//! above runs on Windows: the lock is `LockFileEx`, the record carries the
//! child's creation time, the reaper verifies and terminates through process
//! handles (`crate::reap`), and the lock file's removal is made safe by delete
//! sharing (`crate::lock`).
//!
//! Panes the caller is spawning right now go in `skip`, exactly as for the
//! census: probing one can steal its lock from the starting holder.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::client::{census, Probe};
use crate::lock::{read_record, LockRecord, PaneLock, TryLock};
use crate::pane::{
    check_private_dir, lock_path, report_path, report_tmp_path, socket_path, spec_path, PaneId,
};
use crate::protocol::HelloAck;
use crate::reap::{reap_orphans_of_dead_holder, ReapReport};

/// An endpoint, spec or report file with no lock file beside it is removed
/// once it is older than this. Far beyond the spawner's ready-line bound
/// (`crate::spawn::DEFAULT_REPORT_TIMEOUT`), so a spawn in flight is never
/// swept out from under its holder.
pub const STRAY_MIN_AGE: Duration = Duration::from_secs(10 * 60);

/// How many times a dead pane's lock, found held again, is re-tried.
const RETAKE_TRIES: usize = 5;
/// The pause between those tries.
const RETAKE_PAUSE: Duration = Duration::from_millis(20);

/// A pane's diagnostic log is kept this long after its lock is gone.
pub const DEAD_LOG_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// What the sweep found (and did) for one lock file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepOutcome {
    /// A healthy holder that answered the handshake: the caller adopts it.
    Live {
        hello_ack: HelloAck,
        record: Option<LockRecord>,
    },
    /// A live holder that speaks none of our protocol versions. Left
    /// running; the caller lists it.
    Incompatible { holder_versions: Vec<u32> },
    /// Lock held, no answer (or the pane could not be judged). Untouched.
    Unknown {
        reason: String,
        record: Option<LockRecord>,
    },
    /// The holder was dead. Its proven tree remainder was reaped and its
    /// files removed under its lock.
    Reaped {
        record: Option<LockRecord>,
        reap: ReapReport,
        /// `false` only on Windows, when another process had the lock file
        /// open at the instant of removal (it is left for the next sweep).
        lock_removed: bool,
    },
}

/// One swept lock file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepEntry {
    /// The lock file's stem (not necessarily a valid pane id — a foreign file
    /// is reported as `Unknown`, never skipped).
    pub pane_id: String,
    pub outcome: SweepOutcome,
}

/// Everything one sweep did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// One per lock file, sorted by pane id.
    pub entries: Vec<SweepEntry>,
    /// Files removed because no lock accounted for them.
    pub strays_removed: Vec<PathBuf>,
    /// Removals that failed, one line each. They do not stop the sweep.
    pub errors: Vec<String>,
}

impl SweepReport {
    /// The panes the caller should adopt.
    pub fn live(&self) -> impl Iterator<Item = (&str, &HelloAck)> {
        self.entries.iter().filter_map(|e| match &e.outcome {
            SweepOutcome::Live { hello_ack, .. } => Some((e.pane_id.as_str(), hello_ack)),
            _ => None,
        })
    }
}

/// Sweep `pane_dir`: classify every pane under ONE deadline `timeout` from now,
/// reap the dead, remove stray files. A missing pane directory is an empty
/// report; one that is unreadable or (Unix) not private is an error — the
/// sweep never acts on a directory another user could have planted files in.
pub fn boot_sweep(pane_dir: &Path, timeout: Duration, skip: &[PaneId]) -> io::Result<SweepReport> {
    match check_private_dir(pane_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(SweepReport::default()),
        Err(e) => return Err(e),
    }
    let mut report = SweepReport::default();
    for row in census(pane_dir, timeout, skip)? {
        let outcome = match row.probe {
            Probe::Healthy { hello_ack } => SweepOutcome::Live {
                record: PaneId::new(&row.pane_id)
                    .ok()
                    .and_then(|p| read_record(&lock_path(pane_dir, &p))),
                hello_ack,
            },
            Probe::Incompatible { holder_versions } => {
                SweepOutcome::Incompatible { holder_versions }
            }
            Probe::Unknown { reason, record } => SweepOutcome::Unknown { reason, record },
            // A census walks lock files, so it never answers Absent; a lock
            // file that vanished mid-walk was removed by someone else.
            Probe::Absent => continue,
            Probe::Dead { .. } => match PaneId::new(&row.pane_id) {
                Ok(pane) => reap_dead(pane_dir, &pane, &mut report.errors),
                Err(e) => SweepOutcome::Unknown {
                    reason: e.to_string(),
                    record: None,
                },
            },
        };
        report.entries.push(SweepEntry {
            pane_id: row.pane_id,
            outcome,
        });
    }
    remove_strays(pane_dir, SystemTime::now(), skip, &mut report);
    Ok(report)
}

/// Take a lock the census just found free. A child the caller forks between
/// a lock file's open and its exec shares that open file description — and
/// so the lock — until the exec; a lock seen held just after the census is
/// therefore retried briefly before it counts as a holder that started.
fn retake_lock(lock_file: &Path) -> io::Result<TryLock> {
    let mut retakes = 0;
    loop {
        match PaneLock::try_acquire_existing(lock_file) {
            Ok(TryLock::Held) if retakes < RETAKE_TRIES => {
                retakes += 1;
                std::thread::sleep(RETAKE_PAUSE);
            }
            other => return other,
        }
    }
}

/// A dead holder's pane: re-take its lock (anything else means it is no
/// longer dead), and under it reap, remove its files, remove the lock.
fn reap_dead(pane_dir: &Path, pane: &PaneId, errors: &mut Vec<String>) -> SweepOutcome {
    let lock_file = lock_path(pane_dir, pane);
    let lock = match retake_lock(&lock_file) {
        Ok(TryLock::Acquired(lock)) => lock,
        Ok(TryLock::Held) => {
            return SweepOutcome::Unknown {
                reason: "the lock was free at the census and is held now: a holder started".into(),
                record: read_record(&lock_file),
            }
        }
        Err(e) => {
            return SweepOutcome::Unknown {
                reason: format!("lock could not be re-taken: {e}"),
                record: read_record(&lock_file),
            }
        }
    };
    let record = read_record(&lock_file);
    let reap = match &record {
        Some(r) => reap_orphans_of_dead_holder(r),
        None => {
            let mut r = ReapReport::default();
            r.note("no lock record: nothing to verify a tree against, nothing signalled");
            r
        }
    };
    let mut files = vec![
        spec_path(pane_dir, pane),
        report_path(pane_dir, pane),
        report_tmp_path(pane_dir, pane),
    ];
    if cfg!(unix) {
        files.push(socket_path(pane_dir, pane));
    }
    for f in files {
        match std::fs::remove_file(&f) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => errors.push(format!("{}: {e}", f.display())),
        }
    }
    let lock_removed = match lock.remove() {
        Ok(removed) => removed,
        Err(e) => {
            errors.push(format!("{}: {e}", lock_file.display()));
            false
        }
    };
    SweepOutcome::Reaped {
        record,
        reap,
        lock_removed,
    }
}

/// The per-pane file kinds the stray pass may remove, and the age each must
/// reach first.
fn stray_age(extension: &str) -> Option<Duration> {
    match extension {
        "sock" | "spec" | "report" | "report-tmp" => Some(STRAY_MIN_AGE),
        "log" => Some(DEAD_LOG_RETENTION),
        _ => None,
    }
}

/// Remove per-pane files whose pane has no lock file, once old enough.
/// Symlinks are never followed (`symlink_metadata`, and `remove_file` removes
/// a link, not its target); a pane in `skip` is never touched.
fn remove_strays(pane_dir: &Path, now: SystemTime, skip: &[PaneId], report: &mut SweepReport) {
    let Ok(entries) = std::fs::read_dir(pane_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let (Some(stem), Some(ext)) = (
            path.file_stem().and_then(|s| s.to_str()),
            path.extension().and_then(|s| s.to_str()),
        ) else {
            continue;
        };
        let (Some(min_age), Ok(pane)) = (stray_age(ext), PaneId::new(stem)) else {
            continue;
        };
        if skip.contains(&pane) || lock_path(pane_dir, &pane).exists() {
            continue;
        }
        let Ok(md) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if md.is_dir() {
            continue;
        }
        let old_enough = md
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= min_age);
        if !old_enough {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => report.strays_removed.push(path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => report.errors.push(format!("{}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let d = std::env::temp_dir().join(format!("ptysw-{tag}-{}-{nanos:x}", std::process::id()));
        crate::pane::prepare_private_dir(&d).unwrap();
        d
    }

    fn age(path: &Path, by: Duration) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - by).unwrap();
    }

    #[test]
    fn pty_holder_boot_sweep_missing_dir_is_an_empty_report() {
        let d = std::env::temp_dir().join(format!("ptysw-none-{}", std::process::id()));
        assert_eq!(
            boot_sweep(&d, Duration::from_secs(1), &[]).unwrap(),
            SweepReport::default()
        );
    }

    /// A dead pane with no record: nothing is signalled, its files and its
    /// lock are removed.
    #[test]
    fn pty_holder_boot_sweep_dead_pane_without_record_is_cleaned_not_signalled() {
        let d = dir("dead");
        let pane = PaneId::new("gone").unwrap();
        std::fs::write(lock_path(&d, &pane), b"").unwrap();
        std::fs::write(spec_path(&d, &pane), b"{}").unwrap();
        let report = boot_sweep(&d, Duration::from_secs(5), &[]).unwrap();
        assert_eq!(report.entries.len(), 1, "{report:?}");
        match &report.entries[0].outcome {
            SweepOutcome::Reaped {
                record: None,
                reap,
                lock_removed: true,
            } => assert!(reap.killed.is_empty(), "{reap:?}"),
            other => panic!("{other:?}"),
        }
        assert!(!lock_path(&d, &pane).exists());
        assert!(!spec_path(&d, &pane).exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A held lock is a live process whatever else is true: never reaped,
    /// never cleaned, reported Unknown (no endpoint answers here).
    #[test]
    fn pty_holder_boot_sweep_never_touches_a_held_lock() {
        let d = dir("held");
        let pane = PaneId::new("busy").unwrap();
        let TryLock::Acquired(_held) = PaneLock::try_acquire(&lock_path(&d, &pane)).unwrap() else {
            panic!("fresh lock");
        };
        std::fs::write(spec_path(&d, &pane), b"{}").unwrap();
        age(&spec_path(&d, &pane), STRAY_MIN_AGE * 2);
        let report = boot_sweep(&d, Duration::from_secs(5), &[]).unwrap();
        assert!(
            matches!(report.entries[0].outcome, SweepOutcome::Unknown { .. }),
            "{report:?}"
        );
        assert!(lock_path(&d, &pane).exists());
        assert!(
            spec_path(&d, &pane).exists(),
            "a held pane's files are not strays"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Strays: only lockless, old-enough, per-pane files; skipped panes and
    /// young files survive; a log is kept far longer than an endpoint.
    #[test]
    fn pty_holder_boot_sweep_removes_only_old_lockless_strays() {
        let d = dir("stray");
        let old_spec = d.join("a.spec");
        let young_spec = d.join("b.spec");
        let skipped_spec = d.join("c.spec");
        let recent_log = d.join("d.log");
        let ancient_log = d.join("e.log");
        let foreign = d.join("notes.txt");
        for p in [
            &old_spec,
            &young_spec,
            &skipped_spec,
            &recent_log,
            &ancient_log,
            &foreign,
        ] {
            std::fs::write(p, b"x").unwrap();
        }
        age(&old_spec, STRAY_MIN_AGE * 2);
        age(&skipped_spec, STRAY_MIN_AGE * 2);
        age(&recent_log, STRAY_MIN_AGE * 2);
        age(&ancient_log, DEAD_LOG_RETENTION * 2);
        age(&foreign, DEAD_LOG_RETENTION * 2);
        let skip = [PaneId::new("c").unwrap()];
        let report = boot_sweep(&d, Duration::from_secs(5), &skip).unwrap();
        let mut removed = report.strays_removed.clone();
        removed.sort();
        assert_eq!(removed, vec![old_spec, ancient_log], "{report:?}");
        for kept in [young_spec, skipped_spec, recent_log, foreign] {
            assert!(kept.exists(), "{} was removed", kept.display());
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
