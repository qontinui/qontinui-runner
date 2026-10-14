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
//! settles right after the adoptions. A reap that overruns its budget keeps
//! the sweep unsettled until it finishes (review round 4, L1).
//!
//! The webview does not wait for all of that before restoring anything
//! (review round 4, M1): [`sweep_status`] (the `terminal_holder_sweep_status`
//! command) answers as soon as the census is in — at most [`CENSUS_WAIT`] —
//! with the HELD panes, the live ones still being adopted. Every other record
//! is restored at once; only the held ones wait for [`wait_settled`] (the
//! `terminal_holder_sweep_settled` command), for at most [`RESTORE_WAIT`],
//! which is DERIVED from the steps' bounds (review round 3, R2). Past that it
//! re-decides the sessions it refused as live once the sweep reports settled
//! (`recheckRefusedAfterSweep` in the webview), and an adoption that lands
//! later is an ordinary `terminal-created` tab.
//!
//! # A failed adoption (review rounds 3 and 4, R1/H1/M2)
//!
//! Preserving a live session is the point of the holder, so the sweep NEVER
//! ends a live holder over a failure that may clear ([`adopt_with_retries`],
//! by `ConnectError::disposition`):
//!
//! - the terminal id is already open or being built → skipped: another path
//!   owns the pane;
//! - RETRY — a timeout, an IO error, a busy holder, a reason this build does
//!   not know (a newer holder's), a rejection of a request this runner
//!   malformed (a runner BUG: logged at error), a session that could not be
//!   built → retried [`ATTACH_ATTEMPTS`] times with backoff; if it still
//!   fails, the holder is LEFT ALIVE and handed to a background retry
//!   ([`retry_in_background`]: every [`BACKGROUND_RETRY_EVERY`] for at most
//!   [`BACKGROUND_RETRY_FOR`]). Each background attempt re-reads the
//!   session's registry record, and the retry stops when that record is
//!   retired, the session is hosted by another live terminal, the holder
//!   dies, or another path opens the pane ([`retry_stop_reason`]);
//! - LEAVE ALONE — a live holder below this runner's data path: not adopted,
//!   NOT touched, exactly like a census Incompatible (plan D15);
//! - END — only a FINAL answer: `no_common_version`, `peer_not_authorized`,
//!   or `verb_not_in_version` after a successful handshake. The holder is
//!   ended through its verified lock (`terminate_verified_holder`, which
//!   checks the holder's start token); a child that outlives it is reaped
//!   through the record's proof; then the restore cold-resumes the session.
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

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use qontinui_pty_holder::pane::PaneId;
use qontinui_pty_holder::sweep::{boot_sweep_with_pending, SweepEntry, SweepOutcome, SweepReport};
use qontinui_pty_holder::terminate::terminate_verified_holder;
use tauri::AppHandle;
use tracing::{error, info, warn};

use super::manager::{AdoptError, TerminalManager};
use crate::session::session_lifecycle_store::SessionLifecycleStore;
use qontinui_pty_holder::client::FailureDisposition;

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
/// wait — adoptions run concurrently, so one is the bound), then the dead
/// panes' reaping budget and the orphan re-checks' own budget, plus margin
/// (63 s today). Derived, not chosen: a step that grows moves it
/// (`pty_holder_boot_sweep_restore_wait_covers_every_step`). A reap that
/// overruns its budget keeps the sweep unsettled past this; the webview's
/// re-decision after the settle covers that case.
pub const RESTORE_WAIT: Duration = Duration::from_secs(
    CENSUS_TIMEOUT.as_secs()
        + super::daemon_pane_io::ATTACH_DEADLINE.as_secs() * ATTACH_ATTEMPTS as u64
        + ATTACH_BACKOFF[0].as_secs()
        + ATTACH_BACKOFF[1].as_secs()
        + FAILED_ADOPTION_END_WAIT.as_secs()
        + qontinui_pty_holder::sweep::REAP_BUDGET.as_secs()
        + qontinui_pty_holder::sweep::ORPHAN_RECHECK_BUDGET.as_secs()
        + SETTLE_MARGIN.as_secs(),
);

/// The longest the webview's restore waits for the CENSUS (not the whole
/// sweep): the census deadline plus margin. With it the restore learns which
/// panes are HELD — live, being adopted — and restores every other record at
/// once (review round 4, M1).
pub const CENSUS_WAIT: Duration =
    Duration::from_secs(CENSUS_TIMEOUT.as_secs() + SETTLE_MARGIN.as_secs());

