//! "Is the resumed CLI alive?" — the proof a `--resume` respawn must pass
//! before anything treats it as done, and the retry-then-rollback that runs
//! when it does not.
//!
//! Plan `2026-10-20-account-migration-respawn-dies-during-cli-reinstall-churn`
//! D2 / Phase 4. A successful PTY spawn is NOT a successful respawn: on
//! merytshost (measured in that plan's Phase 1) five of sixty account migrations spawned, closed the old pane,
//! logged `session migrated`, and then watched the new child exit 127 fourteen
//! seconds later — and `poll-dead` closed the row that had been re-keyed onto
//! it. Every one of those sessions was lost.
//!
//! ## The proof ([`judge`])
//!
//! The child is still running AND the screen reads Busy (`esc to interrupt`)
//! or Ready (the idle input prompt, debounced over two identical reads) — the
//! existing [`super::account_migration::probe_idle_prompt`] /
//! [`super::account_migration::ReadyDebounce`]. Only a CLI that is painting can
//! satisfy it. A child exit is [`LivenessVerdict::Died`] at once, never after
//! the deadline; a live child that has painted nothing by the deadline is
//! [`LivenessVerdict::Unconfirmed`] — UNKNOWN, which no caller resolves to the
//! destructive arm.
//!
//! ## The retry ([`confirm_respawn`])
//!
//! The session's closes are HELD for the whole respawn ([`SessionCloseHold`]
//! over `SessionLifecycleStore::hold_session_closes`), from BEFORE the first
//! spawn — the spawn re-keys the row onto a terminal id nobody knows until it
//! returns. A child that exits 127 makes the frontend send its `pty-exit` close
//! at once, while the row is still bound to that pane, and applying it would
//! enqueue a coord `Closed` and revoke the workdir nonce the still-running OLD
//! pane uses (the old pane itself is excluded from the hold). A held close is
//! answered `HeldInFlight` and remembered per terminal.
//!
//! A USER close (`explicit`) of the pane under proof ends the respawn: nothing
//! is retried behind it. With an old pane the session goes back onto it; with
//! none the user's close is applied ([`RespawnOutcome::ClosedByUser`]).
//!
//! On a death the next pane is spawned FIRST (each retry consuming an attempt
//! whether or not its spawn succeeds), then the dead pane is marked superseded
//! by it and closed — so a late exit-close is answered `TerminalSuperseded`
//! and never closes the row the live pane now owns. When the budget is spent
//! and the caller had an OLD pane (an account migration), the pre-migration
//! record is restored EXACTLY with `restore_record_exact(old.clone())` and the
//! last dead pane is marked superseded by the OLD terminal before it closes.
//! With no old pane, the session ended on the last dead pane, and its row is
//! closed for real — the held close is applied, or one is made on its behalf.
//! A CANCELLED respawn (task abort, shutdown) restores the old record exactly
//! when there is one, and applies the current pane's held close when not.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use tracing::{info, warn};

use super::account_migration::{probe_idle_prompt, IdlePromptProbe, ReadyDebounce};
use crate::session::session_lifecycle_store::{
    HeldClose, SessionLifecycleStore, TerminalSessionRecord, USER_CLOSE_REASON,
};

/// How long a resumed CLI gets to paint — the existing idle watcher's bound.
pub(crate) const RESUME_LIVENESS_DEADLINE: Duration = Duration::from_secs(180);
/// Grid/child poll cadence.
pub(crate) const RESUME_LIVENESS_POLL: Duration = Duration::from_millis(500);
/// Respawn attempts in all (the first spawn included) before giving up.
pub(crate) const MAX_RESPAWN_ATTEMPTS: usize = 3;

/// The pause before retry number `retry` (1-based), on the headless spawn
/// helper's own budget ([`crate::claude_cli_spawn::RETRY_DELAYS`]); past its
/// end the last delay repeats.
pub(crate) fn retry_delay(retry: usize) -> Duration {
    let d = &crate::claude_cli_spawn::RETRY_DELAYS;
    d[retry.saturating_sub(1).min(d.len() - 1)]
}

/// A value that MUST be settled exactly once — the account migration's lifted
/// continuation registration and its anchor permit, carried across the
/// respawn's `await`. [`Self::settle`] consumes it, so it cannot settle twice.
///
/// Dropping it unsettled — the migration future cancelled mid-respawn (task
/// abort, shutdown), or an early exit added inside the window — runs the
/// `on_drop` fallback with a `warn!` instead of silently releasing the anchor.
/// It never panics: a cancelled future is an ordinary event, and a panic in a
/// drop during an unwind or a task abort would abort the process.
#[must_use = "an owed settle must be settled, or its drop fallback runs"]
pub(crate) struct OwedSettle<T> {
    inner: Option<T>,
    on_drop: Option<Box<dyn FnOnce(T) + Send>>,
}

impl<T> OwedSettle<T> {
    /// `on_drop` settles the value safely when it is dropped unsettled (for the
    /// migration: re-pin onto the OLD terminal, so the anchor is kept).
    pub(crate) fn new(inner: Option<T>, on_drop: impl FnOnce(T) + Send + 'static) -> Self {
        Self {
            inner,
            on_drop: Some(Box::new(on_drop)),
        }
    }

    /// The carried value, for reads before the settle.
    pub(crate) fn get(&self) -> Option<&T> {
        self.inner.as_ref()
    }

    /// Hand the value (when there is one) to `f`, exactly once.
    pub(crate) fn settle(mut self, f: impl FnOnce(T)) {
        if let Some(v) = self.inner.take() {
            f(v);
        }
    }
}

impl<T> Drop for OwedSettle<T> {
    fn drop(&mut self) {
        if let (Some(v), Some(fallback)) = (self.inner.take(), self.on_drop.take()) {
            warn!("respawn-liveness: an owed settle was dropped unsettled (cancelled respawn?) — settling it through its fallback");
            fallback(v);
        }
    }
}

/// What one look at a resumed pane concludes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LivenessVerdict {
    /// The child runs and the CLI is painting (busy, or idle at its prompt).
    Alive,
    /// The child exited (`Some(code)` when the waiter recorded one), or the
    /// pane itself is gone.
    Died(Option<i32>),
    /// Not decided yet — read again.
    Pending,
    /// The child still runs but painted nothing by the deadline. UNKNOWN.
    Unconfirmed,
}

/// Pure classifier. `child_exited`: `None` while the child runs, `Some(code)`
/// once it exited. `debounce_settled` is meaningful only for a `Ready` probe.
pub(crate) fn judge(
    child_exited: Option<Option<i32>>,
    probe: &IdlePromptProbe,
    debounce_settled: bool,
    past_deadline: bool,
) -> LivenessVerdict {
    if let Some(code) = child_exited {
        return LivenessVerdict::Died(code);
    }
    match probe {
        IdlePromptProbe::Busy => LivenessVerdict::Alive,
        IdlePromptProbe::Ready if debounce_settled => LivenessVerdict::Alive,
        _ if past_deadline => LivenessVerdict::Unconfirmed,
        _ => LivenessVerdict::Pending,
    }
}

/// One read of a pane: whether its child exited, and what its grid shows.
pub(crate) struct PaneReading {
    pub exited: Option<Option<i32>>,
    pub screen: crate::terminal::graceful_exit::ScreenText,
}

/// The poll loop over an injected reader (`None` = the pane is gone), so the
/// arms are testable without a PTY. Returns only a terminal verdict.
pub(crate) async fn await_liveness_with<R>(
    mut read: R,
    poll: Duration,
    deadline: Duration,
) -> LivenessVerdict
where
    R: FnMut() -> Option<PaneReading>,
{
    let until = tokio::time::Instant::now() + deadline;
    let mut debounce = ReadyDebounce::default();
    loop {
        let Some(reading) = read() else {
            return LivenessVerdict::Died(None);
        };
        let probe = probe_idle_prompt(&reading.screen);
        let settled = if probe == IdlePromptProbe::Ready {
            debounce.settled(reading.screen)
        } else {
            debounce.reset();
            false
        };
        let verdict = judge(
            reading.exited,
            &probe,
            settled,
            tokio::time::Instant::now() >= until,
        );
        if verdict != LivenessVerdict::Pending {
            return verdict;
        }
        tokio::time::sleep(poll).await;
    }
}

/// Watch `terminal_id` until its resumed CLI is proven alive, dies, or the
/// deadline passes. Child state first (`is_alive` / `exit_code`), then the grid.
pub(crate) async fn await_resumed_cli(
    tm: &Arc<crate::terminal::TerminalManager>,
    terminal_id: &str,
    deadline: Duration,
) -> LivenessVerdict {
    let verdict = await_liveness_with(
        || {
            let session = tm.get(terminal_id)?;
            let exited = (!session.is_alive()).then(|| session.exit_code());
            Some(PaneReading {
                exited,
                screen: session.grid_screen(),
            })
        },
        RESUME_LIVENESS_POLL,
        deadline,
    )
    .await;
    info!(terminal_id, verdict = ?verdict, "respawn-liveness: resumed CLI judged");
    verdict
}

/// The effects [`confirm_respawn`] drives. Each caller of the resume seam
/// implements it over its own spawn spec; the tests implement it over a fake.
pub(crate) trait RespawnEffects {
    /// The Claude session being respawned. Its closes are held for the whole
    /// respawn, from BEFORE the first spawn re-keys its row.
    fn claude_session_id(&self) -> &str;
    /// Spawn the next resumed pane (async binary pre-step, then the seam).
    /// `Ok(terminal_id)`; the spawn re-keys the session's row onto it.
    fn spawn_next(&mut self) -> impl Future<Output = Result<String, String>> + Send;
    /// Judge one pane ([`await_resumed_cli`] in production).
    fn await_liveness(&mut self, terminal_id: &str)
        -> impl Future<Output = LivenessVerdict> + Send;
    /// The pause before retry number `retry` (1-based).
    fn pause_before_retry(&mut self, retry: usize) -> impl Future<Output = ()> + Send;
    /// Close / probe panes. An owned handle, so a CANCELLED respawn (whose
    /// `&mut self` is gone with its future) can still close its pane.
    fn panes(&self) -> Arc<dyn PaneOps>;
}

/// The pane operations a respawn needs beyond spawning.
pub(crate) trait PaneOps: Send + Sync {
    /// Close a pane. `Err` when it was already gone.
    fn close(&self, terminal_id: &str) -> Result<(), String>;
    /// Is the pane still there with its child running?
    fn alive(&self, terminal_id: &str) -> bool;
    /// Does the pane still HOST a CLI — a live `claude` process in its
    /// inclusive subtree? `alive` only says the PTY child runs: a restored pane
    /// is a shell that `claude --resume` was typed into, so its CLI can have
    /// exited while the shell lives on. `false` whenever it cannot be shown.
    fn hosts_claude<'a>(&'a self, terminal_id: &'a str) -> BoxFuture<'a, bool>;
}

