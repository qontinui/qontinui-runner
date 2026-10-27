//! A short-lived ledger of the account migration's own respawns, read by the
//! duplicate-host detector ([`crate::session::duplicate_hosts`]) so the
//! migration's ~12 s hand-off (old pane still alive while the resumed pane has
//! started) is never reported as a fork (plan
//! `2026-10-21-account-migration-strands-subagent-transcripts-and-forks-live-sessions`,
//! Phase 5).
//!
//! Lives in its own file, not in `account_migration.rs`, which only calls
//! [`note_migration_respawn`] — with the NEW terminal id and the session id,
//! right after a successful respawn, next to `store.mark_terminal_superseded`.
//!
//! It also keeps the respawned PANES for the usage-limit watcher's replay
//! suppression (Phase 2b, [`usage_limit_replay_verdict`]): a pane a migration
//! respawned re-renders the old transcript, so its first usage-limit match is
//! the replayed banner, not news. On 2026-10-22 the watcher re-matched it on
//! the new pane three times in 30 minutes and each probe correctly found the
//! new account fine — churn, and a stale banner that could have spent the
//! migration cap.
//!
//! The replayed banner sits just above the input box, inside the bottom rows,
//! so when a suppression lifts with no turn it can still yield ONE hint per
//! debounce. That hint is silent: `handle_usage_limit_hint` probes the account
//! first and, when it is not exhausted, ends with an info log — no event, no
//! record written, no identity refusal reported.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a respawn excludes its session from duplicate reporting.
pub const HANDOFF_WINDOW_MS: i64 = 60_000;

struct Entry {
    terminal_id: String,
    session_id: String,
    at_ms: i64,
}

static LEDGER: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// Record that `migrate_session` respawned `session_id` into
/// `new_terminal_id` at `now_ms`.
pub fn note_migration_respawn(new_terminal_id: &str, session_id: &str, now_ms: i64) {
    if let Ok(mut l) = LEDGER.lock() {
        l.retain(|e| now_ms - e.at_ms <= HANDOFF_WINDOW_MS);
        l.push(Entry {
            terminal_id: new_terminal_id.to_string(),
            session_id: session_id.to_string(),
            at_ms: now_ms,
        });
    }
}

/// Terminals and session ids respawned by a migration within
/// [`HANDOFF_WINDOW_MS`] of `now_ms`.
pub fn recent_migration_handoffs(now_ms: i64) -> (HashSet<String>, HashSet<String>) {
    let mut terminals = HashSet::new();
    let mut sessions = HashSet::new();
    if let Ok(l) = LEDGER.lock() {
        for e in l.iter().filter(|e| now_ms - e.at_ms <= HANDOFF_WINDOW_MS) {
            terminals.insert(e.terminal_id.clone());
            sessions.insert(e.session_id.clone());
        }
    }
    (terminals, sessions)
}

/// How long a migration-respawned pane's usage-limit matches are ignored when
/// it has produced no new turn.
pub const REPLAY_SUPPRESSION_WINDOW: Duration = Duration::from_secs(10 * 60);

/// One respawned pane: when, whether it has produced a new turn since, and
/// whether its suppression has been lifted.
struct ReplayGuard {
    respawned_at: Instant,
    turned: bool,
    lifted: bool,
}

static REPLAY: Mutex<Option<HashMap<String, ReplayGuard>>> = Mutex::new(None);

/// Record that `migrate_session` respawned the session into `terminal_id`.
pub fn note_respawned_pane(terminal_id: &str, now: Instant) {
    let mut guard = REPLAY.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(HashMap::new).insert(
        terminal_id.to_string(),
        ReplayGuard {
            respawned_at: now,
            turned: false,
            lifted: false,
        },
    );
}

/// Pure: a usage-limit match on a migration-respawned pane is a replay until
/// a scan AFTER the one that first saw a new turn (`turned` is the state
/// BEFORE the current scan), or `window` has passed since the respawn.
pub fn replay_suppressed(
    respawned_at: Instant,
    turned: bool,
    now: Instant,
    window: Duration,
) -> bool {
    !turned && now.saturating_duration_since(respawned_at) < window
}

/// How the usage-limit watcher must treat one scan of a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayVerdict {
    /// Not a pane a migration respawned: every match counts.
    NotRespawned,
    /// A respawned pane still replaying: no match counts.
    Suppressed,
    /// A respawned pane whose suppression has lifted: only a match in the
    /// bottom rows counts (the replayed banner may still sit higher up).
    BottomRowsOnly,
}

