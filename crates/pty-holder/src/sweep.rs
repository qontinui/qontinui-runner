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
//! A dead holder's leftovers that nothing proves are the pane's are not
//! signalled — and not forgotten either: its record moves to the ORPHAN
//! LEDGER (`<pane-id>.orphan`), which every later sweep re-checks with the
//! same rule, dropping an entry once nothing unattributed is left or once its
//! record is from an earlier boot.
//!
//! Then the files no lock accounts for: an endpoint, spec or report whose pane
//! has no lock file and that is older than [`STRAY_MIN_AGE`] (a spawn in
//! flight writes its spec before its holder creates the lock, so a young one
//! is left alone), and a dead pane's diagnostic log once it is older than
//! [`DEAD_LOG_RETENTION`] — a log outlives its pane on purpose, since it is
//! the only account of why a holder died. A dead pane's durable output tail
//! (`.scrollback`) is kept exactly as long, for a restorer; a stray
//! `.scrollback.tmp` goes after [`STRAY_MIN_AGE`].
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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::client::{census, Probe};
use crate::lock::{read_record, LockRecord, PaneLock, TryLock};
use crate::pane::{
    check_private_dir, lock_path, orphan_path, report_path, report_tmp_path, scrollback_tmp_path,
    socket_path, spec_path, PaneId,
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

/// The wall-clock bound on the whole reaping step of a sweep — every dead
/// pane's reap and every orphan-ledger re-check, run concurrently. One reap is
/// bounded by a scope stop ([`crate::spawn::SYSTEMCTL_DEADLINE`]), the lock
/// retakes and [`crate::reap::REAP_PASSES`] short passes; this is that with
/// margin. A reap that overruns it is reported as Unknown and finishes in the
/// background.
pub const REAP_BUDGET: Duration = Duration::from_secs(8);

/// The same bound for the orphan-ledger re-checks, which run after the dead
/// panes' reaps under a budget of their own: one re-check can stop a scope
/// unit and run the reap passes, as one reap can.
pub const ORPHAN_RECHECK_BUDGET: Duration = REAP_BUDGET;

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
        /// The reap left processes it could not attribute, so the record
        /// was kept in the orphan ledger (`<pane-id>.orphan`).
        orphan_kept: bool,
    },
}

/// One orphan-ledger entry the sweep re-checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanEntry {
    pub pane_id: String,
    pub record: Option<LockRecord>,
    pub reap: ReapReport,
    /// Still unattributed processes: the entry stays for the next sweep.
    pub kept: bool,
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
    /// Orphan-ledger entries re-checked this sweep.
    pub orphans: Vec<OrphanEntry>,
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
/// hand the classification — and how many DEAD panes are about to be reaped —
/// to `before_reaping` (the caller adopts the live panes there, so their
/// adoption never waits on reaping), then reap the dead, re-check the orphan
/// ledger, and remove stray files. A missing pane directory is an empty
/// report (and `before_reaping` still runs, with nothing); one that is
/// unreadable or (Unix) not private is an error — the sweep never acts on a
/// directory another user could have planted files in.
pub fn boot_sweep(
    pane_dir: &Path,
    timeout: Duration,
    skip: &[PaneId],
    before_reaping: impl FnOnce(&[SweepEntry], usize),
) -> io::Result<SweepReport> {
    // Reaps still running at their budgets finish on their own threads.
    boot_sweep_with_pending(pane_dir, timeout, skip, before_reaping).map(|(report, _)| report)
}

