//! Prompt detection of a `claude` that the user ended under a living shell.
//!
//! When the user types `/exit` in a pane whose shell outlives `claude`, the
//! PTY does not exit, so the runner sees only `claude` disappearing from the
//! pane's process tree. That alone cannot tell `/exit` from an OOM kill, a
//! SIGKILL or a crash: `claude` is the shell's child, so its exit status is
//! invisible. The positive evidence is Claude Code's own `SessionEnd` hook
//! (delivered by `session::claude_hook`), which fires on `/exit` with reason
//! `prompt_input_exit` and does not fire on a SIGKILL or OOM kill. It reports
//! to `POST /sessions/{id}/session-end`, which records a per-session
//! deliberate-end marker here ([`record_session_end`]).
//!
//! This watch then closes such a record in seconds (the 45 s lifecycle poll
//! debounces for minutes, and a runner killed in that window brought the
//! session back at boot — E2E, round 10) as [`CLAUDE_ENDED_CLOSE_REASON`]:
//! never auto-resumed. It closes a record ONLY with the marker, and only after
//! it has itself seen that pane's `claude` alive and then missing for
//! [`MISSES_TO_CLOSE`] consecutive ticks. Without the marker, `claude` gone is
//! left to the poll, which closes it `poll-dead` — restorable. It never acts
//! while the runner is shutting down, on an unreadable process table, or on a
//! record held for a restore or an account choice.

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

/// The `SessionEnd` reasons that are POSITIVE evidence the user ended the
/// session on purpose. Claude Code sends one of `clear`, `resume`, `logout`,
/// `prompt_input_exit`, `other` (read off the 2.1.304 bundle's own list):
///
/// * `prompt_input_exit` — the input box's exit: `/exit`, a double Ctrl-D, the
///   "exit to background" dialog. The only deliberate END.
/// * `clear` / `resume` — `/clear` and `/resume` end one session id and carry
///   on in the same process: the pane's `claude` did not end.
/// * `logout` — the user logged out; the session is not finished, and a resume
///   after logging back in is wanted. Not counted (conservative: restorable).
/// * `other` — every other graceful shutdown, which in that bundle is where
///   SIGINT (`shutdown(0)`), SIGTERM (`shutdown(143)`), SIGHUP (`shutdown(129)`)
///   and the orphan check (its parent / terminal gone, `shutdown(0)`) land.
///   Those are a restart's deaths, so never a marker.
///
/// A SIGKILL / OOM kill / crash fires no `SessionEnd` at all, so it leaves no
/// marker — which is the point: no marker, no `claude-ended`.
pub const DELIBERATE_END_REASONS: [&str; 1] = ["prompt_input_exit"];

/// Most markers kept: a marker is consumed when its record closes, so the map
/// stays small; this only bounds sessions whose record closed elsewhere.
const MAX_MARKERS: usize = 1024;

/// Claude sessions whose `SessionEnd` hook reported a deliberate end
/// ([`DELIBERATE_END_REASONS`]), keyed by the CLAUDE session id the hook
/// payload carries (so a second session in the same terminal never inherits
/// it), valued by when it was recorded (for the bound's eviction).
static DELIBERATE_ENDS: Mutex<Option<HashMap<String, i64>>> = Mutex::new(None);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The `SessionEnd` hook's report for `claude_session_id`: records the
/// deliberate-end marker when `reason` is one of [`DELIBERATE_END_REASONS`].
/// `true` when a marker was recorded.
pub fn record_session_end(claude_session_id: &str, reason: &str) -> bool {
    let id = claude_session_id.trim();
    if id.is_empty() || !DELIBERATE_END_REASONS.contains(&reason) {
        return false;
    }
    let mut guard = DELIBERATE_ENDS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() >= MAX_MARKERS && !map.contains_key(id) {
        if let Some(oldest) = map
            .iter()
            .min_by_key(|(_, at)| **at)
            .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
    }
    map.insert(id.to_string(), now_ms());
    true
}

/// [`record_session_end`] over the hook's raw payload (`{session_id, reason,
/// ...}`): what `POST /sessions/{id}/session-end` does with its body.
pub fn record_session_end_payload(payload: &serde_json::Value) -> bool {
    match (
        payload.get("session_id").and_then(|v| v.as_str()),
        payload.get("reason").and_then(|v| v.as_str()),
    ) {
        (Some(id), Some(reason)) => record_session_end(id, reason),
        _ => false,
    }
}

/// Whether `claude_session_id` reported a deliberate end.
pub fn deliberate_end(claude_session_id: &str) -> bool {
    DELIBERATE_ENDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|m| m.contains_key(claude_session_id))
}

/// Drop `claude_session_id`'s marker: its record closed (consumed), or the
/// session (re)started — a resume of the same id after an account migration
/// or a boot restore must not inherit the previous run's `/exit`.
pub fn forget_session_end(claude_session_id: &str) {
    if let Some(m) = DELIBERATE_ENDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        m.remove(claude_session_id);
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
/// same decision).
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
    for (id, terminal_id, _) in candidates {
        let Some(present) = present_by_terminal.get(&terminal_id).copied().flatten() else {
            continue; // the shell itself is gone: the PTY exit decides
        };
        // Gone from a living pane is `claude-ended` only with the session's
        // own deliberate-end report (`SessionEnd` `prompt_input_exit`): `claude`
        // is the shell's child, so its exit status is invisible here, and a
        // SIGKILL / OOM kill / crash looks exactly like `/exit` without it.
        // Without the marker the 45 s poll closes it `poll-dead` (restorable).
        if watch.observe(&terminal_id, &id, present)
            && deliberate_end(&id)
            && !crate::commands::terminal_windows::is_app_quitting()
        {
            info!(
                claude_session = %id,
                terminal_id = %terminal_id,
                "claude-ended watch: claude is gone from a living pane — closing {CLAUDE_ENDED_CLOSE_REASON}"
            );
            store.record_close_checked(&id, Some(&terminal_id), CLAUDE_ENDED_CLOSE_REASON);
            forget_session_end(&id);
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

    /// Review round 12 S1: only `prompt_input_exit` is a marker; a signal's
    /// graceful shutdown (`other`), `/clear`, `/resume` and `logout` are not.
    /// Keyed per CLAUDE session, and forgotten on close or restart.
    #[test]
    fn only_a_prompt_input_exit_records_the_deliberate_end_marker() {
        let id = "claude-ended-watch-marker-a";
        for reason in ["other", "clear", "resume", "logout", ""] {
            assert!(!record_session_end(id, reason), "{reason}");
            assert!(!deliberate_end(id), "{reason}");
        }
        assert!(record_session_end(id, "prompt_input_exit"));
        assert!(deliberate_end(id));
        assert!(
            !deliberate_end("claude-ended-watch-marker-b"),
            "per session"
        );
        forget_session_end(id);
        assert!(!deliberate_end(id));
    }

    /// A hook POST body records the marker (what the route does with it).
    #[test]
    fn a_session_end_payload_records_the_marker() {
        let id = "claude-ended-watch-marker-payload";
        assert!(!record_session_end_payload(
            &serde_json::json!({"session_id": id, "reason": "other"})
        ));
        assert!(!record_session_end_payload(
            &serde_json::json!({"reason": "prompt_input_exit"})
        ));
        assert!(record_session_end_payload(&serde_json::json!({
            "session_id": id, "reason": "prompt_input_exit", "hook_event_name": "SessionEnd"
        })));
        assert!(deliberate_end(id));
        forget_session_end(id);
    }
}
