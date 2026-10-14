//! The runner's half of the PTY-holder boot sweep: at start-up, adopt every
//! pane a holder kept alive across the last runner's exit, and reap the dead.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 3 (D8 under D13). The walk itself — classify each pane by handshake
//! and lock, reap a dead holder's PROVEN tree remainder, keep what cannot be
//! attributed in the orphan ledger, clean files — is
//! `qontinui_pty_holder::sweep::boot_sweep`, over this instance's pane
//! directory ([`super::daemon_pane_io::runner_pane_dir`]). What only the
//! runner can do is here: turn each LIVE pane back into a terminal tab,
//! through [`TerminalManager::adopt_holder_pane`] (Phase 2b) — the first
//! production caller of that door.
//!
//! # Identity
//!
//! A holder carries no harness session id (that is Phase 6's OSC-reported
//! `SessionStart` id). The pane id IS the terminal id, and the session
//! lifecycle registry keys its open record by terminal id, so the adoption
//! takes the record's `claude_session_id` as the pinned session id and its
//! `page_id` as the tab's page ([`plan_adoptions`]). That is the id the pane
//! was spawned under: the identity seam records it at spawn. A pane with no
//! open record (a plain shell, or a record lost with the registry) is still
//! adopted — the process is alive and must be visible — under an empty pinned
//! id and the default page, and the log says so.
//!
//! # Ordering against the webview's restore — without blocking setup
//!
//! [`run`] starts the sweep on a thread of its own and returns at once: setup
//! never waits on a census (an unanswering holder costs [`CENSUS_TIMEOUT`]) or
//! on reaping. The ordering the restore needs — adoption BEFORE the webview
//! reads `terminal_list`, so a surviving session is reconnected rather than
//! cold-resumed a second time — is kept by a "settled" signal instead: the
//! sweep settles once the census is in, every live pane's adoption has
//! finished (one that failed definitively ended its holder; one that failed
//! transiently is left alive for a background retry — see below), AND the
//! dead panes are reaped (concurrently, under
//! `qontinui_pty_holder::sweep::REAP_BUDGET`) — so the restore never sees a
//! dead holder's still-running `claude` as live, refuses its resume, and then
//! watches the sweep reap it (review round 2, N4). With no dead panes it
//! settles right after the adoptions. The webview's restore first awaits
//! [`wait_settled`] (the `terminal_holder_sweep_settled` command) for at most
//! [`RESTORE_WAIT`], which is DERIVED from those steps' bounds (review round
//! 3, R2). If it still stops waiting, it re-decides the sessions it refused as
//! live once the sweep reports settled (`recheckRefusedAfterSweep` in the
//! webview), and an adoption that lands later is an ordinary
//! `terminal-created` tab.
//!
//! # A failed adoption (review round 3, R1)
//!
//! Preserving a live session is the point of the holder, so the sweep NEVER
//! ends a live holder over a transient failure ([`adopt_with_retries`]):
//!
//! - the terminal id is already open or being built → skipped: another path
//!   owns the pane;
//! - a timeout, an IO error, a busy holder, a session that could not be built
//!   → retried [`ATTACH_ATTEMPTS`] times with backoff; if it still fails, the
//!   holder is LEFT ALIVE and handed to a background retry
//!   ([`retry_in_background`]: every [`BACKGROUND_RETRY_EVERY`] for at most
//!   [`BACKGROUND_RETRY_FOR`]) that adopts it — the same `terminal-created`
//!   a normal adoption emits, so the tab appears — and stops when the holder
//!   dies or the pane is opened by another path. Logged loudly;
//! - ONLY a definitive failure — the holder speaks no protocol version this
//!   runner drives, or finally refuses (`ConnectError::is_definitive`) — ends
//!   the holder, through its verified lock (`terminate_verified_holder`,
//!   which checks the holder's start token), waits for its child to go, and
//!   logs it as an error; the restore then cold-resumes the session.
//!
//! Incompatible and unanswering (Unknown) panes are not adopted and not
//! touched (D15; nothing proves an Unknown pane is ours to end).
//!
//! # Gate
//!
//! `terminal.pty_holder` governs whether NEW panes get a holder. The sweep runs
//! whenever the pane directory exists, setting on or off: a holder that is
//! alive is a session that is alive, and turning the setting off must not make
//! it invisible (the census-invisibility failure Risk 2 names). With the
//! setting off and no pane directory — the default — it costs one `stat`.

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use qontinui_pty_holder::pane::PaneId;
use qontinui_pty_holder::sweep::{boot_sweep, SweepEntry, SweepOutcome, SweepReport};
use qontinui_pty_holder::terminate::terminate_verified_holder;
use tauri::AppHandle;
use tracing::{error, info, warn};

