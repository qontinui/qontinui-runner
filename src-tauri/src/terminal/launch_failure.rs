//! A Claude CLI that never launched (plan
//! `2026-11-10-migration-holds-for-missing-claude-cli`, Phase 2, decision D2).
//!
//! When the identity shim gives up on the real `claude` — nothing launchable
//! resolved within its bounded wait (`resolve-failed`), or the resolved binary
//! would not exec (`exec-failed`) — it POSTs that to `/control/shim-beacon`
//! just BEFORE its fail-open fall-through, and waits for the reply, so the
//! runner holds the record before the exec that exits 127.
//! [`LaunchFailureRegistry`] keeps the latest such report per terminal: in
//! memory, at most [`MAX_ENTRIES`] terminals, each for [`ENTRY_TTL_MS`].
//!
//! Absence is never evidence. No beacon — no curl on the box, no runner, the
//! Windows `.cmd` shim, a nested `claude` (which never posts, so it never marks
//! its parent's pane) — means no entry, and whatever reads this registry must
//! treat that as UNKNOWN, never as "it launched".

use std::num::NonZeroUsize;
use std::sync::{LazyLock, Mutex, MutexGuard};

use lru::LruCache;

/// The shim gave up waiting for a launchable `claude` and is falling through
/// to dispatch the bare name.
pub const EVENT_RESOLVE_FAILED: &str = "resolve-failed";

/// The `claude` the shim resolved would not exec within its retry bound.
pub const EVENT_EXEC_FAILED: &str = "exec-failed";

/// How long a report stays evidence: one hour.
const ENTRY_TTL_MS: i64 = 3_600_000;

/// At most this many terminals are remembered; the least recently used is
/// evicted first.
const MAX_ENTRIES: NonZeroUsize = match NonZeroUsize::new(512) {
    Some(n) => n,
    None => panic!("MAX_ENTRIES must be non-zero"),
};

/// Is `event` one of the two launch-failure beacon events? Every other event
/// (`invoked`, anything a future shim sends) stays log-only.
pub fn is_launch_failure_event(event: &str) -> bool {
    event == EVENT_RESOLVE_FAILED || event == EVENT_EXEC_FAILED
}

/// One launch failure a shim reported for a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchFailure {
    /// [`EVENT_RESOLVE_FAILED`] or [`EVENT_EXEC_FAILED`].
    pub event: String,
    /// The shim's own detail line, kept verbatim for the logs.
    pub detail: String,
    /// The shim was delivering the SessionStart hook (`settings=true`), so
    /// "no CLI ever started here" is readable from the row's hook stamp.
    pub settings: bool,
    /// The argv chose its own session — `--resume` / `--session-id`
    /// (`user_session_id=true`).
    pub user_chose_session: bool,
    /// When the runner received the report (runner clock, unix millis).
    pub at_ms: i64,
}

impl LaunchFailure {
    /// The failure a beacon reports, or `None` for an event that is not a
    /// launch failure. A flag the detail does not carry reads `false`: absent
    /// evidence is never positive evidence.
    pub fn from_beacon(event: &str, detail: &str, at_ms: i64) -> Option<Self> {
        if !is_launch_failure_event(event) {
            return None;
        }
        Some(Self {
            event: event.to_string(),
            detail: detail.to_string(),
            settings: detail_flag(detail, "settings"),
            user_chose_session: detail_flag(detail, "user_session_id"),
            at_ms,
        })
    }
}

/// `key=true` among the detail's whitespace-separated `key=value` tokens.
fn detail_flag(detail: &str, key: &str) -> bool {
    detail.split_whitespace().any(|token| {
        token
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
            == Some("true")
    })
}

/// The latest launch failure each terminal's shim reported. Bounded and aged:
/// see the module docs.
#[derive(Debug)]
pub struct LaunchFailureRegistry {
    ttl_ms: i64,
    by_terminal: Mutex<LruCache<String, LaunchFailure>>,
}

impl Default for LaunchFailureRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl LaunchFailureRegistry {
    pub fn new() -> Self {
        Self {
            ttl_ms: ENTRY_TTL_MS,
            by_terminal: Mutex::new(LruCache::new(MAX_ENTRIES)),
        }
    }

    /// Remember the failure a beacon reports for `terminal_id`, replacing any
    /// earlier one for that terminal. Returns what was recorded — `None` for an
    /// event that is not a launch failure, or for a blank terminal id (a shim
    /// run outside a runner terminal has no pane to mark).
    pub fn record_beacon(
        &self,
        terminal_id: Option<&str>,
        event: &str,
        detail: &str,
        now_ms: i64,
    ) -> Option<LaunchFailure> {
        let terminal_id = terminal_id.map(str::trim).filter(|t| !t.is_empty())?;
        let failure = LaunchFailure::from_beacon(event, detail, now_ms)?;
        lock(&self.by_terminal).put(terminal_id.to_string(), failure.clone());
        Some(failure)
    }

    /// The latest failure recorded for `terminal_id`, while it is no older than
    /// the TTL. An expired entry is dropped on read.
    pub fn get(&self, terminal_id: &str, now_ms: i64) -> Option<LaunchFailure> {
        fresh(
            &mut lock(&self.by_terminal),
            terminal_id,
            now_ms,
            self.ttl_ms,
            |f| f.at_ms,
        )
    }
}

