//! Tier 2: relaunch a session whose PTY holder is lost under a live runner.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 6 (D11): "Resume after daemon death … `--resume` relaunch. This is
//! tier 2 for when tier 1 itself dies." Tier 1 is the holder outliving the
//! runner. When the holder ITSELF dies — or has to be ended because it stopped
//! answering ([`super::daemon_pane_io`]'s `settle_gone` / `give_up`) — its PTY
//! goes with it and the `claude` in the pane loses its terminal. This module
//! decides which session that pane hosted and hands it to the webview's
//! restore, which relaunches it in a fresh pane as `claude <flags> --resume
//! <id>` (flags: [`crate::session::resume_argv`]).
//!
//! # One relaunch path, not two
//!
//! The hand-off is Phase 3's: the [`HOLDER_SESSION_RELEASED_EVENT`] the boot
//! sweep already emits when a deferred adoption's holder dies, consumed by the
//! webview's `onHolderSessionReleased` → `recheckRefusedAfterSweep` → a restore
//! re-run for the session's page (`useTerminalInitialization.ts`). That re-run
//! creates the pane, types the resume, verifies the handshake, re-asserts the
//! record and surfaces a failure — none of which is duplicated here. The
//! webview also closes the dead pane, so the relaunch takes its zone.
//!
//! # Never a double `--resume`, and never a silent miss
//!
//! A holder's death hangs up its PTY, and that normally ends the `claude` in
//! it — but not instantly, and not necessarily (a process can ignore
//! `SIGHUP`). Relaunching a session some live process still hosts forks its
//! transcript. So the release is gated on the same oracle the webview's
//! `decideColdResume` reads (Claude Code's per-process registry, PID-verified:
//! `claude_session_registry::read_live_sessions`):
//!
//! - read every [`LIVE_POLL_EVERY`] for [`LIVE_SETTLE_WINDOW`] while the
//!   hung-up process exits; not hosted → released;
//! - still HOSTED when the window ends → not released, and the operator is
//!   told ([`RELAUNCH_WITHHELD_EVENT`], reason `still-hosted`): a banner on the
//!   dead tab offers a Resume, which re-checks the same gate;
//! - liveness UNREADABLE → the operator is told (`liveness-unknown`), and the
//!   read is retried with backoff ([`retry_delays`]) for as long as the
//!   session stays restorable (the `pty-exit` grace,
//!   `RESTORABLE_PTY_EXIT_MS`); a later not-hosted read releases it, a hosted
//!   one turns the banner into `still-hosted`;
//! - a holder that could be neither killed through nor terminated
//!   ([`HolderLoss::Unreachable`]) is never released: its `claude` may still
//!   be running behind it. The operator is told (`holder-unreachable`).
//!
//! The loop is CANCELLED (review r2, N2) when the dead pane is closed
//! (`TerminalManager::close` → [`cancel_for_terminal`]) or the session is
//! resumed elsewhere (`terminal_session_clear_restore_pending` →
//! [`cancel_for_session`]), and before it emits `Released` it re-checks that
//! the session's record is still bound to the dead terminal — a session
//! rebound to a live pane meanwhile is not relaunched a second time.
//!
//! The webview re-checks the oracle before it types anything. That second read
//! is LATER, not independent — a `claude` the registry cannot see (an
//! undiscovered config dir, a build that writes no registry file) is invisible
//! to both (review r1, residual risk).
//!
//! # Which session
//!
//! The id the pane's own `SessionStart` hook last reported over the PTY
//! ([`super::session_start_sideband`]) — the process actually running there,
//! including after a `/clear` — when it names an open record on this terminal;
//! else the CONFIRMED open record on this terminal (the hook's
//! `POST /control/session-open` confirmation). Neither → nothing is relaunched:
//! a pane with no known session (a plain shell) has nothing to resume. A
//! session marked finished is not relaunched either — it said its work is done.

use std::collections::HashSet;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tracing::{error, info, warn};