/// [`PaneOps`] over the real [`crate::terminal::TerminalManager`].
pub(crate) struct ManagerPanes(pub(crate) Arc<crate::terminal::TerminalManager>);

impl PaneOps for ManagerPanes {
    fn close(&self, terminal_id: &str) -> Result<(), String> {
        self.0.close(terminal_id)
    }
    fn alive(&self, terminal_id: &str) -> bool {
        self.0.get(terminal_id).is_some_and(|s| s.is_alive())
    }
    fn hosts_claude<'a>(&'a self, terminal_id: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let Some(pid) = self
                .0
                .get(terminal_id)
                .filter(|s| s.is_alive())
                .and_then(|s| s.child_pid())
            else {
                return false;
            };
            // The lifecycle poll's own liveness signal. Reference 0 disables
            // the PID-reuse guard: the pid is the child of a pane this runner
            // holds and whose child is still running, so it cannot have been
            // recycled under it. An empty (failed) snapshot finds no claude —
            // no evidence, so no rollback onto the pane.
            let snapshot =
                crate::process_capture::process_tree::snapshot_process_table_public().await;
            crate::process_capture::process_tree::claude_present_in_inclusive_subtree(
                pid, &snapshot, 0,
            )
        })
    }
}

/// How a respawn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RespawnOutcome {
    /// The FIRST spawn failed: nothing moved, no pane was created.
    NotSpawned(String),
    /// The resumed CLI is painting on `terminal_id`. With an old pane, it has
    /// been marked superseded and closed.
    Alive {
        terminal_id: String,
        attempts: usize,
    },
    /// `terminal_id` runs but never painted, and there was no old pane PROVEN to
    /// still host a CLI to fall back on: it keeps the row. An old pane that is
    /// alive without a CLI (a bare shell) is left open, marked superseded by
    /// `terminal_id`. UNKNOWN, never resolved to the destructive arm.
    Unconfirmed { terminal_id: String },
    /// A migration's new CLI on `terminal_id` never painted while the OLD pane
    /// still hosted a running CLI: the session was kept on the old pane
    /// (restored exactly) and the unproven pane superseded and closed.
    UnconfirmedRolledBack { terminal_id: String },
    /// The USER closed a pane (an `explicit` close). Of the pane being proven:
    /// nothing is retried; with an old pane still running the pre-respawn
    /// record is restored onto it, otherwise the user's close is applied. Of
    /// the OLD pane (a migration): the migration is abandoned, the pane under
    /// proof closed, and the session closed as the user asked.
    ClosedByUser { attempts: usize },
    /// Every attempt died (or a retry could not spawn). The last dead pane is
    /// closed; with an old pane the pre-respawn record is restored onto it.
    Died {
        exit_code: Option<i32>,
        attempts: usize,
        retry_spawn_error: Option<String>,
    },
}

impl RespawnOutcome {
    /// The `respawn-died: exit <code> after <n> attempts` reason.
    pub(crate) fn died_reason(exit_code: Option<i32>, attempts: usize) -> String {
        let code = exit_code.map_or_else(|| "unknown".to_string(), |c| c.to_string());
        format!("respawn-died: exit {code} after {attempts} attempts")
    }
}

/// What became of the OLD pane (an account migration's) while the respawn ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OldPane {
    /// Still there, its child running, nobody closed it.
    Running,
    /// The USER closed it: the session is to end, not to move.
    ClosedByUser,
    /// Gone some other way (its CLI exited, or the pane vanished). Nothing to
    /// roll back onto.
    Gone,
}

/// The close hold on the session for the length of one respawn
/// ([`SessionLifecycleStore::hold_session_closes`]), installed BEFORE the first
/// spawn — the spawn re-keys the row onto a terminal id nobody knows until it
/// returns, and a child that exits in that gap must not close it. The OLD pane
/// (an account migration's) is held too, so a close of it is SEEN — the user
/// closing it ends the migration ([`OldPane::ClosedByUser`]) — rather than
/// applied behind the respawn's back and then undone by the next re-key.
///
/// Every way out settles it: [`Self::finish_replaying`] (the current pane's
/// fate is its own — its held close is applied), [`Self::finish_dropping`] (a
/// rollback or an abort owns the row — held closes concern panes that are
/// gone), and `Drop` — a CANCELLED respawn — which, with an old pane still
/// running, restores it exactly and closes the pane under proof, and otherwise
/// applies what the user or the exit asked for. A hold never outlives the
/// respawn.
struct SessionCloseHold<'s> {
    store: &'s SessionLifecycleStore,
    panes: Arc<dyn PaneOps>,
    session: String,
    old: Option<TerminalSessionRecord>,
    current: Option<String>,
    /// Every pane this respawn spawned — the current one and each it retired.
    /// Only a user close naming one of THESE is the user's answer to the
    /// respawn; the old pane has its own check ([`Self::old_pane`]).
    spawned: Vec<String>,
    settled: bool,
}

impl<'s> SessionCloseHold<'s> {
    fn new(
        store: &'s SessionLifecycleStore,
        panes: Arc<dyn PaneOps>,
        session: &str,
        old: Option<&TerminalSessionRecord>,
    ) -> Self {
        store.hold_session_closes(session);
        Self {
            store,
            panes,
            session: session.to_string(),
            old: old.cloned(),
            current: None,
            spawned: Vec::new(),
            settled: false,
        }
    }

    fn set_current(&mut self, terminal_id: &str) {
        self.current = Some(terminal_id.to_string());
        if !self.spawned.iter().any(|t| t == terminal_id) {
            self.spawned.push(terminal_id.to_string());
        }
    }

    /// Did the USER close a pane this respawn SPAWNED — the one under proof or
    /// one it already retired? (The old pane is [`Self::old_pane`]'s.) Every
    /// spawned pane is read, so a user close landing on a pane after it was
    /// retired is not lost while a retry runs. A close naming a terminal the
    /// respawn never spawned — a stale tab still carrying the session id, which
    /// the hold files under the session because no open row owns it — is NOT
    /// the user's answer: it stays held and is dropped with the hold, as
    /// applying it would have closed nothing.
    fn user_closed(&self) -> bool {
        self.store
            .peek_held_user_close(&self.session, &self.spawned)
            .is_some()
    }

    /// Take the user close [`Self::user_closed`] sees.
    fn take_user_close(&self) -> Option<HeldClose> {
        let c = self
            .store
            .peek_held_user_close(&self.session, &self.spawned)?;
        self.take(&c.terminal_id)
    }

    /// Is any close held for `terminal_id`? A pane with one has exited (or
    /// been closed), whatever its last screen read said.
    fn has_held(&self, terminal_id: &str) -> bool {
        self.store
            .peek_held_close(&self.session, terminal_id)
            .is_some()
    }

    fn take(&self, terminal_id: &str) -> Option<HeldClose> {
        self.store.take_held_close(&self.session, terminal_id)
    }

    /// `None` without an old pane.
    fn old_pane(&self) -> Option<OldPane> {
        let t = self.old.as_ref()?.terminal_id.as_str();
        Some(match self.store.peek_held_close(&self.session, t) {
            Some(c) if c.is_user_close() => OldPane::ClosedByUser,
            Some(_) => OldPane::Gone,
            None if !self.panes.alive(t) => OldPane::Gone,
            None => OldPane::Running,
        })
    }

    /// `to` owns the row now: mark `from` superseded by it and close `from`. A
    /// close of `from` that already arrived means the mark will never be
    /// consumed, so it is withdrawn; that close is returned, so a USER close
    /// of the pane being retired can end the respawn instead of being dropped.
    fn retire(&self, from: &str, to: &str) -> Option<HeldClose> {
        self.store.mark_terminal_superseded(from, to);
        let taken = self.take(from);
        if taken.is_some() {
            self.store.unmark_terminal_superseded(from);
        }
        if let Err(e) = self.panes.close(from) {
            self.store.unmark_terminal_superseded(from);
            info!(from, to, error = %e, "respawn-liveness: superseded pane was already gone");
        }
        taken
    }

    /// End the hold and apply `terminal_id`'s held close, if any; every other
    /// held close concerns a pane that is gone and is dropped. `Some` when a
    /// close was applied.
    fn finish_replaying(mut self, terminal_id: Option<&str>) -> Option<HeldClose> {
        self.settled = true;
        finish_replaying(self.store, &self.session, terminal_id)
    }

    /// End the hold and apply EVERY held close — nothing moved (the first
    /// spawn failed), so each one meant what it said.
    fn finish_replaying_all(mut self) {
        self.settled = true;
        for c in self.store.release_session_hold(&self.session) {
            let t = (!c.terminal_id.is_empty()).then_some(c.terminal_id.as_str());
            self.store
                .record_close_checked(&c.claude_session_id, t, &c.reason);
        }
    }

    /// End the hold and drop what it held: a rollback or an abort decided the
    /// row's fate, and every held close was about a pane that is now gone.
    fn finish_dropping(mut self) {
        self.settled = true;
        log_dropped(self.store.release_session_hold(&self.session));
    }

    /// The user closed the OLD pane: the session ENDS. Close the pane under
    /// proof (its exit-close is still held, and dropped), end the hold, then
    /// apply the user's close to the row wherever it now sits.
    fn abort_for_closed_old(self, current: &str) {
        let user = self.old.as_ref().and_then(|o| self.take(&o.terminal_id));
        if let Err(e) = self.panes.close(current) {
            info!(terminal_id = %current, error = %e, "respawn-liveness: pane under proof already gone");
        }
        let (store, session) = (self.store, self.session.clone());
        self.finish_dropping();
        let reason = user.map_or_else(|| USER_CLOSE_REASON.to_string(), |c| c.reason);
        let outcome = store.record_close_checked(&session, None, &reason);
        warn!(
            session = %session,
            outcome = ?outcome,
            "respawn-liveness: the user closed the OLD pane mid-migration — migration abandoned, the session closed as the user asked"
        );
    }
}

fn finish_replaying(
    store: &SessionLifecycleStore,
    session: &str,
    terminal_id: Option<&str>,
) -> Option<HeldClose> {
    // One lock for the take AND the release: a close landing between two
    // separate calls would be neither taken nor held, and lost.
    let (mine, rest) = store.release_session_hold_taking(session, terminal_id);
    log_dropped(rest);
    let held = mine?;
    let outcome = store.record_close_checked(
        &held.claude_session_id,
        Some(&held.terminal_id),
        &held.reason,
    );
    info!(
        terminal_id = %held.terminal_id,
        reason = %held.reason,
        outcome = ?outcome,
        "respawn-liveness: applied the close held while the pane was being proven"
    );
    Some(held)
}

