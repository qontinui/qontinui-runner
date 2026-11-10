//! A Claude CLI that never launched, and the `launch-failed` close reason it
//! earns (plan `2026-11-10-migration-holds-for-missing-claude-cli`, Phases 2-3,
//! decisions D2 and D3).
//!
//! ## The record (Phase 2, D2)
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
//! its parent's pane) — means no entry, and the row keeps its old close reason:
//! UNKNOWN, never "it launched" and never "it did not".
//!
//! ## The verdict (Phase 3, D3)
//!
//! [`close_reason_for_launch`] decides what a DEATH close records. It turns a
//! restorable `poll-dead` / `pty-exit` into the equally restorable
//! [`LAUNCH_FAILED`] only on positive evidence that no CLI ever ran in the pane:
//!
//! 1. the terminal holds a launch-failure entry saying the shim was delivering
//!    the SessionStart hook (`settings=true`) for a resume-shaped launch
//!    (`user_session_id=true`). A bare pinned `claude` stamps its own row
//!    through `/control/session-open` BEFORE it execs, so condition 2 cannot
//!    be read for it, and its failure keeps the old reason by design;
//! 2. no SessionStart hook stamped the row at or after the terminal's
//!    `created_at`, which is read from an EXACT terminal-id match only;
//! 3. the liveness poll saw no live Claude in the pane at or after the failure
//!    (`last_alive_at`, written only by the poll's KeepAlive — a `record_open`
//!    re-open is bookkeeping, not liveness).
//!
//! The re-close rule: `record_open` clears a row's close reason when it
//! re-opens it, so this registry also remembers, per session, when this
//! process closed its row `launch-failed`. Closed again with no hook stamp and
//! no liveness since, the row keeps `launch-failed`.
//!
//! Every other proposed reason passes through untouched, so a non-restorable
//! close (`terminal-only-idle`, `never-started`, `no-terminal`, an explicit
//! close) is never rewritten. Two writers apply the verdict: the lifecycle
//! poll's `Close` arm (`main.rs`) and the frontend's `pty-exit` close, inside
//! `SessionLifecycleStore::record_close_gated` AFTER its shutdown refusal.

use std::num::NonZeroUsize;
use std::sync::{LazyLock, Mutex, MutexGuard};

use lru::LruCache;

use crate::session::session_lifecycle_store::{CloseOutcome, TerminalSessionRecord};

/// The shim gave up waiting for a launchable `claude` and is falling through
/// to dispatch the bare name.
pub const EVENT_RESOLVE_FAILED: &str = "resolve-failed";

/// The `claude` the shim resolved would not exec within its retry bound.
pub const EVENT_EXEC_FAILED: &str = "exec-failed";

/// The close reason of a pane whose Claude CLI never launched. Restorable
/// exactly like `poll-dead` (`RESTORABLE_LAUNCH_FAILED_MS` in the lifecycle
/// store) and deliberately absent from `NON_RESTORABLE_CLOSE_REASONS`: nothing
/// ran, so the conversation is exactly as it was.
pub const LAUNCH_FAILED: &str = "launch-failed";

/// The only close reasons the verdict may rewrite: the two restorable "the
/// session died" closes.
const DEATH_CLOSE_REASONS: [&str; 2] = ["poll-dead", "pty-exit"];

/// How long a report stays evidence: one hour.
const ENTRY_TTL_MS: i64 = 3_600_000;

/// At most this many terminals (and, separately, sessions) are remembered;
/// the least recently used is evicted first.
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

/// A close this process already recorded for a session, before something
/// re-opened its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriorClose<'a> {
    pub reason: &'a str,
    pub at_ms: i64,
}