use super::holder_boot_sweep::{SessionReleased, HOLDER_SESSION_RELEASED_EVENT};
use super::pane_io::{HolderLoss, PaneIo};
use super::session_start_sideband::SessionStartReport;
use crate::session::session_id::is_valid_session_id;
use crate::session::session_lifecycle_store::{
    SessionLifecycleStore, TerminalSessionRecord, RESTORABLE_PTY_EXIT_MS,
};

/// How long the release waits for a hung-up `claude` to leave the live
/// registry. A `SIGHUP`'d CLI exits in well under a second; the bound covers a
/// loaded box and the registry file's removal.
pub const LIVE_SETTLE_WINDOW: Duration = Duration::from_secs(15);

/// How often the live registry is re-read inside that window.
pub const LIVE_POLL_EVERY: Duration = Duration::from_secs(1);

/// The event that puts a "not relaunched — Resume" banner on a lost holder's
/// dead tab, when the session could not be released for a relaunch.
pub const RELAUNCH_WITHHELD_EVENT: &str = "pty-holder-relaunch-withheld";

/// Why a lost holder's session was not released (the banner's reason).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WithheldReason {
    /// A live process still hosts the session.
    StillHosted,
    /// Liveness could not be read; retried until the restorable grace ends.
    LivenessUnknown,
    /// The holder could be neither killed through nor terminated: the
    /// session may still be running behind it.
    HolderUnreachable,
}

/// [`RELAUNCH_WITHHELD_EVENT`]'s payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelaunchWithheld {
    pub session_id: String,
    /// The dead tab the banner goes on.
    pub terminal_id: String,
    pub page_id: Option<String>,
    pub reason: WithheldReason,
}

/// What the relaunch decision tells the webview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Emission {
    Released(SessionReleased),
    Withheld(RelaunchWithheld),
}

/// One running relaunch decision: its dead terminal, its session, and the
/// flag that cancels it.
struct Pending {
    terminal_id: String,
    session_id: String,
    cancel: Arc<AtomicBool>,
}

/// Every relaunch decision in flight (review r2, N2). Entries are removed
/// when their decision ends.
static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());

fn pending() -> std::sync::MutexGuard<'static, Vec<Pending>> {
    PENDING.lock().unwrap_or_else(|p| p.into_inner())
}

/// Register a decision for `terminal_id` / `session_id`; the returned flag is
/// set when it is cancelled. A second loss on the same terminal cancels the
/// first decision.
fn register(terminal_id: &str, session_id: &str) -> Arc<AtomicBool> {
    cancel_for_terminal(terminal_id);
    let cancel = Arc::new(AtomicBool::new(false));
    pending().push(Pending {
        terminal_id: terminal_id.to_string(),
        session_id: session_id.to_string(),
        cancel: cancel.clone(),
    });
    cancel
}

fn unregister(cancel: &Arc<AtomicBool>) {
    pending().retain(|p| !Arc::ptr_eq(&p.cancel, cancel));
}

/// Cancel the relaunch decision for dead terminal `terminal_id` — its pane
/// was closed. Returns how many were cancelled.
pub fn cancel_for_terminal(terminal_id: &str) -> usize {
    let mut list = pending();
    let before = list.len();
    list.retain(|p| {
        let hit = p.terminal_id == terminal_id;
        if hit {
            p.cancel.store(true, Ordering::Release);
        }
        !hit
    });
    before - list.len()
}

/// Cancel every relaunch decision for `session_id` — it was resumed (or
/// deliberately not resumed) by the restore. Returns how many were cancelled.
pub fn cancel_for_session(session_id: &str) -> usize {
    let mut list = pending();
    let before = list.len();
    list.retain(|p| {
        let hit = p.session_id == session_id;
        if hit {
            p.cancel.store(true, Ordering::Release);
        }
        !hit
    });
    before - list.len()
}

/// The session a lost holder's pane hosted, and where to restore it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaunchTarget {
    pub session_id: String,
    pub page_id: String,
}

