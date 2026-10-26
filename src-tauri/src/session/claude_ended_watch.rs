//! Prompt detection of a `claude` that ended under a living shell.
//!
//! When the user types `/exit` in a pane whose shell outlives `claude`, the
//! PTY does not exit, so the only signal is `claude` disappearing from the
//! pane's process tree. The 45 s lifecycle poll debounces that for several
//! ticks (`LIVE_SHELL_DEAD_TICKS`), so such a record stayed `open` for two to
//! three minutes — and a runner killed in that window brought the session back
//! at its next boot (E2E, round 10). This watch closes it in seconds instead,
//! as [`CLAUDE_ENDED_CLOSE_REASON`]: the live runner saw it end, so it is not a
//! restart's death and is never auto-resumed.
//!
//! It closes a record ONLY after it has itself seen that pane's `claude`
//! alive and then missing for [`MISSES_TO_CLOSE`] consecutive ticks — never a
//! pane whose `claude` it has not yet seen start (a boot restore still typing
//! `claude --resume`, a freshly spawned pane). It never acts while the runner
//! is shutting down, on an unreadable process table, or on a record held for
//! a restore or an account choice.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::info;

use crate::session::session_lifecycle_store::{SessionLifecycleStore, CLAUDE_ENDED_CLOSE_REASON};

/// How often the watch looks.
pub const TICK: Duration = Duration::from_secs(5);

/// Consecutive "claude missing" ticks, after `claude` was seen in the pane,
/// before the record is closed.
pub const MISSES_TO_CLOSE: u32 = 2;

/// Terminals in which this process has SEEN a `claude` alive — by this watch
/// or by the 45 s lifecycle poll. A `claude` missing from a pane only counts as
/// having ENDED there (a deliberate end, `claude-ended`) when it was seen alive
/// in it first; a pane whose `claude` was never seen — an adopted pane under
/// the PID-reuse guard, a root whose `comm` is not `claude`, a boot restore
/// still typing the resume — keeps the restart-shaped close.
static SEEN_ALIVE: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Record that `terminal_id`'s pane hosts a live `claude` right now.
pub fn note_claude_seen(terminal_id: &str) {
    SEEN_ALIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashSet::new)
        .insert(terminal_id.to_string());
}

/// Whether this process has seen a `claude` alive in `terminal_id`'s pane.
pub fn claude_seen(terminal_id: &str) -> bool {
    SEEN_ALIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|s| s.contains(terminal_id))
}

/// Forget a pane (its terminal is gone, or its record was closed).
pub fn forget_pane(terminal_id: &str) {
    if let Some(s) = SEEN_ALIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        s.remove(terminal_id);
    }
}

/// What the watch remembers about one (terminal, session) row.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Pane {
    seen_alive: bool,
    misses: u32,
}

/// The per-row memory, pure over its observations. Keyed by (terminal,
/// session): two open rows on one terminal each keep their own debounce, so
/// neither halves the other's nor resets it when it closes (review S1).
#[derive(Debug, Default)]
pub struct EndedWatch {
    panes: HashMap<(String, String), Pane>,
}

impl EndedWatch {
    /// One observation of the row (`terminal_id`, `session_id`): whether a
    /// `claude` is present in its pane's process tree (read ONCE per terminal
    /// per tick by the caller). `true` when the record should be closed now.
    pub fn observe(&mut self, terminal_id: &str, session_id: &str, claude_present: bool) -> bool {
        let key = (terminal_id.to_string(), session_id.to_string());
        let pane = self.panes.entry(key.clone()).or_default();
        if claude_present {
            *pane = Pane {
                seen_alive: true,
                misses: 0,
            };
            return false;
        }
        if !pane.seen_alive {
            return false;
        }
        pane.misses += 1;
        if pane.misses >= MISSES_TO_CLOSE {
            self.panes.remove(&key);
            return true;
        }
        false
    }

    /// Forget rows that are no longer observed.
    pub fn retain(&mut self, live: &HashSet<(String, String)>) {
        self.panes.retain(|k, _| live.contains(k));
    }
}

/// Spawn the watch for the process's life — on unix only. Its 5 s process-
/// table snapshot is a cheap `/proc` walk there; on Windows the same snapshot
/// is a PowerShell + WMI query (seconds of CPU), too heavy every 5 s. Windows
/// keeps the 45 s lifecycle poll's witnessed rule instead (slower, but the
/// same decision), which also feeds [`note_claude_seen`].
pub fn spawn() {
    if !cfg!(unix) {
        return;
    }
    crate::worker_supervisor::spawn_supervised_on_tauri_with_heartbeat(
        "session.claude_ended_watch",
        move |hb| async move {
            let mut watch = EndedWatch::default();
            let mut ticker = tokio::time::interval(TICK);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                hb.tick();
                tick_live(&mut watch).await;
            }
        },
    );
}

