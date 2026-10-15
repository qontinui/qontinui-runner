//! Tier 2: relaunch a session whose PTY holder died under a live runner.
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
//! record and surfaces a failure — none of which is duplicated here. The two
//! deaths differ only in WHO notices: the sweep, for a holder that died while
//! no runner watched; the pane's waiter thread, for one that dies under a live
//! runner ([`on_holder_lost`]).
//!
//! # Never a double `--resume`
//!
//! A holder's death hangs up its PTY, and that normally ends the `claude` in
//! it — but not instantly, and not necessarily (a process can ignore
//! `SIGHUP`). Relaunching a session some live process still hosts forks its
//! transcript. So the release is gated on the same oracle the webview's
//! `decideColdResume` reads (Claude Code's per-process registry, PID-verified:
//! `claude_session_registry::read_live_sessions`), polled for up to
//! [`LIVE_SETTLE_WINDOW`] while the hung-up process exits:
//!
//! - not hosted → released;
//! - still hosted when the window ends → NOT released (the process lives on;
//!   the session was not lost);
//! - liveness unreadable for the whole window → NOT released (fail closed: a
//!   missing relaunch is recoverable by hand, a forked session is not).
//!
//! The webview re-checks the same oracle before it types anything, so the
//! gate holds even if the process re-appears between the two.
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
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tracing::{error, info, warn};

use super::holder_boot_sweep::{SessionReleased, HOLDER_SESSION_RELEASED_EVENT};
use super::session_start_sideband::SessionStartReport;
use crate::session::session_id::is_valid_session_id;
use crate::session::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};

/// How long the release waits for a hung-up `claude` to leave the live
/// registry. A `SIGHUP`'d CLI exits in well under a second; the bound covers a
/// loaded box and the registry file's removal.
pub const LIVE_SETTLE_WINDOW: Duration = Duration::from_secs(15);

/// How often the live registry is re-read inside that window.
pub const LIVE_POLL_EVERY: Duration = Duration::from_secs(1);

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

/// Read the live registry until it says `session_id` is NOT hosted, at most
/// `attempts` reads `every` apart. Returns the last verdict: `NotHosted` only
/// on a read that said so; otherwise `Hosted` if any read saw it hosted at the
/// end, else `Unknown`. Generic over the read and the pause, so the polling
/// policy is tested without a process table or a clock.
pub async fn await_not_hosted<R, RF, P, PF>(
    session_id: &str,
    attempts: u32,
    mut read: R,
    mut pause: P,
) -> LiveGate
where
    R: FnMut() -> RF,
    RF: Future<Output = Option<HashSet<String>>>,
    P: FnMut() -> PF,
    PF: Future<Output = ()>,
{
    let mut last = LiveGate::Unknown;
    for attempt in 0..attempts.max(1) {
        if attempt > 0 {
            pause().await;
        }
        last = live_gate(read().await.as_ref(), session_id);
        if last == LiveGate::NotHosted {
            return last;
        }
    }
    last
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

/// The pane `terminal_id` ended because its PTY holder was lost
/// ([`super::pane_io::PaneIo::ended_by_holder_loss`]). Decide its session and,
/// once no live process hosts it, hand it to the webview's restore for a
/// `--resume` relaunch. Called from the waiter thread; returns at once.
pub fn on_holder_lost(app: &AppHandle, terminal_id: &str, reported: Option<SessionStartReport>) {
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
    let target = match relaunch_target(
        terminal_id,
        reported.as_ref(),
        |id| store.get(id),
        store.find_confirmed_open_by_terminal(terminal_id),
    ) {
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
    tauri::async_runtime::spawn(async move {
        let attempts = (LIVE_SETTLE_WINDOW.as_millis() / LIVE_POLL_EVERY.as_millis()) as u32;
        let gate = await_not_hosted(&target.session_id, attempts, live_session_ids, || {
            tokio::time::sleep(LIVE_POLL_EVERY)
        })
        .await;
        if gate != LiveGate::NotHosted {
            warn!(
                terminal_id = %terminal_id,
                session = %target.session_id,
                ?gate,
                "holder relaunch: NOT relaunching — a live process still hosts the session, or \
                 liveness could not be read (a forked session is worse than a missing tab)"
            );
            return;
        }
        let released = SessionReleased {
            session_id: target.session_id.clone(),
            terminal_id: terminal_id.clone(),
            page_id: Some(target.page_id),
        };
        warn!(
            terminal_id = %terminal_id,
            session = %released.session_id,
            "holder relaunch: the pane's PTY holder was lost — its session is handed to the \
             restore for a --resume relaunch"
        );
        if let Err(e) = app.emit(HOLDER_SESSION_RELEASED_EVENT, &released) {
            error!(error = %e, "holder relaunch: could not emit {HOLDER_SESSION_RELEASED_EVENT}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

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
            argv: None,
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

    /// The relaunch happens ONLY once no live process hosts the session: a
    /// hung-up `claude` still exiting is waited out, one that lives on is
    /// never relaunched over, and an unreadable registry fails closed.
    #[tokio::test]
    async fn holder_relaunch_waits_until_no_live_process_hosts_the_session() {
        let hosted: HashSet<String> = ["s".to_string()].into();
        let gone: HashSet<String> = HashSet::new();

        // Exiting: hosted twice, then gone → released on the third read.
        let reads = RefCell::new(vec![
            Some(gone.clone()),
            Some(hosted.clone()),
            Some(hosted.clone()),
        ]);
        let pauses = RefCell::new(0);
        let gate = await_not_hosted(
            "s",
            5,
            || {
                let next = reads.borrow_mut().pop().unwrap();
                async move { next }
            },
            || {
                *pauses.borrow_mut() += 1;
                async {}
            },
        )
        .await;
        assert_eq!(gate, LiveGate::NotHosted);
        assert_eq!(*pauses.borrow(), 2);

        // Lives on for the whole window → never released.
        let gate = await_not_hosted(
            "s",
            4,
            || {
                let h = hosted.clone();
                async move { Some(h) }
            },
            || async {},
        )
        .await;
        assert_eq!(gate, LiveGate::Hosted);

        // Unreadable throughout → fail closed.
        let gate = await_not_hosted("s", 3, || async { None }, || async {}).await;
        assert_eq!(gate, LiveGate::Unknown);

        // Hosted, then unreadable at the end → still not released.
        let reads = RefCell::new(vec![None, Some(hosted.clone())]);
        let gate = await_not_hosted(
            "s",
            2,
            || {
                let next = reads.borrow_mut().pop().unwrap();
                async move { next }
            },
            || async {},
        )
        .await;
        assert_ne!(gate, LiveGate::NotHosted);
    }
}