use super::manager::{AdoptError, TerminalManager};
use crate::session::session_lifecycle_store::SessionLifecycleStore;

/// One deadline for classifying every pane (the census's handshakes run
/// concurrently under it).
pub const CENSUS_TIMEOUT: Duration = Duration::from_secs(5);

/// Attach attempts one adoption gets inside the sweep before it is handed to
/// the background retry.
pub const ATTACH_ATTEMPTS: usize = 3;

/// The pauses between those attempts.
pub const ATTACH_BACKOFF: [Duration; ATTACH_ATTEMPTS - 1] =
    [Duration::from_secs(1), Duration::from_secs(2)];

/// How often the background retry tries an adoption the sweep deferred.
pub const BACKGROUND_RETRY_EVERY: Duration = Duration::from_secs(15);

/// How long the background retry keeps trying.
pub const BACKGROUND_RETRY_FOR: Duration = Duration::from_secs(10 * 60);

/// How long a definitively failed adoption waits for the holder it ended, and
/// its child, to be gone.
pub const FAILED_ADOPTION_END_WAIT: Duration = Duration::from_secs(5);

/// Slack for scheduling and the lock and record reads around the steps.
const SETTLE_MARGIN: Duration = Duration::from_secs(4);

/// The longest the sweep takes to settle — and so the longest the webview's
/// restore waits for it: the census, then one adoption's worst case (every
/// attempt timing out, the backoffs between, and a definitive failure's end
/// wait — adoptions run concurrently, so one is the bound), then the reaping
/// step's own budget, plus margin. Derived, not chosen: a step that grows
/// moves it (`pty_holder_boot_sweep_restore_wait_covers_every_step`).
pub const RESTORE_WAIT: Duration = Duration::from_secs(
    CENSUS_TIMEOUT.as_secs()
        + super::daemon_pane_io::ATTACH_DEADLINE.as_secs() * ATTACH_ATTEMPTS as u64
        + ATTACH_BACKOFF[0].as_secs()
        + ATTACH_BACKOFF[1].as_secs()
        + FAILED_ADOPTION_END_WAIT.as_secs()
        + qontinui_pty_holder::sweep::REAP_BUDGET.as_secs()
        + SETTLE_MARGIN.as_secs(),
);

/// Where the boot sweep is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// [`run`] was never called (a process with no boot sweep): nothing to
    /// wait for.
    NotStarted,
    Running,
    /// The census is in and every live pane's adoption has finished.
    Settled,
}

static PHASE: Mutex<Phase> = Mutex::new(Phase::NotStarted);
static PHASE_CHANGED: Condvar = Condvar::new();

fn set_phase(phase: Phase) {
    *PHASE.lock().unwrap_or_else(|p| p.into_inner()) = phase;
    PHASE_CHANGED.notify_all();
}

/// Block until the boot sweep has settled (see the module docs), at most
/// `timeout`. `true` when it settled or never started; `false` when the wait
/// ran out.
pub fn wait_settled(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut phase = PHASE.lock().unwrap_or_else(|p| p.into_inner());
    loop {
        if *phase != Phase::Running {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        phase = match PHASE_CHANGED.wait_timeout(phase, left) {
            Ok((g, _)) => g,
            Err(p) => p.into_inner().0,
        };
    }
}

/// One pane to adopt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adoption {
    pub pane_id: String,
    /// The registry's page for the pane; `None` → the default page.
    pub page_id: Option<String>,
    /// The registry's session id for the pane; `""` when it has no open
    /// record.
    pub pinned_session_id: String,
    /// The holder that answered the census — the one a failed adoption ends.
    pub holder_pid: u32,
}