/// [`boot_sweep`], also returning the reaps still running at their budgets
/// ([`PendingReaps`]): what a caller waits on before it may say the dead are
/// reaped.
pub fn boot_sweep_with_pending(
    pane_dir: &Path,
    timeout: Duration,
    skip: &[PaneId],
    before_reaping: impl FnOnce(&[SweepEntry], usize),
) -> io::Result<(SweepReport, PendingReaps)> {
    match check_private_dir(pane_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            before_reaping(&[], 0);
            return Ok((SweepReport::default(), PendingReaps::default()));
        }
        Err(e) => return Err(e),
    }
    let mut report = SweepReport::default();
    let mut dead: Vec<String> = Vec::new();
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
            Probe::Dead { .. } => {
                dead.push(row.pane_id);
                continue;
            }
        };
        report.entries.push(SweepEntry {
            pane_id: row.pane_id,
            outcome,
        });
    }
    before_reaping(&report.entries, dead.len());
    // The reaping step — every dead pane, then every orphan-ledger entry —
    // runs CONCURRENTLY under one wall-clock budget (review round 3, R2): a
    // pane's reap can take a `systemctl` stop and several passes, and a
    // caller's own deadline (the webview's restore) is derived from this one.
    let deadline = Instant::now() + REAP_BUDGET;
    let jobs = dead
        .into_iter()
        .map(|pane_id| {
            let dir = pane_dir.to_path_buf();
            let id = pane_id.clone();
            let job: ReapJob<(SweepOutcome, Vec<String>)> =
                Box::new(move || match PaneId::new(&id) {
                    Ok(pane) => reap_dead(&dir, &pane),
                    Err(e) => (
                        SweepOutcome::Unknown {
                            reason: e.to_string(),
                            record: None,
                        },
                        Vec::new(),
                    ),
                });
            (pane_id, job)
        })
        .collect();
    let (done, pending, mut late) = run_until(jobs, deadline);
    for (pane_id, (outcome, errors)) in done {
        report.errors.extend(errors);
        report.entries.push(SweepEntry { pane_id, outcome });
    }
    for pane_id in pending {
        report.entries.push(SweepEntry {
            outcome: SweepOutcome::Unknown {
                reason: format!(
                    "dead holder: its reap was still running at the {}s reap budget",
                    REAP_BUDGET.as_secs()
                ),
                record: None,
            },
            pane_id,
        });
    }
    report.entries.sort_by(|a, b| a.pane_id.cmp(&b.pane_id));
    // The orphan re-checks get a budget of their own, not what the dead
    // panes' reaps left over (review round 4, L1).
    late.absorb(recheck_orphans(
        pane_dir,
        Instant::now() + ORPHAN_RECHECK_BUDGET,
        &mut report,
    ));
    remove_strays(pane_dir, SystemTime::now(), skip, &mut report);
    Ok((report, late))
}

/// One job of the reaping step, run on a thread of its own.
type ReapJob<T> = Box<dyn FnOnce() -> T + Send>;

/// An orphan-ledger re-check: the entry, and a removal error if any.
type OrphanJob = ReapJob<(OrphanEntry, Option<String>)>;

/// Reaps (and orphan re-checks) still running when their budget ran out.
/// They finish on their own threads; [`PendingReaps::wait`] blocks until they
/// have. A caller that must not act before every reap is done — the runner's
/// "settled" signal (review round 4, L1) — waits on it.
#[derive(Default)]
pub struct PendingReaps {
    waiters: Vec<Box<dyn FnOnce() + Send>>,
    count: usize,
}

impl PendingReaps {
    /// How many reaps are still running.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Block until every pending reap has finished. Each is bounded by its
    /// own steps (a `systemctl` stop's deadline, the reap passes).
    pub fn wait(self) {
        for w in self.waiters {
            w();
        }
    }

    fn absorb(&mut self, other: PendingReaps) {
        self.count += other.count;
        self.waiters.extend(other.waiters);
    }
}

impl std::fmt::Debug for PendingReaps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingReaps")
            .field("count", &self.count)
            .finish()
    }
}