/// The reason a DEATH close of a row records (D3). Returns `proposed` unless
/// it is `poll-dead` / `pty-exit` and either the re-close rule holds or all
/// three conditions do — see the module docs. Every tie keeps `proposed`: a
/// cause is never asserted on weaker evidence than its rule names.
///
/// - `prior_reason`: a close this process recorded for the row before a
///   re-open, if any;
/// - `failure`: the row's terminal's launch-failure entry, if any;
/// - `terminal_created_at_ms`: that terminal's spawn time, from an EXACT
///   terminal-id match only (`None` when no such match was made);
/// - `last_session_start_at` / `last_alive_at`: the row's own stamps.
pub fn close_reason_for_launch<'a>(
    proposed: &'a str,
    prior_reason: Option<PriorClose<'_>>,
    failure: Option<&LaunchFailure>,
    terminal_created_at_ms: Option<i64>,
    last_session_start_at: Option<i64>,
    last_alive_at: Option<i64>,
) -> &'a str {
    if !DEATH_CLOSE_REASONS.contains(&proposed) {
        return proposed;
    }
    // The re-close rule: already closed `launch-failed`, and neither a CLI
    // hook nor the poll has seen the CLI since.
    if let Some(prior) = prior_reason {
        let since_prior = |stamp: Option<i64>| stamp.is_some_and(|at| at >= prior.at_ms);
        if prior.reason == LAUNCH_FAILED
            && !since_prior(last_session_start_at)
            && !since_prior(last_alive_at)
        {
            return LAUNCH_FAILED;
        }
    }
    // 1. A resume-shaped launch failure, reported while the shim delivered
    //    the SessionStart hook.
    let Some(failure) = failure else {
        return proposed;
    };
    if !failure.settings || !failure.user_chose_session {
        return proposed;
    }
    // 2. No CLI started in this pane.
    let Some(created_at) = terminal_created_at_ms else {
        return proposed;
    };
    if last_session_start_at.is_some_and(|at| at >= created_at) {
        return proposed;
    }
    // 3. Nothing alive in it after the failure.
    if last_alive_at.is_some_and(|at| at >= failure.at_ms) {
        return proposed;
    }
    LAUNCH_FAILED
}

/// The reason one death close records, and what rewrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCloseVerdict<'a> {
    /// The reason to record.
    pub reason: &'a str,
    /// Set only when the verdict rewrote the proposed reason: the evidence
    /// behind it, for the close's log line.
    pub evidence: Option<String>,
}

/// The latest launch failure each terminal's shim reported, and the sessions
/// this process closed `launch-failed`. Bounded and aged: see the module docs.
#[derive(Debug)]
pub struct LaunchFailureRegistry {
    ttl_ms: i64,
    by_terminal: Mutex<LruCache<String, LaunchFailure>>,
    /// `claude_session_id` → when this process closed that row
    /// `launch-failed`: the re-close rule's memory across a re-open.
    launch_failed_closes: Mutex<LruCache<String, i64>>,
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
            launch_failed_closes: Mutex::new(LruCache::new(MAX_ENTRIES)),
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

    /// The verdict for one death close of `rec`: [`close_reason_for_launch`]
    /// fed from this registry and the row's own stamps. Pure apart from the
    /// reads; [`Self::settle`] records the outcome once the close has landed.
    pub fn verdict<'a>(
        &self,
        proposed: &'a str,
        rec: &TerminalSessionRecord,
        terminal_created_at_ms: Option<i64>,
        now_ms: i64,
    ) -> LaunchCloseVerdict<'a> {
        let failure = self.get(&rec.terminal_id, now_ms);
        let prior_at = fresh(
            &mut lock(&self.launch_failed_closes),
            &rec.claude_session_id,
            now_ms,
            self.ttl_ms,
            |at| *at,
        );
        let reason = close_reason_for_launch(
            proposed,
            prior_at.map(|at_ms| PriorClose {
                reason: LAUNCH_FAILED,
                at_ms,
            }),
            failure.as_ref(),
            terminal_created_at_ms,
            rec.last_session_start_at,
            rec.last_alive_at,
        );
        let evidence = (reason != proposed).then(|| {
            let mut parts = Vec::with_capacity(2);
            if let Some(f) = &failure {
                parts.push(format!("{} {}", f.event, f.detail));
            }
            if let Some(at) = prior_at {
                parts.push(format!("closed {LAUNCH_FAILED} before at_ms={at}"));
            }
            parts.join("; ")
        });
        LaunchCloseVerdict { reason, evidence }
    }

    /// After the close: remember a row the verdict closed `launch-failed`, for
    /// the re-close rule. An outcome that closed nothing records nothing.
    /// Returns whether a row was closed `launch-failed` — what a caller's log
    /// line may claim.
    pub fn settle(
        &self,
        verdict: &LaunchCloseVerdict<'_>,
        outcome: &CloseOutcome,
        now_ms: i64,
    ) -> bool {
        if verdict.reason != LAUNCH_FAILED {
            return false;
        }
        let Some(closed) = closed_session(outcome) else {
            return false;
        };
        lock(&self.launch_failed_closes).put(closed.to_string(), now_ms);
        true
    }
}

