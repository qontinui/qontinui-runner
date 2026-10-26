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

/// How long a marker stays evidence (review round 13 N3). Every consumer acts
/// well inside it — the pane's own exit at once, the watch within seconds, the
/// 45 s poll after `LIVE_SHELL_DEAD_TICKS` ticks (minutes) — so an older marker
/// belongs to no close in flight and counts as absent (and is pruned).
pub const MARKER_TTL_MS: i64 = 10 * 60 * 1000;

/// Claude sessions whose `SessionEnd` hook reported a deliberate end
/// ([`DELIBERATE_END_REASONS`]), keyed by the CLAUDE session id the hook
/// payload carries (so a second session in the same terminal never inherits
/// it), valued by when it was recorded (for the TTL and the bound).
static DELIBERATE_ENDS: Mutex<Option<HashMap<String, i64>>> = Mutex::new(None);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// PURE: whether `id` is shaped like a Claude session id — a UUID
/// (8-4-4-4-12 hex, 36 bytes). Anything else is refused before it is stored
/// or logged: the id comes from a loopback POST any local process can send,
/// so an unbounded or control-character id must never reach the map or a log
/// line (review round 13 S1).
pub fn is_session_id_shaped(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Records the deliberate-end marker for `claude_session_id` when `reason`
/// is one of [`DELIBERATE_END_REASONS`] and the id is UUID-shaped. `true` when
/// a marker was recorded. Callers holding a hook report go through
/// [`report_session_end`], which also requires the session to be OPEN on the
/// reporting terminal.
pub fn record_session_end(claude_session_id: &str, reason: &str) -> bool {
    record_session_end_at(claude_session_id, reason, now_ms())
}

fn record_session_end_at(claude_session_id: &str, reason: &str, now: i64) -> bool {
    let id = claude_session_id;
    if !is_session_id_shaped(id) || !DELIBERATE_END_REASONS.contains(&reason) {
        return false;
    }
    let mut guard = DELIBERATE_ENDS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    map.retain(|_, at| now - *at < MARKER_TTL_MS);
    if map.len() >= MAX_MARKERS && !map.contains_key(id) {
        if let Some(oldest) = map
            .iter()
            .min_by_key(|(_, at)| **at)
            .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
    }
    map.insert(id.to_string(), now);
    true
}

/// What a `SessionEnd` hook report came to ([`report_session_end`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEndVerdict {
    /// The marker was recorded.
    Recorded,
    /// A well-formed report whose reason is not a deliberate end: nothing to
    /// record.
    NotDeliberate,
    /// No `session_id`, or one that is not UUID-shaped: refused.
    BadSessionId,
    /// The session has no OPEN lifecycle record hosted on the reporting
    /// terminal: refused (a forged or misdirected report).
    NoOpenRecordOnTerminal,
}

/// The `SessionEnd` hook's report (`{session_id, reason, ...}`) from the pane
/// on `terminal_id`. Records the marker only for a UUID-shaped id whose OPEN
/// lifecycle record is hosted on THAT terminal, and only for a deliberate
/// reason — so a local process cannot mark an arbitrary session ended without
/// also naming the terminal it actually runs on (review round 13 S1).
pub fn report_session_end(
    store: &SessionLifecycleStore,
    terminal_id: &str,
    payload: &serde_json::Value,
) -> SessionEndVerdict {
    let Some(id) = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|id| is_session_id_shaped(id))
    else {
        return SessionEndVerdict::BadSessionId;
    };
    let hosted_here = store
        .get(id)
        .is_some_and(|r| r.state == "open" && r.terminal_id == terminal_id);
    if !hosted_here {
        return SessionEndVerdict::NoOpenRecordOnTerminal;
    }
    let reason = payload.get("reason").and_then(|v| v.as_str()).unwrap_or("");
    if record_session_end(id, reason) {
        SessionEndVerdict::Recorded
    } else {
        SessionEndVerdict::NotDeliberate
    }
}

/// Whether `claude_session_id` reported a deliberate end within
/// [`MARKER_TTL_MS`].
pub fn deliberate_end(claude_session_id: &str) -> bool {
    deliberate_end_at(claude_session_id, now_ms())
}

fn deliberate_end_at(claude_session_id: &str, now: i64) -> bool {
    let mut guard = DELIBERATE_ENDS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = guard.as_mut() else {
        return false;
    };
    match map.get(claude_session_id) {
        Some(at) if now - *at < MARKER_TTL_MS => true,
        Some(_) => {
            map.remove(claude_session_id);
            false
        }
        None => false,
    }
}