/// Run `jobs` each on a thread of its own and collect what finishes by
/// `deadline`; the keys of the rest are returned as pending, with a
/// [`PendingReaps`] to wait for them. A job whose thread cannot start runs
/// INLINE, on the caller's thread (review round 4, L2): no pane is ever
/// dropped from the report.
fn run_until<T: Send + 'static>(
    jobs: Vec<(String, ReapJob<T>)>,
    deadline: Instant,
) -> (Vec<(String, T)>, Vec<String>, PendingReaps) {
    let (tx, rx) = std::sync::mpsc::channel::<(String, T)>();
    let mut pending: Vec<String> = Vec::new();
    let mut done = Vec::new();
    for (key, job) in jobs {
        let tx = tx.clone();
        let k = key.clone();
        // The job sits in a slot the thread takes it from, so a thread that
        // cannot start leaves it here, to run inline.
        let slot = Arc::new(Mutex::new(Some(job)));
        let thread_slot = Arc::clone(&slot);
        let spawned = std::thread::Builder::new()
            .name("pty-holder-reap".into())
            .spawn(move || {
                let job = thread_slot.lock().unwrap_or_else(|p| p.into_inner()).take();
                if let Some(job) = job {
                    let _ = tx.send((k, job()));
                }
            });
        if spawned.is_ok() {
            pending.push(key);
        } else if let Some(job) = slot.lock().unwrap_or_else(|p| p.into_inner()).take() {
            done.push((key, job()));
        }
    }
    drop(tx);
    while !pending.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok((key, value)) => {
                pending.retain(|p| p != &key);
                done.push((key, value));
            }
            Err(_) => break,
        }
    }
    let count = pending.len();
    let waiter: Box<dyn FnOnce() + Send> = Box::new(move || {
        for _ in 0..count {
            if rx.recv().is_err() {
                break;
            }
        }
    });
    let late = PendingReaps {
        waiters: if count == 0 { Vec::new() } else { vec![waiter] },
        count,
    };
    (done, pending, late)
}

/// Write `record` to the pane's orphan-ledger entry (`<pane-id>.orphan`,
/// 0600, never through a symlink).
fn write_orphan(pane_dir: &Path, pane: &PaneId, record: &LockRecord) -> io::Result<()> {
    use std::io::Write;
    let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(orphan_path(pane_dir, pane))?;
    f.write_all(&bytes)?;
    f.sync_data()
}

/// Re-check every orphan-ledger entry whose pane has no lock file: reap what
/// is now PROVABLY the pane's (the same rule as for a dead holder), and drop
/// the entry once nothing unattributed is left — or once its record is from
/// an earlier boot, after which none of the pane's processes can exist.
fn recheck_orphans(pane_dir: &Path, deadline: Instant, report: &mut SweepReport) -> PendingReaps {
    let Ok(entries) = std::fs::read_dir(pane_dir) else {
        return PendingReaps::default();
    };
    let mut jobs: Vec<(String, OrphanJob)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("orphan") {
            continue;
        }
        let Some(pane) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| PaneId::new(s).ok())
        else {
            continue;
        };
        if lock_path(pane_dir, &pane).exists() {
            continue;
        }
        let record = read_record(&path);
        // An entry is stale once it is from an earlier boot: by its token's
        // boot id; or, for a record with NO token, by the entry's own age —
        // it expires once it predates this boot (the host has restarted
        // since, so none of what it listed can still be running). Where the
        // boot time is unreadable (macOS) a token-less entry is kept.
        let written = entry.metadata().and_then(|m| m.modified()).ok();
        let stale = record
            .as_ref()
            .is_none_or(|r| match r.child_start.as_deref() {
                Some(t) => !crate::proc_identity::token_is_from_this_boot(t),
                None => matches!(
                    (written, crate::proc_identity::boot_time()),
                    (Some(w), Some(boot)) if w < boot
                ),
            });
        let key = pane.to_string();
        jobs.push((
            key.clone(),
            Box::new(move || {
                let reap = match (&record, stale) {
                    (Some(r), false) => reap_orphans_of_dead_holder(r),
                    _ => ReapReport::default(),
                };
                // An unreadable snapshot is UNKNOWN, not "no orphans": keep.
                let kept = !stale && (!reap.unattributed.is_empty() || reap.snapshot_unreadable);
                let mut error = None;
                if !kept {
                    match std::fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => error = Some(format!("{}: {e}", path.display())),
                    }
                }
                (
                    OrphanEntry {
                        pane_id: key,
                        record,
                        reap,
                        kept,
                    },
                    error,
                )
            }),
        ));
    }
    let (done, pending, late) = run_until(jobs, deadline);
    for (_, (orphan, error)) in done {
        report.errors.extend(error);
        report.orphans.push(orphan);
    }
    for pane_id in pending {
        report.errors.push(format!(
            "orphan-ledger entry {pane_id}: its re-check was still running at its budget"
        ));
    }
    late
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
fn reap_dead(pane_dir: &Path, pane: &PaneId) -> (SweepOutcome, Vec<String>) {
    let mut errors = Vec::new();
    let outcome = reap_dead_into(pane_dir, pane, &mut errors);
    (outcome, errors)
}