/// Where the boot sweep is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// [`run`] was never called (a process with no boot sweep): nothing to
    /// wait for.
    NotStarted,
    Running,
    /// The census is in, every live pane's adoption has finished, and every
    /// dead pane's reap — including one that overran its budget — is done.
    Settled,
}

/// The sweep's published state.
struct SweepState {
    phase: Phase,
    /// The terminal ids of the live panes still being adopted — `None` until
    /// the census is in. Every other record may be restored at once.
    held: Option<BTreeSet<String>>,
}

static STATE: Mutex<SweepState> = Mutex::new(SweepState {
    phase: Phase::NotStarted,
    held: None,
});
static STATE_CHANGED: Condvar = Condvar::new();

fn with_state(f: impl FnOnce(&mut SweepState)) {
    f(&mut STATE.lock().unwrap_or_else(|p| p.into_inner()));
    STATE_CHANGED.notify_all();
}

fn set_phase(phase: Phase) {
    with_state(|s| {
        s.phase = phase;
        if phase == Phase::Running {
            s.held = None;
        }
    });
}

/// Publish the census: these panes are held (being adopted).
fn publish_held(ids: impl IntoIterator<Item = String>) {
    with_state(|s| s.held = Some(ids.into_iter().collect()));
}

/// One held pane's adoption finished — adopted, skipped, ended, left alone,
/// or deferred to the background retry. Its record is the restore's again.
fn release_held(id: &str) {
    with_state(|s| {
        if let Some(held) = &mut s.held {
            held.remove(id);
        }
    });
}

/// Block until `ready` holds for the state, at most `timeout`; return it
/// either way.
fn wait_for<T>(
    timeout: Duration,
    ready: impl Fn(&SweepState) -> bool,
    read: impl Fn(&SweepState) -> T,
) -> (bool, T) {
    let deadline = Instant::now() + timeout;
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    loop {
        if ready(&state) {
            return (true, read(&state));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return (false, read(&state));
        }
        state = match STATE_CHANGED.wait_timeout(state, left) {
            Ok((g, _)) => g,
            Err(p) => p.into_inner().0,
        };
    }
}

/// Block until the boot sweep has settled (see the module docs), at most
/// `timeout`. `true` when it settled or never started; `false` when the wait
/// ran out.
pub fn wait_settled(timeout: Duration) -> bool {
    wait_for(timeout, |s| s.phase != Phase::Running, |_| ()).0
}

/// What the webview's restore needs to start: whether the sweep has settled,
/// and — once the census is in — which terminal ids are HELD (live panes
/// still being adopted). `held: None` means the census did not come in within
/// the wait: the restore then waits for the whole sweep, as before.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SweepStatus {
    pub settled: bool,
    pub held: Option<Vec<String>>,
}