/// The live panes of `entries` that `already_open` does not name, each with
/// the identity `open_record` (terminal id → `(session id, page id)`) gives
/// it. Pure, so the identity rule is tested without a sweep.
pub fn plan_adoptions(
    entries: &[SweepEntry],
    already_open: &[String],
    open_record: impl Fn(&str) -> Option<(String, String)>,
) -> Vec<Adoption> {
    entries
        .iter()
        .filter(|e| !already_open.contains(&e.pane_id))
        .filter_map(|e| {
            let SweepOutcome::Live { hello_ack, .. } = &e.outcome else {
                return None;
            };
            let (pinned_session_id, page_id) = match open_record(&e.pane_id) {
                Some((session, page)) => (session, Some(page)),
                None => (String::new(), None),
            };
            Some(Adoption {
                pane_id: e.pane_id.clone(),
                page_id,
                pinned_session_id,
                holder_pid: hello_ack.holder_pid,
            })
        })
        .collect()
}

/// One log line per pane, so a reaped orphan or an unanswering holder is
/// never silent.
fn log_report(pane_dir: &Path, report: &SweepReport) {
    for entry in &report.entries {
        match &entry.outcome {
            SweepOutcome::Live { hello_ack, .. } => info!(
                pane = %entry.pane_id,
                holder_pid = hello_ack.holder_pid,
                child_pid = ?hello_ack.child_pid,
                "pty holder boot sweep: live holder found"
            ),
            SweepOutcome::Incompatible { holder_versions } => warn!(
                pane = %entry.pane_id,
                ?holder_versions,
                "pty holder boot sweep: a live holder speaks no protocol version this runner \
                 drives — left running, not adopted (plan D15)"
            ),
            SweepOutcome::Unknown { reason, record } => warn!(
                pane = %entry.pane_id,
                holder_pid = ?record.as_ref().map(|r| r.holder_pid),
                reason = %reason,
                "pty holder boot sweep: pane is UNKNOWN (lock held, no answer) — left untouched"
            ),
            SweepOutcome::Reaped {
                record,
                reap,
                lock_removed,
                orphan_kept,
            } => info!(
                pane = %entry.pane_id,
                holder_pid = ?record.as_ref().map(|r| r.holder_pid),
                lock_removed,
                orphan_kept,
                reap = %reap.summary(),
                "pty holder boot sweep: dead holder reaped"
            ),
        }
    }
    for orphan in &report.orphans {
        if orphan.kept {
            warn!(
                pane = %orphan.pane_id,
                unattributed = ?orphan.reap.unattributed,
                "pty holder boot sweep: orphan-ledger entry KEPT — processes a dead holder may \
                 have left are still running and nothing proves they are the pane's"
            );
        } else {
            info!(pane = %orphan.pane_id, "pty holder boot sweep: orphan-ledger entry cleared");
        }
    }
    for path in &report.strays_removed {
        info!(path = %path.display(), "pty holder boot sweep: stray file removed");
    }
    for err in &report.errors {
        warn!(pane_dir = %pane_dir.display(), error = %err, "pty holder boot sweep: removal failed");
    }
}

/// Start the boot sweep of this instance's pane directory on a thread of its
/// own and return at once. See the module docs for how the webview's restore
/// is ordered after it.
pub fn run(
    manager: &Arc<TerminalManager>,
    app_handle: &AppHandle,
    store: Option<Arc<SessionLifecycleStore>>,
) {
    let pane_dir = match super::daemon_pane_io::runner_pane_dir() {
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "pty holder boot sweep: no pane directory — skipped");
            return;
        }
    };
    if !pane_dir.exists() {
        return;
    }
    set_phase(Phase::Running);
    let (manager, app_handle) = (manager.clone(), app_handle.clone());
    let spawned = std::thread::Builder::new()
        .name("pty-holder-boot-sweep".into())
        .spawn(move || {
            // Settled however the sweep ends — a panic included, or every
            // page restore would wait out RESTORE_WAIT.
            let _settled = SettleOnDrop;
            sweep_and_adopt(&manager, &app_handle, store.as_deref(), &pane_dir);
        });
    if let Err(e) = spawned {
        set_phase(Phase::Settled);
        warn!(error = %e, "pty holder boot sweep: thread did not start — nothing adopted");
    }
}

/// Sets [`Phase::Settled`] when dropped.
struct SettleOnDrop;

impl Drop for SettleOnDrop {
    fn drop(&mut self) {
        set_phase(Phase::Settled);
    }
}