fn log_dropped(dropped: Vec<HeldClose>) {
    for c in dropped {
        info!(
            terminal_id = %c.terminal_id,
            reason = %c.reason,
            "respawn-liveness: dropped a held close of a pane the respawn retired"
        );
    }
}

impl Drop for SessionCloseHold<'_> {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        self.settled = true;
        warn!(
            session = %self.session,
            current = ?self.current,
            "respawn-liveness: respawn ended without settling its close hold (cancelled?)"
        );
        match (self.old.clone(), self.old_pane()) {
            // A migration cancelled mid-proof with the old pane still running:
            // the session goes back onto it EXACTLY, and the pane under proof
            // is superseded and CLOSED — otherwise its `claude --resume` would
            // run on untracked, a second CLI on one session id.
            (Some(old), Some(OldPane::Running)) => {
                self.store.restore_record_exact(old.clone());
                if let Some(cur) = self.current.clone() {
                    self.store.mark_terminal_superseded(&cur, &old.terminal_id);
                    if self.take(&cur).is_some() {
                        self.store.unmark_terminal_superseded(&cur);
                    }
                    if self.panes.close(&cur).is_err() {
                        self.store.unmark_terminal_superseded(&cur);
                    }
                }
                log_dropped(self.store.release_session_hold(&self.session));
            }
            // The user closed the old pane: the session ends, as they asked.
            (Some(old), Some(OldPane::ClosedByUser)) => {
                let user = self.take(&old.terminal_id);
                if let Some(cur) = self.current.clone() {
                    let _ = self.panes.close(&cur);
                }
                log_dropped(self.store.release_session_hold(&self.session));
                let reason = user.map_or_else(|| USER_CLOSE_REASON.to_string(), |c| c.reason);
                self.store
                    .record_close_checked(&self.session, None, &reason);
            }
            // Nothing to roll back onto: the current pane's held close (if
            // any) is that session's own end, and is applied.
            _ => {
                finish_replaying(self.store, &self.session, self.current.as_deref());
            }
        }
    }
}

/// How a failed respawn ended, for [`end_failed`].
enum Failure {
    /// Every attempt died (or could not spawn).
    Died {
        exit_code: Option<i32>,
        retry_spawn_error: Option<String>,
    },
    /// The user closed a pane under proof; this is their close.
    ClosedByUser(HeldClose),
}

/// The end of a respawn that did not prove alive. With the OLD pane still
/// running: roll back onto it EXACTLY and close the pane under proof. With no
/// old pane, or one that is gone: the session ended on `current` — close its
/// row for real (the user's close, the held exit-close, or one on its behalf)
/// and close the pane.
fn end_failed(
    hold: SessionCloseHold<'_>,
    store: &SessionLifecycleStore,
    old: Option<&TerminalSessionRecord>,
    current: &str,
    attempts: usize,
    failure: Failure,
) -> RespawnOutcome {
    let panes = Arc::clone(&hold.panes);
    match (old, hold.old_pane()) {
        (Some(old), Some(OldPane::Running)) => {
            // Roll back: the row, its account and every re-keyed field go back
            // onto the old terminal EXACTLY as they were, THEN the pane under
            // proof leaves.
            store.restore_record_exact(old.clone());
            let _ = hold.retire(current, &old.terminal_id);
            hold.finish_dropping();
        }
        _ => {
            let applied = match &failure {
                Failure::ClosedByUser(c) => {
                    hold.finish_dropping();
                    store.record_close_checked(&c.claude_session_id, Some(current), &c.reason);
                    true
                }
                Failure::Died { .. } => hold.finish_replaying(Some(current)).is_some(),
            };
            if !applied {
                if let Some(rec) = store.find_open_by_terminal(current) {
                    store.record_close_checked(&rec.claude_session_id, Some(current), "pty-exit");
                }
            }
            if let Err(e) = panes.close(current) {
                info!(terminal_id = %current, error = %e, "respawn-liveness: dead pane already gone");
            }
        }
    }
    match failure {
        Failure::ClosedByUser(_) => RespawnOutcome::ClosedByUser { attempts },
        Failure::Died {
            exit_code,
            retry_spawn_error,
        } => RespawnOutcome::Died {
            exit_code,
            attempts,
            retry_spawn_error,
        },
    }
}

/// Spawn, prove, retry, and (with an `old` pane) roll back. See the module
/// docs. `old` is the pre-respawn record when a live pane is being replaced
/// (account migration); `None` when nothing is left to keep (the respawn
/// receiver, the resume door).
///
/// The session's closes are HELD for the whole respawn ([`SessionCloseHold`]),
/// from before the first spawn, so a child that dies at once cannot close the
/// row the retry or the rollback is about to rebind. A USER close ends the
/// respawn: of the pane under proof (no retry is spawned behind it), or of the
/// OLD pane (the migration is abandoned and the session closed).
pub(crate) async fn confirm_respawn<E: RespawnEffects>(
    effects: &mut E,
    store: &SessionLifecycleStore,
    old: Option<&TerminalSessionRecord>,
    max_attempts: usize,
) -> RespawnOutcome {
    let session = effects.claude_session_id().to_string();
    let panes = effects.panes();
    let mut hold = SessionCloseHold::new(store, Arc::clone(&panes), &session, old);
    // A migration's `old` record was read before this hold existed (and before
    // the caller's network awaits). A close of the old pane in that window was
    // APPLIED, not held — so re-read: the row must still be open on the old
    // pane, and the pane alive. Otherwise nothing moves: the spawn's re-key
    // would reopen a session the user closed.
    if let Some(old) = old {
        let row_still_there = store
            .get(&session)
            .is_some_and(|r| r.state == "open" && r.terminal_id == old.terminal_id);
        if !row_still_there || !panes.alive(&old.terminal_id) {
            hold.finish_replaying_all();
            return RespawnOutcome::NotSpawned(
                "old_pane_gone: the session's pane closed before the migration began — nothing moved"
                    .to_string(),
            );
        }
    }
    let mut current = match effects.spawn_next().await {
        Ok(t) => t,
        Err(e) => {
            hold.finish_replaying_all();
            return RespawnOutcome::NotSpawned(e);
        }
    };
    hold.set_current(&current);
    let mut attempts = 1;
    'proof: loop {
        // The old pane can be closed by the user at any await (the spawn can
        // wait out a CLI install for minutes): checked before the 3-minute
        // proof is started, and again after it.
        if hold.old_pane() == Some(OldPane::ClosedByUser) {
            hold.abort_for_closed_old(&current);
            return RespawnOutcome::ClosedByUser { attempts };
        }
        if let Some(c) = hold.take_user_close() {
            return end_failed(
                hold,
                store,
                old,
                &current,
                attempts,
                Failure::ClosedByUser(c),
            );
        }
        let verdict = effects.await_liveness(&current).await;
        // Does the old pane still HOST a CLI? Asked only where the answer is
        // used (an unpainted pane during a migration), and BEFORE the
        // user-close checks below, so a close landing during this await is
        // still seen. `Running` only says the old PTY child lives with no
        // close held: a restored pane is a shell that `claude --resume` was
        // typed into, and its CLI may have exited while the shell lives.
        let old_hosts_cli = match (&verdict, old) {
            (LivenessVerdict::Unconfirmed | LivenessVerdict::Pending, Some(o))
                if hold.old_pane() == Some(OldPane::Running) =>
            {
                panes.hosts_claude(&o.terminal_id).await
            }
            _ => false,
        };
        if hold.old_pane() == Some(OldPane::ClosedByUser) {
            hold.abort_for_closed_old(&current);
            return RespawnOutcome::ClosedByUser { attempts };
        }
        if let Some(c) = hold.take_user_close() {
            return end_failed(
                hold,
                store,
                old,
                &current,
                attempts,
                Failure::ClosedByUser(c),
            );
        }
        // A pane whose exit-close is already held has EXITED, whatever its
        // last screen read said — it must not be handed the session.
        let verdict = match verdict {
            LivenessVerdict::Died(code) => LivenessVerdict::Died(code),
            other if hold.has_held(&current) => {
                warn!(
                    terminal_id = %current,
                    verdict = ?other,
                    "respawn-liveness: the pane's exit-close is already held — judged Died"
                );
                LivenessVerdict::Died(None)
            }
            other => other,
        };
        match verdict {
            LivenessVerdict::Alive => {
                if let (Some(old), Some(OldPane::Running)) = (old, hold.old_pane()) {
                    hand_off(store, &*panes, &old.terminal_id, &current);
                }
                hold.finish_replaying(Some(&current));
                return RespawnOutcome::Alive {
                    terminal_id: current,
                    attempts,
                };
            }
            LivenessVerdict::Unconfirmed | LivenessVerdict::Pending => {
                let old_running = match (old, hold.old_pane()) {
                    (Some(old), Some(OldPane::Running)) => Some(old),
                    _ => None,
                };
                // Rolling back onto an old pane whose CLI exited would close
                // the new pane's RUNNING CLI and land the session on a bare
                // shell — so the rollback needs the old pane to still host one.
                if let (Some(old), true) = (old_running, old_hosts_cli) {
                    // A migration whose new CLI never painted, while the old
                    // CLI still runs: keep the session where it is PROVEN to
                    // live. Leaving the row on the unpainted pane would strand
                    // the old CLI with no row, and that pane's later close
                    // would end the session (coord `Closed`, the workdir nonce
                    // revoked) under the still-running old CLI. The unproven
                    // pane is superseded and closed; it held nothing the old
                    // pane does not.
                    store.restore_record_exact(old.clone());
                    let _ = hold.retire(&current, &old.terminal_id);
                    hold.finish_dropping();
                    warn!(
                        terminal_id = %current,
                        "respawn-liveness: resumed CLI is running but never painted — the migration is rolled back onto the old pane"
                    );
                    return RespawnOutcome::UnconfirmedRolledBack {
                        terminal_id: current,
                    };
                }
                // No old pane to fall back on — none, gone, or a shell whose
                // CLI exited: the new pane is the session's only host and keeps
                // the row. UNKNOWN, not dead. An old pane still alive as a bare
                // shell is left open (nothing is killed on an UNKNOWN), marked
                // superseded so its later close ends nothing.
                if let Some(o) = old_running {
                    store.mark_terminal_superseded(&o.terminal_id, &current);
                    if hold.has_held(&o.terminal_id) {
                        // Its close already arrived (and is dropped with the
                        // hold): no later close will consume the mark.
                        store.unmark_terminal_superseded(&o.terminal_id);
                    }
                }
                hold.finish_replaying(Some(&current));
                warn!(
                    terminal_id = %current,
                    "respawn-liveness: resumed CLI is running but never painted — kept (respawn-unconfirmed)"
                );
                return RespawnOutcome::Unconfirmed {
                    terminal_id: current,
                };
            }
            LivenessVerdict::Died(code) => {
                warn!(
                    terminal_id = %current,
                    exit_code = ?code,
                    attempt = attempts,
                    "respawn-liveness: resumed CLI exited before painting"
                );
                let mut retry_spawn_error = None;
                // Each retry consumes an attempt whether or not its spawn
                // succeeds. A USER close of any pane of this respawn is checked
                // before the pause, after it, and when the pane is retired —
                // never respawned behind.
                while attempts < max_attempts && !hold.user_closed() {
                    effects.pause_before_retry(attempts).await;
                    if hold.old_pane() == Some(OldPane::ClosedByUser) {
                        continue 'proof;
                    }
                    if hold.user_closed() {
                        break;
                    }
                    attempts += 1;
                    match effects.spawn_next().await {
                        Ok(next) => {
                            let retired = hold.retire(&current, &next);
                            current = next;
                            hold.set_current(&current);
                            if let Some(c) = retired.filter(HeldClose::is_user_close) {
                                // The user closed the dead pane while the retry
                                // spawned: honour it — the new pane goes too.
                                return end_failed(
                                    hold,
                                    store,
                                    old,
                                    &current,
                                    attempts,
                                    Failure::ClosedByUser(c),
                                );
                            }
                            continue 'proof;
                        }
                        Err(e) => {
                            warn!(attempt = attempts, error = %e, "respawn-liveness: retry spawn failed");
                            retry_spawn_error = Some(e);
                        }
                    }
                }
                let failure = match hold.take_user_close() {
                    Some(c) => Failure::ClosedByUser(c),
                    None => Failure::Died {
                        exit_code: code,
                        retry_spawn_error,
                    },
                };
                return end_failed(hold, store, old, &current, attempts, failure);
            }
        }
    }
}