/// Why nothing is relaunched for a lost holder's pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoRelaunch {
    /// No reported id names an open record here, and no confirmed open record
    /// is bound to the terminal: no known session to resume.
    NoSession,
    /// The session's id is not shell-safe.
    InvalidSessionId,
    /// The session was marked finished.
    Finished,
}

/// Pick the session to relaunch for `terminal_id` (module docs, "Which
/// session"). `record` looks a session id up in the lifecycle store;
/// `confirmed_on_terminal` is the confirmed open record bound to the terminal.
/// Pure, so the rule is tested without a store.
pub fn relaunch_target(
    terminal_id: &str,
    reported: Option<&SessionStartReport>,
    record: impl Fn(&str) -> Option<TerminalSessionRecord>,
    confirmed_on_terminal: Option<TerminalSessionRecord>,
) -> Result<RelaunchTarget, NoRelaunch> {
    let from_report = reported
        .and_then(|r| record(&r.session_id))
        .filter(|rec| rec.state == "open" && rec.terminal_id == terminal_id);
    let rec = from_report
        .or(confirmed_on_terminal)
        .ok_or(NoRelaunch::NoSession)?;
    if !is_valid_session_id(&rec.claude_session_id) {
        return Err(NoRelaunch::InvalidSessionId);
    }
    if rec.finished_at.is_some() {
        return Err(NoRelaunch::Finished);
    }
    Ok(RelaunchTarget {
        session_id: rec.claude_session_id,
        page_id: rec.page_id,
    })
}

/// What one read of the live registry says about a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveGate {
    /// No live process hosts it: it may be relaunched.
    NotHosted,
    /// A live process hosts it: relaunching would fork it.
    Hosted,
    /// The registry could not be read.
    Unknown,
}

/// `live` is the set of session ids live processes host, or `None` when it
/// could not be read.
pub fn live_gate(live: Option<&HashSet<String>>, session_id: &str) -> LiveGate {
    match live {
        None => LiveGate::Unknown,
        Some(ids) if ids.contains(session_id) => LiveGate::Hosted,
        Some(_) => LiveGate::NotHosted,
    }
}

/// The pauses between the retries of an UNKNOWN verdict: 2 s doubling to a
/// 60 s cap, summing to at most `grace`.
pub fn retry_delays(grace: Duration) -> Vec<Duration> {
    let mut out = Vec::new();
    let mut next = Duration::from_secs(2);
    let mut total = Duration::ZERO;
    while total + next <= grace {
        out.push(next);
        total += next;
        next = (next * 2).min(Duration::from_secs(60));
    }
    out
}