/// [`SweepStatus`], waiting at most `timeout` for the census.
pub fn sweep_status(timeout: Duration) -> SweepStatus {
    let (_, status) = wait_for(
        timeout,
        |s| s.phase != Phase::Running || s.held.is_some(),
        |s| SweepStatus {
            settled: s.phase != Phase::Running,
            held: match s.phase {
                Phase::Running => s.held.as_ref().map(|h| h.iter().cloned().collect()),
                Phase::NotStarted | Phase::Settled => Some(Vec::new()),
            },
        },
    );
    status
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
            sweep_and_adopt(&manager, &app_handle, store, &pane_dir);
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
    let mut child_reaped = false;
    loop {
        let holder_gone = holder_gone(pane_dir, adoption);
        let child_gone = child.as_ref().is_none_or(|(pid, token)| {
            !qontinui_pty_holder::proc_identity::is_same_process(*pid, Some(token))
        });
        if holder_gone && child_gone {
            return;
        }
        // Review round 4, L4: a child that survives its ended holder (it
        // ignores the hang-up) would keep the session "live" — the restore
        // would refuse to resume it, with no tab. The session is being ended
        // anyway, so end the child too: once the holder is gone, through the
        // dead-holder reap, which signals only what the record's start token
        // proves is the pane's.
        if holder_gone && !child_reaped {
            child_reaped = true;
            if let Some(r) = &record {
                let reap = qontinui_pty_holder::reap::reap_orphans_of_dead_holder(r);
                warn!(
                    pane = %adoption.pane_id,
                    reap = %reap.summary(),
                    "pty holder boot sweep: the ended holder's child outlived it — reaped"
                );
            }
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
    /// The holder is live but below the data path this runner drives: not
    /// adopted, not touched (plan D15; review round 4, H1).
    LeftAlone(String),
}

/// One attempt's verdict, by the failure's [`FailureDisposition`].
fn judge(result: Result<(), AdoptError>, end: &dyn Fn(&str)) -> Result<AdoptOutcome, String> {
    match result {
        Ok(()) => Ok(AdoptOutcome::Adopted),
        Err(AdoptError::Claimed(why)) => Ok(AdoptOutcome::Skipped(why)),
        Err(AdoptError::Attach(e)) => match e.disposition {
            FailureDisposition::End => {
                end(&e.reason);
                Ok(AdoptOutcome::Ended(e.reason))
            }
            FailureDisposition::LeaveAlone => Ok(AdoptOutcome::LeftAlone(e.reason)),
            FailureDisposition::Retry => Err(e.reason),
        },
        Err(e @ AdoptError::Session(_)) => Err(e.to_string()),
    }
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
        match judge(adopt(), end) {
            Ok(outcome) => return outcome,
            Err(why) => last = why,
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
        match judge(adopt(), end) {
            Ok(outcome) => return outcome,
            Err(why) => last = why,
        }
    }
    AdoptOutcome::Deferred(last)
}

/// The adoption door for `adoption`, as [`adopt_with_retries`] and
/// The adoption door for `adoption`, as [`adopt_with_retries`] and
/// [`retry_in_background`] call it. A rejection of a request this runner
/// malformed is a runner BUG: logged loudly here, and only retried.
fn adopt_once(
    manager: &TerminalManager,
    app_handle: &AppHandle,
    adoption: &Adoption,
) -> Result<(), AdoptError> {
    let result = manager
        .adopt_holder_pane(
            &adoption.pane_id,
            None,
            adoption.page_id.clone(),
            None,
            app_handle.clone(),
            adoption.pinned_session_id.clone(),
        )
        .map(|_| ());
    if let Err(AdoptError::Attach(e)) = &result {
        if e.runner_bug {
            error!(
                pane = %adoption.pane_id,
                reason = %e.reason,
                "pty holder boot sweep: the holder REJECTED a request this runner malformed — \
                 a runner bug; retried, never a reason to end the session"
            );
        }
    }
    result
}

/// What the background retry knows, at one attempt, about the session the
/// pane hosted when the sweep planned it (review round 4, M2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionNow {
    /// The sweep found no open record for the pane: nothing to track.
    Untracked,
    /// The registry's record for that session id is no longer open.
    Retired,
    /// The record is open, hosted by terminal `terminal_id`.
    OpenOn {
        terminal_id: String,
        page_id: String,
    },
}

/// Whether the background retry must stop, and why: the session's record was
/// retired, or the session is now hosted by a DIFFERENT live terminal (the
/// user cold-resumed it by hand). Pure, so it is tested on its own.
pub fn retry_stop_reason(
    pane_id: &str,
    now: &SessionNow,
    terminal_live: impl Fn(&str) -> bool,
) -> Option<String> {
    match now {
        SessionNow::Untracked => None,
        SessionNow::Retired => Some("the session's registry record was retired".into()),
        SessionNow::OpenOn { terminal_id, .. }
            if terminal_id != pane_id && terminal_live(terminal_id) =>
        {
            Some(format!(
                "the session is now hosted by live terminal {terminal_id}"
            ))
        }
        SessionNow::OpenOn { .. } => None,
    }
}

/// [`SessionNow`] for `adoption`, read fresh from the registry.
fn session_now(store: Option<&SessionLifecycleStore>, adoption: &Adoption) -> SessionNow {
    let (Some(store), false) = (store, adoption.pinned_session_id.is_empty()) else {
        return SessionNow::Untracked;
    };
    match store.get(&adoption.pinned_session_id) {
        Some(r) if r.state == "open" => SessionNow::OpenOn {
            terminal_id: r.terminal_id,
            page_id: r.page_id,
        },
        _ => SessionNow::Retired,
    }
}

/// Hand a deferred adoption to [`retry_in_background`] on a thread of its
/// own: the sweep does not wait for it. Each attempt re-reads the session's
/// registry record — the tab is placed on its CURRENT page — and the retry
/// stops when that record is retired or the session is hosted elsewhere
/// ([`retry_stop_reason`]).
fn spawn_background_retry(
    manager: Arc<TerminalManager>,
    app_handle: AppHandle,
    pane_dir: std::path::PathBuf,
    store: Option<Arc<SessionLifecycleStore>>,
    adoption: Adoption,
) {
    let name = format!("pty-holder-readopt-{}", adoption.pane_id);
    let spawned = std::thread::Builder::new().name(name).spawn(move || {
        let adopt = || {
            let mut fresh = adoption.clone();
            if let SessionNow::OpenOn { page_id, .. } = session_now(store.as_deref(), &adoption) {
                fresh.page_id = Some(page_id);
            }
            adopt_once(&manager, &app_handle, &fresh)
        };
        let end = |why: &str| end_unadoptable_holder(&pane_dir, &adoption, why);
        let stop = || {
            if manager.get(&adoption.pane_id).is_some() {
                Some("the pane was opened by another path".to_string())
            } else if holder_gone(&pane_dir, &adoption) {
                Some("the holder is gone".to_string())
            } else {
                retry_stop_reason(
                    &adoption.pane_id,
                    &session_now(store.as_deref(), &adoption),
                    |terminal| manager.get(terminal).is_some(),
                )
            }
        };
        let late = retry_in_background(
            &adopt,
            &end,
            &stop,
            BACKGROUND_RETRY_EVERY,
            BACKGROUND_RETRY_FOR,
        );
        if matches!(late, AdoptOutcome::Deferred(_)) {
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
    store: Option<Arc<SessionLifecycleStore>>,
    adoptions: Vec<Adoption>,
) -> usize {
    // The census is in: publish which panes are held, so the restore can
    // start on every other record now (review round 4, M1).
    publish_held(adoptions.iter().map(|a| a.pane_id.clone()));
    let mut threads = Vec::new();
    for adoption in adoptions {
        if adoption.pinned_session_id.is_empty() {
            warn!(
                pane = %adoption.pane_id,
                "pty holder boot sweep: no open registry record for this pane — adopting it \
                 with no pinned session id, on the default page"
            );
        }
        let (manager, app_handle, pane_dir, store) = (
            manager.clone(),
            app_handle.clone(),
            pane_dir.to_path_buf(),
            store.clone(),
        );
        let pane_id = adoption.pane_id.clone();
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
                    AdoptOutcome::LeftAlone(why) => warn!(
                        pane = %adoption.pane_id,
                        why = %why,
                        "pty holder boot sweep: a live holder below this runner's data path — \
                         left running, not adopted (plan D15)"
                    ),
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
                            store.clone(),
                            adoption.clone(),
                        );
                    }
                }
                release_held(&adoption.pane_id);
                outcome == AdoptOutcome::Adopted
            });
        match thread {
            Ok(t) => threads.push(t),
            Err(e) => {
                release_held(&pane_id);
                warn!(error = %e, "pty holder boot sweep: adoption thread did not start");
            }
        }
    }
    threads
        .into_iter()
        .map(|t| t.join().unwrap_or(false))
        .filter(|ok| *ok)
        .count()
}

