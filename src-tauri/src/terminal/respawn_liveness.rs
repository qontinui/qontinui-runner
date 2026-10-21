//! "Is the resumed CLI alive?" — the proof a `--resume` respawn must pass
//! before anything treats it as done, and the retry-then-rollback that runs
//! when it does not.
//!
//! Plan `2026-10-20-account-migration-respawn-dies-during-cli-reinstall-churn`
//! D2 / Phase 4. A successful PTY spawn is NOT a successful respawn: on
//! merytshost five of sixty account migrations spawned, closed the old pane,
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
//! On a death the next pane is spawned FIRST, then the dead pane is marked
//! superseded by it, and only then closed — so the dead pane's exit-close is
//! answered `TerminalSuperseded` and never closes the row the live pane now
//! owns. When the budget is spent and the caller had an OLD pane (an account
//! migration), the pre-migration record is restored whole with
//! `record_open(old.clone())` and the last dead pane is marked superseded by
//! the OLD terminal before it closes.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use super::account_migration::{probe_idle_prompt, IdlePromptProbe, ReadyDebounce};
use crate::session::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};

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
/// respawn's `await`. [`Self::settle`] consumes it, so it cannot settle twice;
/// dropping it unsettled outside a panic (an early exit added inside the
/// window) is a `debug_assert` failure and a `warn!` in release, instead of the
/// silent anchor release it would otherwise be.
#[must_use = "an owed settle must be settled, or the anchor it carries is silently released"]
pub(crate) struct OwedSettle<T> {
    inner: Option<T>,
}