fn reap_dead_into(pane_dir: &Path, pane: &PaneId, errors: &mut Vec<String>) -> SweepOutcome {
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
        // A rewrite in flight when the holder died: garbage. The tail itself
        // stays for a restorer (`crate::scrollback`).
        scrollback_tmp_path(pane_dir, pane),
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
    // Processes nothing proves are the pane's are left alone — but not
    // forgotten: the record moves to the orphan ledger, where every later
    // sweep re-checks it, instead of vanishing with the lock.
    let orphan_kept = match &record {
        // An unreadable snapshot is UNKNOWN, not "nothing left": keep it too.
        Some(r) if !reap.unattributed.is_empty() || reap.snapshot_unreadable => {
            match write_orphan(pane_dir, pane, r) {
                Ok(()) => true,
                Err(e) => {
                    errors.push(format!("{}: {e}", orphan_path(pane_dir, pane).display()));
                    false
                }
            }
        }
        _ => false,
    };
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
        orphan_kept,
    }
}

/// The per-pane file kinds the stray pass may remove: the pane id a file name
/// belongs to, and the age the file must reach first.
fn stray_kind(file_name: &str) -> Option<(&str, Duration)> {
    if let Some(pane) = file_name.strip_suffix(".scrollback.tmp") {
        return Some((pane, STRAY_MIN_AGE));
    }
    // A successor's staged socket (`<pane-id>.sock.upg`, plan Phase 8) left
    // by one that died between its bind and its rename.
    if let Some(pane) = file_name.strip_suffix(".sock.upg") {
        return Some((pane, STRAY_MIN_AGE));
    }
    let (pane, extension) = file_name.rsplit_once('.')?;
    match extension {
        // `upgrade` / `upgrade-tmp`: a holder upgrade's snapshot (plan Phase
        // 8) left by a predecessor that died mid-handoff — it carries the
        // pane's ring, so it is not kept.
        "sock" | "spec" | "report" | "report-tmp" | "upgrade" | "upgrade-tmp" => {
            Some((pane, STRAY_MIN_AGE))
        }
        "log" | "scrollback" => Some((pane, DEAD_LOG_RETENTION)),
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
        let Some((stem, min_age)) = path
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(stray_kind)
        else {
            continue;
        };
        let Ok(pane) = PaneId::new(stem) else {
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

    /// Review round 4, L1/L2: a job past the deadline is reported pending,
    /// and its PendingReaps waits for it; a finished one is reported done.
    #[test]
    fn pty_holder_boot_sweep_reaps_past_the_budget_are_pending_until_done() {
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let f = Arc::clone(&finished);
        let jobs: Vec<(String, ReapJob<u8>)> = vec![
            ("quick".into(), Box::new(|| 1)),
            (
                "slow".into(),
                Box::new(move || {
                    std::thread::sleep(Duration::from_millis(300));
                    f.store(true, std::sync::atomic::Ordering::SeqCst);
                    2
                }),
            ),
        ];
        let (done, pending, late) = run_until(jobs, Instant::now() + Duration::from_millis(100));
        assert_eq!(done, vec![("quick".to_string(), 1)]);
        assert_eq!(pending, vec!["slow".to_string()]);
        assert_eq!(late.len(), 1);
        assert!(!finished.load(std::sync::atomic::Ordering::SeqCst));
        late.wait();
        assert!(finished.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// The reap budget covers one pane's worst case: a scope stop, the lock
    /// retakes, and the reap passes' pauses.
    #[test]
    fn pty_holder_boot_sweep_reap_budget_covers_one_reap() {
        let one = crate::spawn::SYSTEMCTL_DEADLINE
            + RETAKE_PAUSE * RETAKE_TRIES as u32
            + Duration::from_millis(20) * crate::reap::REAP_PASSES as u32;
        assert!(REAP_BUDGET > one, "{REAP_BUDGET:?} <= {one:?}");
    }

    #[test]
    fn pty_holder_boot_sweep_missing_dir_is_an_empty_report() {
        let d = std::env::temp_dir().join(format!("ptysw-none-{}", std::process::id()));
        assert_eq!(
            boot_sweep(&d, Duration::from_secs(1), &[], |_, _| {}).unwrap(),
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
        let tail = crate::pane::scrollback_path(&d, &pane);
        std::fs::write(&tail, b"tail").unwrap();
        std::fs::write(scrollback_tmp_path(&d, &pane), b"torn").unwrap();
        let report = boot_sweep(&d, Duration::from_secs(5), &[], |_, _| {}).unwrap();
        assert_eq!(report.entries.len(), 1, "{report:?}");
        assert!(tail.exists(), "a dead pane's tail is kept for a restorer");
        assert!(!scrollback_tmp_path(&d, &pane).exists());
        match &report.entries[0].outcome {
            SweepOutcome::Reaped {
                record: None,
                reap,
                lock_removed: true,
                orphan_kept: false,
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
        let report = boot_sweep(&d, Duration::from_secs(5), &[], |_, _| {}).unwrap();
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
        let recent_tail = d.join("f.scrollback");
        let ancient_tail = d.join("g.scrollback");
        let old_tmp = d.join("h.scrollback.tmp");
        for p in [
            &old_spec,
            &young_spec,
            &skipped_spec,
            &recent_log,
            &ancient_log,
            &foreign,
            &recent_tail,
            &ancient_tail,
            &old_tmp,
        ] {
            std::fs::write(p, b"x").unwrap();
        }
        age(&old_spec, STRAY_MIN_AGE * 2);
        age(&skipped_spec, STRAY_MIN_AGE * 2);
        age(&recent_log, STRAY_MIN_AGE * 2);
        age(&ancient_log, DEAD_LOG_RETENTION * 2);
        age(&foreign, DEAD_LOG_RETENTION * 2);
        age(&recent_tail, STRAY_MIN_AGE * 2);
        age(&ancient_tail, DEAD_LOG_RETENTION * 2);
        age(&old_tmp, STRAY_MIN_AGE * 2);
        let skip = [PaneId::new("c").unwrap()];
        let report = boot_sweep(&d, Duration::from_secs(5), &skip, |_, _| {}).unwrap();
        let mut removed = report.strays_removed.clone();
        removed.sort();
        assert_eq!(
            removed,
            vec![old_spec, ancient_log, ancient_tail, old_tmp],
            "{report:?}"
        );
        for kept in [young_spec, skipped_spec, recent_log, foreign, recent_tail] {
            assert!(kept.exists(), "{} was removed", kept.display());
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Plan Phase 8 (review r1, finding 2): a holder upgrade's leftovers — a
    /// snapshot, its temp, a successor's staged socket — are strays like any
    /// other once old and lockless; a live pane's are never touched.
    #[test]
    fn pty_holder_handoff_sweep_removes_stray_upgrade_leftovers() {
        let d = dir("upgstray");
        let strays = [
            d.join("a.upgrade"),
            d.join("a.upgrade-tmp"),
            d.join("a.sock.upg"),
        ];
        let live = d.join("b.sock.upg");
        for p in strays.iter().chain([&live]) {
            std::fs::write(p, b"x").unwrap();
            age(p, STRAY_MIN_AGE * 2);
        }
        // Pane b is live: its lock is HELD for the whole sweep.
        let held =
            match crate::lock::PaneLock::try_acquire(&lock_path(&d, &PaneId::new("b").unwrap()))
                .unwrap()
            {
                crate::lock::TryLock::Acquired(l) => l,
                crate::lock::TryLock::Held => panic!("fresh lock held"),
            };
        let report = boot_sweep(&d, Duration::from_millis(300), &[], |_, _| {}).unwrap();
        drop(held);
        for p in &strays {
            assert!(!p.exists(), "{} was kept: {report:?}", p.display());
        }
        assert!(live.exists(), "a pane with a lock keeps its staged socket");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Finding #7: a dead holder's leftovers that nothing proves are the
    /// pane's are left alone AND kept in the orphan ledger, re-checked by every
    /// sweep, and dropped once they are gone.
    #[cfg(target_os = "linux")]
    #[test]
    fn pty_holder_boot_sweep_keeps_unattributed_leftovers_in_the_orphan_ledger() {
        use std::os::unix::process::CommandExt;
        /// Kills (and reaps) what this test spawned, even on a failed assert.
        struct Fixture(std::process::Child);
        impl Drop for Fixture {
            fn drop(&mut self) {
                // SAFETY: the process group of a child this test spawned and
                // made a session (and group) leader.
                unsafe {
                    libc::kill(-(self.0.id() as i32), libc::SIGKILL);
                }
                let _ = self.0.wait();
            }
        }
        let d = dir("orphan");
        let pane = PaneId::new("o").unwrap();
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "sleep 300 & wait"]);
        // SAFETY: setsid in the forked child before exec; async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let leader = Fixture(cmd.spawn().unwrap());
        // A dead holder's record that names the leader WITHOUT a start token:
        // nothing about it is provable.
        {
            let TryLock::Acquired(mut lock) = PaneLock::try_acquire(&lock_path(&d, &pane)).unwrap()
            else {
                panic!("fresh lock");
            };
            lock.write_record(&LockRecord {
                holder_pid: 999_999,
                child_pid: Some(leader.0.id()),
                versions: vec![3],
                started_at_unix_ms: 0,
                holder_build: "t".into(),
                child_start: None,
                scope_unit: None,
                holder_start: None,
                ended: None,
            })
            .unwrap();
        }
        // Let `sh` fork its job.
        std::thread::sleep(Duration::from_millis(200));
        let report = boot_sweep(&d, Duration::from_secs(5), &[], |_, _| {}).unwrap();
        match &report.entries[0].outcome {
            SweepOutcome::Reaped {
                reap, orphan_kept, ..
            } => {
                assert!(reap.killed.is_empty(), "{reap:?}");
                assert!(!reap.unattributed.is_empty(), "{reap:?}");
                assert!(orphan_kept);
            }
            other => panic!("{other:?}"),
        }
        assert!(orphan_path(&d, &pane).exists());
        let again = boot_sweep(&d, Duration::from_secs(5), &[], |_, _| {}).unwrap();
        assert!(again.orphans.iter().any(|o| o.kept), "{again:?}");
        drop(leader);
        std::thread::sleep(Duration::from_millis(200));
        let gone = boot_sweep(&d, Duration::from_secs(5), &[], |_, _| {}).unwrap();
        assert!(gone.orphans.iter().all(|o| !o.kept), "{gone:?}");
        assert!(!orphan_path(&d, &pane).exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
