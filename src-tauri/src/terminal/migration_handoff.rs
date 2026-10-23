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
//! suppression (Phase 2b, [`usage_limit_match_is_replay`]): a pane a migration
//! respawned re-renders the old transcript, so its first usage-limit match is
//! the replayed banner, not news. On 2026-10-22 the watcher re-matched it on
//! the new pane three times in 30 minutes and each probe correctly found the
//! new account fine — churn, and a stale banner that could have spent the
//! migration cap.

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

/// One respawned pane: when, and whether it has produced a new turn since.
struct ReplayGuard {
    respawned_at: Instant,
    turned: bool,
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
        },
    );
}

/// Pure: a usage-limit match on a migration-respawned pane is a replay until
/// the pane has produced one new turn, or `window` has passed since the
/// respawn.
pub fn replay_suppressed(
    respawned_at: Instant,
    turned: bool,
    now: Instant,
    window: Duration,
) -> bool {
    !turned && now.saturating_duration_since(respawned_at) < window
}

/// The usage-limit watcher's per-scan question for `terminal_id`: must a match
/// on this screen be ignored as a replay? `screen_shows_turn` is whether the
/// screen being scanned shows a turn in progress; seeing one ends the
/// suppression for good. A pane that was not respawned by a migration, or
/// whose suppression has ended, answers `false` (and is forgotten).
pub fn usage_limit_match_is_replay(
    terminal_id: &str,
    screen_shows_turn: bool,
    now: Instant,
) -> bool {
    let mut guard = REPLAY.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = guard.as_mut() else {
        return false;
    };
    let Some(entry) = map.get_mut(terminal_id) else {
        return false;
    };
    entry.turned |= screen_shows_turn;
    let suppressed = replay_suppressed(
        entry.respawned_at,
        entry.turned,
        now,
        REPLAY_SUPPRESSION_WINDOW,
    );
    if !suppressed {
        map.remove(terminal_id);
    }
    suppressed
}

/// Forget respawned panes that are no longer live.
pub fn retain_live_panes(live: &HashSet<&String>) {
    let mut guard = REPLAY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(map) = guard.as_mut() {
        map.retain(|tid, _| live.contains(tid));
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

    #[test]
    fn only_a_respawned_pane_is_suppressed_and_a_turn_ends_it_for_good() {
        let t0 = Instant::now();
        let tid = "replay-test-respawned";
        assert!(
            !usage_limit_match_is_replay("replay-test-never-respawned", false, t0),
            "an ordinary pane's match is news"
        );
        note_respawned_pane(tid, t0);
        assert!(usage_limit_match_is_replay(
            tid,
            false,
            t0 + Duration::from_secs(5)
        ));
        assert!(
            !usage_limit_match_is_replay(tid, true, t0 + Duration::from_secs(6)),
            "the scan that sees the turn already lets the match through"
        );
        assert!(
            !usage_limit_match_is_replay(tid, false, t0 + Duration::from_secs(7)),
            "and the pane stays un-suppressed after the turn"
        );
    }

    #[test]
    fn a_respawned_pane_with_no_turn_is_released_after_the_window() {
        let t0 = Instant::now();
        let tid = "replay-test-window";
        note_respawned_pane(tid, t0);
        assert!(usage_limit_match_is_replay(
            tid,
            false,
            t0 + Duration::from_secs(60)
        ));
        assert!(!usage_limit_match_is_replay(
            tid,
            false,
            t0 + REPLAY_SUPPRESSION_WINDOW + Duration::from_secs(1)
        ));
    }
}