/// Mark `from` superseded by `to`, then close `from`. A close that fails (the
/// pane was already gone) withdraws the mark, so the register holds only
/// closes still expected.
fn hand_off(store: &SessionLifecycleStore, panes: &dyn PaneOps, from: &str, to: &str) {
    store.mark_terminal_superseded(from, to);
    if let Err(e) = panes.close(from) {
        store.unmark_terminal_superseded(from);
        info!(from, to, error = %e, "respawn-liveness: superseded pane was already gone");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::session_lifecycle_store::CloseOutcome;
    use crate::terminal::graceful_exit::ScreenText;
    use std::collections::VecDeque;

    const SID: &str = "7b4a9fb4-5526-4ab0-b89e-f13b6316874a";

    fn screen(rows: &[&str], cursor_row: u16) -> ScreenText {
        ScreenText {
            lines: rows.iter().map(|r| r.to_string()).collect(),
            cursor_row,
            cursor_col: 2,
        }
    }

    fn ready_screen() -> ScreenText {
        let rule = "─".repeat(75);
        screen(
            &[
                "● resumed",
                &rule,
                "❯\u{a0}",
                &rule,
                "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
            ],
            2,
        )
    }

    fn busy_screen() -> ScreenText {
        screen(&["✻ Thinking… (esc to interrupt)"], 0)
    }

    fn blank_screen() -> ScreenText {
        screen(&[""], 0)
    }

    // ---- OwedSettle ---------------------------------------------------------

    #[test]
    fn an_owed_settle_runs_its_settle_once() {
        let mut got = None;
        OwedSettle::new(Some(7), |_| panic!("settled, so no fallback")).settle(|v| got = Some(v));
        assert_eq!(got, Some(7));
        OwedSettle::<u8>::new(None, |_| panic!("nothing owed"))
            .settle(|_| panic!("nothing to settle"));
    }

    /// Suggestion: a cancelled migration future drops its owed settle — that
    /// must settle through the fallback (keeping the anchor), never panic.
    #[test]
    fn an_owed_settle_dropped_unsettled_runs_its_fallback_without_panicking() {
        let got = Arc::new(std::sync::Mutex::new(None));
        let g = Arc::clone(&got);
        drop(OwedSettle::new(Some(7), move |v| {
            *g.lock().unwrap() = Some(v)
        }));
        assert_eq!(*got.lock().unwrap(), Some(7));
    }

    #[tokio::test]
    async fn an_owed_settle_held_by_an_aborted_task_settles_through_its_fallback() {
        let (tx, rx) = std::sync::mpsc::channel();
        let task = tokio::spawn(async move {
            let _owed = OwedSettle::new(Some(9), move |v| tx.send(v).unwrap());
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 9);
    }

    #[test]
    fn retry_delays_follow_the_spawn_budget() {
        assert_eq!(retry_delay(1), Duration::from_secs(1));
        assert_eq!(retry_delay(2), Duration::from_secs(3));
        assert_eq!(retry_delay(9), Duration::from_secs(6));
    }

    // ---- judge: every arm, literal inputs --------------------------------

    #[test]
    fn judge_child_exit_is_died_at_once_whatever_the_screen() {
        assert_eq!(
            judge(Some(Some(127)), &IdlePromptProbe::Busy, true, false),
            LivenessVerdict::Died(Some(127))
        );
        assert_eq!(
            judge(Some(None), &IdlePromptProbe::Ready, true, true),
            LivenessVerdict::Died(None)
        );
    }

    #[test]
    fn judge_busy_is_alive() {
        assert_eq!(
            judge(None, &IdlePromptProbe::Busy, false, false),
            LivenessVerdict::Alive
        );
    }

    #[test]
    fn judge_ready_is_alive_only_once_settled() {
        assert_eq!(
            judge(None, &IdlePromptProbe::Ready, true, false),
            LivenessVerdict::Alive
        );
        assert_eq!(
            judge(None, &IdlePromptProbe::Ready, false, false),
            LivenessVerdict::Pending
        );
    }

    #[test]
    fn judge_no_paint_is_pending_then_unconfirmed_at_the_deadline() {
        let p = IdlePromptProbe::NotYet("no prompt".into());
        assert_eq!(judge(None, &p, false, false), LivenessVerdict::Pending);
        assert_eq!(judge(None, &p, false, true), LivenessVerdict::Unconfirmed);
        assert_eq!(
            judge(None, &IdlePromptProbe::Ready, false, true),
            LivenessVerdict::Unconfirmed,
            "an unsettled Ready at the deadline is still unknown"
        );
    }

    // ---- the poll loop ---------------------------------------------------

    fn scripted(
        readings: Vec<Option<(Option<Option<i32>>, ScreenText)>>,
    ) -> impl FnMut() -> Option<PaneReading> {
        let mut q: VecDeque<_> = readings.into();
        let last = q.back().cloned().flatten();
        move || {
            let r = q.pop_front().unwrap_or_else(|| last.clone());
            r.map(|(exited, screen)| PaneReading { exited, screen })
        }
    }

    /// The 11:51Z shape: a child that exited is Died on the first read, not
    /// after the 3-minute deadline.
    #[tokio::test(start_paused = true)]
    async fn a_dead_child_is_died_without_waiting_out_the_deadline() {
        let started = tokio::time::Instant::now();
        let v = await_liveness_with(
            scripted(vec![Some((Some(Some(127)), blank_screen()))]),
            RESUME_LIVENESS_POLL,
            RESUME_LIVENESS_DEADLINE,
        )
        .await;
        assert_eq!(v, LivenessVerdict::Died(Some(127)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_ready_prompt_is_alive_on_the_second_identical_read() {
        let v = await_liveness_with(
            scripted(vec![
                Some((None, blank_screen())),
                Some((None, ready_screen())),
                Some((None, ready_screen())),
            ]),
            RESUME_LIVENESS_POLL,
            RESUME_LIVENESS_DEADLINE,
        )
        .await;
        assert_eq!(v, LivenessVerdict::Alive);
    }

    #[tokio::test(start_paused = true)]
    async fn a_live_unpainted_child_is_unconfirmed_at_the_deadline() {
        let v = await_liveness_with(
            scripted(vec![Some((None, blank_screen()))]),
            RESUME_LIVENESS_POLL,
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(v, LivenessVerdict::Unconfirmed);
    }

    #[tokio::test(start_paused = true)]
    async fn a_vanished_pane_is_died() {
        let v = await_liveness_with(
            scripted(vec![Some((None, busy_screen())), None]),
            RESUME_LIVENESS_POLL,
            RESUME_LIVENESS_DEADLINE,
        )
        .await;
        assert_eq!(v, LivenessVerdict::Alive, "busy on the first read wins");
        let v = await_liveness_with(
            scripted(vec![None]),
            RESUME_LIVENESS_POLL,
            RESUME_LIVENESS_DEADLINE,
        )
        .await;
        assert_eq!(v, LivenessVerdict::Died(None));
    }

    // ---- confirm_respawn: the migration arms ------------------------------

    fn record(terminal_id: &str, config_dir: &str) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: SID.to_string(),
            config_dir: Some(config_dir.to_string()),
            working_dir: Some("/work".to_string()),
            page_id: "default".to_string(),
            zone_index: 2,
            title: None,
            terminal_id: terminal_id.to_string(),
            opened_at: 0,
            last_seen_at: 0,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: crate::session::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
            origin: None,
            restore_pending_at: None,
            confirmed_at: Some(1),
            handle: None,
            account_label: None,
            account_wrapper: None,
            session_name: None,
            name_source: None,
            tenant_id: None,
            task_run_id: None,
            bypass_permissions: None,
            restored_from_boot_at: None,
            restore_tier: None,
            finished_at: None,
            wind_down_outcome: None,
            wind_down_at: None,
            finish_reason: None,
            finish_synced: false,
            spawn_device_default: None,
            adopted_from: None,
        }
    }

    /// A fake resume seam over a REAL lifecycle store: each spawn re-keys the
    /// row onto a new terminal under the destination account, as the pinned
    /// capture hint does; each pane's verdict is scripted.
    /// Fake panes: a pane is alive until it is closed or listed `gone`.
    #[derive(Default)]
    struct FakePanes {
        closed: std::sync::Mutex<Vec<String>>,
        /// Panes whose close fails (already gone).
        gone: std::sync::Mutex<Vec<String>>,
        /// Panes still alive whose subtree holds no `claude` (a bare shell).
        no_claude: std::sync::Mutex<Vec<String>>,
    }

    impl PaneOps for FakePanes {
        fn close(&self, terminal_id: &str) -> Result<(), String> {
            if self.gone.lock().unwrap().iter().any(|g| g == terminal_id) {
                return Err("gone".into());
            }
            self.closed.lock().unwrap().push(terminal_id.to_string());
            Ok(())
        }
        fn alive(&self, terminal_id: &str) -> bool {
            !self.gone.lock().unwrap().iter().any(|g| g == terminal_id)
                && !self.closed.lock().unwrap().iter().any(|c| c == terminal_id)
        }
        fn hosts_claude<'a>(&'a self, terminal_id: &'a str) -> BoxFuture<'a, bool> {
            let hosts = self.alive(terminal_id)
                && !self
                    .no_claude
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|n| n == terminal_id);
            Box::pin(async move { hosts })
        }
    }

    struct Fake<'a> {
        store: &'a SessionLifecycleStore,
        verdicts: VecDeque<LivenessVerdict>,
        spawn_errors: VecDeque<Option<String>>,
        spawned: Vec<String>,
        panes: Arc<FakePanes>,
        /// When set, a pane judged `Died` first delivers its exit-close — the
        /// frontend's `pty-exit`, which fires the moment the child exits —
        /// exactly as production does, before the verdict returns.
        exit_close: bool,
        exit_close_outcomes: Vec<CloseOutcome>,
        /// When set, a pane judged `Died` was closed by the USER first (the
        /// frontend's `explicit` close, then its exit).
        user_close: bool,
        /// When set, every spawn's child exits INSIDE the spawn — after the
        /// row is re-keyed onto it, before `spawn_next` returns — and its
        /// pty-exit close arrives in that gap.
        close_in_spawn: bool,
        /// When set, the USER closes the OLD pane while the FIRST spawn runs.
        close_old_in_spawn: bool,
        /// When set, the USER closes the dead pane during the pause before a
        /// retry — after its verdict was read.
        user_close_in_pause: bool,
        /// When set, the USER closes the dead pane while the RETRY spawns.
        user_close_prev_in_spawn: bool,
        /// Every pane `await_liveness` was asked to prove, in order.
        awaited: Vec<String>,
        /// When set, the USER closes the OLD pane during the retry pause.
        close_old_in_pause: bool,
        /// When set, the USER closes the OLD pane while a RETRY spawns.
        close_old_in_retry_spawn: bool,
        /// When set, the OLD pane's CLI EXITS (a `pty-exit`, not the user)
        /// while the first spawn runs; the pane itself still reads alive.
        old_exits_in_spawn: bool,
        /// When set, the OLD pane VANISHES (no close seen) while the first
        /// spawn runs.
        old_vanishes_in_spawn: bool,
        /// When set, the FIRST pane judged `Alive` has already exited: its
        /// pty-exit close is delivered before the verdict returns.
        exit_on_first_alive: bool,
        /// When set, the USER closes `new-1` (already retired) while `new-2`
        /// is being proven.
        user_close_retired_while_proving: bool,
        /// When set, the USER dismisses a STALE tab (`stale`, a dead pane
        /// still carrying this session id, owning no open row) while each
        /// pane is being proven.
        user_close_stale_while_proving: bool,
    }

    impl Fake<'_> {
        fn closed(&self) -> Vec<String> {
            self.panes.closed.lock().unwrap().clone()
        }
    }

    impl<'a> Fake<'a> {
        fn new(store: &'a SessionLifecycleStore, verdicts: Vec<LivenessVerdict>) -> Self {
            Self {
                store,
                verdicts: verdicts.into(),
                spawn_errors: VecDeque::new(),
                spawned: Vec::new(),
                panes: Arc::new(FakePanes::default()),
                exit_close: false,
                exit_close_outcomes: Vec::new(),
                user_close: false,
                close_in_spawn: false,
                close_old_in_spawn: false,
                user_close_in_pause: false,
                user_close_prev_in_spawn: false,
                awaited: Vec::new(),
                close_old_in_pause: false,
                close_old_in_retry_spawn: false,
                old_exits_in_spawn: false,
                old_vanishes_in_spawn: false,
                exit_on_first_alive: false,
                user_close_retired_while_proving: false,
                user_close_stale_while_proving: false,
            }
        }
    }

    impl RespawnEffects for Fake<'_> {
        fn claude_session_id(&self) -> &str {
            SID
        }
        async fn spawn_next(&mut self) -> Result<String, String> {
            if let Some(Some(e)) = self.spawn_errors.pop_front() {
                return Err(e);
            }
            if self.old_vanishes_in_spawn && self.spawned.is_empty() {
                self.panes.gone.lock().unwrap().push("old".into());
            }
            if self.old_exits_in_spawn && self.spawned.is_empty() {
                let o = self
                    .store
                    .record_close_checked(SID, Some("old"), "pty-exit");
                self.exit_close_outcomes.push(o);
            }
            if self.close_old_in_retry_spawn && !self.spawned.is_empty() {
                let o = self
                    .store
                    .record_close_checked(SID, Some("old"), "explicit");
                self.exit_close_outcomes.push(o);
                self.panes.gone.lock().unwrap().push("old".into());
            }
            if self.close_old_in_spawn && self.spawned.is_empty() {
                let o = self
                    .store
                    .record_close_checked(SID, Some("old"), "explicit");
                self.exit_close_outcomes.push(o);
                self.panes.gone.lock().unwrap().push("old".into());
            }
            if self.user_close_prev_in_spawn {
                if let Some(prev) = self.spawned.last() {
                    let o = self.store.record_close_checked(SID, Some(prev), "explicit");
                    self.exit_close_outcomes.push(o);
                }
            }
            let id = format!("new-{}", self.spawned.len() + 1);
            self.store.record_open(record(&id, "/dst"));
            if self.close_in_spawn {
                let o = self.store.record_close_checked(SID, Some(&id), "pty-exit");
                self.exit_close_outcomes.push(o);
            }
            self.spawned.push(id.clone());
            Ok(id)
        }
        async fn await_liveness(&mut self, terminal_id: &str) -> LivenessVerdict {
            self.awaited.push(terminal_id.to_string());
            let v = self
                .verdicts
                .pop_front()
                .expect("a scripted verdict per pane");
            if self.exit_on_first_alive && v == LivenessVerdict::Alive && self.awaited.len() == 1 {
                let o = self
                    .store
                    .record_close_checked(SID, Some(terminal_id), "pty-exit");
                self.exit_close_outcomes.push(o);
            }
            if self.user_close_stale_while_proving {
                let o = self
                    .store
                    .record_close_checked(SID, Some("stale"), "explicit");
                self.exit_close_outcomes.push(o);
            }
            if self.user_close_retired_while_proving && terminal_id == "new-2" {
                let o = self
                    .store
                    .record_close_checked(SID, Some("new-1"), "explicit");
                self.exit_close_outcomes.push(o);
            }
            if self.user_close && matches!(v, LivenessVerdict::Died(_)) {
                let o = self
                    .store
                    .record_close_checked(SID, Some(terminal_id), "explicit");
                self.exit_close_outcomes.push(o);
            }
            if (self.exit_close || self.user_close) && matches!(v, LivenessVerdict::Died(_)) {
                let o = self
                    .store
                    .record_close_checked(SID, Some(terminal_id), "pty-exit");
                self.exit_close_outcomes.push(o);
            }
            v
        }
        async fn pause_before_retry(&mut self, _retry: usize) {
            if self.close_old_in_pause {
                let o = self
                    .store
                    .record_close_checked(SID, Some("old"), "explicit");
                self.exit_close_outcomes.push(o);
                self.panes.gone.lock().unwrap().push("old".into());
            }
            if self.user_close_in_pause {
                if let Some(prev) = self.spawned.last() {
                    let o = self.store.record_close_checked(SID, Some(prev), "explicit");
                    self.exit_close_outcomes.push(o);
                }
            }
        }
        fn panes(&self) -> Arc<dyn PaneOps> {
            self.panes.clone()
        }
    }

    fn fresh_store() -> (tempfile::TempDir, SessionLifecycleStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        store.record_open(record("old", "/src"));
        (dir, store)
    }

    fn superseded(store: &SessionLifecycleStore, terminal: &str) -> bool {
        matches!(
            store.record_close_checked(SID, Some(terminal), "pty-exit"),
            CloseOutcome::TerminalSuperseded { .. }
        )
    }

    #[tokio::test]
    async fn alive_and_painted_closes_the_old_pane_once() {
        let (_d, store) = fresh_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Alive]);
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(
            out,
            RespawnOutcome::Alive {
                terminal_id: "new-1".into(),
                attempts: 1
            }
        );
        assert_eq!(fx.closed(), vec!["old".to_string()]);
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.terminal_id.as_str(), row.config_dir.as_deref()),
            ("new-1", Some("/dst"))
        );
        assert!(
            superseded(&store, "old"),
            "the old pane's exit-close is TerminalSuperseded"
        );
        assert_eq!(store.get(SID).unwrap().state, "open");
    }

    #[tokio::test]
    async fn died_then_alive_on_retry_two_closes_the_old_pane_once() {
        let (_d, store) = fresh_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(
            &store,
            vec![LivenessVerdict::Died(Some(127)), LivenessVerdict::Alive],
        );
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(
            out,
            RespawnOutcome::Alive {
                terminal_id: "new-2".into(),
                attempts: 2
            }
        );
        assert_eq!(fx.closed(), vec!["new-1".to_string(), "old".to_string()]);
        assert_eq!(
            fx.closed().iter().filter(|c| *c == "old").count(),
            1,
            "the old pane closes exactly once"
        );
        assert!(
            superseded(&store, "new-1"),
            "the dead pane's exit-close closes nothing"
        );
        assert!(superseded(&store, "old"));
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.terminal_id.as_str()),
            ("open", "new-2")
        );
    }

    #[tokio::test]
    async fn died_on_every_attempt_rolls_back_onto_the_old_pane() {
        let (_d, store) = fresh_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(
            &store,
            vec![
                LivenessVerdict::Died(Some(127)),
                LivenessVerdict::Died(Some(127)),
                LivenessVerdict::Died(Some(1)),
            ],
        );
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(
            out,
            RespawnOutcome::Died {
                exit_code: Some(1),
                attempts: 3,
                retry_spawn_error: None
            }
        );
        assert_eq!(fx.spawned.len(), 3);
        assert!(
            !fx.closed().contains(&"old".to_string()),
            "the old pane stays open"
        );
        let row = store.get(SID).unwrap();
        assert_eq!(row.state, "open");
        assert_eq!(
            row.terminal_id, "old",
            "the row is bound to the old terminal again"
        );
        assert_eq!(
            row.config_dir.as_deref(),
            Some("/src"),
            "and to the old account"
        );
        for dead in ["new-1", "new-2", "new-3"] {
            assert!(
                superseded(&store, dead),
                "{dead}'s exit-close must close nothing"
            );
        }
        assert_eq!(store.get(SID).unwrap().state, "open");
        assert_eq!(
            RespawnOutcome::died_reason(Some(1), 3),
            "respawn-died: exit 1 after 3 attempts"
        );
    }

    /// A row a closer reached first is re-opened by the rollback.
    #[tokio::test]
    async fn rollback_reopens_a_row_a_closer_got_to_first() {
        let (_d, store) = fresh_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Died(Some(127))]);
        // Every retry spawn fails, so the budget is spent on spawn errors.
        fx.spawn_errors = vec![
            None,
            Some("claude_cli_unavailable:not_found".into()),
            Some("claude_cli_unavailable:not_found".into()),
        ]
        .into();
        // Simulate `poll-dead` on the dead pane between spawn and rollback.
        struct Closing<'a, 'b>(&'b mut Fake<'a>);
        impl RespawnEffects for Closing<'_, '_> {
            fn claude_session_id(&self) -> &str {
                SID
            }
            async fn spawn_next(&mut self) -> Result<String, String> {
                self.0.spawn_next().await
            }
            async fn await_liveness(&mut self, t: &str) -> LivenessVerdict {
                let v = self.0.await_liveness(t).await;
                self.0.store.record_close(SID, "poll-dead");
                v
            }
            async fn pause_before_retry(&mut self, _r: usize) {}
            fn panes(&self) -> Arc<dyn PaneOps> {
                self.0.panes()
            }
        }
        let out = confirm_respawn(
            &mut Closing(&mut fx),
            &store,
            Some(&old),
            MAX_RESPAWN_ATTEMPTS,
        )
        .await;
        assert!(
            matches!(
                out,
                RespawnOutcome::Died {
                    attempts: 3,
                    retry_spawn_error: Some(_),
                    ..
                }
            ),
            "{out:?}"
        );
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.terminal_id.as_str()),
            ("open", "old")
        );
    }

    #[tokio::test]
    async fn alive_but_unpainted_with_the_old_pane_running_rolls_back_onto_it() {
        // Round 4, finding 1: the unproven pane is closed and the session stays
        // on the old pane, whose CLI still runs — never stranded with no row.
        let (_d, store, closes) = observed_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Unconfirmed]);
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(
            out,
            RespawnOutcome::UnconfirmedRolledBack {
                terminal_id: "new-1".into()
            }
        );
        assert_eq!(
            fx.closed(),
            vec!["new-1".to_string()],
            "the unproven pane is closed"
        );
        let row = store.get(SID).unwrap();
        assert_eq!(
            (
                row.state.as_str(),
                row.terminal_id.as_str(),
                row.config_dir.as_deref()
            ),
            ("open", "old", Some("/src"))
        );
        assert!(
            superseded(&store, "new-1"),
            "the unproven pane's exit-close closes nothing"
        );
        assert_eq!(
            closes.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no coord Closed, nonce kept"
        );
        assert_eq!(store.get(SID).unwrap().state, "open");
    }

    /// Without an old pane (respawn receiver, resume door) an unpainted pane is
    /// the session's only host: it keeps the row, UNKNOWN.
    #[tokio::test]
    async fn alive_but_unpainted_without_an_old_pane_keeps_the_row() {
        let (_d, store) = fresh_store();
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Unconfirmed]);
        let out = confirm_respawn(&mut fx, &store, None, MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(
            out,
            RespawnOutcome::Unconfirmed {
                terminal_id: "new-1".into()
            }
        );
        assert!(fx.closed().is_empty());
        assert_eq!(store.get(SID).unwrap().terminal_id, "new-1");
    }

    /// The existing `unmark` arm: the old pane was closed by the operator after
    /// exhaustion, so its close fails and the supersede mark is withdrawn.
    #[tokio::test]
    async fn an_old_pane_already_gone_withdraws_its_supersede_mark() {
        let (_d, store) = fresh_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Alive]);
        fx.old_vanishes_in_spawn = true;
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert!(matches!(out, RespawnOutcome::Alive { .. }));
        assert!(
            !superseded(&store, "old"),
            "a mark nothing will consume must be withdrawn"
        );
    }

    #[tokio::test]
    async fn a_failed_first_spawn_moves_nothing() {
        let (_d, store) = fresh_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![]);
        fx.spawn_errors = vec![Some("boom".into())].into();
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(out, RespawnOutcome::NotSpawned("boom".into()));
        assert!(fx.closed().is_empty());
        assert_eq!(store.get(SID).unwrap().terminal_id, "old");
    }

    /// A second hint on the old terminal after a migration finds no row — the
    /// row was re-keyed — so it is a no-op (the cap would admit it); after a
    /// rollback the row is back on the old terminal and a hint can migrate.
    #[tokio::test]
    async fn the_old_terminal_owns_no_row_after_a_migration_and_owns_it_again_after_a_rollback() {
        let (_d, store) = fresh_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Alive]);
        confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert!(store.find_open_by_terminal("old").is_none());

        let (_d2, store2) = fresh_store();
        let mut fx = Fake::new(&store2, vec![LivenessVerdict::Died(Some(127)); 3]);
        confirm_respawn(&mut fx, &store2, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        let again = store2.find_open_by_terminal("old").expect("rolled back");
        assert_eq!(again.config_dir.as_deref(), Some("/src"));
    }

    // ---- confirm_respawn: no old pane (receiver, resume door) -------------

    #[tokio::test]
    async fn without_an_old_pane_every_death_closes_the_dead_pane_and_reports_died() {
        let (_d, store) = fresh_store();
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Died(Some(127)); 3]);
        let out = confirm_respawn(&mut fx, &store, None, MAX_RESPAWN_ATTEMPTS).await;
        assert!(matches!(
            out,
            RespawnOutcome::Died {
                exit_code: Some(127),
                attempts: 3,
                ..
            }
        ));
        assert_eq!(fx.closed(), vec!["new-1", "new-2", "new-3"]);
        assert!(superseded(&store, "new-1"));
        assert!(superseded(&store, "new-2"));
    }

    // ---- the dead pane's own exit-close (review finding 1) ----------------

    /// A store whose close observer (the production coord `Closed` enqueue,
    /// and the same branch that revokes the workdir nonce) counts real closes.
    fn observed_store() -> (
        tempfile::TempDir,
        SessionLifecycleStore,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let (d, store) = fresh_store();
        let closes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&closes);
        store.attach_close_observer(move |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        (d, store, closes)
    }

    #[tokio::test]
    async fn a_dead_panes_exit_close_during_a_migration_closes_nothing_and_rolls_back() {
        let (_d, store, closes) = observed_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Died(Some(127)); 3]);
        fx.exit_close = true;
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert!(
            matches!(out, RespawnOutcome::Died { attempts: 3, .. }),
            "{out:?}"
        );
        assert_eq!(fx.exit_close_outcomes.len(), 3);
        for o in &fx.exit_close_outcomes {
            assert!(
                matches!(o, CloseOutcome::HeldInFlight { .. }),
                "a dead pane's exit-close is held, never applied: {o:?}"
            );
        }
        assert_eq!(
            closes.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no coord Closed was enqueued and no workdir nonce was released"
        );
        let row = store.get(SID).unwrap();
        assert_eq!(
            (
                row.state.as_str(),
                row.terminal_id.as_str(),
                row.config_dir.as_deref()
            ),
            ("open", "old", Some("/src"))
        );
        assert_eq!(
            store.held_session_count(),
            0,
            "no hold outlives the respawn"
        );
        assert_eq!(
            store.superseded_terminal_count(),
            0,
            "a mark whose close already came is withdrawn"
        );
    }

    #[tokio::test]
    async fn a_dead_panes_exit_close_then_a_live_retry_keeps_the_row_open_on_the_retry() {
        let (_d, store, closes) = observed_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(
            &store,
            vec![LivenessVerdict::Died(Some(127)), LivenessVerdict::Alive],
        );
        fx.exit_close = true;
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert!(
            matches!(out, RespawnOutcome::Alive { attempts: 2, .. }),
            "{out:?}"
        );
        assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 0);
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.terminal_id.as_str()),
            ("open", "new-2")
        );
        assert_eq!(store.held_session_count(), 0);
    }

    #[tokio::test]
    async fn without_an_old_pane_the_last_dead_pane_is_closed_for_real_exactly_once() {
        for exit_close in [true, false] {
            let (_d, store, closes) = observed_store();
            let mut fx = Fake::new(&store, vec![LivenessVerdict::Died(Some(127)); 3]);
            fx.exit_close = exit_close;
            let out = confirm_respawn(&mut fx, &store, None, MAX_RESPAWN_ATTEMPTS).await;
            assert!(matches!(out, RespawnOutcome::Died { .. }), "{out:?}");
            assert_eq!(
                closes.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "exit_close={exit_close}: the session ended, and is closed once"
            );
            let row = store.get(SID).unwrap();
            assert_eq!(
                (row.state.as_str(), row.close_reason.as_deref()),
                ("closed", Some("pty-exit")),
                "exit_close={exit_close}"
            );
            assert_eq!(store.held_session_count(), 0);
            assert_eq!(
                store.record_close_checked(SID, Some("new-3"), "pty-exit"),
                CloseOutcome::AlreadyClosed {
                    claude_session_id: SID.into()
                },
                "a late exit-close is a benign repeat"
            );
        }
    }

    /// Round 2, finding 2: a respawn future dropped mid-proof (task abort,
    /// shutdown) with NO old pane releases its hold and applies the current
    /// pane's held close — that session's own end; nothing stays held.
    #[tokio::test]
    async fn a_cancelled_respawn_without_an_old_pane_replays_the_current_panes_close() {
        let (_d, store, closes) = observed_store();
        store.record_open(record("new-1", "/dst"));
        {
            let mut hold = SessionCloseHold::new(&store, Arc::new(FakePanes::default()), SID, None);
            hold.set_current("new-1");
            assert!(matches!(
                store.record_close_checked(SID, Some("new-1"), "pty-exit"),
                CloseOutcome::HeldInFlight { .. }
            ));
            assert!(
                matches!(
                    store.record_close_checked(SID, None, "poll-dead"),
                    CloseOutcome::HeldInFlight { .. }
                ),
                "a terminal-less close of a held session is held too"
            );
            assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
        assert_eq!(store.held_session_count(), 0);
        assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 1);
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.close_reason.as_deref()),
            ("closed", Some("pty-exit")),
            "the FIRST held close is the one replayed"
        );
    }

    /// Round 2, finding 2 / round 3, finding 3: a MIGRATION cancelled
    /// mid-proof closes nothing of the session — the old pane still runs, so
    /// the old record is restored exactly — and CLOSES the pane under proof, so
    /// no second `claude --resume` runs on untracked.
    #[tokio::test]
    async fn a_cancelled_migration_restores_the_old_record_and_closes_nothing() {
        let (_d, store, closes) = observed_store();
        let old = record("old", "/src");
        store.record_open(record("new-1", "/dst"));
        let panes = Arc::new(FakePanes::default());
        {
            let mut hold = SessionCloseHold::new(&store, panes.clone(), SID, Some(&old));
            hold.set_current("new-1");
            assert!(matches!(
                store.record_close_checked(SID, Some("new-1"), "pty-exit"),
                CloseOutcome::HeldInFlight { .. }
            ));
        }
        assert_eq!(
            closes.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no coord Closed, no nonce release"
        );
        let row = store.get(SID).unwrap();
        assert_eq!(
            (
                row.state.as_str(),
                row.terminal_id.as_str(),
                row.config_dir.as_deref()
            ),
            ("open", "old", Some("/src"))
        );
        assert_eq!(store.held_session_count(), 0);
        assert_eq!(
            *panes.closed.lock().unwrap(),
            vec!["new-1".to_string()],
            "the pane under proof is closed, never left running untracked"
        );

        // And with no close held yet, the pane under proof is superseded, so
        // its later exit-close closes nothing either.
        let (_d2, store2, closes2) = observed_store();
        store2.record_open(record("new-1", "/dst"));
        {
            let mut hold =
                SessionCloseHold::new(&store2, Arc::new(FakePanes::default()), SID, Some(&old));
            hold.set_current("new-1");
        }
        assert!(superseded(&store2, "new-1"));
        assert_eq!(closes2.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(store2.get(SID).unwrap().terminal_id, "old");
    }

    /// The whole `confirm_respawn` future dropped mid-proof (not just the
    /// guard) leaves the migration restored onto the old pane.
    #[tokio::test]
    async fn an_aborted_migration_respawn_task_restores_the_old_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap(),
        );
        store.record_open(record("old", "/src"));
        struct Hang(Arc<SessionLifecycleStore>);
        impl RespawnEffects for Hang {
            fn claude_session_id(&self) -> &str {
                SID
            }
            async fn spawn_next(&mut self) -> Result<String, String> {
                self.0.record_open(record("new-1", "/dst"));
                Ok("new-1".into())
            }
            async fn await_liveness(&mut self, _t: &str) -> LivenessVerdict {
                std::future::pending().await
            }
            async fn pause_before_retry(&mut self, _r: usize) {}
            fn panes(&self) -> Arc<dyn PaneOps> {
                Arc::new(FakePanes::default())
            }
        }
        let s2 = Arc::clone(&store);
        let task = tokio::spawn(async move {
            let old = record("old", "/src");
            confirm_respawn(
                &mut Hang(Arc::clone(&s2)),
                &s2,
                Some(&old),
                MAX_RESPAWN_ATTEMPTS,
            )
            .await
        });
        for _ in 0..100 {
            if store.get(SID).unwrap().terminal_id == "new-1" {
                break;
            }
            tokio::task::yield_now().await;
        }
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.terminal_id.as_str()),
            ("open", "old")
        );
        assert_eq!(store.held_session_count(), 0);
    }

    // ---- round 2, finding 1: a USER close of the pane under proof ----------

    #[tokio::test]
    async fn a_user_close_of_the_pane_under_proof_is_not_respawned() {
        let (_d, store, closes) = observed_store();
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Died(None); 3]);
        fx.user_close = true;
        let out = confirm_respawn(&mut fx, &store, None, MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(out, RespawnOutcome::ClosedByUser { attempts: 1 });
        assert_eq!(
            fx.spawned.len(),
            1,
            "no `claude --resume` pane is spawned behind the user's close"
        );
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.close_reason.as_deref()),
            ("closed", Some("explicit")),
            "the user's close is honoured as the user's close"
        );
        assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(store.held_session_count(), 0);
    }

    #[tokio::test]
    async fn a_user_close_of_the_new_pane_during_a_migration_rolls_back_without_a_retry() {
        let (_d, store, closes) = observed_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Died(None); 3]);
        fx.user_close = true;
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(out, RespawnOutcome::ClosedByUser { attempts: 1 });
        assert_eq!(fx.spawned.len(), 1);
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.terminal_id.as_str()),
            ("open", "old")
        );
        assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    // ---- round 2, finding 3: the spawn-to-hold gap ------------------------

    /// A child that exits INSIDE the spawn (row already re-keyed onto it,
    /// `spawn_next` not yet returned) has its close held too.
    #[tokio::test]
    async fn an_exit_close_inside_the_spawn_gap_is_held() {
        let (_d, store, closes) = observed_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Died(Some(127)); 3]);
        fx.close_in_spawn = true;
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert!(
            matches!(out, RespawnOutcome::Died { attempts: 3, .. }),
            "{out:?}"
        );
        for o in &fx.exit_close_outcomes {
            assert!(matches!(o, CloseOutcome::HeldInFlight { .. }), "{o:?}");
        }
        assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 0);
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.terminal_id.as_str()),
            ("open", "old")
        );
    }

    /// Nit: a failed retry spawn consumes an attempt and the loop goes on.
    #[tokio::test]
    async fn a_failed_retry_spawn_consumes_an_attempt_and_the_retry_continues() {
        let (_d, store) = fresh_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(
            &store,
            vec![LivenessVerdict::Died(Some(127)), LivenessVerdict::Alive],
        );
        fx.spawn_errors = vec![None, Some("claude_cli_unavailable:not_found".into()), None].into();
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(
            out,
            RespawnOutcome::Alive {
                terminal_id: "new-2".into(),
                attempts: 3
            }
        );
    }

    /// Suggestion: the rollback restores the old record exactly — a field the
    /// failed respawn stamped is not kept just because the old one was `None`.
    #[tokio::test]
    async fn rollback_restores_the_old_record_exactly() {
        let (_d, store) = fresh_store();
        let old = record("old", "/src");
        struct Stamping<'a, 'b>(&'b mut Fake<'a>);
        impl RespawnEffects for Stamping<'_, '_> {
            fn claude_session_id(&self) -> &str {
                SID
            }
            async fn spawn_next(&mut self) -> Result<String, String> {
                let id = self.0.spawn_next().await?;
                let mut rec = record(&id, "/dst");
                rec.account_label = Some("dst-account".into());
                rec.account_wrapper = Some("claude-dst".into());
                rec.session_name = Some("renamed".into());
                self.0.store.record_open(rec);
                Ok(id)
            }
            async fn await_liveness(&mut self, t: &str) -> LivenessVerdict {
                self.0.await_liveness(t).await
            }
            async fn pause_before_retry(&mut self, _r: usize) {}
            fn panes(&self) -> Arc<dyn PaneOps> {
                self.0.panes()
            }
        }
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Died(Some(127)); 3]);
        confirm_respawn(
            &mut Stamping(&mut fx),
            &store,
            Some(&old),
            MAX_RESPAWN_ATTEMPTS,
        )
        .await;
        let row = store.get(SID).unwrap();
        assert_eq!(row.terminal_id, "old");
        assert_eq!(row.account_label, None);
        assert_eq!(row.account_wrapper, None);
        assert_eq!(row.session_name, None);
    }

    // ---- round 3 -----------------------------------------------------------

    /// Finding 1: the user closes the OLD pane while the first spawn waits
    /// (e.g. out a CLI install). The migration is abandoned: the session stays
    /// closed as the user asked — the spawn's re-key does not resurrect it —
    /// and the pane under proof is closed.
    #[tokio::test]
    async fn a_user_close_of_the_old_pane_mid_migration_ends_the_session() {
        let (_d, store, closes) = observed_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Alive]);
        fx.close_old_in_spawn = true;
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(out, RespawnOutcome::ClosedByUser { attempts: 1 });
        assert_eq!(fx.spawned.len(), 1);
        assert!(
            fx.closed().contains(&"new-1".to_string()),
            "the new pane is closed"
        );
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.close_reason.as_deref()),
            ("closed", Some("explicit")),
            "the user's close stands"
        );
        assert_eq!(
            closes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "closed exactly once"
        );
        assert_eq!(store.held_session_count(), 0);
    }

    /// Finding 1, the other half: the old pane is GONE (not by the user) and
    /// every attempt dies — nothing to roll back onto, so the row is closed
    /// for real rather than restored onto a dead terminal.
    #[tokio::test]
    async fn every_attempt_dying_with_the_old_pane_gone_closes_rather_than_restores() {
        let (_d, store, closes) = observed_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Died(Some(127)); 3]);
        fx.old_vanishes_in_spawn = true;
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert!(
            matches!(out, RespawnOutcome::Died { attempts: 3, .. }),
            "{out:?}"
        );
        let row = store.get(SID).unwrap();
        assert_eq!(row.state, "closed", "never reopened on a dead old terminal");
        assert_ne!(row.terminal_id, "old");
        assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// Finding 2: the user's close of the dead pane lands during the PAUSE
    /// before a retry — after the verdict was read. No retry is spawned.
    #[tokio::test]
    async fn a_user_close_during_the_retry_pause_is_honoured() {
        let (_d, store, closes) = observed_store();
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Died(Some(127)); 3]);
        fx.user_close_in_pause = true;
        let out = confirm_respawn(&mut fx, &store, None, MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(out, RespawnOutcome::ClosedByUser { attempts: 1 });
        assert_eq!(
            fx.spawned.len(),
            1,
            "no pane is respawned behind the user's close"
        );
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.close_reason.as_deref()),
            ("closed", Some("explicit"))
        );
        assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// Finding 2: the user's close of the dead pane lands WHILE the retry
    /// spawns. `retire` takes it, and it ends the respawn — the just-spawned
    /// pane is closed too — instead of being dropped.
    #[tokio::test]
    async fn a_user_close_during_the_retry_spawn_ends_the_respawn() {
        let (_d, store, closes) = observed_store();
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Died(Some(127)); 3]);
        fx.user_close_prev_in_spawn = true;
        let out = confirm_respawn(&mut fx, &store, None, MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(out, RespawnOutcome::ClosedByUser { attempts: 2 });
        assert_eq!(fx.spawned.len(), 2);
        assert_eq!(fx.closed(), vec!["new-1".to_string(), "new-2".to_string()]);
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.close_reason.as_deref()),
            ("closed", Some("explicit"))
        );
        assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// Nit: an UNCONFIRMED migration whose old pane is already gone leaves no
    /// supersede mark behind — no close of it is coming to consume one.
    #[tokio::test]
    async fn an_unconfirmed_migration_with_the_old_pane_gone_leaves_no_mark() {
        let (_d, store) = fresh_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Unconfirmed]);
        fx.old_vanishes_in_spawn = true;
        confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(store.superseded_terminal_count(), 0);
    }

    // ---- round 4 -----------------------------------------------------------

    /// Finding 2: the old pane was closed BEFORE the respawn's hold existed
    /// (during the caller's usage probe / transcript copy). Its close was
    /// applied; nothing may move, or the spawn's re-key would reopen it.
    #[tokio::test]
    async fn an_old_pane_closed_before_the_hold_moves_nothing() {
        let (_d, store, closes) = observed_store();
        let old = record("old", "/src");
        store.record_close_checked(SID, Some("old"), "explicit");
        assert_eq!(
            closes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "fixture: applied"
        );
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Alive]);
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert!(matches!(out, RespawnOutcome::NotSpawned(_)), "{out:?}");
        assert!(fx.spawned.is_empty(), "no pane is spawned");
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.close_reason.as_deref()),
            ("closed", Some("explicit")),
            "the user's close stands"
        );
        assert_eq!(store.held_session_count(), 0);

        // The same when the old pane is simply gone while its row is open.
        let (_d2, store2) = fresh_store();
        let mut fx2 = Fake::new(&store2, vec![LivenessVerdict::Alive]);
        fx2.panes.gone.lock().unwrap().push("old".into());
        let out2 = confirm_respawn(&mut fx2, &store2, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert!(matches!(out2, RespawnOutcome::NotSpawned(_)), "{out2:?}");
        assert!(fx2.spawned.is_empty());
    }

    /// Finding 3: a pane judged Alive whose exit-close is already held has
    /// exited. It must not be handed the session (closing the healthy old
    /// pane); it is treated as Died and retried.
    #[tokio::test]
    async fn an_alive_verdict_on_a_pane_whose_exit_is_held_is_treated_as_died() {
        let (_d, store, closes) = observed_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Alive, LivenessVerdict::Alive]);
        fx.exit_on_first_alive = true;
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(
            out,
            RespawnOutcome::Alive {
                terminal_id: "new-2".into(),
                attempts: 2
            }
        );
        assert_eq!(fx.closed(), vec!["new-1".to_string(), "old".to_string()]);
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.terminal_id.as_str()),
            ("open", "new-2")
        );
        assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// Note: the top-of-loop old-pane check aborts BEFORE the next 3-minute
    /// proof starts — the pane spawned after the user closed the old pane is
    /// never awaited.
    #[tokio::test]
    async fn an_old_pane_closed_during_a_retry_spawn_aborts_before_the_next_proof() {
        let (_d, store) = fresh_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(
            &store,
            vec![LivenessVerdict::Died(Some(127)), LivenessVerdict::Alive],
        );
        fx.close_old_in_retry_spawn = true;
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(out, RespawnOutcome::ClosedByUser { attempts: 2 });
        assert_eq!(
            fx.awaited,
            vec!["new-1".to_string()],
            "new-2 is never proven"
        );
        assert!(fx.closed().contains(&"new-2".to_string()));
        assert_eq!(store.get(SID).unwrap().state, "closed");
    }

    /// Note: the old-pane check after the retry PAUSE — no retry is spawned
    /// once the user closed the old pane.
    #[tokio::test]
    async fn an_old_pane_closed_during_the_retry_pause_spawns_no_retry() {
        let (_d, store) = fresh_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Died(Some(127)); 3]);
        fx.close_old_in_pause = true;
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(out, RespawnOutcome::ClosedByUser { attempts: 1 });
        assert_eq!(
            fx.spawned.len(),
            1,
            "no retry spawned after the old pane's close"
        );
        assert_eq!(store.get(SID).unwrap().state, "closed");
    }

    /// Note: the `Some(_) => Gone` arm — the old pane's CLI EXITED (a held
    /// pty-exit, the pane still listed) — nothing is restored onto it.
    #[tokio::test]
    async fn an_old_pane_whose_cli_exited_is_never_restored_onto() {
        let (_d, store) = fresh_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Died(Some(127)); 3]);
        fx.old_exits_in_spawn = true;
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert!(
            matches!(out, RespawnOutcome::Died { attempts: 3, .. }),
            "{out:?}"
        );
        let row = store.get(SID).unwrap();
        assert_eq!(row.state, "closed", "not restored onto the exited old CLI");
        assert_ne!(row.terminal_id, "old");
    }

    /// Note: a user close of a pane the respawn already RETIRED (its exit had
    /// been taken, so no supersede mark absorbs it) is read session-wide and
    /// ends the respawn — not lost while the retry pane runs.
    #[tokio::test]
    async fn a_user_close_of_a_retired_pane_ends_the_respawn() {
        let (_d, store, closes) = observed_store();
        let mut fx = Fake::new(
            &store,
            vec![LivenessVerdict::Died(Some(127)), LivenessVerdict::Alive],
        );
        fx.exit_close = true;
        fx.user_close_retired_while_proving = true;
        let out = confirm_respawn(&mut fx, &store, None, MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(out, RespawnOutcome::ClosedByUser { attempts: 2 });
        assert!(fx.closed().contains(&"new-2".to_string()));
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.close_reason.as_deref()),
            ("closed", Some("explicit"))
        );
        assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// Note: the take and the release of the current pane's close are ONE
    /// store operation. A close of the current pane is injected at the START
    /// of the release (the store's test hook): taken and released in one lock
    /// it is APPLIED; with a separate take before the release it would land in
    /// between, be held, come back with the rest and be dropped — leaving the
    /// row open on a pane that exited.
    #[test]
    fn take_and_release_is_one_store_operation() {
        let (_d, store) = fresh_store();
        store.record_open(record("new-1", "/dst"));
        store.hold_session_closes(SID);
        store.record_close_checked(SID, Some("x"), "explicit");
        store.set_release_hook(|s| {
            s.record_close_checked(SID, Some("new-1"), "pty-exit");
        });
        let applied = finish_replaying(&store, SID, Some("new-1"));
        assert_eq!(
            applied.map(|c| c.terminal_id),
            Some("new-1".to_string()),
            "the close that landed as the release began is applied, not dropped"
        );
        let row = store.get(SID).unwrap();
        assert_eq!(
            (row.state.as_str(), row.close_reason.as_deref()),
            ("closed", Some("pty-exit"))
        );
        assert_eq!(store.held_session_count(), 0);
    }

    // ---- round 5 -----------------------------------------------------------

    /// Finding 1: the user dismisses a STALE tab (a dead pane still carrying
    /// this session id, owning no open row) while the resumed pane is being
    /// proven. That close is filed under the session, but it names a terminal
    /// this respawn never spawned: it is not the user's answer, and the
    /// healthy resumed pane keeps the session.
    #[tokio::test]
    async fn a_close_of_a_pane_the_respawn_never_spawned_does_not_end_it() {
        for old in [None, Some(record("old", "/src"))] {
            let (_d, store, closes) = observed_store();
            let mut fx = Fake::new(&store, vec![LivenessVerdict::Alive]);
            fx.user_close_stale_while_proving = true;
            let out = confirm_respawn(&mut fx, &store, old.as_ref(), MAX_RESPAWN_ATTEMPTS).await;
            assert_eq!(
                out,
                RespawnOutcome::Alive {
                    terminal_id: "new-1".into(),
                    attempts: 1
                },
                "old = {:?}",
                old.as_ref().map(|o| &o.terminal_id)
            );
            assert!(
                matches!(fx.exit_close_outcomes[0], CloseOutcome::HeldInFlight { .. }),
                "fixture: the stale close is filed under the session"
            );
            assert!(!fx.closed().contains(&"new-1".to_string()));
            let row = store.get(SID).unwrap();
            assert_eq!(
                (row.state.as_str(), row.terminal_id.as_str()),
                ("open", "new-1")
            );
            assert_eq!(
                closes.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "no coord Closed, nonce kept"
            );
            assert_eq!(store.held_session_count(), 0);
        }
    }

    /// Finding 1: the same stale close while a DEAD pane waits for its retry
    /// spawns the retry — the stale close stops nothing.
    #[tokio::test]
    async fn a_stale_tab_close_does_not_stop_a_retry() {
        let (_d, store, closes) = observed_store();
        let mut fx = Fake::new(
            &store,
            vec![LivenessVerdict::Died(Some(127)), LivenessVerdict::Alive],
        );
        fx.exit_close = true;
        fx.user_close_stale_while_proving = true;
        let out = confirm_respawn(&mut fx, &store, None, MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(
            out,
            RespawnOutcome::Alive {
                terminal_id: "new-2".into(),
                attempts: 2
            }
        );
        assert_eq!(store.get(SID).unwrap().state, "open");
        assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// Finding 2: the old pane is alive but no longer HOSTS a CLI (a restored
    /// pane is a shell that `claude --resume` was typed into; the CLI exited,
    /// the shell lives). An unconfirmed migration must not roll the session
    /// back onto a bare shell: the new pane keeps the row, and the old pane is
    /// marked superseded so its later close ends nothing.
    #[tokio::test]
    async fn an_unconfirmed_migration_whose_old_pane_hosts_no_cli_keeps_the_new_pane() {
        let (_d, store, closes) = observed_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Unconfirmed]);
        fx.panes.no_claude.lock().unwrap().push("old".into());
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(
            out,
            RespawnOutcome::Unconfirmed {
                terminal_id: "new-1".into()
            }
        );
        assert!(
            !fx.closed().contains(&"new-1".to_string()),
            "the running new CLI is not killed"
        );
        let row = store.get(SID).unwrap();
        assert_eq!(
            (
                row.state.as_str(),
                row.terminal_id.as_str(),
                row.config_dir.as_deref()
            ),
            ("open", "new-1", Some("/dst"))
        );
        assert!(
            superseded(&store, "old"),
            "the old shell's later close ends nothing"
        );
        assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(store.get(SID).unwrap().state, "open");
    }
}