/// The session a close actually closed, or `None` for an outcome that mutated
/// nothing. No wildcard arm: a new outcome must decide which it is.
fn closed_session(outcome: &CloseOutcome) -> Option<&str> {
    match outcome {
        CloseOutcome::Closed { claude_session_id } => Some(claude_session_id),
        CloseOutcome::Redirected { closed, .. }
        | CloseOutcome::RedirectedAmbiguous { closed, .. } => Some(closed),
        CloseOutcome::AlreadyClosed { .. }
        | CloseOutcome::TerminalSuperseded { .. }
        | CloseOutcome::IgnoredDuringShutdown { .. }
        | CloseOutcome::NotFound { .. } => None,
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

/// This process's registry: `/control/shim-beacon` writes it, and the two
/// death-close writers read it.
pub fn registry() -> &'static LaunchFailureRegistry {
    &REGISTRY
}

/// A poisoned lock is recovered rather than refused: the caches hold no
/// invariant a panic can break, and refusing would silently drop evidence.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::session_lifecycle_store::{test_open_record, SessionLifecycleStore};

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
    /// `false`; the native shim's extra `error=` token is simply skipped.
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

    // ---- Phase 3: the `launch-failed` verdict (D3) ------------------------

    /// The incident's pane (nilan, terminal a3790d23, 2026-11-10): spawned
    /// 04:03:55.546Z, its shim gave up 04:04:05.170Z, and its row's last
    /// SessionStart stamp was 2026-11-09T22:30:00Z.
    const CREATED: i64 = 1_794_283_435_546;
    const FAILED: i64 = T0;
    const HOOK: i64 = 1_794_263_400_000;

    fn resume_failure() -> LaunchFailure {
        LaunchFailure::from_beacon(
            "resolve-failed",
            "waited=10 stub=false settings=true user_session_id=true",
            FAILED,
        )
        .unwrap()
    }

    /// Every proposed reason × every evidence arm. Only a restorable DEATH
    /// close with all three conditions met becomes `launch-failed`; every
    /// other cell keeps the reason it was proposed with.
    #[test]
    fn the_verdict_over_every_proposed_reason_and_evidence_arm() {
        let met = resume_failure();
        let no_hook = LaunchFailure {
            settings: false,
            ..resume_failure()
        };
        let bare = LaunchFailure {
            user_chose_session: false,
            ..resume_failure()
        };
        // (arm, failure, created_at, last_session_start_at, last_alive_at)
        let arms = [
            ("no failure", None, Some(CREATED), Some(HOOK), None),
            (
                "failure with settings=false",
                Some(&no_hook),
                Some(CREATED),
                Some(HOOK),
                None,
            ),
            (
                "bare pinned launch",
                Some(&bare),
                Some(CREATED),
                Some(HOOK),
                None,
            ),
            (
                "SessionStart stamp after created_at",
                Some(&met),
                Some(CREATED),
                Some(CREATED + 1),
                None,
            ),
            (
                "last_alive_at after the failure",
                Some(&met),
                Some(CREATED),
                Some(HOOK),
                Some(FAILED + 1),
            ),
            (
                "no exact terminal match",
                Some(&met),
                None,
                Some(HOOK),
                None,
            ),
            (
                "all three conditions met",
                Some(&met),
                Some(CREATED),
                Some(HOOK),
                None,
            ),
        ];
        // (proposed, what it records when all three conditions are met)
        let proposed_reasons = [
            ("poll-dead", "launch-failed"),
            ("pty-exit", "launch-failed"),
            ("terminal-only-idle", "terminal-only-idle"),
            ("never-started", "never-started"),
            ("no-terminal", "no-terminal"),
            ("explicit", "explicit"),
            ("superseded-terminal-reuse", "superseded-terminal-reuse"),
        ];
        for (proposed, when_met) in proposed_reasons {
            for (arm, failure, created, start, alive) in arms {
                let expected = if arm == "all three conditions met" {
                    when_met
                } else {
                    proposed
                };
                assert_eq!(
                    close_reason_for_launch(proposed, None, failure, created, start, alive),
                    expected,
                    "{proposed} × {arm}"
                );
            }
        }
    }

    /// Both sides of each boundary. A stamp AT the pane's spawn is a CLI that
    /// started there, and liveness AT the failure is liveness after it: every
    /// tie keeps the old reason.
    #[test]
    fn every_tie_keeps_the_proposed_reason() {
        let f = resume_failure();
        let decide = |start, alive| {
            close_reason_for_launch("poll-dead", None, Some(&f), Some(CREATED), start, alive)
        };
        assert_eq!(decide(Some(CREATED - 1), None), "launch-failed");
        assert_eq!(decide(Some(CREATED), None), "poll-dead");
        assert_eq!(
            decide(None, Some(FAILED - 1)),
            "launch-failed",
            "seen alive during the shim's own wait is not alive after the failure"
        );
        assert_eq!(decide(None, Some(FAILED)), "poll-dead");
        assert_eq!(
            decide(None, None),
            "launch-failed",
            "a row never stamped at all"
        );
    }

    /// The incident's literal stamps: the first close, then the 04:27:17.305Z
    /// re-close after a `record_open` re-open, with the 04:04:41.928Z close as
    /// the prior and nothing else readable any more.
    #[test]
    fn the_incidents_literal_stamps_close_launch_failed() {
        let failure = LaunchFailure::from_beacon(
            "resolve-failed",
            "waited=10 stub=false settings=true user_session_id=true",
            1_794_283_445_170,
        )
        .unwrap();
        assert_eq!(
            close_reason_for_launch(
                "poll-dead",
                None,
                Some(&failure),
                Some(1_794_283_435_546),
                Some(1_794_263_400_000),
                None,
            ),
            "launch-failed"
        );
        let first_close = PriorClose {
            reason: "launch-failed",
            at_ms: 1_794_283_481_928,
        };
        assert_eq!(
            close_reason_for_launch(
                "poll-dead",
                Some(first_close),
                None,
                None,
                Some(1_794_263_400_000),
                None,
            ),
            "launch-failed"
        );
    }

    /// The re-close rule holds only for a prior `launch-failed` close with
    /// neither a CLI-hook stamp nor liveness since, and never reaches a
    /// non-restorable reason.
    #[test]
    fn a_reclose_keeps_launch_failed_only_while_nothing_ran_since() {
        let prior = PriorClose {
            reason: "launch-failed",
            at_ms: FAILED + 36_758,
        };
        let reclose = |prior, start, alive| {
            close_reason_for_launch("poll-dead", prior, None, None, start, alive)
        };
        assert_eq!(reclose(Some(prior), Some(HOOK), None), "launch-failed");
        assert_eq!(reclose(Some(prior), None, None), "launch-failed");
        assert_eq!(
            reclose(Some(prior), Some(prior.at_ms), None),
            "poll-dead",
            "a SessionStart stamp since the prior close: the CLI ran"
        );
        assert_eq!(
            reclose(Some(prior), None, Some(prior.at_ms + 1)),
            "poll-dead",
            "seen alive since the prior close"
        );
        let prior_death = PriorClose {
            reason: "poll-dead",
            ..prior
        };
        assert_eq!(reclose(Some(prior_death), None, None), "poll-dead");
        assert_eq!(reclose(None, None, None), "poll-dead");
        assert_eq!(
            close_reason_for_launch("no-terminal", Some(prior), None, None, None, None),
            "no-terminal"
        );
    }

    /// No evidence — by either rule — rewrites a non-restorable reason.
    #[test]
    fn a_non_restorable_reason_is_never_rewritten() {
        let f = resume_failure();
        let prior = PriorClose {
            reason: "launch-failed",
            at_ms: FAILED,
        };
        for reason in [
            "no-terminal",
            "never-started",
            "terminal-only-idle",
            "explicit",
        ] {
            assert_eq!(
                close_reason_for_launch(reason, Some(prior), Some(&f), Some(CREATED), None, None),
                reason
            );
        }
    }

    // ---- through the store, as the two writers call it --------------------

    fn open_store() -> (tempfile::TempDir, SessionLifecycleStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        (dir, store)
    }

    /// A confirmed, resumed session's row on `terminal`, last hook-stamped at
    /// `hook_at`.
    fn resumed_row(id: &str, terminal: &str, hook_at: i64) -> TerminalSessionRecord {
        TerminalSessionRecord {
            terminal_id: terminal.to_string(),
            confirmed_at: Some(hook_at),
            last_session_start_at: Some(hook_at),
            ..test_open_record(id)
        }
    }

    const RESUME_FAILED: &str = "waited=10 stub=false settings=true user_session_id=true";

    /// `last_alive_at` is liveness, not bookkeeping: the poll's KeepAlive
    /// `touch` writes it, and `record_open` — a new row or a re-open — never
    /// does.
    #[test]
    fn last_alive_at_is_written_by_touch_and_never_by_record_open() {
        let _amb = crate::test_env::isolated_ambient();
        let (_dir, store) = open_store();
        store.record_open(TerminalSessionRecord {
            last_alive_at: Some(42),
            ..test_open_record("s")
        });
        assert_eq!(store.get("s").unwrap().last_alive_at, None);
        let before = chrono::Utc::now().timestamp_millis();
        store.touch("s");
        let alive = store
            .get("s")
            .unwrap()
            .last_alive_at
            .expect("touch stamps liveness");
        assert!(alive >= before);
        store.record_open(test_open_record("s"));
        assert_eq!(store.get("s").unwrap().last_alive_at, Some(alive));
    }

    /// The incident's sequence through the calls the poll's `Close` arm makes:
    /// the first close records `launch-failed`; a `record_open` re-open (which
    /// stamps `last_seen_at`, not liveness) and a second poll close 23 minutes
    /// later, with no CLI-hook stamp, stay `launch-failed` — whether the pane's
    /// spawn time is still readable or only the prior close remains.
    #[test]
    fn the_incident_reopen_and_reclose_stays_launch_failed() {
        let _amb = crate::test_env::isolated_ambient();
        let (_dir, store) = open_store();
        let registry = LaunchFailureRegistry::new();
        let (id, term) = ("sess-incident", "term-incident");
        let created = chrono::Utc::now().timestamp_millis() - 30 * 60_000;
        let failed = created + 9_624;
        store.record_open(resumed_row(id, term, created - 6 * 3_600_000));
        registry.record_beacon(Some(term), "resolve-failed", RESUME_FAILED, failed);

        let first_at = failed + 36_758;
        let verdict = registry.verdict(
            "poll-dead",
            &store.get(id).unwrap(),
            Some(created),
            first_at,
        );
        assert_eq!(verdict.reason, "launch-failed");
        assert_eq!(
            verdict.evidence.as_deref(),
            Some("resolve-failed waited=10 stub=false settings=true user_session_id=true")
        );
        let outcome = store.record_close_checked(id, None, verdict.reason);
        registry.settle(&verdict, &outcome, first_at);
        assert_eq!(
            store.get(id).unwrap().close_reason.as_deref(),
            Some("launch-failed")
        );

        store.record_open(resumed_row(id, term, created - 6 * 3_600_000));
        let reopened = store.get(id).unwrap();
        assert_eq!(
            (reopened.state.as_str(), reopened.close_reason.as_deref()),
            ("open", None)
        );
        assert_eq!(reopened.last_alive_at, None, "a re-open is not liveness");
        let second_at = first_at + 22 * 60_000 + 35_377;
        for created_at in [Some(created), None] {
            assert_eq!(
                registry
                    .verdict("poll-dead", &reopened, created_at, second_at)
                    .reason,
                "launch-failed",
                "created_at {created_at:?}"
            );
        }
        let verdict = registry.verdict("poll-dead", &reopened, None, second_at);
        let outcome = store.record_close_checked(id, None, verdict.reason);
        registry.settle(&verdict, &outcome, second_at);
        assert_eq!(
            store.get(id).unwrap().close_reason.as_deref(),
            Some("launch-failed")
        );
    }

    /// The other side of the re-close rule: a CLI that DID start after the
    /// re-open (its SessionStart hook stamped the row) dies as a death again.
    #[test]
    fn a_cli_that_started_after_the_reopen_closes_as_a_death() {
        let _amb = crate::test_env::isolated_ambient();
        let (_dir, store) = open_store();
        let registry = LaunchFailureRegistry::new();
        let (id, term) = ("sess-ran-later", "term-ran-later");
        let created = chrono::Utc::now().timestamp_millis() - 30 * 60_000;
        let failed = created + 9_624;
        store.record_open(resumed_row(id, term, created - 6 * 3_600_000));
        registry.record_beacon(Some(term), "resolve-failed", RESUME_FAILED, failed);
        let first_at = failed + 36_758;
        let verdict = registry.verdict(
            "poll-dead",
            &store.get(id).unwrap(),
            Some(created),
            first_at,
        );
        let outcome = store.record_close_checked(id, None, verdict.reason);
        registry.settle(&verdict, &outcome, first_at);

        store.record_open(resumed_row(id, term, created - 6 * 3_600_000));
        store.note_session_start_hook(id);
        let reopened = store.get(id).unwrap();
        let later = chrono::Utc::now().timestamp_millis() + 1_000;
        for created_at in [Some(created), None] {
            assert_eq!(
                registry
                    .verdict("poll-dead", &reopened, created_at, later)
                    .reason,
                "poll-dead",
                "created_at {created_at:?}"
            );
        }
    }

    /// A bare pinned `claude` POSTs `/control/session-open` (whose route stamps
    /// the row's SessionStart) BEFORE it execs, then reports `resolve-failed`
    /// with `user_session_id=false`. Nothing distinguishes that from a CLI that
    /// ran, so the row keeps its old reason — UNKNOWN by design.
    #[test]
    fn a_bare_pinned_launch_failure_keeps_its_old_reason() {
        let _amb = crate::test_env::isolated_ambient();
        let (_dir, store) = open_store();
        let registry = LaunchFailureRegistry::new();
        let (id, term) = ("sess-bare", "term-bare");
        let created = chrono::Utc::now().timestamp_millis() - 15_000;
        store.record_open(TerminalSessionRecord {
            terminal_id: term.to_string(),
            ..test_open_record(id)
        });
        store.note_session_start_hook(id);
        let failed = chrono::Utc::now().timestamp_millis();
        registry.record_beacon(
            Some(term),
            "resolve-failed",
            "waited=10 stub=false settings=true user_session_id=false",
            failed,
        );
        let rec = store.get(id).unwrap();
        assert!(rec.last_session_start_at.is_some_and(|at| at >= created));
        for proposed in ["poll-dead", "pty-exit"] {
            let verdict = registry.verdict(proposed, &rec, Some(created), failed + 40_000);
            assert_eq!((verdict.reason, verdict.evidence), (proposed, None));
        }
    }

    /// Only a close that actually closed a row is remembered for the re-close
    /// rule, and only a `launch-failed` one.
    #[test]
    fn settle_remembers_only_a_row_closed_launch_failed() {
        let registry = LaunchFailureRegistry::new();
        let rec = |id: &str| TerminalSessionRecord {
            terminal_id: format!("term-{id}"),
            ..test_open_record(id)
        };
        let launch_failed = LaunchCloseVerdict {
            reason: "launch-failed",
            evidence: Some("resolve-failed".to_string()),
        };
        let death = LaunchCloseVerdict {
            reason: "poll-dead",
            evidence: None,
        };
        registry.settle(
            &launch_failed,
            &CloseOutcome::AlreadyClosed {
                claude_session_id: "a".to_string(),
            },
            T0,
        );
        registry.settle(
            &death,
            &CloseOutcome::Closed {
                claude_session_id: "b".to_string(),
            },
            T0,
        );
        registry.settle(
            &launch_failed,
            &CloseOutcome::Redirected {
                requested: "x".to_string(),
                closed: "c".to_string(),
                terminal_id: "term-c".to_string(),
            },
            T0,
        );
        let reclose = |id: &str| {
            registry
                .verdict("poll-dead", &rec(id), None, T0 + 60_000)
                .reason
        };
        assert_eq!(reclose("a"), "poll-dead", "an AlreadyClosed closed nothing");
        assert_eq!(
            reclose("b"),
            "poll-dead",
            "a poll-dead close is not remembered"
        );
        assert_eq!(
            reclose("c"),
            "launch-failed",
            "the row a redirect actually closed"
        );
        assert_eq!(
            reclose("x"),
            "poll-dead",
            "not the row the caller merely named"
        );
    }

    /// A launch-failure pane's `pty-exit` that arrives during a runner shutdown
    /// is refused like any other, and its row stays open: the verdict runs only
    /// AFTER the shutdown refusal. Outside a shutdown the same close records
    /// `launch-failed`, so the refusal is not vacuous.
    #[test]
    fn a_launch_failure_panes_pty_exit_during_shutdown_stays_ignored() {
        let _amb = crate::test_env::isolated_ambient();
        let (_dir, store) = open_store();
        let tag = uuid::Uuid::new_v4();
        let now = chrono::Utc::now().timestamp_millis();
        let created = now - 20_000;
        let rows = [
            (
                format!("sess-shutdown-{tag}"),
                format!("term-shutdown-{tag}"),
            ),
            (format!("sess-running-{tag}"), format!("term-running-{tag}")),
        ];
        for (id, term) in &rows {
            store.record_open(resumed_row(id, term, created - 3_600_000));
            registry().record_beacon(
                Some(term.as_str()),
                "exec-failed",
                "status=126 waited=10 settings=true user_session_id=true",
                now - 5_000,
            );
        }
        let [(down_id, down_term), (live_id, live_term)] = &rows;
        assert_eq!(
            store.record_close_gated(
                true,
                down_id,
                Some(down_term.as_str()),
                "pty-exit",
                Some(created)
            ),
            CloseOutcome::IgnoredDuringShutdown {
                claude_session_id: down_id.clone()
            }
        );
        let kept = store.get(down_id).unwrap();
        assert_eq!(
            (kept.state.as_str(), kept.close_reason.as_deref()),
            ("open", None)
        );

        assert_eq!(
            store.record_close_gated(
                false,
                live_id,
                Some(live_term.as_str()),
                "pty-exit",
                Some(created)
            ),
            CloseOutcome::Closed {
                claude_session_id: live_id.clone()
            }
        );
        assert_eq!(
            store.get(live_id).unwrap().close_reason.as_deref(),
            Some("launch-failed")
        );
    }

    /// `launch-failed` is restorable exactly like `poll-dead`: within 600 000
    /// ms of the registry's last moment of life, and not one millisecond past.
    #[test]
    fn restorable_records_admit_launch_failed_inside_the_grace_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let anchor = 1_794_284_837_305_i64;
        let closed = |id: &str, reason: &str, ago: i64| TerminalSessionRecord {
            state: "closed".to_string(),
            closed_at: Some(anchor - ago),
            close_reason: Some(reason.to_string()),
            last_seen_at: anchor - ago,
            opened_at: anchor - ago - 60_000,
            ..test_open_record(id)
        };
        let rows = [
            // The registry's last moment of life: an open row seen at the anchor.
            TerminalSessionRecord {
                last_seen_at: anchor,
                opened_at: anchor - 60_000,
                ..test_open_record("anchor")
            },
            closed("launch-failed-30s", "launch-failed", 30_000),
            closed("launch-failed-at-grace", "launch-failed", 600_000),
            closed("launch-failed-past-grace", "launch-failed", 600_001),
            closed("poll-dead-at-grace", "poll-dead", 600_000),
        ];
        let map: std::collections::HashMap<String, TerminalSessionRecord> = rows
            .into_iter()
            .map(|r| (r.claude_session_id.clone(), r))
            .collect();
        std::fs::write(&path, serde_json::to_vec_pretty(&map).unwrap()).unwrap();
        let store = SessionLifecycleStore::open(&path).unwrap();

        let mut ids: Vec<String> = store
            .restorable_records(None, true)
            .into_iter()
            .map(|r| r.claude_session_id)
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            [
                "anchor",
                "launch-failed-30s",
                "launch-failed-at-grace",
                "poll-dead-at-grace"
            ]
        );
    }
}