/// Drop `claude_session_id`'s marker: the session (re)started — a resume of
/// the same id after an account migration or a boot restore must not inherit
/// the previous run's `/exit`.
pub fn forget_session_end(claude_session_id: &str) {
    if let Some(m) = DELIBERATE_ENDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        m.remove(claude_session_id);
    }
}

/// Consume `claude_session_id`'s marker after a close — only when that close
/// actually CLOSED its record (review round 13 S3). A refused or redirected
/// close (`AlreadyClosed`, `NotFound`, `TerminalSuperseded`, …) leaves the
/// record open, and the marker must survive for the close that does land.
pub fn consume_marker_after(
    outcome: &crate::session::session_lifecycle_store::CloseOutcome,
    claude_session_id: &str,
) {
    if matches!(
        outcome,
        crate::session::session_lifecycle_store::CloseOutcome::Closed { .. }
    ) {
        forget_session_end(claude_session_id);
    }
}

/// PURE over the watch and the marker register: whether this observation of
/// the row closes it `claude-ended` now. Gone from a living pane is
/// `claude-ended` only with the session's own deliberate-end report
/// (`SessionEnd` `prompt_input_exit`): `claude` is the shell's child, so its
/// exit status is invisible here, and a SIGKILL / OOM kill / crash looks
/// exactly like `/exit` without it. Without the marker the record stays open
/// here and the 45 s poll closes it `poll-dead` (restorable).
///
/// The debounce is spent either way: a marker that arrives AFTER the watch's
/// debounce completed is intentionally left to the 45 s poll, which reads the
/// marker at its own close (review round 13 N6) — the watch is only the fast
/// path, never the only one.
pub fn closes_now(
    watch: &mut EndedWatch,
    terminal_id: &str,
    claude_session_id: &str,
    claude_present: bool,
    quitting: bool,
) -> bool {
    watch.observe(terminal_id, claude_session_id, claude_present)
        && deliberate_end(claude_session_id)
        && !quitting
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
        if closes_now(
            watch,
            &terminal_id,
            &id,
            present,
            crate::commands::terminal_windows::is_app_quitting(),
        ) {
            info!(
                claude_session = ?id,
                terminal_id = ?terminal_id,
                "claude-ended watch: claude is gone from a living pane — closing {CLAUDE_ENDED_CLOSE_REASON}"
            );
            let outcome =
                store.record_close_checked(&id, Some(&terminal_id), CLAUDE_ENDED_CLOSE_REASON);
            consume_marker_after(&outcome, &id);
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

    const SID_A: &str = "0f6c2a3e-1b2c-4d5e-8f90-a1b2c3d4e5f6";
    const SID_B: &str = "1f6c2a3e-1b2c-4d5e-8f90-a1b2c3d4e5f6";

    /// Review round 12 S1: only `prompt_input_exit` is a marker; a signal's
    /// graceful shutdown (`other`), `/clear`, `/resume` and `logout` are not.
    /// Keyed per CLAUDE session, and forgotten on restart.
    #[test]
    fn only_a_prompt_input_exit_records_the_deliberate_end_marker() {
        let id = "2f6c2a3e-1b2c-4d5e-8f90-a1b2c3d4e5f6";
        for reason in ["other", "clear", "resume", "logout", ""] {
            assert!(!record_session_end(id, reason), "{reason}");
            assert!(!deliberate_end(id), "{reason}");
        }
        assert!(record_session_end(id, "prompt_input_exit"));
        assert!(deliberate_end(id));
        assert!(
            !deliberate_end("3f6c2a3e-1b2c-4d5e-8f90-a1b2c3d4e5f6"),
            "per session"
        );
        forget_session_end(id);
        assert!(!deliberate_end(id));
    }

    /// Review round 13 S1: the id must be UUID-shaped — never an unbounded or
    /// control-character string from a forged POST.
    #[test]
    fn a_non_uuid_session_id_is_refused() {
        assert!(is_session_id_shaped(SID_A));
        for bad in [
            "",
            "sess-1",
            "0f6c2a3e-1b2c-4d5e-8f90-a1b2c3d4e5f6x",
            "0f6c2a3e_1b2c-4d5e-8f90-a1b2c3d4e5f6",
            "0f6c2a3e-1b2c-4d5e-8f90-a1b2c3d4e5fZ",
            "0f6c2a3e-1b2c-4d5e-8f90-a1b2c3d4e5\n",
        ] {
            assert!(!is_session_id_shaped(bad), "{bad:?}");
            assert!(!record_session_end(bad, "prompt_input_exit"), "{bad:?}");
        }
        let huge = "a".repeat(2 * 1024 * 1024);
        assert!(!record_session_end(&huge, "prompt_input_exit"));
    }

    fn store_with_open(dir: &tempfile::TempDir, id: &str, terminal: &str) -> SessionLifecycleStore {
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        let mut rec = crate::session::session_lifecycle_store::test_open_record(id);
        rec.terminal_id = terminal.to_string();
        store.record_open(rec);
        store
    }

    /// Review round 13 S1: a report is recorded only for the session's OPEN
    /// record on the reporting terminal; a mismatched terminal, an unknown or
    /// closed session, or a bad id records nothing.
    #[test]
    fn a_session_end_report_records_only_for_the_session_open_on_that_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_open(&dir, SID_A, "term-a");
        let exit = |id: &str| serde_json::json!({"session_id": id, "reason": "prompt_input_exit"});
        assert_eq!(
            report_session_end(&store, "term-other", &exit(SID_A)),
            SessionEndVerdict::NoOpenRecordOnTerminal
        );
        assert!(!deliberate_end(SID_A));
        assert_eq!(
            report_session_end(&store, "term-a", &exit(SID_B)),
            SessionEndVerdict::NoOpenRecordOnTerminal,
            "no record for that session"
        );
        assert_eq!(
            report_session_end(&store, "term-a", &exit("sess-1")),
            SessionEndVerdict::BadSessionId
        );
        assert_eq!(
            report_session_end(
                &store,
                "term-a",
                &serde_json::json!({"session_id": SID_A, "reason": "other"})
            ),
            SessionEndVerdict::NotDeliberate
        );
        assert!(!deliberate_end(SID_A));
        assert_eq!(
            report_session_end(&store, "term-a", &exit(SID_A)),
            SessionEndVerdict::Recorded
        );
        assert!(deliberate_end(SID_A));
        forget_session_end(SID_A);

        // A closed record is no longer a target.
        store.record_close(SID_A, "explicit");
        assert_eq!(
            report_session_end(&store, "term-a", &exit(SID_A)),
            SessionEndVerdict::NoOpenRecordOnTerminal
        );
    }

    /// Review round 13 N3: a marker older than the TTL counts as absent and
    /// is pruned.
    #[test]
    fn a_marker_past_its_ttl_counts_as_absent() {
        let id = "4f6c2a3e-1b2c-4d5e-8f90-a1b2c3d4e5f6";
        assert!(record_session_end_at(id, "prompt_input_exit", 1_000));
        assert!(deliberate_end_at(id, 1_000 + MARKER_TTL_MS - 1));
        assert!(!deliberate_end_at(id, 1_000 + MARKER_TTL_MS));
        // Pruned: even a read "back in time" finds nothing now.
        assert!(!deliberate_end_at(id, 1_000));
    }

    /// Review round 13 S3: the marker is consumed only by a close that
    /// actually closed the record; a refused close leaves it for the close
    /// that will land.
    #[test]
    fn only_a_landed_close_consumes_the_marker() {
        use crate::session::session_lifecycle_store::CloseOutcome;
        let id = "5f6c2a3e-1b2c-4d5e-8f90-a1b2c3d4e5f6";
        assert!(record_session_end(id, "prompt_input_exit"));
        consume_marker_after(
            &CloseOutcome::AlreadyClosed {
                claude_session_id: id.to_string(),
            },
            id,
        );
        assert!(deliberate_end(id), "a refused close keeps the marker");
        consume_marker_after(
            &CloseOutcome::Closed {
                claude_session_id: id.to_string(),
            },
            id,
        );
        assert!(!deliberate_end(id), "a landed close consumes it");
    }

    /// The watch closes a gone row only with the marker: without it the
    /// record stays open (the poll closes it restorably).
    #[test]
    fn the_watch_keeps_a_gone_row_open_without_a_marker() {
        let id = "6f6c2a3e-1b2c-4d5e-8f90-a1b2c3d4e5f6";
        let mut w = EndedWatch::default();
        assert!(!closes_now(&mut w, "t", id, true, false));
        assert!(!closes_now(&mut w, "t", id, false, false));
        assert!(
            !closes_now(&mut w, "t", id, false, false),
            "debounce met, but no deliberate-end report"
        );
        let mut w = EndedWatch::default();
        assert!(record_session_end(id, "prompt_input_exit"));
        assert!(!closes_now(&mut w, "t", id, true, false));
        assert!(!closes_now(&mut w, "t", id, false, false));
        assert!(closes_now(&mut w, "t", id, false, false));
        // Never while the runner is shutting down.
        let mut w = EndedWatch::default();
        assert!(!closes_now(&mut w, "t", id, true, true));
        assert!(!closes_now(&mut w, "t", id, false, true));
        assert!(!closes_now(&mut w, "t", id, false, true));
        forget_session_end(id);
    }
}