/// Classify, `adopt` the live panes, reap the dead — and `settle` only once
/// the dead are reaped (every reap, including one that overran its budget),
/// or right after the adoptions when there are none (review round 2, N4). Returns the report and how many were adopted.
/// Separate from the Tauri wiring so the ordering is tested on its own.
fn sweep_then_settle(
    pane_dir: &Path,
    skip: &[PaneId],
    adopt: impl FnOnce(&[SweepEntry]) -> usize,
    settle: impl Fn(),
) -> std::io::Result<(SweepReport, usize)> {
    let mut adopted = 0;
    let swept = boot_sweep_with_pending(pane_dir, CENSUS_TIMEOUT, skip, |entries, dead| {
        adopted = adopt(entries);
        if dead == 0 {
            settle();
        }
    });
    let report = match swept {
        Ok((report, pending)) => {
            // A reap that overran its budget is still running: the sweep is
            // NOT settled until it is done, so a restore that ran meanwhile
            // records what it refused, and re-decides it at the settle
            // (review round 4, L1).
            if !pending.is_empty() {
                warn!(
                    pending = pending.len(),
                    "pty holder boot sweep: reaps overran their budget — not settled until they \
                     finish"
                );
                pending.wait();
            }
            Ok((report, adopted))
        }
        Err(e) => Err(e),
    };
    settle();
    report
}