/// The usage-limit watcher's per-scan question for `terminal_id`.
/// `screen_shows_turn` is whether the screen being scanned shows a turn in
/// progress. Seeing one does NOT lift the suppression for THIS scan — the
/// banner on that very screen may still be the replay — but from the next
/// scan on (2026-10-23 review fix 3). Once lifted, the pane stays
/// [`ReplayVerdict::BottomRowsOnly`] until it closes ([`retain_live_panes`]).
pub fn usage_limit_replay_verdict(
    terminal_id: &str,
    screen_shows_turn: bool,
    now: Instant,
) -> ReplayVerdict {
    let mut guard = REPLAY.lock().unwrap_or_else(|e| e.into_inner());
    let Some(entry) = guard.as_mut().and_then(|m| m.get_mut(terminal_id)) else {
        return ReplayVerdict::NotRespawned;
    };
    if entry.lifted {
        return ReplayVerdict::BottomRowsOnly;
    }
    let suppressed = replay_suppressed(
        entry.respawned_at,
        entry.turned,
        now,
        REPLAY_SUPPRESSION_WINDOW,
    );
    entry.turned |= screen_shows_turn;
    if suppressed {
        ReplayVerdict::Suppressed
    } else {
        entry.lifted = true;
        ReplayVerdict::BottomRowsOnly
    }
}

/// Whether `terminal_id`'s suppression is due to lift at `now` (a turn was
/// seen, or the window has run out) but has not been lifted by a scan yet.
/// The watcher scans such a pane even when its grid has not changed, so a
/// banner frozen on a static screen is not skipped forever by the grid gate
/// (2026-10-23 review fix 4).
pub fn replay_release_due(terminal_id: &str, now: Instant) -> bool {
    let guard = REPLAY.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()
        .and_then(|m| m.get(terminal_id))
        .is_some_and(|e| {
            !e.lifted
                && !replay_suppressed(e.respawned_at, e.turned, now, REPLAY_SUPPRESSION_WINDOW)
        })
}

/// Forget respawned panes that are no longer live. `live` is the terminal
/// list the scan took at `scan_started`; a pane respawned AFTER that instant
/// is not in it yet, and is kept — dropping it would let the very next scan
/// read its replayed banner as news (the 2026-10-22 churn this guard exists
/// to stop; round-11 review W2).
pub fn retain_live_panes(live: &HashSet<&String>, scan_started: Instant) {
    let mut guard = REPLAY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(map) = guard.as_mut() {
        retain_guards(map, live, scan_started);
    }
}

/// Pure: [`retain_live_panes`] over one map — the unit-test seam (the global
/// map is shared by every test in the process).
fn retain_guards(
    map: &mut HashMap<String, ReplayGuard>,
    live: &HashSet<&String>,
    scan_started: Instant,
) {
    map.retain(|tid, g| live.contains(tid) || g.respawned_at >= scan_started);
}

