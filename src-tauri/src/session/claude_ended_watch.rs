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
use std::sync::Arc;
use std::time::Duration;

use tracing::info;

use crate::session::session_lifecycle_store::{SessionLifecycleStore, CLAUDE_ENDED_CLOSE_REASON};

/// How often the watch looks.
pub const TICK: Duration = Duration::from_secs(5);

/// Consecutive "claude missing" ticks, after `claude` was seen in the pane,
/// before the record is closed.
pub const MISSES_TO_CLOSE: u32 = 2;

/// What the watch remembers about one pane.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Pane {
    seen_alive: bool,
    misses: u32,
}

/// The per-terminal memory, pure over its observations.
#[derive(Debug, Default)]
pub struct EndedWatch {
    panes: HashMap<String, Pane>,
}

impl EndedWatch {
    /// One observation of `terminal_id`: whether a `claude` is present in its
    /// pane's process tree. `true` when the record should be closed now.
    pub fn observe(&mut self, terminal_id: &str, claude_present: bool) -> bool {
        let pane = self.panes.entry(terminal_id.to_string()).or_default();
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
            self.panes.remove(terminal_id);
            return true;
        }
        false
    }

    /// Forget panes that are no longer observed.
    pub fn retain(&mut self, live: &HashSet<String>) {
        self.panes.retain(|t, _| live.contains(t));
    }
}

/// Spawn the watch for the process's life.
pub fn spawn() {
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
    let live: HashSet<String> = candidates.iter().map(|(_, t, _)| t.clone()).collect();
    watch.retain(&live);
    if candidates.is_empty() {
        return;
    }
    let snap = crate::process_capture::process_tree::snapshot_process_table_public().await;
    if snap.names.is_empty() {
        return; // unreadable table: says nothing
    }
    for (id, terminal_id, pid) in candidates {
        if !crate::process_capture::health::pid_alive(pid) {
            continue;
        }
        let present = crate::process_capture::process_tree::claude_present_in_inclusive_subtree(
            pid,
            &snap,
            i64::MIN,
        );
        if watch.observe(&terminal_id, present)
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

    /// A pane whose `claude` was seen and then is missing for
    /// [`MISSES_TO_CLOSE`] ticks closes — in seconds, not minutes.
    #[test]
    fn a_claude_seen_then_gone_closes_after_the_debounce() {
        let mut w = EndedWatch::default();
        assert!(!w.observe("t", true));
        assert!(!w.observe("t", false), "one miss is a transient");
        assert!(w.observe("t", false), "two in a row: it ended");
        // Forgotten after closing: a later miss alone does nothing.
        assert!(!w.observe("t", false));
    }

    /// Never a pane whose `claude` it has not seen start: a boot restore still
    /// typing `claude --resume` into a fresh shell must not be closed.
    #[test]
    fn a_pane_never_seen_alive_is_never_closed() {
        let mut w = EndedWatch::default();
        for _ in 0..10 {
            assert!(!w.observe("fresh", false));
        }
        // A blip back to present resets the count.
        let mut w = EndedWatch::default();
        assert!(!w.observe("t", true));
        assert!(!w.observe("t", false));
        assert!(!w.observe("t", true));
        assert!(!w.observe("t", false));
        assert!(w.observe("t", false));
    }

    #[test]
    fn panes_no_longer_observed_are_forgotten() {
        let mut w = EndedWatch::default();
        w.observe("gone", true);
        w.observe("kept", true);
        w.retain(&["kept".to_string()].into_iter().collect());
        assert!(
            !w.observe("gone", false),
            "its earlier sighting was forgotten"
        );
        assert!(!w.observe("kept", false));
        assert!(w.observe("kept", false));
    }
}