/// `key`'s value while `now_ms - at(value) <= ttl_ms`; an older one is popped.
fn fresh<V: Clone>(
    cache: &mut LruCache<String, V>,
    key: &str,
    now_ms: i64,
    ttl_ms: i64,
    at: impl Fn(&V) -> i64,
) -> Option<V> {
    let age = now_ms.saturating_sub(at(cache.peek(key)?));
    if age > ttl_ms {
        cache.pop(key);
        return None;
    }
    cache.get(key).cloned()
}

static REGISTRY: LazyLock<LaunchFailureRegistry> = LazyLock::new(LaunchFailureRegistry::new);

/// This process's registry: `/control/shim-beacon` writes it.
pub fn registry() -> &'static LaunchFailureRegistry {
    &REGISTRY
}

/// A poisoned lock is recovered rather than refused: the cache holds no
/// invariant a panic can break, and refusing would silently drop evidence.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_794_283_445_170; // 2026-11-10T04:04:05.170Z

    #[test]
    fn a_failure_beacon_is_recorded_and_read_back_per_terminal() {
        let reg = LaunchFailureRegistry::new();
        let recorded = reg.record_beacon(
            Some("term-a"),
            "resolve-failed",
            "waited=10 stub=false settings=true user_session_id=true",
            T0,
        );
        let expected = LaunchFailure {
            event: "resolve-failed".to_string(),
            detail: "waited=10 stub=false settings=true user_session_id=true".to_string(),
            settings: true,
            user_chose_session: true,
            at_ms: T0,
        };
        assert_eq!(recorded.as_ref(), Some(&expected));
        assert_eq!(reg.get("term-a", T0 + 1), Some(expected));
        assert_eq!(reg.get("term-b", T0 + 1), None, "keyed by terminal");

        // A later report for the same terminal replaces the earlier one.
        reg.record_beacon(
            Some("term-a"),
            "exec-failed",
            "status=126 waited=10 settings=false user_session_id=true",
            T0 + 5,
        );
        let latest = reg.get("term-a", T0 + 6).unwrap();
        assert_eq!(latest.event, "exec-failed");
        assert!(!latest.settings);
        assert!(latest.user_chose_session);
        assert_eq!(latest.at_ms, T0 + 5);
    }

    /// Only the two failure events are evidence, and only for a named
    /// terminal; everything else stays log-only.
    #[test]
    fn only_a_launch_failure_for_a_named_terminal_is_recorded() {
        let reg = LaunchFailureRegistry::new();
        let invoked = "user_session_id=true settings=true mcp_config=false pinned=true";
        assert_eq!(reg.record_beacon(Some("t"), "invoked", invoked, T0), None);
        assert_eq!(reg.record_beacon(Some("t"), "", "settings=true", T0), None);
        assert_eq!(reg.get("t", T0), None);
        for blank in [None, Some(""), Some("   ")] {
            assert_eq!(
                reg.record_beacon(blank, "resolve-failed", "settings=true", T0),
                None,
                "{blank:?}"
            );
        }
        assert!(is_launch_failure_event("resolve-failed"));
        assert!(is_launch_failure_event("exec-failed"));
        assert!(!is_launch_failure_event("invoked"));
        assert!(!is_launch_failure_event("Resolve-Failed"));
    }

    /// The two flags are read as exact `key=true` tokens. Anything else — a
    /// missing token, another value, a key that merely shares a prefix — is
    /// `false`, including the native shim's extra `error=` token.
    #[test]
    fn detail_flags_are_exact_true_tokens() {
        let f = |detail: &str| LaunchFailure::from_beacon("resolve-failed", detail, T0).unwrap();
        let both = f("waited=10 stub=false settings=true user_session_id=true");
        assert!(both.settings && both.user_chose_session);
        let native =
            f("status=127 waited=0 error=PermissionDenied settings=true user_session_id=false");
        assert!(native.settings && !native.user_chose_session);
        let none = f("waited=10 stub=true");
        assert!(!none.settings && !none.user_chose_session);
        let near_miss = f("settingsx=true settings=TRUE user_session_id=1 user_session_id_=true");
        assert!(!near_miss.settings && !near_miss.user_chose_session);
    }

    #[test]
    fn an_entry_is_evidence_for_one_hour_and_then_gone() {
        let reg = LaunchFailureRegistry::new();
        reg.record_beacon(Some("t"), "resolve-failed", "settings=true", T0);
        assert!(
            reg.get("t", T0 + 3_600_000).is_some(),
            "exactly one hour old"
        );
        assert_eq!(reg.get("t", T0 + 3_600_001), None, "one hour and 1 ms old");
        // Dropped on read, not merely hidden.
        assert_eq!(reg.get("t", T0), None);
    }

    #[test]
    fn at_most_512_terminals_are_kept_and_the_oldest_goes_first() {
        let reg = LaunchFailureRegistry::new();
        for i in 0..512 {
            reg.record_beacon(Some(&format!("t{i}")), "exec-failed", "settings=true", T0);
        }
        assert!(reg.get("t0", T0).is_some(), "512 entries all fit");
        // `t0` was just read, so `t1` is now the least recently used.
        reg.record_beacon(Some("t512"), "exec-failed", "settings=true", T0);
        assert_eq!(reg.get("t1", T0), None, "the 513th evicts the LRU entry");
        assert!(reg.get("t0", T0).is_some());
        assert!(reg.get("t512", T0).is_some());
        assert!(reg.get("t511", T0).is_some());
    }
}