async fn tick_live(watch: &mut EndedWatch) {
    use tauri::Manager;
    if crate::commands::terminal_windows::is_app_quitting() {
        return;
    }
    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(store) = app.try_state::<Arc<SessionLifecycleStore>>() else {
        return;
    };
    let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
        return;
    };
    let candidates: Vec<(String, String, u32)> = store
        .open_records()
        .into_iter()
        .filter(|r| {
            r.provider == crate::session::session_lifecycle_store::DEFAULT_PROVIDER
                && r.task_run_id.is_none()
                && r.restore_pending_at.is_none()
                && r.awaiting_account_since.is_none()
                && r.confirmed_at.is_some()
        })
        .filter_map(|r| {
            let session = tm.get(&r.terminal_id)?;
            if !session.is_alive() {
                return None; // the PTY's own exit records this close
            }
            Some((r.claude_session_id, r.terminal_id, session.child_pid()?))
        })
        .collect();
    let live: HashSet<(String, String)> = candidates
        .iter()
        .map(|(id, t, _)| (t.clone(), id.clone()))
        .collect();
    watch.retain(&live);
    if candidates.is_empty() {
        return;
    }
    let snap = crate::process_capture::process_tree::snapshot_process_table_public().await;
    if snap.names.is_empty() {
        return; // unreadable table: says nothing
    }
    // Read each pane ONCE per tick, however many rows it carries.
    let mut present_by_terminal: HashMap<String, Option<bool>> = HashMap::new();
    for (_, terminal_id, pid) in &candidates {
        present_by_terminal
            .entry(terminal_id.clone())
            .or_insert_with(|| {
                crate::process_capture::health::pid_alive(*pid).then(|| {
                    crate::process_capture::process_tree::claude_present_in_inclusive_subtree(
                        *pid,
                        &snap,
                        i64::MIN,
                    )
                })
            });
    }
    for (terminal_id, present) in &present_by_terminal {
        if *present == Some(true) {
            note_claude_seen(terminal_id);
        }
    }
    for (id, terminal_id, _) in candidates {
        let Some(present) = present_by_terminal.get(&terminal_id).copied().flatten() else {
            continue; // the shell itself is gone: the PTY exit decides
        };
        if watch.observe(&terminal_id, &id, present)
            && !crate::commands::terminal_windows::is_app_quitting()
        {
            info!(
                claude_session = %id,
                terminal_id = %terminal_id,
                "claude-ended watch: claude is gone from a living pane — closing {CLAUDE_ENDED_CLOSE_REASON}"
            );
            store.record_close_checked(&id, Some(&terminal_id), CLAUDE_ENDED_CLOSE_REASON);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row whose `claude` was seen and then is missing for
    /// [`MISSES_TO_CLOSE`] ticks closes — in seconds, not minutes.
    #[test]
    fn a_claude_seen_then_gone_closes_after_the_debounce() {
        let mut w = EndedWatch::default();
        assert!(!w.observe("t", "s", true));
        assert!(!w.observe("t", "s", false), "one miss is a transient");
        assert!(w.observe("t", "s", false), "two in a row: it ended");
        // Forgotten after closing: a later miss alone does nothing.
        assert!(!w.observe("t", "s", false));
    }

    /// Never a pane whose `claude` it has not seen start: a boot restore still
    /// typing `claude --resume` into a fresh shell must not be closed.
    #[test]
    fn a_pane_never_seen_alive_is_never_closed() {
        let mut w = EndedWatch::default();
        for _ in 0..10 {
            assert!(!w.observe("fresh", "s", false));
        }
        // A blip back to present resets the count.
        let mut w = EndedWatch::default();
        assert!(!w.observe("t", "s", true));
        assert!(!w.observe("t", "s", false));
        assert!(!w.observe("t", "s", true));
        assert!(!w.observe("t", "s", false));
        assert!(w.observe("t", "s", false));
    }

    /// Review S1: two open rows on ONE terminal keep their own debounce — the
    /// pane is read once per tick and each row sees that one reading, so
    /// neither closes after a single tick and closing one leaves the other's
    /// state intact.
    #[test]
    fn two_rows_on_one_terminal_keep_their_own_debounce() {
        let mut w = EndedWatch::default();
        assert!(!w.observe("t", "a", true));
        assert!(!w.observe("t", "b", true));
        // Tick 1: claude gone — one miss each, nobody closes.
        assert!(!w.observe("t", "a", false));
        assert!(!w.observe("t", "b", false));
        // Tick 2: both reach the debounce on the same tick.
        assert!(w.observe("t", "a", false));
        assert!(w.observe("t", "b", false), "a's close did not reset b");
    }

    #[test]
    fn rows_no_longer_observed_are_forgotten() {
        let mut w = EndedWatch::default();
        w.observe("gone", "s", true);
        w.observe("kept", "s", true);
        w.retain(
            &[("kept".to_string(), "s".to_string())]
                .into_iter()
                .collect(),
        );
        assert!(
            !w.observe("gone", "s", false),
            "its earlier sighting was forgotten"
        );
        assert!(!w.observe("kept", "s", false));
        assert!(w.observe("kept", "s", false));
    }

    #[test]
    fn the_seen_alive_register_is_per_terminal() {
        let t = "claude-ended-watch-seen-register";
        assert!(!claude_seen(t));
        note_claude_seen(t);
        assert!(claude_seen(t));
        forget_pane(t);
        assert!(!claude_seen(t));
    }
}
