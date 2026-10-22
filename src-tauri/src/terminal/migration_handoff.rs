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

use std::collections::HashSet;
use std::sync::Mutex;

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
}