/// Decide and announce the relaunch of `target` after a holder `loss`. Reads
/// the registry with `read`, pauses with `pause` (given the delay), and tells
/// the webview through `emit`. Generic, so the whole policy — the settle
/// window, the withheld banner, the retry of an unknown verdict — is tested
/// without a process table, a clock or a Tauri app.
#[allow(clippy::too_many_arguments)]
pub async fn decide_relaunch<R, RF, P, PF, E, C, B>(
    terminal_id: &str,
    target: &RelaunchTarget,
    loss: HolderLoss,
    mut read: R,
    mut pause: P,
    emit: &E,
    cancelled: &C,
    still_bound: &B,
) where
    E: Fn(Emission),
    C: Fn() -> bool,
    B: Fn() -> bool,
    R: FnMut() -> RF,
    RF: Future<Output = Option<HashSet<String>>>,
    P: FnMut(Duration) -> PF,
    PF: Future<Output = ()>,
{
    let withheld = |reason| {
        if cancelled() {
            return;
        }
        emit(Emission::Withheld(RelaunchWithheld {
            session_id: target.session_id.clone(),
            terminal_id: terminal_id.to_string(),
            page_id: Some(target.page_id.clone()),
            reason,
        }))
    };
    let released = || {
        if cancelled() {
            return;
        }
        // The record must still be bound to the dead terminal: a session
        // resumed into a live pane meanwhile is not relaunched again.
        if !still_bound() {
            info!(
                terminal_id,
                session = %target.session_id,
                "holder relaunch: the session is no longer bound to the dead pane — not released"
            );
            return;
        }
        emit(Emission::Released(SessionReleased {
            session_id: target.session_id.clone(),
            terminal_id: terminal_id.to_string(),
            page_id: Some(target.page_id.clone()),
        }))
    };
    if loss == HolderLoss::Unreachable {
        withheld(WithheldReason::HolderUnreachable);
        return;
    }
    let settle_reads = (LIVE_SETTLE_WINDOW.as_millis() / LIVE_POLL_EVERY.as_millis()).max(1);
    let mut last = LiveGate::Unknown;
    for attempt in 0..settle_reads {
        if attempt > 0 {
            pause(LIVE_POLL_EVERY).await;
        }
        if cancelled() {
            return;
        }
        last = live_gate(read().await.as_ref(), &target.session_id);
        if last == LiveGate::NotHosted {
            released();
            return;
        }
    }
    if last == LiveGate::Hosted {
        withheld(WithheldReason::StillHosted);
        return;
    }
    // Unknown: say so now, and keep trying while the session is restorable.
    withheld(WithheldReason::LivenessUnknown);
    let grace =
        Duration::from_millis(RESTORABLE_PTY_EXIT_MS as u64).saturating_sub(LIVE_SETTLE_WINDOW);
    for delay in retry_delays(grace) {
        pause(delay).await;
        if cancelled() {
            return;
        }
        match live_gate(read().await.as_ref(), &target.session_id) {
            LiveGate::NotHosted => {
                released();
                return;
            }
            LiveGate::Hosted => {
                withheld(WithheldReason::StillHosted);
                return;
            }
            LiveGate::Unknown => {}
        }
    }
}

/// The session ids live `claude` processes host right now, or `None` when
/// that cannot be read — the oracle `terminal_claude_session_list_live` serves
/// the webview, read the same way (an empty process table is a failed read).
async fn live_session_ids() -> Option<HashSet<String>> {
    let config_dirs = crate::terminal::transcript::find_claude_config_dirs();
    let snapshot = crate::process_capture::process_tree::snapshot_process_table_public().await;
    let live_pids =
        crate::session::claude_session_registry::live_pids_from_snapshot(&snapshot).ok()?;
    Some(
        crate::session::claude_session_registry::read_live_sessions(&config_dirs, &live_pids)
            .into_iter()
            .map(|s| s.session_id)
            .collect(),
    )
}

/// The waiter thread's step after a pane's `wait` returned: when `io` reports
/// its PTY holder lost, hand the loss and the pane's latest `session_start`
/// report to `on_lost`. Factored out so the wiring is tested with a fake pane.
pub fn after_pane_exit(
    io: &dyn PaneIo,
    report: &Mutex<Option<SessionStartReport>>,
    on_lost: impl FnOnce(HolderLoss, Option<SessionStartReport>),
) {
    if let Some(loss) = io.holder_loss() {
        let reported = report.lock().ok().and_then(|slot| slot.clone());
        on_lost(loss, reported);
    }
}

/// The target for a lost holder's pane, from the store — or why there is none.
pub fn target_from_store(
    store: &SessionLifecycleStore,
    terminal_id: &str,
    reported: Option<&SessionStartReport>,
) -> Result<RelaunchTarget, NoRelaunch> {
    relaunch_target(
        terminal_id,
        reported,
        |id| store.get(id),
        store.find_confirmed_open_by_terminal(terminal_id),
    )
}