/// End the holder of a pane whose adoption failed DEFINITIVELY (see the
/// module docs) and wait for it and its child to be gone.
fn end_unadoptable_holder(pane_dir: &Path, adoption: &Adoption, why: &str) {
    let Ok(pane) = PaneId::new(&adoption.pane_id) else {
        return;
    };
    let lock_file = qontinui_pty_holder::pane::lock_path(pane_dir, &pane);
    let record = qontinui_pty_holder::lock::read_record(&lock_file);
    let t = terminate_verified_holder(pane_dir, &pane, adoption.holder_pid);
    error!(
        pane = %adoption.pane_id,
        holder_pid = adoption.holder_pid,
        why,
        signalled = t.signalled,
        detail = %t.detail,
        "pty holder boot sweep: adoption failed DEFINITIVELY — ending the holder so the \
         session is cold-resumed instead of left running with no tab"
    );
    if !t.signalled {
        return;
    }
    let end = Instant::now() + FAILED_ADOPTION_END_WAIT;
    let child = record
        .as_ref()
        .and_then(|r| r.child_pid.zip(r.child_start.clone()));
    loop {
        let holder_gone = holder_gone(pane_dir, adoption);
        let child_gone = child.as_ref().is_none_or(|(pid, token)| {
            !qontinui_pty_holder::proc_identity::is_same_process(*pid, Some(token))
        });
        if holder_gone && child_gone {
            return;
        }
        if Instant::now() >= end {
            warn!(
                pane = %adoption.pane_id,
                holder_gone,
                child_gone,
                "pty holder boot sweep: the ended holder's pane is not gone yet"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether the holder an adoption is for is gone: its pane's lock is FREE (or
/// the lock file is gone), or the lock's record no longer names a live holder
/// with that pid.
fn holder_gone(pane_dir: &Path, adoption: &Adoption) -> bool {
    use qontinui_pty_holder::lock::{read_record, record_names_live_holder, PaneLock, TryLock};
    let Ok(pane) = PaneId::new(&adoption.pane_id) else {
        return true;
    };
    let lock_file = qontinui_pty_holder::pane::lock_path(pane_dir, &pane);
    match PaneLock::try_acquire_existing(&lock_file) {
        Ok(TryLock::Acquired(_)) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        // Held, or unreadable: gone only if the record says so.
        _ => {
            let record = read_record(&lock_file);
            !(record
                .as_ref()
                .is_some_and(|r| r.holder_pid == adoption.holder_pid)
                && record_names_live_holder(record.as_ref()))
        }
    }
}

/// How one adoption ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdoptOutcome {
    Adopted,
    /// Another path owns the pane (already open, or being built).
    Skipped(String),
    /// A definitive failure: the holder was ended.
    Ended(String),
    /// Transient failures only: the holder is alive, and the background
    /// retry takes the pane.
    Deferred(String),
}

/// One adoption, with the retry policy of the module docs: up to
/// [`ATTACH_ATTEMPTS`] attempts, `pause(i)` between them; `end` only on a
/// definitive failure. Generic over the door, so the policy is tested
/// without a Tauri runtime.
fn adopt_with_retries(
    adopt: &dyn Fn() -> Result<(), AdoptError>,
    end: &dyn Fn(&str),
    pause: &dyn Fn(usize),
) -> AdoptOutcome {
    let mut last = String::new();
    for attempt in 0..ATTACH_ATTEMPTS {
        if attempt > 0 {
            pause(attempt - 1);
        }
        match adopt() {
            Ok(()) => return AdoptOutcome::Adopted,
            Err(AdoptError::Claimed(why)) => return AdoptOutcome::Skipped(why),
            Err(AdoptError::Attach(e)) if e.definitive => {
                end(&e.reason);
                return AdoptOutcome::Ended(e.reason);
            }
            Err(e) => last = e.to_string(),
        }
    }
    AdoptOutcome::Deferred(last)
}

/// The background retry of a deferred adoption: every `every`, for at most
/// `for_`, try `adopt` — unless `stop()` says the pane is no longer this
/// retry's to adopt (the holder died, or another path opened the pane). Ends
/// the holder only on a definitive failure, as [`adopt_with_retries`].
fn retry_in_background(
    adopt: &dyn Fn() -> Result<(), AdoptError>,
    end: &dyn Fn(&str),
    stop: &dyn Fn() -> Option<String>,
    every: Duration,
    for_: Duration,
) -> AdoptOutcome {
    let until = Instant::now() + for_;
    let mut last = String::from("the retry window ran out");
    while Instant::now() + every <= until {
        std::thread::sleep(every);
        if let Some(why) = stop() {
            return AdoptOutcome::Skipped(why);
        }
        match adopt() {
            Ok(()) => return AdoptOutcome::Adopted,
            Err(AdoptError::Claimed(why)) => return AdoptOutcome::Skipped(why),
            Err(AdoptError::Attach(e)) if e.definitive => {
                end(&e.reason);
                return AdoptOutcome::Ended(e.reason);
            }
            Err(e) => last = e.to_string(),
        }
    }
    AdoptOutcome::Deferred(last)
}

/// The adoption door for `adoption`, as [`adopt_with_retries`] and
/// [`retry_in_background`] call it.
fn adopt_once(
    manager: &TerminalManager,
    app_handle: &AppHandle,
    adoption: &Adoption,
) -> Result<(), AdoptError> {
    manager
        .adopt_holder_pane(
            &adoption.pane_id,
            None,
            adoption.page_id.clone(),
            None,
            app_handle.clone(),
            adoption.pinned_session_id.clone(),
        )
        .map(|_| ())
}

/// Hand a deferred adoption to [`retry_in_background`] on a thread of its
/// own: the sweep does not wait for it.
fn spawn_background_retry(
    manager: Arc<TerminalManager>,
    app_handle: AppHandle,
    pane_dir: std::path::PathBuf,
    adoption: Adoption,
) {
    let name = format!("pty-holder-readopt-{}", adoption.pane_id);
    let spawned = std::thread::Builder::new().name(name).spawn(move || {
        let adopt = || adopt_once(&manager, &app_handle, &adoption);
        let end = |why: &str| end_unadoptable_holder(&pane_dir, &adoption, why);
        let stop = || {
            if manager.get(&adoption.pane_id).is_some() {
                Some("the pane was opened by another path".to_string())
            } else if holder_gone(&pane_dir, &adoption) {
                Some("the holder is gone".to_string())
            } else {
                None
            }
        };
        let late = retry_in_background(
            &adopt,
            &end,
            &stop,
            BACKGROUND_RETRY_EVERY,
            BACKGROUND_RETRY_FOR,
        );
        let level_error = matches!(late, AdoptOutcome::Deferred(_));
        if level_error {
            error!(
                pane = %adoption.pane_id,
                outcome = ?late,
                "pty holder boot sweep: background adoption retry GAVE UP — the holder is left \
                 running with no tab until the next start's sweep"
            );
        } else {
            info!(
                pane = %adoption.pane_id,
                outcome = ?late,
                "pty holder boot sweep: background adoption retry finished"
            );
        }
    });
    if let Err(e) = spawned {
        error!(error = %e, "pty holder boot sweep: background adoption retry did not start");
    }
}

/// Adopt `adoptions` concurrently; returns how many were adopted inside the
/// sweep. Each follows [`adopt_with_retries`]; a deferred one is handed to
/// [`spawn_background_retry`], which the sweep does not wait for.
fn adopt_all(
    manager: &Arc<TerminalManager>,
    app_handle: &AppHandle,
    pane_dir: &Path,
    adoptions: Vec<Adoption>,
) -> usize {
    let mut threads = Vec::new();
    for adoption in adoptions {
        if adoption.pinned_session_id.is_empty() {
            warn!(
                pane = %adoption.pane_id,
                "pty holder boot sweep: no open registry record for this pane — adopting it \
                 with no pinned session id, on the default page"
            );
        }
        let (manager, app_handle, pane_dir) =
            (manager.clone(), app_handle.clone(), pane_dir.to_path_buf());
        let thread = std::thread::Builder::new()
            .name(format!("pty-holder-adopt-{}", adoption.pane_id))
            .spawn(move || {
                let adopt = || adopt_once(&manager, &app_handle, &adoption);
                let end = |why: &str| end_unadoptable_holder(&pane_dir, &adoption, why);
                let outcome = adopt_with_retries(&adopt, &end, &|i| {
                    std::thread::sleep(ATTACH_BACKOFF[i])
                });
                match &outcome {
                    AdoptOutcome::Adopted => {
                        info!(pane = %adoption.pane_id, "pty holder boot sweep: pane adopted into a terminal tab")
                    }
                    AdoptOutcome::Skipped(why) => info!(
                        pane = %adoption.pane_id,
                        why = %why,
                        "pty holder boot sweep: pane not adopted — another path owns it"
                    ),
                    AdoptOutcome::Ended(_) => {}
                    AdoptOutcome::Deferred(why) => {
                        error!(
                            pane = %adoption.pane_id,
                            holder_pid = adoption.holder_pid,
                            why = %why,
                            "pty holder boot sweep: could not adopt a LIVE holder after {} \
                             attempts — it is left running, and retried in the background \
                             every {}s for up to {}s",
                            ATTACH_ATTEMPTS,
                            BACKGROUND_RETRY_EVERY.as_secs(),
                            BACKGROUND_RETRY_FOR.as_secs()
                        );
                        spawn_background_retry(
                            manager.clone(),
                            app_handle.clone(),
                            pane_dir.clone(),
                            adoption.clone(),
                        );
                    }
                }
                outcome == AdoptOutcome::Adopted
            });
        match thread {
            Ok(t) => threads.push(t),
            Err(e) => warn!(error = %e, "pty holder boot sweep: adoption thread did not start"),
        }
    }
    threads
        .into_iter()
        .map(|t| t.join().unwrap_or(false))
        .filter(|ok| *ok)
        .count()
}

/// Classify, `adopt` the live panes, reap the dead — and `settle` only once
/// the dead are reaped, or right after the adoptions when there are none
/// (review round 2, N4). Returns the report and how many were adopted.
/// Separate from the Tauri wiring so the ordering is tested on its own.
fn sweep_then_settle(
    pane_dir: &Path,
    skip: &[PaneId],
    adopt: impl FnOnce(&[SweepEntry]) -> usize,
    settle: impl Fn(),
) -> std::io::Result<(SweepReport, usize)> {
    let mut adopted = 0;
    let swept = boot_sweep(pane_dir, CENSUS_TIMEOUT, skip, |entries, dead| {
        adopted = adopt(entries);
        if dead == 0 {
            settle();
        }
    });
    settle();
    swept.map(|report| (report, adopted))
}

/// The sweep proper, wired to this runner: adopt into `manager`, settle the
/// phase, log.
fn sweep_and_adopt(
    manager: &Arc<TerminalManager>,
    app_handle: &AppHandle,
    store: Option<&SessionLifecycleStore>,
    pane_dir: &Path,
) {
    let started = Instant::now();
    // Never probe a pane this runner already serves or is building: a probe
    // can steal a starting holder's lock.
    let already_open = manager.open_or_in_flight_ids();
    let skip: Vec<PaneId> = already_open
        .iter()
        .filter_map(|id| PaneId::new(id).ok())
        .collect();
    let swept = sweep_then_settle(
        pane_dir,
        &skip,
        |entries| {
            let adoptions = plan_adoptions(entries, &already_open, |terminal_id| {
                store?
                    .find_open_by_terminal(terminal_id)
                    .map(|r| (r.claude_session_id, r.page_id))
            });
            adopt_all(manager, app_handle, pane_dir, adoptions)
        },
        || set_phase(Phase::Settled),
    );
    match swept {
        Ok((report, adopted)) => {
            log_report(pane_dir, &report);
            info!(
                swept = report.entries.len(),
                adopted,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "pty holder boot sweep: done"
            );
        }
        Err(e) => warn!(
            pane_dir = %pane_dir.display(),
            error = %e,
            "pty holder boot sweep: the pane directory could not be swept — live holders in \
             it are NOT adopted this run"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_pty_holder::protocol::HelloAck;

    fn live(id: &str) -> SweepEntry {
        SweepEntry {
            pane_id: id.into(),
            outcome: SweepOutcome::Live {
                hello_ack: HelloAck {
                    version: 3,
                    holder_build: "b".into(),
                    holder_pid: 10,
                    child_pid: Some(11),
                },
                record: None,
            },
        }
    }

    #[test]
    fn pty_holder_boot_sweep_adopts_live_panes_under_their_registry_identity() {
        let entries = vec![
            live("t-known"),
            live("t-unrecorded"),
            live("t-open-already"),
            SweepEntry {
                pane_id: "t-wedged".into(),
                outcome: SweepOutcome::Unknown {
                    reason: "x".into(),
                    record: None,
                },
            },
            SweepEntry {
                pane_id: "t-old".into(),
                outcome: SweepOutcome::Incompatible {
                    holder_versions: vec![1],
                },
            },
        ];
        let plans = plan_adoptions(&entries, &["t-open-already".to_string()], |id| {
            (id == "t-known").then(|| ("sess-1".to_string(), "page-2".to_string()))
        });
        assert_eq!(
            plans,
            vec![
                Adoption {
                    pane_id: "t-known".into(),
                    page_id: Some("page-2".into()),
                    pinned_session_id: "sess-1".into(),
                    holder_pid: 10,
                },
                Adoption {
                    pane_id: "t-unrecorded".into(),
                    page_id: None,
                    pinned_session_id: String::new(),
                    holder_pid: 10,
                },
            ],
            "only live panes not already open are adopted; unknown, incompatible and open ones \
             are not"
        );
    }

    /// The restore's wait: a sweep that never started is no reason to wait; a
    /// running one is waited for until it settles, and no longer than asked.
    #[test]
    fn pty_holder_boot_sweep_restore_waits_for_settle_and_no_longer() {
        set_phase(Phase::NotStarted);
        assert!(wait_settled(Duration::from_secs(5)), "never started");
        set_phase(Phase::Running);
        let t = Instant::now();
        assert!(!wait_settled(Duration::from_millis(50)), "still running");
        assert!(t.elapsed() < Duration::from_secs(2));
        let settle = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(50));
            set_phase(Phase::Settled);
        });
        assert!(wait_settled(Duration::from_secs(5)));
        settle.join().unwrap();
        set_phase(Phase::NotStarted);
    }

    /// Review round 2, N4: with a dead pane to reap, the sweep settles only
    /// AFTER the reap — at every settle, the dead pane's lock is already gone.
    /// With none, it settles right after the adoptions.
    #[test]
    fn pty_holder_boot_sweep_settles_only_after_the_dead_are_reaped() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!("ptysettle-{}-{nanos:x}", std::process::id()));
        qontinui_pty_holder::pane::prepare_private_dir(&dir).unwrap();
        let dead = PaneId::new("dead").unwrap();
        let lock = qontinui_pty_holder::pane::lock_path(&dir, &dead);
        std::fs::write(&lock, b"").unwrap();

        let seen: Mutex<Vec<(&str, bool)>> = Mutex::new(Vec::new());
        let (report, adopted) = sweep_then_settle(
            &dir,
            &[],
            |_| {
                seen.lock().unwrap().push(("adopt", lock.exists()));
                0
            },
            || seen.lock().unwrap().push(("settle", lock.exists())),
        )
        .unwrap();
        assert_eq!(adopted, 0);
        assert_eq!(report.entries.len(), 1, "{report:?}");
        let seen = seen.into_inner().unwrap();
        assert_eq!(
            seen.first(),
            Some(&("adopt", true)),
            "adopted before the reap: {seen:?}"
        );
        let settles: Vec<bool> = seen
            .iter()
            .filter(|(what, _)| *what == "settle")
            .map(|(_, lock_present)| *lock_present)
            .collect();
        assert!(!settles.is_empty());
        assert!(
            settles.iter().all(|present| !present),
            "settled while the dead pane was still unreaped: {seen:?}"
        );

        // No dead pane: settled already from the adoption step.
        let settled_early = Mutex::new(false);
        sweep_then_settle(&dir, &[], |_| 0, || *settled_early.lock().unwrap() = true).unwrap();
        assert!(*settled_early.lock().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn transient() -> AdoptError {
        AdoptError::Attach(super::super::daemon_pane_io::AttachError {
            reason: "attach timed out".into(),
            definitive: false,
        })
    }

    fn definitive() -> AdoptError {
        AdoptError::Attach(super::super::daemon_pane_io::AttachError {
            reason: "no common protocol version".into(),
            definitive: true,
        })
    }

    /// Review round 3, R1: a claim refusal skips — another path owns the
    /// pane — and never ends the holder.
    #[test]
    fn pty_holder_boot_sweep_claim_refusal_leaves_the_holder_alive() {
        use std::cell::Cell;
        let (tries, ends) = (Cell::new(0), Cell::new(0));
        let outcome = adopt_with_retries(
            &|| {
                tries.set(tries.get() + 1);
                Err(AdoptError::Claimed("already open".into()))
            },
            &|_| ends.set(ends.get() + 1),
            &|_| {},
        );
        assert_eq!(outcome, AdoptOutcome::Skipped("already open".into()));
        assert_eq!((tries.get(), ends.get()), (1, 0));
    }

    /// R1: transient failures are retried, then DEFERRED with the holder
    /// alive; the background retry then adopts it.
    #[test]
    fn pty_holder_boot_sweep_transient_failure_leaves_the_holder_alive_and_retry_adopts() {
        use std::cell::Cell;
        let (tries, ends, pauses) = (Cell::new(0), Cell::new(0), Cell::new(0));
        let outcome = adopt_with_retries(
            &|| {
                tries.set(tries.get() + 1);
                Err(transient())
            },
            &|_| ends.set(ends.get() + 1),
            &|_| pauses.set(pauses.get() + 1),
        );
        assert!(matches!(outcome, AdoptOutcome::Deferred(_)), "{outcome:?}");
        assert_eq!(tries.get(), ATTACH_ATTEMPTS);
        assert_eq!(pauses.get(), ATTACH_ATTEMPTS - 1);
        assert_eq!(
            ends.get(),
            0,
            "a transient failure never ends a live holder"
        );

        // The background retry: one more transient failure, then success.
        let late_tries = Cell::new(0);
        let late = retry_in_background(
            &|| {
                late_tries.set(late_tries.get() + 1);
                if late_tries.get() < 2 {
                    Err(transient())
                } else {
                    Ok(())
                }
            },
            &|_| ends.set(ends.get() + 1),
            &|| None,
            Duration::from_millis(1),
            Duration::from_secs(5),
        );
        assert_eq!(late, AdoptOutcome::Adopted);
        assert_eq!(ends.get(), 0);

        // It stops — touching nothing — once the pane is no longer its to adopt.
        let stopped = retry_in_background(
            &|| panic!("must not adopt after stop"),
            &|_| panic!("must not end after stop"),
            &|| Some("the holder is gone".into()),
            Duration::from_millis(1),
            Duration::from_secs(5),
        );
        assert_eq!(stopped, AdoptOutcome::Skipped("the holder is gone".into()));
    }

    /// R1: only a definitive failure ends the holder — once.
    #[test]
    fn pty_holder_boot_sweep_definitive_failure_ends_the_holder() {
        use std::cell::Cell;
        let ends = Cell::new(0);
        let outcome = adopt_with_retries(
            &|| Err(definitive()),
            &|_| ends.set(ends.get() + 1),
            &|_| {},
        );
        assert!(matches!(outcome, AdoptOutcome::Ended(_)), "{outcome:?}");
        assert_eq!(ends.get(), 1);
        // After a transient one, too.
        let tries = Cell::new(0);
        let outcome = adopt_with_retries(
            &|| {
                tries.set(tries.get() + 1);
                if tries.get() == 1 {
                    Err(transient())
                } else {
                    Err(definitive())
                }
            },
            &|_| ends.set(ends.get() + 1),
            &|_| {},
        );
        assert!(matches!(outcome, AdoptOutcome::Ended(_)));
        assert_eq!(ends.get(), 2);
    }

    /// Review round 3, R2: the restore's wait covers every step the sweep
    /// takes before it settles — pinned to the constants, not chosen.
    #[test]
    fn pty_holder_boot_sweep_restore_wait_covers_every_step() {
        let adoption_worst = super::super::daemon_pane_io::ATTACH_DEADLINE * ATTACH_ATTEMPTS as u32
            + ATTACH_BACKOFF.iter().sum::<Duration>()
            + FAILED_ADOPTION_END_WAIT;
        let steps = CENSUS_TIMEOUT
            + adoption_worst
            + qontinui_pty_holder::sweep::REAP_BUDGET
            + SETTLE_MARGIN;
        assert_eq!(RESTORE_WAIT, steps);
        assert!(
            RESTORE_WAIT
                > CENSUS_TIMEOUT + adoption_worst + qontinui_pty_holder::sweep::REAP_BUDGET
        );
    }
}