/// The sweep proper, wired to this runner: adopt into `manager`, settle the
/// phase, log.
fn sweep_and_adopt(
    manager: &Arc<TerminalManager>,
    app_handle: &AppHandle,
    store: Option<Arc<SessionLifecycleStore>>,
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
                store
                    .as_deref()?
                    .find_open_by_terminal(terminal_id)
                    .map(|r| (r.claude_session_id, r.page_id))
            });
            adopt_all(manager, app_handle, pane_dir, store.clone(), adoptions)
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

    /// The tests that drive the process-global sweep state run one at a time.
    static GLOBAL_STATE_TESTS: Mutex<()> = Mutex::new(());

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
        let _serial = GLOBAL_STATE_TESTS.lock().unwrap_or_else(|p| p.into_inner());
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

    fn attach_err(reason: &str, disposition: FailureDisposition) -> AdoptError {
        AdoptError::Attach(super::super::daemon_pane_io::AttachError {
            reason: reason.into(),
            disposition,
            runner_bug: false,
        })
    }

    fn transient() -> AdoptError {
        attach_err("attach timed out", FailureDisposition::Retry)
    }

    fn definitive() -> AdoptError {
        attach_err("no common protocol version", FailureDisposition::End)
    }

    /// Review round 4, H1: a holder below the data path is left alone — not
    /// ended, not retried.
    #[test]
    fn pty_holder_boot_sweep_a_holder_below_the_data_path_is_left_alone() {
        use std::cell::Cell;
        let (tries, ends) = (Cell::new(0), Cell::new(0));
        let outcome = adopt_with_retries(
            &|| {
                tries.set(tries.get() + 1);
                Err(attach_err("v1 only", FailureDisposition::LeaveAlone))
            },
            &|_| ends.set(ends.get() + 1),
            &|_| {},
        );
        assert_eq!(outcome, AdoptOutcome::LeftAlone("v1 only".into()));
        assert_eq!((tries.get(), ends.get()), (1, 0));
    }

    /// Review round 4, M2: the background retry stops once the session's
    /// record is retired, or the session is hosted by another live terminal.
    #[test]
    fn pty_holder_boot_sweep_background_retry_stops_on_a_moved_or_retired_session() {
        let live = |t: &str| t == "t-other-live";
        assert_eq!(
            retry_stop_reason("t-pane", &SessionNow::Untracked, live),
            None
        );
        assert!(retry_stop_reason("t-pane", &SessionNow::Retired, live).is_some());
        let on = |terminal: &str| SessionNow::OpenOn {
            terminal_id: terminal.into(),
            page_id: "p".into(),
        };
        assert_eq!(retry_stop_reason("t-pane", &on("t-pane"), live), None);
        assert!(retry_stop_reason("t-pane", &on("t-other-live"), live).is_some());
        assert_eq!(
            retry_stop_reason("t-pane", &on("t-dead-terminal"), live),
            None,
            "a record still pointing at a dead terminal is not another host"
        );
    }

    /// Review round 4, M1: the census publishes the held panes; the restore
    /// can start on everything else before the sweep settles.
    #[test]
    fn pty_holder_boot_sweep_status_publishes_the_held_panes_before_the_settle() {
        let _serial = GLOBAL_STATE_TESTS.lock().unwrap_or_else(|p| p.into_inner());
        set_phase(Phase::Running);
        let t = Instant::now();
        assert_eq!(
            sweep_status(Duration::from_millis(30)),
            SweepStatus {
                settled: false,
                held: None
            },
            "before the census, nothing is known"
        );
        assert!(t.elapsed() < Duration::from_secs(2));
        publish_held(["t-held".to_string(), "t-other".to_string()]);
        release_held("t-other");
        assert_eq!(
            sweep_status(Duration::from_secs(5)),
            SweepStatus {
                settled: false,
                held: Some(vec!["t-held".into()])
            }
        );
        set_phase(Phase::Settled);
        assert_eq!(
            sweep_status(Duration::from_secs(5)),
            SweepStatus {
                settled: true,
                held: Some(Vec::new())
            }
        );
        set_phase(Phase::NotStarted);
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
            + qontinui_pty_holder::sweep::ORPHAN_RECHECK_BUDGET
            + SETTLE_MARGIN;
        assert_eq!(RESTORE_WAIT, steps);
        assert!(
            RESTORE_WAIT
                > CENSUS_TIMEOUT + adoption_worst + qontinui_pty_holder::sweep::REAP_BUDGET
        );
    }
}