impl<T> OwedSettle<T> {
    pub(crate) fn new(inner: Option<T>) -> Self {
        Self { inner }
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
        if self.inner.is_some() && !std::thread::panicking() {
            warn!("respawn-liveness: an owed settle was dropped unsettled — the carried continuation's anchor was released");
            if cfg!(debug_assertions) {
                panic!("OwedSettle dropped without settle()");
            }
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
    /// Spawn the next resumed pane (async binary pre-step, then the seam).
    /// `Ok(terminal_id)`; the spawn re-keys the session's row onto it.
    fn spawn_next(&mut self) -> impl Future<Output = Result<String, String>> + Send;
    /// Judge one pane ([`await_resumed_cli`] in production).
    fn await_liveness(&mut self, terminal_id: &str)
        -> impl Future<Output = LivenessVerdict> + Send;
    /// The pause before retry number `retry` (1-based).
    fn pause_before_retry(&mut self, retry: usize) -> impl Future<Output = ()> + Send;
    /// Close a pane.
    fn close_pane(&mut self, terminal_id: &str) -> Result<(), String>;
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
    /// `terminal_id` runs but never painted. Both panes are kept; with an old
    /// pane, the old one is marked superseded by the new (the new owns the row).
    Unconfirmed { terminal_id: String },
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

/// Mark `from` superseded by `to`, then close `from`. A close that fails (the
/// pane was already gone) withdraws the mark, so the register holds only
/// closes still expected.
fn hand_off<E: RespawnEffects>(
    store: &SessionLifecycleStore,
    effects: &mut E,
    from: &str,
    to: &str,
) {
    store.mark_terminal_superseded(from, to);
    if let Err(e) = effects.close_pane(from) {
        store.unmark_terminal_superseded(from);
        info!(from, to, error = %e, "respawn-liveness: superseded pane was already gone");
    }
}

/// Spawn, prove, retry, and (with an `old` pane) roll back. See the module
/// docs. `old` is the pre-respawn record when a live pane is being replaced
/// (account migration); `None` when nothing is left to keep (the respawn
/// receiver, the resume door).
pub(crate) async fn confirm_respawn<E: RespawnEffects>(
    effects: &mut E,
    store: &SessionLifecycleStore,
    old: Option<&TerminalSessionRecord>,
    max_attempts: usize,
) -> RespawnOutcome {
    let mut current = match effects.spawn_next().await {
        Ok(t) => t,
        Err(e) => return RespawnOutcome::NotSpawned(e),
    };
    let mut attempts = 1;
    loop {
        match effects.await_liveness(&current).await {
            LivenessVerdict::Alive => {
                if let Some(old) = old {
                    hand_off(store, effects, &old.terminal_id, &current);
                }
                return RespawnOutcome::Alive {
                    terminal_id: current,
                    attempts,
                };
            }
            LivenessVerdict::Unconfirmed | LivenessVerdict::Pending => {
                // UNKNOWN: keep both panes. The new pane owns the row; the old
                // one is marked so whichever closes first closes nothing it
                // does not own.
                if let Some(old) = old {
                    store.mark_terminal_superseded(&old.terminal_id, &current);
                }
                warn!(
                    terminal_id = %current,
                    "respawn-liveness: resumed CLI is running but never painted — keeping every pane (migration-unconfirmed)"
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
                if attempts < max_attempts {
                    effects.pause_before_retry(attempts).await;
                    match effects.spawn_next().await {
                        Ok(next) => {
                            // Next pane FIRST, then the hand-off: the dead pane's
                            // exit-close resolves as TerminalSuperseded.
                            hand_off(store, effects, &current, &next);
                            current = next;
                            attempts += 1;
                            continue;
                        }
                        Err(e) => retry_spawn_error = Some(e),
                    }
                }
                match old {
                    Some(old) => {
                        // Roll back: the row, its account and every re-keyed
                        // field go back onto the old terminal (re-opening it if
                        // a closer got there first), THEN the dead pane leaves.
                        store.record_open(old.clone());
                        hand_off(store, effects, &current, &old.terminal_id);
                    }
                    None => {
                        if let Err(e) = effects.close_pane(&current) {
                            info!(terminal_id = %current, error = %e, "respawn-liveness: dead pane already gone");
                        }
                    }
                }
                return RespawnOutcome::Died {
                    exit_code: code,
                    attempts,
                    retry_spawn_error,
                };
            }
        }
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
        OwedSettle::new(Some(7)).settle(|v| got = Some(v));
        assert_eq!(got, Some(7));
        OwedSettle::<u8>::new(None).settle(|_| panic!("nothing to settle"));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "OwedSettle dropped without settle()")]
    fn an_owed_settle_dropped_unsettled_is_loud() {
        let _owed = OwedSettle::new(Some(7));
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
    struct Fake<'a> {
        store: &'a SessionLifecycleStore,
        verdicts: VecDeque<LivenessVerdict>,
        spawn_errors: VecDeque<Option<String>>,
        spawned: Vec<String>,
        closed: Vec<String>,
        /// Panes whose close fails (already gone).
        gone: Vec<String>,
    }

    impl<'a> Fake<'a> {
        fn new(store: &'a SessionLifecycleStore, verdicts: Vec<LivenessVerdict>) -> Self {
            Self {
                store,
                verdicts: verdicts.into(),
                spawn_errors: VecDeque::new(),
                spawned: Vec::new(),
                closed: Vec::new(),
                gone: Vec::new(),
            }
        }
    }

    impl RespawnEffects for Fake<'_> {
        async fn spawn_next(&mut self) -> Result<String, String> {
            if let Some(Some(e)) = self.spawn_errors.pop_front() {
                return Err(e);
            }
            let id = format!("new-{}", self.spawned.len() + 1);
            self.store.record_open(record(&id, "/dst"));
            self.spawned.push(id.clone());
            Ok(id)
        }
        async fn await_liveness(&mut self, _terminal_id: &str) -> LivenessVerdict {
            self.verdicts
                .pop_front()
                .expect("a scripted verdict per pane")
        }
        async fn pause_before_retry(&mut self, _retry: usize) {}
        fn close_pane(&mut self, terminal_id: &str) -> Result<(), String> {
            if self.gone.iter().any(|g| g == terminal_id) {
                return Err("gone".into());
            }
            self.closed.push(terminal_id.to_string());
            Ok(())
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
        assert_eq!(fx.closed, vec!["old".to_string()]);
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
        assert_eq!(fx.closed, vec!["new-1".to_string(), "old".to_string()]);
        assert_eq!(
            fx.closed.iter().filter(|c| *c == "old").count(),
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
            !fx.closed.contains(&"old".to_string()),
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
        fx.spawn_errors = vec![None, Some("claude_cli_unavailable:not_found".into())].into();
        // Simulate `poll-dead` on the dead pane between spawn and rollback.
        struct Closing<'a, 'b>(&'b mut Fake<'a>);
        impl RespawnEffects for Closing<'_, '_> {
            async fn spawn_next(&mut self) -> Result<String, String> {
                self.0.spawn_next().await
            }
            async fn await_liveness(&mut self, t: &str) -> LivenessVerdict {
                let v = self.0.await_liveness(t).await;
                self.0.store.record_close(SID, "poll-dead");
                v
            }
            async fn pause_before_retry(&mut self, _r: usize) {}
            fn close_pane(&mut self, t: &str) -> Result<(), String> {
                self.0.close_pane(t)
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
                    attempts: 1,
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
    async fn alive_but_unpainted_keeps_both_panes_and_hands_the_row_to_the_new_one() {
        let (_d, store) = fresh_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Unconfirmed]);
        let out = confirm_respawn(&mut fx, &store, Some(&old), MAX_RESPAWN_ATTEMPTS).await;
        assert_eq!(
            out,
            RespawnOutcome::Unconfirmed {
                terminal_id: "new-1".into()
            }
        );
        assert!(fx.closed.is_empty(), "no pane is closed on an unknown");
        assert_eq!(store.get(SID).unwrap().terminal_id, "new-1");
        assert!(
            superseded(&store, "old"),
            "the old pane closing later closes nothing"
        );
        assert_eq!(store.get(SID).unwrap().state, "open");
    }

    /// The existing `unmark` arm: the old pane was closed by the operator after
    /// exhaustion, so its close fails and the supersede mark is withdrawn.
    #[tokio::test]
    async fn an_old_pane_already_gone_withdraws_its_supersede_mark() {
        let (_d, store) = fresh_store();
        let old = record("old", "/src");
        let mut fx = Fake::new(&store, vec![LivenessVerdict::Alive]);
        fx.gone.push("old".into());
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
        assert!(fx.closed.is_empty());
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
        assert_eq!(fx.closed, vec!["new-1", "new-2", "new-3"]);
        assert!(superseded(&store, "new-1"));
        assert!(superseded(&store, "new-2"));
    }
}