/// Record that the RUNNER submitted a prompt into `terminal_id` (the
/// migration's continue nudge). Whatever the pane renders after that is the
/// session working, not the transcript replaying, so it counts as a turn:
/// the next scan lifts the suppression exactly as a seen spinner would. A
/// pane with no guard is left alone.
pub fn note_runner_submission(terminal_id: &str) {
    let mut guard = REPLAY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = guard.as_mut().and_then(|m| m.get_mut(terminal_id)) {
        entry.turned = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mutation check: changing the window comparison to `< 0` fails the
    /// in-window assertion.
    #[test]
    fn a_respawn_is_excluded_for_the_window_only() {
        let t0 = 9_000_000_000_i64;
        note_migration_respawn("handoff-test-term", "handoff-test-sid", t0);
        let (terms, sids) = recent_migration_handoffs(t0 + 30_000);
        assert!(terms.contains("handoff-test-term"));
        assert!(sids.contains("handoff-test-sid"));
        let (terms, _) = recent_migration_handoffs(t0 + HANDOFF_WINDOW_MS + 1);
        assert!(!terms.contains("handoff-test-term"));
    }

    // ---- Phase 2b: replay suppression ------------------------------------

    /// Mutation check: dropping `!turned` fails the "after a turn" arm;
    /// flipping `<` to `>=` fails both in-window arms.
    #[test]
    fn replay_is_suppressed_until_a_turn_or_the_window() {
        let t0 = Instant::now();
        let w = REPLAY_SUPPRESSION_WINDOW;
        assert!(replay_suppressed(t0, false, t0, w), "right after respawn");
        assert!(
            replay_suppressed(t0, false, t0 + Duration::from_secs(9 * 60), w),
            "inside the window, no turn yet"
        );
        assert!(
            !replay_suppressed(t0, true, t0 + Duration::from_secs(1), w),
            "a new turn ends it"
        );
        assert!(
            !replay_suppressed(t0, false, t0 + w, w),
            "the window ends it"
        );
    }

    /// Fix 3: the scan that first SEES the turn is still suppressed; the
    /// lift comes on the next scan, and after it the pane is bottom-rows-only
    /// for good.
    ///
    /// Mutation check: updating `turned` before computing `suppressed` lets
    /// the turn's own scan through (the second assertion fails).
    #[test]
    fn only_a_respawned_pane_is_suppressed_and_the_lift_follows_the_turn() {
        let t0 = Instant::now();
        let tid = "replay-test-respawned";
        assert_eq!(
            usage_limit_replay_verdict("replay-test-never-respawned", false, t0),
            ReplayVerdict::NotRespawned,
            "an ordinary pane's match is news"
        );
        note_respawned_pane(tid, t0);
        assert_eq!(
            usage_limit_replay_verdict(tid, false, t0 + Duration::from_secs(5)),
            ReplayVerdict::Suppressed
        );
        assert!(!replay_release_due(tid, t0 + Duration::from_secs(5)));
        assert_eq!(
            usage_limit_replay_verdict(tid, true, t0 + Duration::from_secs(6)),
            ReplayVerdict::Suppressed,
            "the scan that first sees the turn does not lift the suppression"
        );
        assert!(
            replay_release_due(tid, t0 + Duration::from_secs(7)),
            "a seen turn makes the release due"
        );
        assert_eq!(
            usage_limit_replay_verdict(tid, false, t0 + Duration::from_secs(7)),
            ReplayVerdict::BottomRowsOnly
        );
        assert_eq!(
            usage_limit_replay_verdict(tid, false, t0 + Duration::from_secs(8)),
            ReplayVerdict::BottomRowsOnly,
            "and stays bottom-rows-only"
        );
        assert!(!replay_release_due(tid, t0 + Duration::from_secs(8)));
    }

    /// Fix 4: with no turn, the release is due once the window has run out —
    /// the watcher's cue to scan a static screen.
    ///
    /// Mutation check: making `replay_release_due` require a seen turn fails
    /// the past-window assertion.
    #[test]
    fn a_respawned_pane_with_no_turn_is_released_after_the_window() {
        let t0 = Instant::now();
        let tid = "replay-test-window";
        note_respawned_pane(tid, t0);
        assert_eq!(
            usage_limit_replay_verdict(tid, false, t0 + Duration::from_secs(60)),
            ReplayVerdict::Suppressed
        );
        let past = t0 + REPLAY_SUPPRESSION_WINDOW + Duration::from_secs(1);
        assert!(replay_release_due(tid, past));
        assert_eq!(
            usage_limit_replay_verdict(tid, false, past),
            ReplayVerdict::BottomRowsOnly
        );
    }

    /// Round-11 W2: a scan takes its terminal list, then retains against it.
    /// A pane respawned in between is not in that list but must survive the
    /// retain; a pane respawned BEFORE the scan and absent from it is gone.
    ///
    /// Mutation check: retaining on `live.contains(tid)` alone drops the
    /// mid-scan respawn and fails the first assertion.
    #[test]
    fn a_pane_respawned_during_a_scan_survives_that_scans_retain() {
        let t0 = Instant::now();
        let guard_at = |at: Instant| ReplayGuard {
            respawned_at: at,
            turned: false,
            lifted: false,
        };
        let mut map = HashMap::new();
        map.insert("closed-before".to_string(), guard_at(t0));
        map.insert(
            "mid-scan".to_string(),
            guard_at(t0 + Duration::from_millis(5)),
        );
        let live_tid = "live".to_string();
        map.insert(live_tid.clone(), guard_at(t0));
        let live: HashSet<&String> = [&live_tid].into_iter().collect();
        retain_guards(&mut map, &live, t0 + Duration::from_millis(1));
        assert!(
            map.contains_key("mid-scan"),
            "respawned after the scan began"
        );
        assert!(map.contains_key("live"));
        assert!(!map.contains_key("closed-before"), "gone before the scan");
        retain_guards(&mut map, &live, t0 + Duration::from_millis(10));
        assert!(
            !map.contains_key("mid-scan"),
            "the next scan, which did see the list after it, drops it"
        );
    }

    /// Round-11 S5: a runner submission counts as a turn — the next scan
    /// lifts the suppression — and touches no pane without a guard.
    ///
    /// Mutation check: making `note_runner_submission` a no-op fails the
    /// second verdict.
    #[test]
    fn a_runner_submission_lifts_the_suppression_from_the_next_scan() {
        let t0 = Instant::now();
        let tid = "replay-test-submitted";
        note_respawned_pane(tid, t0);
        note_runner_submission(tid);
        note_runner_submission("replay-test-never-noted");
        assert_eq!(
            usage_limit_replay_verdict(tid, false, t0 + Duration::from_secs(1)),
            ReplayVerdict::BottomRowsOnly
        );
        assert_eq!(
            usage_limit_replay_verdict("replay-test-never-noted", false, t0),
            ReplayVerdict::NotRespawned
        );
    }
}