/// The pane `terminal_id` ended because its PTY holder was lost
/// ([`PaneIo::holder_loss`]). Decide its session and, once no live process
/// hosts it, hand it to the webview's restore for a `--resume` relaunch — or
/// put the reason it was not on the dead tab. Called from the waiter thread;
/// returns at once.
pub fn on_holder_lost(
    app: &AppHandle,
    terminal_id: &str,
    reported: Option<SessionStartReport>,
    loss: HolderLoss,
) {
    use tauri::Manager;

    let Some(store) = app
        .try_state::<std::sync::Arc<SessionLifecycleStore>>()
        .map(|s| s.inner().clone())
    else {
        warn!(
            terminal_id,
            "holder relaunch: no lifecycle store — the lost holder's session is not relaunched"
        );
        return;
    };
    let target = match target_from_store(&store, terminal_id, reported.as_ref()) {
        Ok(t) => t,
        Err(why) => {
            info!(
                terminal_id,
                ?why,
                "holder relaunch: the lost holder's pane has no session to relaunch"
            );
            return;
        }
    };
    let (app, terminal_id) = (app.clone(), terminal_id.to_string());
    let cancel = register(&terminal_id, &target.session_id);
    tauri::async_runtime::spawn(async move {
        let emit = |e: Emission| emit_to_webview(&app, &e);
        let cancelled = || cancel.load(Ordering::Acquire);
        let still_bound = || {
            store
                .get(&target.session_id)
                .is_some_and(|r| r.terminal_id == terminal_id)
        };
        decide_relaunch(
            &terminal_id,
            &target,
            loss,
            live_session_ids,
            tokio::time::sleep,
            &emit,
            &cancelled,
            &still_bound,
        )
        .await;
        unregister(&cancel);
    });
}

fn emit_to_webview(app: &AppHandle, emission: &Emission) {
    let result = match emission {
        Emission::Released(r) => {
            warn!(
                terminal_id = %r.terminal_id,
                session = %r.session_id,
                "holder relaunch: the pane's PTY holder was lost — its session is handed to the \
                 restore for a --resume relaunch"
            );
            app.emit(HOLDER_SESSION_RELEASED_EVENT, r)
        }
        Emission::Withheld(w) => {
            warn!(
                terminal_id = %w.terminal_id,
                session = %w.session_id,
                reason = ?w.reason,
                "holder relaunch: NOT relaunched (a forked session is worse than a missing tab) — \
                 the operator is told on the dead tab"
            );
            app.emit(RELAUNCH_WITHHELD_EVENT, w)
        }
    };
    if let Err(e) = result {
        error!(error = %e, "holder relaunch: could not emit to the webview");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    const TERM: &str = "t-lost";

    fn open_record(id: &str, terminal: &str) -> TerminalSessionRecord {
        let mut rec = crate::session::session_lifecycle_store::test_open_record(id);
        rec.terminal_id = terminal.to_string();
        rec.page_id = "page-7".into();
        rec.confirmed_at = Some(1);
        rec
    }

    fn report(id: &str) -> SessionStartReport {
        SessionStartReport {
            session_id: id.into(),
            source: Some("clear".into()),
        }
    }

    fn target() -> RelaunchTarget {
        RelaunchTarget {
            session_id: "s".into(),
            page_id: "page-7".into(),
        }
    }

    #[test]
    fn holder_relaunch_prefers_the_pty_reported_id_over_the_confirmed_record() {
        let reported = open_record("after-clear", TERM);
        let confirmed = open_record("before-clear", TERM);
        let target = relaunch_target(
            TERM,
            Some(&report("after-clear")),
            |id| (id == "after-clear").then(|| reported.clone()),
            Some(confirmed),
        )
        .unwrap();
        assert_eq!(
            target,
            RelaunchTarget {
                session_id: "after-clear".into(),
                page_id: "page-7".into()
            }
        );
    }

    #[test]
    fn holder_relaunch_a_report_for_another_terminal_or_a_closed_record_falls_back() {
        let elsewhere = open_record("moved", "t-other");
        let mut closed = open_record("closed", TERM);
        closed.state = "closed".into();
        for (id, rec) in [("moved", elsewhere), ("closed", closed)] {
            let target = relaunch_target(
                TERM,
                Some(&report(id)),
                |_| Some(rec.clone()),
                Some(open_record("confirmed", TERM)),
            )
            .unwrap();
            assert_eq!(target.session_id, "confirmed", "{id}");
        }
    }

    /// No reported id and no confirmed record → no relaunch: a missing id is
    /// never guessed.
    #[test]
    fn holder_relaunch_a_missing_id_relaunches_nothing() {
        assert_eq!(
            relaunch_target(TERM, None, |_| None, None),
            Err(NoRelaunch::NoSession)
        );
        assert_eq!(
            relaunch_target(TERM, Some(&report("ghost")), |_| None, None),
            Err(NoRelaunch::NoSession)
        );
    }

    #[test]
    fn holder_relaunch_an_unsafe_or_finished_session_is_not_relaunched() {
        let mut bad = open_record("ok", TERM);
        bad.claude_session_id = "a b;c".into();
        assert_eq!(
            relaunch_target(TERM, None, |_| None, Some(bad)),
            Err(NoRelaunch::InvalidSessionId)
        );
        let mut done = open_record("done", TERM);
        done.finished_at = Some(5);
        assert_eq!(
            relaunch_target(TERM, None, |_| None, Some(done)),
            Err(NoRelaunch::Finished)
        );
    }

    #[test]
    fn holder_relaunch_live_gate_reads_the_registry_three_ways() {
        let live: HashSet<String> = ["s-1".to_string()].into();
        assert_eq!(live_gate(Some(&live), "s-1"), LiveGate::Hosted);
        assert_eq!(live_gate(Some(&live), "s-2"), LiveGate::NotHosted);
        assert_eq!(live_gate(None, "s-1"), LiveGate::Unknown);
    }

    #[test]
    fn holder_relaunch_retry_delays_back_off_within_the_grace() {
        let grace = Duration::from_millis(RESTORABLE_PTY_EXIT_MS as u64) - LIVE_SETTLE_WINDOW;
        let delays = retry_delays(grace);
        assert_eq!(delays[0], Duration::from_secs(2));
        assert!(delays.windows(2).all(|w| w[1] >= w[0]), "{delays:?}");
        assert!(delays.iter().all(|d| *d <= Duration::from_secs(60)));
        let total: Duration = delays.iter().sum();
        assert!(
            total <= grace && total + Duration::from_secs(60) > grace,
            "{total:?}"
        );
    }

    /// Run [`decide_relaunch`] over a scripted registry (`None` = unreadable;
    /// the last entry repeats), with no-op pauses; return what it emitted and
    /// how many pauses it took.
    async fn run(loss: HolderLoss, reads: Vec<Option<HashSet<String>>>) -> (Vec<Emission>, usize) {
        let reads = RefCell::new(VecDeque::from(reads));
        let pauses = RefCell::new(0usize);
        let emitted = RefCell::new(Vec::new());
        decide_relaunch(
            TERM,
            &target(),
            loss,
            || {
                let next = {
                    let mut q = reads.borrow_mut();
                    if q.len() > 1 {
                        q.pop_front().unwrap()
                    } else {
                        q.front().cloned().unwrap()
                    }
                };
                async move { next }
            },
            |_| {
                *pauses.borrow_mut() += 1;
                async {}
            },
            &|e| emitted.borrow_mut().push(e),
            &|| false,
            &|| true,
        )
        .await;
        let pauses = *pauses.borrow();
        (emitted.into_inner(), pauses)
    }

    fn released() -> Emission {
        Emission::Released(SessionReleased {
            session_id: "s".into(),
            terminal_id: TERM.into(),
            page_id: Some("page-7".into()),
        })
    }

    fn withheld(reason: WithheldReason) -> Emission {
        Emission::Withheld(RelaunchWithheld {
            session_id: "s".into(),
            terminal_id: TERM.into(),
            page_id: Some("page-7".into()),
            reason,
        })
    }

    /// The relaunch happens ONLY once no live process hosts the session: a
    /// hung-up `claude` still exiting is waited out; one that lives on is
    /// never relaunched over and the operator is told; an unreadable registry
    /// is said so and retried; an unreachable holder is never relaunched.
    #[tokio::test]
    async fn holder_relaunch_releases_only_when_no_live_process_hosts_the_session() {
        let hosted: HashSet<String> = ["s".to_string()].into();
        let gone: HashSet<String> = HashSet::new();

        // Exiting: hosted twice, then gone → released on the third read.
        let (out, pauses) = run(
            HolderLoss::Ended,
            vec![
                Some(hosted.clone()),
                Some(hosted.clone()),
                Some(gone.clone()),
            ],
        )
        .await;
        assert_eq!(out, vec![released()]);
        assert_eq!(pauses, 2);

        // Lives on for the whole window → banner, never released.
        let (out, _) = run(HolderLoss::Ended, vec![Some(hosted.clone())]).await;
        assert_eq!(out, vec![withheld(WithheldReason::StillHosted)]);

        // Unreadable throughout → banner, retried to the grace, never released.
        let (out, pauses) = run(HolderLoss::Ended, vec![None]).await;
        assert_eq!(out, vec![withheld(WithheldReason::LivenessUnknown)]);
        let settle = (LIVE_SETTLE_WINDOW.as_millis() / LIVE_POLL_EVERY.as_millis()) as usize;
        let grace = Duration::from_millis(RESTORABLE_PTY_EXIT_MS as u64) - LIVE_SETTLE_WINDOW;
        assert_eq!(pauses, settle - 1 + retry_delays(grace).len());

        // Unreadable for the window, then readable and gone → released late.
        let mut reads = vec![None; settle + 1];
        reads.push(Some(gone.clone()));
        let (out, _) = run(HolderLoss::Ended, reads).await;
        assert_eq!(
            out,
            vec![withheld(WithheldReason::LivenessUnknown), released()]
        );

        // Unreadable, then readable and still hosted → the banner says so.
        let mut reads = vec![None; settle + 1];
        reads.push(Some(hosted.clone()));
        let (out, _) = run(HolderLoss::Ended, reads).await;
        assert_eq!(
            out,
            vec![
                withheld(WithheldReason::LivenessUnknown),
                withheld(WithheldReason::StillHosted)
            ]
        );

        // An unreachable holder: never relaunched, not even read.
        let (out, pauses) = run(HolderLoss::Unreachable, vec![Some(gone)]).await;
        assert_eq!(out, vec![withheld(WithheldReason::HolderUnreachable)]);
        assert_eq!(pauses, 0);
    }

    /// Review r1, L3: the production path from the store — the reported id's
    /// record supplies the page — through the decision to the emission.
    #[tokio::test]
    async fn holder_relaunch_from_the_store_emits_the_session_and_its_page() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        let mut rec = open_record("0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0", TERM);
        rec.page_id = "page-42".into();
        store.record_open(rec);
        store.confirm_session("0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0");

        let reported = SessionStartReport {
            session_id: "0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0".into(),
            source: Some("startup".into()),
        };
        let target = target_from_store(&store, TERM, Some(&reported)).unwrap();
        assert_eq!(target.page_id, "page-42");
        let emitted = RefCell::new(Vec::new());
        decide_relaunch(
            TERM,
            &target,
            HolderLoss::Ended,
            || async { Some(HashSet::new()) },
            |_| async {},
            &|e| emitted.borrow_mut().push(e),
            &|| false,
            &|| {
                store
                    .get("0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0")
                    .is_some_and(|r| r.terminal_id == TERM)
            },
        )
        .await;
        assert_eq!(
            emitted.into_inner(),
            vec![Emission::Released(SessionReleased {
                session_id: "0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0".into(),
                terminal_id: TERM.into(),
                page_id: Some("page-42".into()),
            })]
        );
        // And a pane whose terminal has no session relaunches nothing.
        assert_eq!(
            target_from_store(&store, "t-plain-shell", None),
            Err(NoRelaunch::NoSession)
        );
    }

    /// Review r2, N2: a decision CANCELLED mid-wait (its pane closed, its
    /// session resumed elsewhere) emits nothing more, and one whose session
    /// was rebound to another pane meanwhile never releases it.
    #[tokio::test]
    async fn holder_relaunch_a_cancelled_or_rebound_decision_releases_nothing() {
        let gone: HashSet<String> = HashSet::new();
        // Cancelled during the settle window: unreadable, then the pane closes.
        let reads = RefCell::new(0usize);
        let cancel = Arc::new(AtomicBool::new(false));
        let emitted = RefCell::new(Vec::new());
        decide_relaunch(
            TERM,
            &target(),
            HolderLoss::Ended,
            || {
                *reads.borrow_mut() += 1;
                if *reads.borrow() == 3 {
                    cancel.store(true, Ordering::Release);
                }
                async { None }
            },
            |_| async {},
            &|e| emitted.borrow_mut().push(e),
            &|| cancel.load(Ordering::Acquire),
            &|| true,
        )
        .await;
        assert!(emitted.borrow().is_empty(), "{:?}", emitted.borrow());
        assert_eq!(*reads.borrow(), 3, "no read after the cancel");

        // Rebound to a live pane before release: nothing released.
        let emitted = RefCell::new(Vec::new());
        decide_relaunch(
            TERM,
            &target(),
            HolderLoss::Ended,
            || {
                let g = gone.clone();
                async move { Some(g) }
            },
            |_| async {},
            &|e| emitted.borrow_mut().push(e),
            &|| false,
            &|| false,
        )
        .await;
        assert!(emitted.borrow().is_empty());

        // The registry: cancel by terminal and by session.
        let a = register("t-cancel-a", "s-cancel");
        let b = register("t-cancel-b", "s-cancel");
        assert_eq!(cancel_for_terminal("t-cancel-a"), 1);
        assert!(a.load(Ordering::Acquire) && !b.load(Ordering::Acquire));
        assert_eq!(cancel_for_session("s-cancel"), 1);
        assert!(b.load(Ordering::Acquire));
        // A second loss on the same terminal cancels the first decision.
        let first = register("t-cancel-c", "s1");
        let second = register("t-cancel-c", "s2");
        assert!(first.load(Ordering::Acquire) && !second.load(Ordering::Acquire));
        unregister(&second);
        assert_eq!(cancel_for_terminal("t-cancel-c"), 0);
    }

    /// A pane whose holder was lost — and only such a pane — reaches
    /// `on_lost`, carrying the loss and the pane's latest report (review r1,
    /// L3: the waiter's wiring).
    #[test]
    fn holder_relaunch_after_pane_exit_wires_only_a_holder_loss() {
        use crate::terminal::pane_io::{CredentialScrub, InertPaneIo};
        use std::io::{Read, Write};

        struct Lost(HolderLoss);
        impl PaneIo for Lost {
            fn reader(&self) -> Result<Box<dyn Read + Send>, String> {
                InertPaneIo.reader()
            }
            fn writer(&self) -> Result<Box<dyn Write + Send>, String> {
                InertPaneIo.writer()
            }
            fn resize(&self, c: u16, r: u16) -> Result<(), String> {
                InertPaneIo.resize(c, r)
            }
            fn wait(&self) -> Result<i32, String> {
                Err("holder gone".into())
            }
            fn kill(&self, b: Duration) -> Result<(), String> {
                InertPaneIo.kill(b)
            }
            fn set_paused(&self, p: bool) -> Result<(), String> {
                InertPaneIo.set_paused(p)
            }
            fn pid(&self) -> Option<u32> {
                None
            }
            fn holder_loss(&self) -> Option<HolderLoss> {
                Some(self.0)
            }
            fn credential_scrub(&self) -> CredentialScrub {
                CredentialScrub::ScrubbedOutOfProcess
            }
            fn release(&self, b: Duration) -> Result<(), String> {
                InertPaneIo.release(b)
            }
        }

        let slot = Mutex::new(Some(report("after-clear")));
        let mut seen = None;
        after_pane_exit(&Lost(HolderLoss::Ended), &slot, |loss, rep| {
            seen = Some((loss, rep))
        });
        assert_eq!(seen, Some((HolderLoss::Ended, Some(report("after-clear")))));

        let mut called = false;
        after_pane_exit(&InertPaneIo, &slot, |_, _| called = true);
        assert!(
            !called,
            "a child exit, a kill or a detach relaunches nothing"
        );
    }
}
