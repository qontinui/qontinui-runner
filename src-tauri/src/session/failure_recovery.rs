//! The one place a session failure is recorded, announced and acted on.
//!
//! Plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 7. Producers (the usage-limit grid scan, the stream-json lane's exit
//! path, the resume-handshake verifier, the PTY waiter) no longer act on what
//! they see: each hands a [`FailureSignal`] to [`report`], which
//!
//! 1. classifies it ([`super::failure::classify`]),
//! 2. DECIDES what it will do about it ([`Plan`]) — the kind's
//!    [`RecoveryPolicy`], unless that policy cannot run now (its cap is spent,
//!    the runner is draining, the lane has no mechanism for it), in which case
//!    the failure is re-stated as manual
//!    ([`super::failure::with_manual_recovery`]) BEFORE anyone hears of it,
//! 3. records it as the session's ACTIVE failure of that kind — readable at
//!    `GET /terminals/{id}/failures` so a headless consumer does not depend on
//!    having been subscribed,
//! 4. announces it on the `session-failure` Tauri event plus the WS
//!    re-broadcast `terminal::exit_notice` uses (and through the caller's own
//!    hook, [`report_then`], so a lane's in-conversation status line states
//!    exactly the decision that runs), and
//! 5. carries the decision out by calling the mechanism that already owns
//!    it — never a second copy of one:
//!
//! | policy | PTY lane | structured (stream-json) lane |
//! |---|---|---|
//! | `migrate_account` | `terminal::account_migration::handle_usage_limit_hint` — its usage probe confirms (Hint → Confirmed) or refutes (clears) the failure before anything moves | the in-place restart on a rotated account ([`StructuredRestart`]) |
//! | `backoff_then_retry` | none: the CLI's TUI owns its own retry, and the runner holds the account (no rotation) | in-place restart on the SAME account after [`backoff_delay`] |
//! | `handoff_new_session` | `terminal::context_watcher`'s trigger (its flag, threshold and once-per-session debounce apply) | none: the lane has no handoff machinery, so the failure is stated as manual |
//! | `resume_same_id` | respawn `--resume <id>` on the same account through `account_migration::spawn_resumed_pane` (Claude only) | in-place restart on the same account |
//! | `never` (and the two unproduced policies) | none | none |
//!
//! Every automatic restart is bounded per session over a rolling window:
//! [`STRUCTURED_RESTART_CAP`] for the structured lane (all three policies
//! share it), [`RESUME_CAP`] for the PTY lane's crash resume — separate from
//! the account migration's own `MIGRATION_CAP`, so a crash loop cannot spend
//! the quota migrations and a quota migration cannot spend the crash resumes.
//! Past a cap the failure stays active and says no automatic action is coming.
//!
//! Everything runs in the runner process. Nothing here touches the dev
//! supervisor (`:9875`): an end user has no supervisor.
//!
//! **A failure clears by its policy's own evidence, never by a timer**
//! ([`Evidence`], [`cleared_by`]): a verified resume handshake, a usage probe
//! that finds the account not exhausted, a replacement session that PROVED it
//! is working (the structured restart's answered `initialize`; a respawned
//! pane's own `SessionStart` hook — never merely the spawn), or a real
//! successful turn on the structured lane. An operator may also dismiss one
//! explicitly, and a session's failures go when the session does.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use qontinui_types::cli_session::{
    FailureConfidence, FailureEvidenceSource, FailureKind, RecoveryPolicy, SessionFailure,
};
use serde::Serialize;
use tracing::{info, warn};

use super::failure::{classify_for, title_for, with_manual_recovery, FailureSignal};
use crate::session::session_lifecycle_store::TerminalSessionRecord;

/// The Tauri event (and WS channel) every failure notice travels on.
pub const SESSION_FAILURE_EVENT: &str = "session-failure";

/// Which lane a failure key belongs to. Decides pruning: a PTY key goes when
/// its terminal leaves the `TerminalManager`, a structured key when its
/// session leaves the `SessionManager`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// A PTY-hosted CLI; the key is the runner terminal id.
    Pty,
    /// A stream-json session; the key is the AI session id.
    Structured,
}

/// Restarts one stream-json session in place. Implemented by the lane itself
/// (`claude_session::session`), which owns spawn + conversation replay.
pub trait StructuredRestart: Send + Sync {
    /// Whether the restart is still wanted, asked AFTER any backoff wait and
    /// before restarting: `Err(why)` when the session the failure belonged to
    /// is no longer the one registered under its id (the operator closed it,
    /// or something replaced it) or the runner is draining. A backoff restart
    /// that skipped this check resurrected sessions closed during the wait.
    fn still_wanted(&self) -> Result<(), String>;
    /// Restart the session under its existing id. With `rotate_account` the
    /// runner first moves to another account; without it the account stays.
    /// Returns once the new process has answered its `initialize` — that
    /// answer is the evidence the replacement works.
    fn restart(&self, rotate_account: bool) -> Result<(), String>;
}

/// Where a failure happened, carrying what its recovery needs.
pub enum RecoveryTarget {
    /// A PTY terminal. `record` is its lifecycle row, captured when the
    /// signal was observed (the frontend closes the row on a pane exit, so a
    /// later lookup could miss it). `None` for a terminal with no registered
    /// AI session.
    Pty {
        terminal_id: String,
        record: Option<Box<TerminalSessionRecord>>,
    },
    /// A stream-json session.
    Structured {
        session_id: String,
        restart: Arc<dyn StructuredRestart>,
    },
}

impl RecoveryTarget {
    fn key(&self) -> &str {
        match self {
            RecoveryTarget::Pty { terminal_id, .. } => terminal_id,
            RecoveryTarget::Structured { session_id, .. } => session_id,
        }
    }

    fn lane(&self) -> Lane {
        match self {
            RecoveryTarget::Pty { .. } => Lane::Pty,
            RecoveryTarget::Structured { .. } => Lane::Structured,
        }
    }
}

/// An observation that a failure is over. Each clears the kinds [`cleared_by`]
/// lists for it — nothing clears on a timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// The resume handshake verified: the conversation is back.
    HandshakeVerified,
    /// The usage probe found the session's account NOT exhausted: the scraped
    /// limit phrase was echoed text or a historical repaint.
    QuotaNotExhausted,
    /// A replacement took the session over AND proved it works: a structured
    /// restart whose new process answered `initialize`, or a respawned PTY
    /// pane whose session's own `SessionStart` hook fired. A spawn alone is
    /// not this evidence — a `--resume` of a lost conversation spawns fine and
    /// then exits.
    SessionReplaced,
    /// The session completed a real, successful turn (a stream-json `result`
    /// that is a success and not `is_error`), so whatever failed before is
    /// not stopping it now.
    TurnSucceeded,
}

/// Which kinds a piece of evidence ends. Pure; the clear rules in one table.
pub fn cleared_by(kind: FailureKind, evidence: Evidence) -> bool {
    match evidence {
        Evidence::HandshakeVerified => kind == FailureKind::ResumeFailed,
        Evidence::QuotaNotExhausted => kind == FailureKind::QuotaExhausted,
        Evidence::SessionReplaced | Evidence::TurnSucceeded => true,
    }
}

/// Wire envelope of one `session-failure` notice: the failure, the session key
/// it belongs to (a terminal id on the PTY lane), and whether it is now active
/// (`false` = it just cleared).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFailureNotice {
    pub terminal_id: String,
    pub failure: SessionFailure,
    pub active: bool,
}

/// Where notices go. Production is [`TauriFailureSink`]; tests record.
pub trait FailureNoticeSink {
    fn notify(&self, notice: &SessionFailureNotice);
}

/// The Tauri event plus the WS relay re-broadcast — the same pair
/// `terminal::exit_notice::TauriExitSink` sends, so a remote or headless
/// consumer hears it too.
pub struct TauriFailureSink;

impl FailureNoticeSink for TauriFailureSink {
    fn notify(&self, notice: &SessionFailureNotice) {
        use tauri::Emitter;
        let Some(app) = crate::tauri_app_handle::current() else {
            return;
        };
        if let Err(e) = app.emit(SESSION_FAILURE_EVENT, notice) {
            warn!(key = %notice.terminal_id, error = %e, "failed to emit session-failure");
        }
        if crate::event_system::ws_notification_has_receivers(&app) {
            match serde_json::to_value(notice) {
                Ok(v) => {
                    crate::event_system::broadcast_ws_notification(&app, SESSION_FAILURE_EVENT, &v)
                }
                Err(e) => warn!(error = %e, "session-failure notice did not serialize"),
            }
        }
    }
}

fn announce(sink: &dyn FailureNoticeSink, key: &str, failure: SessionFailure, active: bool) {
    sink.notify(&SessionFailureNotice {
        terminal_id: key.to_string(),
        failure,
        active,
    });
}

// ============================================================================
// The active-failure store
// ============================================================================

/// One active failure and when it was last observed.
struct Stored {
    failure: SessionFailure,
    /// Process-wide observation order (larger = later). Refreshed by every
    /// repeat of the kind, so "most recent" means most recently OBSERVED.
    seq: u64,
    observed_at: Instant,
    /// Moved here from a pane a replacement took over, and waiting for that
    /// replacement to prove it works ([`on_pty_session_confirmed`]).
    awaiting_replacement: bool,
}

struct Entry {
    lane: Lane,
    failures: Vec<Stored>,
}

/// Active failures per session key. Const-initializable (the `Option` wrap),
/// like the grid scanners' state maps.
static STORE: Mutex<Option<HashMap<String, Entry>>> = Mutex::new(None);

/// Source of [`Stored::seq`].
static NEXT_SEQ: AtomicU64 = AtomicU64::new(0);

fn with_store<R>(f: impl FnOnce(&mut HashMap<String, Entry>) -> R) -> R {
    let mut guard = STORE.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(HashMap::new))
}

/// Record `failure` as `key`'s active failure of its kind. A repeat of a kind
/// already active replaces it but keeps the original id, so a surface updates
/// one row instead of stacking duplicates; its observation order and time move
/// to now. Returns what was stored.
fn upsert(key: &str, lane: Lane, failure: SessionFailure) -> SessionFailure {
    upsert_at(key, lane, failure, Instant::now())
}

fn upsert_at(key: &str, lane: Lane, mut failure: SessionFailure, now: Instant) -> SessionFailure {
    let seq = NEXT_SEQ.fetch_add(1, Ordering::Relaxed);
    with_store(|store| {
        let entry = store.entry(key.to_string()).or_insert_with(|| Entry {
            lane,
            failures: Vec::new(),
        });
        if let Some(existing) = entry
            .failures
            .iter_mut()
            .find(|s| s.failure.kind == failure.kind)
        {
            failure.id = existing.failure.id.clone();
            existing.failure = failure.clone();
            existing.seq = seq;
            existing.observed_at = now;
            existing.awaiting_replacement = false;
        } else {
            entry.failures.push(Stored {
                failure: failure.clone(),
                seq,
                observed_at: now,
                awaiting_replacement: false,
            });
        }
        failure
    })
}

/// Remove every failure under `key` that `pred` selects; returns them.
fn remove_where(key: &str, pred: impl Fn(&Stored) -> bool) -> Vec<SessionFailure> {
    with_store(|store| remove_where_in(store, key, pred))
}

fn remove_where_in(
    store: &mut HashMap<String, Entry>,
    key: &str,
    pred: impl Fn(&Stored) -> bool,
) -> Vec<SessionFailure> {
    let Some(entry) = store.get_mut(key) else {
        return Vec::new();
    };
    let (gone, kept): (Vec<_>, Vec<_>) = entry.failures.drain(..).partition(|s| pred(s));
    entry.failures = kept;
    if entry.failures.is_empty() {
        store.remove(key);
    }
    gone.into_iter().map(|s| s.failure).collect()
}

/// PTY panes whose provider `SessionStart` confirmed, and when. A replacement
/// pane can confirm BEFORE [`hand_over`] moves the failures to it (the
/// spawn returns after the CLI is already up); `hand_over` reads this so those
/// failures clear at once instead of waiting for a confirm that already came.
/// Only touched while holding [`STORE`] (lock order STORE → this), so a
/// confirm and a hand-over cannot interleave between the move and the check.
static CONFIRMED_PANES: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

/// How long a pane's confirmation is remembered for a late hand-over. A
/// hand-over follows its pane's spawn by seconds; this only bounds the map.
const CONFIRMED_PANE_TTL: Duration = Duration::from_secs(10 * 60);

/// Remember (`now = Some`) or ask about `terminal_id`'s confirmation, pruning
/// expired entries. Call only while holding [`STORE`].
fn confirmed_pane(terminal_id: &str, now: Option<Instant>) -> bool {
    let mut guard = CONFIRMED_PANES.lock().unwrap_or_else(|e| e.into_inner());
    let panes = guard.get_or_insert_with(HashMap::new);
    let at = Instant::now();
    panes.retain(|_, t| at.duration_since(*t) < CONFIRMED_PANE_TTL);
    match now {
        Some(t) => {
            panes.insert(terminal_id.to_string(), t);
            true
        }
        None => panes.contains_key(terminal_id),
    }
}

/// Replace `key`'s active failure `id` with `f(it)`; returns the new value.
fn update(
    key: &str,
    id: &str,
    f: impl FnOnce(SessionFailure) -> SessionFailure,
) -> Option<SessionFailure> {
    with_store(|store| {
        let stored = store
            .get_mut(key)?
            .failures
            .iter_mut()
            .find(|s| s.failure.id == id)?;
        stored.failure = f(stored.failure.clone());
        Some(stored.failure.clone())
    })
}

/// The failures currently active under `key` (a terminal id on the PTY lane,
/// an AI session id on the structured lane). Empty when there are none.
pub fn active(key: &str) -> Vec<SessionFailure> {
    with_store(|store| {
        store
            .get(key)
            .map(|e| e.failures.iter().map(|s| s.failure.clone()).collect())
            .unwrap_or_default()
    })
}

fn announce_cleared(sink: &dyn FailureNoticeSink, key: &str, gone: Vec<SessionFailure>) {
    for failure in gone {
        announce(sink, key, failure, false);
    }
}

/// Clear every failure under `key` that `evidence` ends, and announce each.
pub fn clear_on_evidence(key: &str, evidence: Evidence) {
    clear_on_evidence_with(&TauriFailureSink, key, evidence);
}

fn clear_on_evidence_with(sink: &dyn FailureNoticeSink, key: &str, evidence: Evidence) {
    let gone = remove_where(key, |s| cleared_by(s.failure.kind, evidence));
    if !gone.is_empty() {
        info!(
            key,
            ?evidence,
            cleared = gone.len(),
            "session failure(s) cleared by evidence"
        );
    }
    announce_cleared(sink, key, gone);
}

/// An operator explicitly acknowledged failure `failure_id`. Returns whether
/// it was active.
pub fn dismiss(key: &str, failure_id: &str) -> bool {
    let gone = remove_where(key, |s| s.failure.id == failure_id);
    let found = !gone.is_empty();
    announce_cleared(&TauriFailureSink, key, gone);
    found
}

/// Drop the failures of PTY terminals that are no longer live — the session a
/// failure described is gone. Called from the grid-scan tick, which already
/// holds the live set.
pub fn retain_live_terminals(live: &HashSet<&String>) {
    prune_dead(Lane::Pty, live, |_| true);
}

/// Drop the failures of structured sessions the `SessionManager` no longer
/// holds — the operator closed them, so there is nothing left to fail.
/// Called from the same tick as [`retain_live_terminals`].
pub fn retain_live_structured(live: &HashSet<&String>) {
    prune_dead(Lane::Structured, live, |_| true);
}

/// The pruning both retain calls share, over the keys `in_scope` selects — the
/// whole store in production; one test's own keys under the parallel runner.
fn prune_dead(lane: Lane, live: &HashSet<&String>, in_scope: impl Fn(&str) -> bool) {
    let gone: Vec<(String, Vec<SessionFailure>)> = with_store(|store| {
        let dead: Vec<String> = store
            .iter()
            .filter(|(k, e)| e.lane == lane && in_scope(k) && !live.contains(k))
            .map(|(k, _)| k.clone())
            .collect();
        dead.into_iter()
            .filter_map(|k| {
                store
                    .remove(&k)
                    .map(|e| (k, e.failures.into_iter().map(|s| s.failure).collect()))
            })
            .collect()
    });
    for (key, failures) in gone {
        announce_cleared(&TauriFailureSink, &key, failures);
    }
}

/// The usage probe confirmed a scraped quota hint: re-word and re-announce it
/// as confirmed. The evidence source stays what observed it.
fn confirm(sink: &dyn FailureNoticeSink, key: &str, kind: FailureKind) {
    let updated = with_store(|store| {
        let stored = store
            .get_mut(key)?
            .failures
            .iter_mut()
            .find(|s| s.failure.kind == kind)?;
        stored.failure.evidence.confidence = FailureConfidence::Confirmed;
        stored.failure.title = title_for(kind, FailureConfidence::Confirmed).to_string();
        Some(stored.failure.clone())
    });
    if let Some(failure) = updated {
        announce(sink, key, failure, true);
    }
}

/// The automatic recovery the runner chose not to (or could not) carry out:
/// re-state failure `id` as manual and re-announce it, so no surface keeps
/// promising an action that is not coming.
fn declined(sink: &dyn FailureNoticeSink, key: &str, id: &str, why: &str) {
    warn!(
        key,
        why, "automatic recovery declined — the failure is left for the operator"
    );
    if let Some(failure) = update(key, id, |f| with_manual_recovery(f, why)) {
        announce(sink, key, failure, true);
    }
}

/// A replacement pane `to` took over the session pane `from` hosted: its
/// failures move with it (announced cleared on `from`, active on `to`) and
/// wait for the replacement to prove itself. They clear on
/// [`on_pty_session_confirmed`], never on the spawn.
fn hand_over(sink: &dyn FailureNoticeSink, from: &str, to: &str) {
    let (moved, cleared): (Vec<Stored>, Vec<SessionFailure>) = with_store(|store| {
        let Some(entry) = store.remove(from) else {
            return (Vec::new(), Vec::new());
        };
        let target = store.entry(to.to_string()).or_insert_with(|| Entry {
            lane: Lane::Pty,
            failures: Vec::new(),
        });
        let mut moved = Vec::new();
        for mut s in entry.failures {
            s.awaiting_replacement = true;
            target.failures.retain(|t| t.failure.kind != s.failure.kind);
            moved.push(Stored {
                failure: s.failure.clone(),
                seq: s.seq,
                observed_at: s.observed_at,
                awaiting_replacement: true,
            });
            target.failures.push(s);
        }
        // The replacement already proved itself: its SessionStart arrived
        // before this hand-over, and no second one is coming.
        let cleared = if confirmed_pane(to, None) {
            remove_where_in(store, to, |s| s.awaiting_replacement)
        } else {
            Vec::new()
        };
        (moved, cleared)
    });
    for s in moved {
        announce(sink, from, s.failure.clone(), false);
        announce(sink, to, s.failure, true);
    }
    if !cleared.is_empty() {
        info!(
            terminal_id = to,
            cleared = cleared.len(),
            evidence = ?Evidence::SessionReplaced,
            "replacement session had already confirmed — the failures it took over are over"
        );
    }
    announce_cleared(sink, to, cleared);
}

/// A provider's `SessionStart` hook confirmed a session on PTY `terminal_id`.
/// When that pane is a replacement the runner spawned for a failed session,
/// this is the proof it works: the failures handed over to it end
/// ([`Evidence::SessionReplaced`]). A pane that is no replacement keeps its
/// failures — a `SessionStart` after `/clear` says nothing about a quota.
pub fn on_pty_session_confirmed(terminal_id: &str) {
    on_pty_session_confirmed_with(&TauriFailureSink, terminal_id);
}

fn on_pty_session_confirmed_with(sink: &dyn FailureNoticeSink, terminal_id: &str) {
    let gone = with_store(|store| {
        confirmed_pane(terminal_id, Some(Instant::now()));
        remove_where_in(store, terminal_id, |s| s.awaiting_replacement)
    });
    if !gone.is_empty() {
        info!(
            terminal_id,
            cleared = gone.len(),
            evidence = ?Evidence::SessionReplaced,
            "replacement session confirmed — the failures it took over are over"
        );
    }
    announce_cleared(sink, terminal_id, gone);
}

// ============================================================================
// Report: classify → decide → record → announce → carry out
// ============================================================================

/// What the runner will do about one failure, decided before it is announced.
enum Plan {
    /// No automatic action, and the failure's policy says so already.
    Nothing,
    /// The policy cannot run now; `why` is stated on the failure.
    Declined(String),
    /// PTY: confirm the quota and migrate the account.
    PtyMigrate,
    /// PTY: route to the context-handoff trigger.
    PtyHandoff,
    /// PTY: respawn `--resume <id>` from this record.
    PtyResume(Box<TerminalSessionRecord>),
    /// PTY: the CLI retries a transient fault itself; the account stays.
    PtyHoldAccount,
    /// Structured: restart in place after `delay`.
    StructuredRestart { rotate: bool, delay: Duration },
}

/// Classify `signal` for a session of `provider`, decide, record, announce and
/// carry out its recovery. `None` when the signal reports no failure.
/// Structured-lane recovery runs on the calling thread (the lane's waiter
/// thread, which exists for this); PTY-lane recovery is spawned.
pub fn report(
    target: RecoveryTarget,
    provider: &str,
    account: Option<String>,
    signal: FailureSignal,
) -> Option<SessionFailure> {
    report_then(target, provider, account, signal, |_| {})
}

/// [`report`], calling `on_decided` with the failure exactly as recorded and
/// announced — policy and details reflecting the decision that will run — after
/// it is announced and BEFORE the recovery starts. A lane that also states the
/// failure in its own conversation does it here, from this value, so it can
/// never describe a different failure or a different decision than the one the
/// runner carries out.
pub fn report_then(
    target: RecoveryTarget,
    provider: &str,
    account: Option<String>,
    signal: FailureSignal,
    on_decided: impl FnOnce(&SessionFailure),
) -> Option<SessionFailure> {
    report_with(
        &TauriFailureSink,
        target,
        provider,
        account,
        signal,
        Instant::now(),
        &std::thread::sleep,
        on_decided,
    )
}

#[allow(clippy::too_many_arguments)]
fn report_with(
    sink: &dyn FailureNoticeSink,
    target: RecoveryTarget,
    provider: &str,
    account: Option<String>,
    signal: FailureSignal,
    now: Instant,
    sleep: &dyn Fn(Duration),
    on_decided: impl FnOnce(&SessionFailure),
) -> Option<SessionFailure> {
    let failure = match exit_tail_of_structured_failure(&target, &signal, now) {
        Some(prior) => {
            info!(
                key = target.key(),
                kind = ?prior.kind,
                "stream-json exit follows a typed failure already reported on the stream — one failure, executing its policy"
            );
            prior
        }
        None => {
            let profile = qontinui_runner_lib::cli_profile::profile_for(provider);
            let mut failure = classify_for(&signal, provider, profile)?;
            failure.account = account;
            failure
        }
    };
    let plan = plan_for(&target, &failure, now);
    let failure = match &plan {
        Plan::Declined(why) => with_manual_recovery(failure, why),
        _ => failure,
    };
    let stored = upsert_at(target.key(), target.lane(), failure, now);
    info!(
        key = target.key(),
        kind = ?stored.kind,
        confidence = ?stored.evidence.confidence,
        policy = ?stored.recovery_policy,
        "session failure recorded"
    );
    announce(sink, target.key(), stored.clone(), true);
    on_decided(&stored);
    carry_out(sink, target, &stored, plan, sleep);
    Some(stored)
}

/// How long after a typed failure frame the lane's exit still counts as that
/// failure's tail. The CLI exits right after an API-errored turn (Phase 2
/// probe Q3: the errored `result`, then a non-zero exit, within the same
/// second), so a minute is generous; an exit later than that is a new
/// observation, and an old typed failure the session went on past (without a
/// successful turn to clear it) must not dictate what that exit does.
pub const EXIT_FOLD_WINDOW: Duration = Duration::from_secs(60);

/// One failure, not two. On the structured lane an errored turn is stated
/// twice: by a typed frame on the stream (recorded then by
/// `claude_session::dispatcher`, without recovery, since the child is still
/// running) and by the child's exit that follows it. When the lane's exit
/// signal (its exit status or its stderr) arrives for a session with a typed
/// failure observed within [`EXIT_FOLD_WINDOW`], the exit IS that failure's
/// tail, and [`report`] executes its policy instead of recording a second,
/// weaker one.
///
/// Several typed failures in one turn (a `rate_limit_event` reporting
/// `rejected`, then the `result` with HTTP 429) pick by [`fold_precedence`],
/// then by observation order — never by which the store happened to list last.
fn exit_tail_of_structured_failure(
    target: &RecoveryTarget,
    signal: &FailureSignal,
    now: Instant,
) -> Option<SessionFailure> {
    let RecoveryTarget::Structured { session_id, .. } = target else {
        return None;
    };
    if !matches!(
        signal,
        FailureSignal::Exit { .. } | FailureSignal::Stderr(_)
    ) {
        return None;
    }
    with_store(|store| {
        store
            .get(session_id.as_str())?
            .failures
            .iter()
            .filter(|s| s.failure.evidence.source == FailureEvidenceSource::StructuredEvent)
            .filter(|s| now.saturating_duration_since(s.observed_at) <= EXIT_FOLD_WINDOW)
            .max_by_key(|s| (fold_precedence(s.failure.kind), s.seq))
            .map(|s| s.failure.clone())
    })
}

/// Which typed failure a session's exit executes when one turn stated several.
/// The account's capacity being gone is the most specific statement — another
/// account is the only fix, and a backoff on the dead account would wait for
/// nothing — so `quota_exhausted` / `budget_exhausted` win; an unnamed
/// failure loses to any named one.
fn fold_precedence(kind: FailureKind) -> u8 {
    match kind {
        FailureKind::QuotaExhausted | FailureKind::BudgetExhausted => 2,
        FailureKind::Unknown => 0,
        _ => 1,
    }
}

/// Decide what to do about `failure`, consuming a restart slot when the
/// decision is to restart. Synchronous and cheap — everything that can refuse
/// up front refuses here, so the failure is announced with the decision.
fn plan_for(target: &RecoveryTarget, failure: &SessionFailure, now: Instant) -> Plan {
    let policy = failure.recovery_policy;
    match target {
        RecoveryTarget::Pty {
            terminal_id,
            record,
        } => match policy {
            RecoveryPolicy::MigrateAccount => Plan::PtyMigrate,
            RecoveryPolicy::HandoffNewSession => Plan::PtyHandoff,
            RecoveryPolicy::BackoffThenRetry => Plan::PtyHoldAccount,
            RecoveryPolicy::ResumeSameId => {
                let Some(record) = record else {
                    return Plan::Declined("the exited pane has no session record".into());
                };
                if let Err(why) = resume_in_place_supported(record) {
                    return Plan::Declined(why);
                }
                if crate::drain::is_draining() {
                    return Plan::Declined("the runner is draining".into());
                }
                if !take_resume_slot(&record.claude_session_id, now) {
                    return Plan::Declined(format!(
                        "its automatic resume cap ({RESUME_CAP} in {} hours) is spent",
                        RESUME_WINDOW.as_secs() / 3600
                    ));
                }
                info!(
                    terminal_id,
                    "exited session will be resumed under the same id"
                );
                Plan::PtyResume(record.clone())
            }
            RecoveryPolicy::Never
            | RecoveryPolicy::WaitUntilReset
            | RecoveryPolicy::LoginThenResume => Plan::Nothing,
        },
        RecoveryTarget::Structured { session_id, .. } => {
            let rotate = match policy {
                RecoveryPolicy::MigrateAccount => true,
                RecoveryPolicy::ResumeSameId | RecoveryPolicy::BackoffThenRetry => false,
                RecoveryPolicy::HandoffNewSession => {
                    return Plan::Declined(
                        "a structured session has no handoff to a continuation session".into(),
                    )
                }
                RecoveryPolicy::Never
                | RecoveryPolicy::WaitUntilReset
                | RecoveryPolicy::LoginThenResume => return Plan::Nothing,
            };
            let Some(attempt) = take_restart_slot(session_id, now) else {
                return Plan::Declined(format!(
                    "its automatic restart cap ({STRUCTURED_RESTART_CAP} an hour) is spent"
                ));
            };
            let delay = if policy == RecoveryPolicy::BackoffThenRetry {
                backoff_delay(attempt)
            } else {
                Duration::ZERO
            };
            Plan::StructuredRestart { rotate, delay }
        }
    }
}

fn carry_out(
    sink: &dyn FailureNoticeSink,
    target: RecoveryTarget,
    failure: &SessionFailure,
    plan: Plan,
    sleep: &dyn Fn(Duration),
) {
    let key = target.key().to_string();
    match (target, plan) {
        (_, Plan::Nothing | Plan::Declined(_)) => {}
        (RecoveryTarget::Pty { terminal_id, .. }, Plan::PtyMigrate) => {
            let kind = failure.kind;
            let id = failure.id.clone();
            // The matched phrase, for the migration's own log lines.
            let phrase = failure.reason.clone().unwrap_or_default();
            tauri::async_runtime::spawn(async move {
                use crate::terminal::account_migration::{handle_usage_limit_hint, HintOutcome};
                let sink = TauriFailureSink;
                match handle_usage_limit_hint(terminal_id.clone(), phrase).await {
                    HintOutcome::NotConfirmed => {
                        clear_on_evidence(&terminal_id, Evidence::QuotaNotExhausted)
                    }
                    HintOutcome::Confirmed { migrated_to } => {
                        confirm(&sink, &terminal_id, kind);
                        match migrated_to {
                            Some(new_terminal) => hand_over(&sink, &terminal_id, &new_terminal),
                            None => declined(
                                &sink,
                                &terminal_id,
                                &id,
                                "no account could take the session over",
                            ),
                        }
                    }
                    HintOutcome::NotApplicable(why) => declined(&sink, &terminal_id, &id, why),
                }
            });
        }
        (RecoveryTarget::Pty { terminal_id, .. }, Plan::PtyHandoff) => {
            let outcome = crate::terminal::context_watcher::on_precompact_signal(
                &terminal_id,
                &serde_json::json!({ "trigger": "auto" }),
            );
            info!(
                terminal_id,
                fired = outcome.fired,
                mode = outcome.mode,
                reason = %outcome.reason,
                "context-exhausted failure routed to the context-handoff trigger"
            );
            if !outcome.fired {
                declined(sink, &terminal_id, &failure.id, &outcome.reason);
            }
        }
        (RecoveryTarget::Pty { terminal_id, .. }, Plan::PtyResume(record)) => {
            let id = failure.id.clone();
            tauri::async_runtime::spawn(async move {
                let sink = TauriFailureSink;
                match resume_in_place(&record) {
                    Ok(new_terminal_id) => {
                        info!(
                            terminal_id,
                            new_terminal_id,
                            "exited session respawned under the same id — its failure moves to the new pane until the session confirms"
                        );
                        hand_over(&sink, &terminal_id, &new_terminal_id);
                    }
                    Err(why) => declined(&sink, &terminal_id, &id, &why),
                }
            });
        }
        (RecoveryTarget::Pty { terminal_id, .. }, Plan::PtyHoldAccount) => info!(
            terminal_id,
            kind = ?failure.kind,
            "transient provider failure on a PTY session — the CLI retries, the account stays"
        ),
        (RecoveryTarget::Structured { restart, .. }, Plan::StructuredRestart { rotate, delay }) => {
            if !delay.is_zero() {
                info!(
                    session_id = %key,
                    delay_secs = delay.as_secs(),
                    "backing off before an in-place restart"
                );
                sleep(delay);
            }
            if let Err(why) = restart.still_wanted() {
                declined(sink, &key, &failure.id, &why);
                return;
            }
            match restart.restart(rotate) {
                Ok(()) => clear_on_evidence_with(sink, &key, Evidence::SessionReplaced),
                Err(e) => declined(sink, &key, &failure.id, &format!("the restart failed: {e}")),
            }
        }
        // A plan is only ever made for its own lane.
        (_, plan) => warn!(
            key,
            plan = plan_name(&plan),
            "recovery plan does not match its lane — nothing done"
        ),
    }
}

fn plan_name(plan: &Plan) -> &'static str {
    match plan {
        Plan::Nothing => "nothing",
        Plan::Declined(_) => "declined",
        Plan::PtyMigrate => "pty_migrate",
        Plan::PtyHandoff => "pty_handoff",
        Plan::PtyResume(_) => "pty_resume",
        Plan::PtyHoldAccount => "pty_hold_account",
        Plan::StructuredRestart { .. } => "structured_restart",
    }
}

// ============================================================================
// The PTY waiter's report
// ============================================================================

/// The AI CLI a PTY pane's own child process IS — known at spawn from its
/// argv, and carried by the pane's waiter to its exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneCli {
    /// The CLI profile id the argv head names.
    pub provider: String,
    /// The argv resumes or continues an existing session.
    pub resumed: bool,
}

/// How the pane whose child exited was launched and how its exit looked — what
/// the PTY waiter knows when its child goes.
#[derive(Debug, Clone)]
pub struct PtyExit<'a> {
    /// The exit code; `None` when it could not be read.
    pub code: Option<i32>,
    /// The CLI profile id of the pane's own child, when that child was an AI
    /// CLI (a direct exec); `None` for a shell pane or a remote pane.
    pub provider: Option<&'a str>,
    /// The child's argv resumed or continued an existing session.
    pub resumed: bool,
    /// How long the child ran.
    pub lifetime: Duration,
    /// The pane's rendered screen at exit.
    pub screen: &'a str,
}

/// A resumed CLI that exits sooner than this, unsuccessfully, never got its
/// conversation back: a working resume does not exit by itself within seconds,
/// and an operator's own quick `/exit` exits 0.
pub const RESUME_EXIT_GRACE: Duration = Duration::from_secs(20);

/// The PTY waiter's report: pane `terminal_id`'s child exited.
///
/// Not a failure when the runner closed the pane itself, when the exit was
/// clean (the classifier's `Exit { code: Some(0) }` is `None`), or when
/// [`pty_exit_session`] finds no AI session the exit ended — a plain shell's
/// exit is nobody's failure. The lifecycle row is read HERE, synchronously on
/// the waiter thread, before the exit notice lets the frontend close it.
pub fn report_pty_exit(terminal_id: &str, closed_deliberately: bool, exit: &PtyExit<'_>) {
    use tauri::Manager;
    if closed_deliberately {
        return;
    }
    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(store) =
        app.try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
    else {
        return;
    };
    let Some(record) = pty_exit_session(&store, terminal_id, exit.provider) else {
        return;
    };
    let provider = record.provider.clone();
    let account = record.config_dir.clone();
    let signal = pty_exit_signal(
        qontinui_runner_lib::cli_profile::profile_for(&provider),
        exit,
    );
    report(
        RecoveryTarget::Pty {
            terminal_id: terminal_id.to_string(),
            record: Some(Box::new(record)),
        },
        &provider,
        account,
        signal,
    );
}

/// The AI session a PTY exit ended, or `None` when the exit is no AI
/// session's failure. Two pieces of evidence are both required:
///
/// - **The exiting process was an AI CLI** (`exit_provider`, from the pane's
///   own argv head). A shell pane's PTY exit is the SHELL's: `exit 1` in a
///   plain pane is not a crashed agent, even if `claude` ran in it earlier
///   and ended with its own clean `/exit`.
/// - **A CONFIRMED session of that provider is open on the pane.** The
///   spawn-time identity seam writes a provisional `open` row for EVERY pane,
///   shells included, under a minted id no CLI ever ran under
///   ([`SessionLifecycleStore::find_confirmed_open_by_terminal`] says why).
///   Treating that row as a session made a shell's exit a `process_exited`
///   whose `resume_same_id` typed `claude --resume <an id that never
///   existed>` — which exits again, up to the respawn cap.
///
/// [`SessionLifecycleStore::find_confirmed_open_by_terminal`]:
/// crate::session::session_lifecycle_store::SessionLifecycleStore::find_confirmed_open_by_terminal
pub(crate) fn pty_exit_session(
    store: &crate::session::session_lifecycle_store::SessionLifecycleStore,
    terminal_id: &str,
    exit_provider: Option<&str>,
) -> Option<TerminalSessionRecord> {
    let provider = exit_provider?;
    let record = store.find_confirmed_open_by_terminal(terminal_id)?;
    (record.provider == provider).then_some(record)
}

/// The failure signal a PTY AI-CLI exit reports. A RESUMED child that exits
/// unsuccessfully is a failed resume — `resume_failed`, whose policy never
/// retries — not a crash to resume again: either its screen shows one of its
/// profile's resume-failure markers (`No conversation found with session ID`),
/// or it went within [`RESUME_EXIT_GRACE`]. Every other exit is the exit.
/// Without this, a respawned `--resume` of a lost conversation exited as
/// `process_exited` → `resume_same_id` → the same respawn, until the cap.
pub(crate) fn pty_exit_signal(
    profile: Option<&qontinui_types::cli_session::CliProfile>,
    exit: &PtyExit<'_>,
) -> FailureSignal {
    if exit.resumed && exit.code != Some(0) {
        if let (Some(profile), Some(provider)) = (profile, exit.provider) {
            if let Some(line) = super::failure::resume_failure_line(profile, exit.screen) {
                return FailureSignal::GridPhrase {
                    provider: provider.to_string(),
                    phrase: line,
                };
            }
        }
        if exit.lifetime < RESUME_EXIT_GRACE {
            return FailureSignal::HandshakeTimeout;
        }
    }
    FailureSignal::Exit { code: exit.code }
}

/// Steps 1 and 3–4 of [`report`], without deciding or executing anything. The
/// door for a producer whose recovery is someone else's (the frontend's resume
/// verifier owns its own retry) or comes later (a structured frame, whose
/// recovery runs at the child's exit).
pub fn record_only(
    key: &str,
    lane: Lane,
    provider: &str,
    signal: &FailureSignal,
) -> Option<SessionFailure> {
    let profile = qontinui_runner_lib::cli_profile::profile_for(provider);
    let failure = classify_for(signal, provider, profile)?;
    let stored = upsert(key, lane, failure);
    announce(&TauriFailureSink, key, stored.clone(), true);
    Some(stored)
}

// ============================================================================
// Bounded restarts (structured lane)
// ============================================================================

/// Most automatic restarts one structured session gets within
/// [`STRUCTURED_RESTART_WINDOW`] — migrate, backoff and resume alike. Without
/// one shared cap a session whose every account is exhausted would migrate in
/// a loop, and a backoff and a resume could each spend their own budget on the
/// same broken session.
pub const STRUCTURED_RESTART_CAP: usize = 3;
/// The rolling window [`STRUCTURED_RESTART_CAP`] counts over.
const STRUCTURED_RESTART_WINDOW: Duration = Duration::from_secs(60 * 60);
/// First backoff delay; each further restart in the window doubles it.
const BACKOFF_BASE: Duration = Duration::from_secs(30);

/// The delay before a backoff restart that is restart `attempt` (0-based) in
/// the window: 30 s, 60 s, 120 s.
pub fn backoff_delay(attempt: usize) -> Duration {
    BACKOFF_BASE * 2u32.pow(attempt.min(STRUCTURED_RESTART_CAP) as u32)
}

static RESTART_HISTORY: Mutex<Option<HashMap<String, Vec<Instant>>>> = Mutex::new(None);

/// Claim the next restart slot for `key` at `now`: how many restarts it had in
/// the window before this one, or `None` when the cap is spent. Every key's
/// history is trimmed to the window on the way, and a key left with none is
/// dropped — the map holds only sessions restarted within the last hour.
fn take_restart_slot(key: &str, now: Instant) -> Option<usize> {
    take_slot(
        &RESTART_HISTORY,
        key,
        now,
        STRUCTURED_RESTART_CAP,
        STRUCTURED_RESTART_WINDOW,
    )
}

fn take_slot(
    history: &Mutex<Option<HashMap<String, Vec<Instant>>>>,
    key: &str,
    now: Instant,
    cap: usize,
    window: Duration,
) -> Option<usize> {
    let mut guard = history.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    map.retain(|_, stamps| {
        stamps.retain(|t| now.saturating_duration_since(*t) < window);
        !stamps.is_empty()
    });
    let stamps = map.entry(key.to_string()).or_default();
    let before = stamps.len();
    if before >= cap {
        return None;
    }
    stamps.push(now);
    Some(before)
}

// ============================================================================
// resume_same_id on the PTY lane
// ============================================================================

/// Most automatic crash resumes one session gets within [`RESUME_WINDOW`].
/// Its own budget: the account migration's `MIGRATION_CAP` counts quota hops,
/// and a crash loop must neither spend those nor be allowed more because of
/// them.
pub const RESUME_CAP: usize = 3;
/// The rolling window [`RESUME_CAP`] counts over.
const RESUME_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

static RESUME_HISTORY: Mutex<Option<HashMap<String, Vec<Instant>>>> = Mutex::new(None);

/// Claim a crash-resume slot for session `claude_session_id`; `false` when
/// its [`RESUME_CAP`] is spent.
fn take_resume_slot(claude_session_id: &str, now: Instant) -> bool {
    take_slot(
        &RESUME_HISTORY,
        claude_session_id,
        now,
        RESUME_CAP,
        RESUME_WINDOW,
    )
    .is_some()
}

/// Whether [`resume_in_place`] can relaunch `record`'s session at all. Its
/// mechanism, `account_migration::spawn_resumed_pane`, launches the CLAUDE
/// binary with Claude's resume flags and pins `CLAUDE_CONFIG_DIR` — it cannot
/// drive another profile, so a record of any other provider is refused here
/// (and the failure stays active with its manual actions) rather than being
/// relaunched as `claude --resume <another CLI's id>`. The record must also
/// carry an account and a working dir, and an id that is safe on a command
/// line ([`qontinui_runner_lib::cli_profile::is_valid_session_id`]).
fn resume_in_place_supported(record: &TerminalSessionRecord) -> Result<(), String> {
    if record.provider != qontinui_runner_lib::cli_profile::claude::ID {
        return Err(format!(
            "in-place resume launches the Claude CLI and cannot resume a {} session",
            record.provider
        ));
    }
    if !qontinui_runner_lib::cli_profile::is_valid_session_id(&record.claude_session_id) {
        return Err("the session id is not safe to put on a command line".into());
    }
    if record.config_dir.is_none() {
        return Err("the session's account is unknown".into());
    }
    if record.working_dir.is_none() {
        return Err("the session has no working dir".into());
    }
    Ok(())
}

/// Respawn an exited PTY session with `--resume <id>` on the account it ran
/// under, through the shared resume seam
/// ([`crate::terminal::account_migration::spawn_resumed_pane`], the one the
/// account migration and the cross-machine respawn use), then retire the dead
/// pane the way the migration retires its old one. [`plan_for`] has already
/// checked [`resume_in_place_supported`], the drain and the resume cap.
fn resume_in_place(record: &TerminalSessionRecord) -> Result<String, String> {
    use tauri::Manager;

    resume_in_place_supported(record)?;
    let config_dir = record
        .config_dir
        .as_deref()
        .ok_or("session's account is unknown")?;
    let working_dir = record
        .working_dir
        .as_deref()
        .ok_or("session has no working dir")?;
    let app = crate::tauri_app_handle::current().ok_or("no app handle")?;
    let terminal_manager = app
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .ok_or("TerminalManager not managed")?
        .inner()
        .clone();
    let session_registry = app
        .try_state::<Arc<crate::session::SessionRegistry>>()
        .ok_or("SessionRegistry not managed")?
        .inner()
        .clone();
    let store = app
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        .ok_or("SessionLifecycleStore not managed")?
        .inner()
        .clone();

    let title = record
        .title
        .clone()
        .unwrap_or_else(|| "Resumed session".to_string());
    let (new_terminal_id, _coord) = crate::terminal::account_migration::spawn_resumed_pane(
        &app,
        &terminal_manager,
        &session_registry,
        crate::terminal::account_migration::ResumeSpawn {
            claude_session_id: &record.claude_session_id,
            working_dir,
            model_transcript: crate::terminal::transcript::session_transcript_path(
                std::path::Path::new(config_dir),
                working_dir,
                &record.claude_session_id,
            ),
            config_dir,
            title,
            page_id: record.page_id.clone(),
            zone_index: record.zone_index,
            // The same session continuing: no new work unit, topic, repo or
            // lineage claim.
            work_unit_slug: None,
            correlation_topic: None,
            intent_repo: None,
            coord_lineage: None,
            // The source already exited, so this is something genuinely new on
            // the box — the resource gate may refuse it, exactly as it may
            // refuse the cross-machine respawn.
            resource_override: false,
            // The dead pane's exit hook already answered any gate it carried.
            gate_identity: None,
        },
    )?;

    // The dead pane's row now belongs to the new terminal; its later
    // `pty-exit` close must answer `TerminalSuperseded` rather than close the
    // resumed session's row.
    store.mark_terminal_superseded(&record.terminal_id, &new_terminal_id);
    if let Err(e) = terminal_manager.close(&record.terminal_id) {
        store.unmark_terminal_superseded(&record.terminal_id);
        info!(
            terminal_id = %record.terminal_id,
            error = %e,
            "dead pane already gone after the resume"
        );
    }
    Ok(new_terminal_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::failure::classify;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<SessionFailureNotice>>);

    impl FailureNoticeSink for Recorder {
        fn notify(&self, notice: &SessionFailureNotice) {
            self.0.lock().unwrap().push(notice.clone());
        }
    }

    impl Recorder {
        fn take(&self) -> Vec<(FailureKind, bool)> {
            self.0
                .lock()
                .unwrap()
                .drain(..)
                .map(|n| (n.failure.kind, n.active))
                .collect()
        }

        fn notices(&self) -> Vec<SessionFailureNotice> {
            self.0.lock().unwrap().drain(..).collect()
        }
    }

    /// A structured restart that records each call and refuses (or, with
    /// `wanted == false`, reports the session gone).
    struct Counting {
        calls: Mutex<Vec<bool>>,
        wanted: Mutex<bool>,
    }

    impl Counting {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                wanted: Mutex::new(true),
            })
        }
        fn calls(&self) -> Vec<bool> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl StructuredRestart for Counting {
        fn still_wanted(&self) -> Result<(), String> {
            if *self.wanted.lock().unwrap() {
                Ok(())
            } else {
                Err("the session was closed".into())
            }
        }
        fn restart(&self, rotate: bool) -> Result<(), String> {
            self.calls.lock().unwrap().push(rotate);
            Err("not in a test".into())
        }
    }

    fn pty(key: &str) -> RecoveryTarget {
        RecoveryTarget::Pty {
            terminal_id: key.to_string(),
            record: None,
        }
    }

    fn structured(key: &str, restart: Arc<Counting>) -> RecoveryTarget {
        RecoveryTarget::Structured {
            session_id: key.to_string(),
            restart,
        }
    }

    fn provider() -> &'static str {
        qontinui_runner_lib::cli_profile::claude::ID
    }

    fn grid(phrase: &str) -> FailureSignal {
        FailureSignal::GridPhrase {
            provider: provider().to_string(),
            phrase: phrase.to_string(),
        }
    }

    fn no_sleep(_: Duration) {}

    /// Classify, record and announce — no decision, no recovery.
    fn record_and_announce(
        sink: &dyn FailureNoticeSink,
        target: &RecoveryTarget,
        provider: &str,
        account: Option<String>,
        signal: &FailureSignal,
    ) -> Option<SessionFailure> {
        let profile = qontinui_runner_lib::cli_profile::profile_for(provider);
        let mut failure = classify_for(signal, provider, profile)?;
        failure.account = account;
        let stored = upsert(target.key(), target.lane(), failure);
        announce(sink, target.key(), stored.clone(), true);
        Some(stored)
    }

    /// [`report_with`] on the structured lane, synchronously, sleeping
    /// through `sleep`.
    fn report_structured(
        sink: &dyn FailureNoticeSink,
        key: &str,
        restart: &Arc<Counting>,
        signal: FailureSignal,
        now: Instant,
        sleep: &dyn Fn(Duration),
    ) -> Option<SessionFailure> {
        report_with(
            sink,
            structured(key, restart.clone()),
            provider(),
            None,
            signal,
            now,
            sleep,
            |_| {},
        )
    }

    fn structured_frame(rate_limit_status: Option<&str>, api_status: Option<u16>) -> FailureSignal {
        FailureSignal::StructuredEvent {
            error_code: None,
            api_status,
            rate_limit_status: rate_limit_status.map(str::to_string),
            message: None,
            reset_at: None,
        }
    }

    /// Every kind has exactly the clear rule the module docs state.
    #[test]
    fn clear_rules_table() {
        assert!(cleared_by(
            FailureKind::ResumeFailed,
            Evidence::HandshakeVerified
        ));
        assert!(!cleared_by(
            FailureKind::QuotaExhausted,
            Evidence::HandshakeVerified
        ));
        assert!(cleared_by(
            FailureKind::QuotaExhausted,
            Evidence::QuotaNotExhausted
        ));
        assert!(!cleared_by(
            FailureKind::RateLimited,
            Evidence::QuotaNotExhausted
        ));
        assert!(!cleared_by(
            FailureKind::ResumeFailed,
            Evidence::QuotaNotExhausted
        ));
        for kind in [
            FailureKind::Unknown,
            FailureKind::ProcessExited,
            FailureKind::QuotaExhausted,
            FailureKind::BadRequest,
        ] {
            assert!(cleared_by(kind, Evidence::SessionReplaced));
            assert!(cleared_by(kind, Evidence::TurnSucceeded));
        }
    }

    /// Plan Phase 8: an errored stream-json turn recorded from its frame, then
    /// the child's non-zero exit (and its stderr), is ONE failure — the exit
    /// executes the recorded failure's policy instead of recording another.
    #[test]
    fn structured_exit_after_a_typed_failure_is_one_failure() {
        let key = "fr-test-structured-dedupe";
        let restart = Counting::new();
        let sink = Recorder::default();
        let now = Instant::now();
        // The stream's rate_limit_event said `rejected`: recorded, not executed.
        let frame = record_only(
            key,
            Lane::Structured,
            provider(),
            &structured_frame(Some("rejected"), None),
        )
        .unwrap();
        assert_eq!(frame.kind, FailureKind::QuotaExhausted);
        assert!(
            restart.calls().is_empty(),
            "no recovery while the child runs"
        );

        // The exit, carrying a stderr the classifier would call a rate limit.
        let exit = report_structured(
            &sink,
            key,
            &restart,
            FailureSignal::Stderr("API error (429): rate limit".into()),
            now,
            &no_sleep,
        )
        .unwrap();
        assert_eq!(exit.id, frame.id, "the exit is the recorded failure");
        assert_eq!(active(key).len(), 1, "one failure, not two");
        // Its policy ran once, at exit: migrate (rotate) for a quota.
        assert_eq!(restart.calls(), vec![true]);

        // A non-zero exit with no stderr folds the same way.
        report_structured(
            &sink,
            key,
            &restart,
            FailureSignal::Exit { code: Some(1) },
            now,
            &no_sleep,
        )
        .unwrap();
        assert_eq!(active(key).len(), 1);

        // A successful turn clears it; a later exit is then its own failure.
        clear_on_evidence_with(&Recorder::default(), key, Evidence::TurnSucceeded);
        assert!(active(key).is_empty());
        let fresh = report_structured(
            &sink,
            key,
            &restart,
            FailureSignal::Exit { code: Some(1) },
            now,
            &no_sleep,
        )
        .unwrap();
        assert_ne!(fresh.id, frame.id);
        assert_eq!(fresh.kind, FailureKind::ProcessExited);
        remove_where(key, |_| true);

        // The PTY lane never folds: its exit is always its own observation.
        let pty_key = "fr-test-structured-dedupe-pty";
        record_only(
            pty_key,
            Lane::Pty,
            provider(),
            &FailureSignal::StructuredEvent {
                error_code: Some("model_not_found".into()),
                api_status: None,
                rate_limit_status: None,
                message: None,
                reset_at: None,
            },
        )
        .unwrap();
        assert!(exit_tail_of_structured_failure(
            &pty(pty_key),
            &FailureSignal::Exit { code: Some(1) },
            Instant::now(),
        )
        .is_none());
        remove_where(pty_key, |_| true);
    }

    /// M1: one turn states a quota (`rate_limit_event` `rejected`) and then a
    /// rate limit (the errored `result`, HTTP 429). The exit executes the
    /// QUOTA's `migrate_account` — the account is the fault — not the more
    /// recently stored rate limit's backoff on the dead account. And the
    /// choice does not depend on store order: a quota re-observed after the
    /// rate limit wins the same way.
    #[test]
    fn a_quota_beats_a_later_rate_limit_in_the_same_turn() {
        let key = "fr-test-m1-precedence";
        let restart = Counting::new();
        let sink = Recorder::default();
        let now = Instant::now();
        record_only(
            key,
            Lane::Structured,
            provider(),
            &structured_frame(Some("rejected"), None),
        )
        .unwrap();
        record_only(
            key,
            Lane::Structured,
            provider(),
            &structured_frame(None, Some(429)),
        )
        .unwrap();
        assert_eq!(active(key).len(), 2);
        let slept = Mutex::new(Vec::new());
        let decided = report_structured(
            &sink,
            key,
            &restart,
            FailureSignal::Exit { code: Some(1) },
            now,
            &|d| slept.lock().unwrap().push(d),
        )
        .unwrap();
        assert_eq!(decided.kind, FailureKind::QuotaExhausted);
        assert_eq!(decided.recovery_policy, RecoveryPolicy::MigrateAccount);
        assert_eq!(restart.calls(), vec![true], "migrated, with a rotation");
        assert!(
            slept.lock().unwrap().is_empty(),
            "no backoff on a dead account"
        );
        remove_where(key, |_| true);
    }

    /// Minor: a typed failure observed long before the exit (the session went
    /// on past it with no successful turn to clear it) does not dictate what a
    /// later, unrelated exit does — the exit is its own observation.
    #[test]
    fn a_stale_typed_failure_does_not_capture_a_later_exit() {
        let key = "fr-test-stale-fold";
        let restart = Counting::new();
        let sink = Recorder::default();
        let then = Instant::now();
        upsert_at(
            key,
            Lane::Structured,
            classify(
                &structured_frame(Some("rejected"), None),
                qontinui_runner_lib::cli_profile::profile_for(provider()).unwrap(),
            )
            .unwrap(),
            then,
        );
        let later = then + EXIT_FOLD_WINDOW + Duration::from_secs(1);
        let decided = report_structured(
            &sink,
            key,
            &restart,
            FailureSignal::Exit { code: Some(1) },
            later,
            &no_sleep,
        )
        .unwrap();
        assert_eq!(decided.kind, FailureKind::ProcessExited);
        assert_eq!(restart.calls(), vec![false], "resumed, not migrated");
        remove_where(key, |_| true);
    }

    /// M2: a backoff restart re-checks, after its wait, that the session it
    /// would restart is still the one that failed. The operator closed it
    /// during the wait: nothing is restarted, and the failure stays recorded,
    /// re-stated as manual.
    #[test]
    fn a_session_closed_during_the_backoff_is_not_resurrected() {
        let key = "fr-test-m2-closed";
        let restart = Counting::new();
        let sink = Recorder::default();
        let closer = restart.clone();
        let decided = report_structured(
            &sink,
            key,
            &restart,
            FailureSignal::Stderr("API error (529): overloaded".into()),
            Instant::now(),
            // The operator closes the session while the runner waits.
            &move |_| *closer.wanted.lock().unwrap() = false,
        )
        .unwrap();
        assert_eq!(decided.recovery_policy, RecoveryPolicy::BackoffThenRetry);
        assert!(
            restart.calls().is_empty(),
            "a closed session is not restarted"
        );
        let left = active(key);
        assert_eq!(left.len(), 1, "the failure stays recorded");
        assert_eq!(left[0].recovery_policy, RecoveryPolicy::Never);
        assert!(left[0]
            .details
            .as_deref()
            .unwrap_or_default()
            .contains("closed"));
        remove_where(key, |_| true);
    }

    /// M3 + U3: every structured restart policy shares one per-session cap.
    /// Past it the failure stays active, announced as manual — never as the
    /// automatic action that is not coming.
    #[test]
    fn structured_restarts_share_one_bounded_cap() {
        let key = "fr-test-m3-cap";
        let restart = Counting::new();
        let sink = Recorder::default();
        let now = Instant::now();
        let quota = || structured_frame(Some("rejected"), None);
        for _ in 0..STRUCTURED_RESTART_CAP {
            record_only(key, Lane::Structured, provider(), &quota()).unwrap();
            report_structured(
                &sink,
                key,
                &restart,
                FailureSignal::Exit { code: Some(1) },
                now,
                &no_sleep,
            )
            .unwrap();
        }
        assert_eq!(restart.calls(), vec![true; STRUCTURED_RESTART_CAP]);
        sink.notices();

        record_only(key, Lane::Structured, provider(), &quota()).unwrap();
        let capped = report_structured(
            &sink,
            key,
            &restart,
            FailureSignal::Exit { code: Some(1) },
            now,
            &no_sleep,
        )
        .unwrap();
        assert_eq!(
            restart.calls().len(),
            STRUCTURED_RESTART_CAP,
            "no restart past the cap"
        );
        assert_eq!(capped.recovery_policy, RecoveryPolicy::Never);
        assert!(capped.details.as_deref().unwrap().contains("cap"));
        assert_eq!(active(key).len(), 1, "still active and visible");
        let announced = sink.notices();
        assert!(
            announced
                .iter()
                .all(|n| n.failure.recovery_policy == RecoveryPolicy::Never),
            "the capped failure is never announced as an automatic migration: {announced:?}"
        );

        // A different policy (a crash resume) is under the SAME cap.
        clear_on_evidence_with(&Recorder::default(), key, Evidence::TurnSucceeded);
        report_structured(
            &sink,
            key,
            &restart,
            FailureSignal::Exit { code: Some(1) },
            now,
            &no_sleep,
        )
        .unwrap();
        assert_eq!(restart.calls().len(), STRUCTURED_RESTART_CAP);
        remove_where(key, |_| true);
    }

    /// The structured backoff doubles within the window.
    #[test]
    fn structured_backoff_doubles() {
        assert_eq!(backoff_delay(0), Duration::from_secs(30));
        assert_eq!(backoff_delay(1), Duration::from_secs(60));
        assert_eq!(backoff_delay(2), Duration::from_secs(120));

        let t0 = Instant::now();
        let key = "fr-test-backoff";
        assert_eq!(take_restart_slot(key, t0), Some(0));
        assert_eq!(take_restart_slot(key, t0), Some(1));
        assert_eq!(take_restart_slot(key, t0), Some(2));
        assert_eq!(take_restart_slot(key, t0), None);
        assert_eq!(
            take_restart_slot(key, t0 + STRUCTURED_RESTART_WINDOW + Duration::from_secs(1)),
            Some(0)
        );
    }

    /// Minor: the restart history keeps only keys restarted within the
    /// window — it does not grow with every session ever restarted.
    #[test]
    fn restart_history_is_pruned_by_window() {
        let history: Mutex<Option<HashMap<String, Vec<Instant>>>> = Mutex::new(None);
        let t0 = Instant::now();
        let window = Duration::from_secs(10);
        assert_eq!(take_slot(&history, "a", t0, 3, window), Some(0));
        assert_eq!(take_slot(&history, "b", t0, 3, window), Some(0));
        take_slot(
            &history,
            "c",
            t0 + window + Duration::from_secs(1),
            3,
            window,
        );
        let keys: Vec<String> = history
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys, vec!["c".to_string()]);
    }

    /// Minor: crash resumes and quota migrations are separate budgets.
    /// Spending every crash resume leaves the migration cap untouched.
    #[test]
    fn crash_resumes_do_not_spend_the_migration_cap() {
        let session = "fr-test-separate-budgets";
        let now = Instant::now();
        for _ in 0..RESUME_CAP {
            assert!(take_resume_slot(session, now));
        }
        assert!(!take_resume_slot(session, now), "the resume cap is spent");
        assert!(
            crate::terminal::account_migration::migration_cap_permits(
                session,
                chrono::Utc::now().timestamp_millis()
            ),
            "the migration cap is its own"
        );
    }

    /// Structured arms with no mechanism do not restart; a handoff on this
    /// lane is stated as manual.
    #[test]
    fn structured_arms_that_do_not_restart() {
        let restart = Counting::new();
        let target = || structured("fr-test-arms", restart.clone());
        let failure = |policy| {
            let mut f = classify(
                &FailureSignal::Exit { code: Some(1) },
                qontinui_runner_lib::cli_profile::profile_for(provider()).unwrap(),
            )
            .unwrap();
            f.recovery_policy = policy;
            f
        };
        let now = Instant::now();
        for policy in [
            RecoveryPolicy::Never,
            RecoveryPolicy::LoginThenResume,
            RecoveryPolicy::WaitUntilReset,
        ] {
            assert!(matches!(
                plan_for(&target(), &failure(policy), now),
                Plan::Nothing
            ));
        }
        assert!(matches!(
            plan_for(&target(), &failure(RecoveryPolicy::HandoffNewSession), now),
            Plan::Declined(_)
        ));
        assert!(matches!(
            plan_for(&target(), &failure(RecoveryPolicy::MigrateAccount), now),
            Plan::StructuredRestart { rotate: true, delay } if delay.is_zero()
        ));
        assert!(matches!(
            plan_for(&target(), &failure(RecoveryPolicy::ResumeSameId), now),
            Plan::StructuredRestart { rotate: false, delay } if delay.is_zero()
        ));
    }

    /// Single source: the caller's hook sees the failure exactly as recorded
    /// and announced — the folded typed failure, with the decided policy —
    /// never its own re-classification of the exit signal.
    #[test]
    fn the_caller_hook_sees_what_report_decided() {
        let key = "fr-test-single-source";
        let restart = Counting::new();
        let sink = Recorder::default();
        let recorded = record_only(
            key,
            Lane::Structured,
            provider(),
            &structured_frame(Some("rejected"), None),
        )
        .unwrap();
        let seen = Mutex::new(None);
        report_with(
            &sink,
            structured(key, restart.clone()),
            provider(),
            None,
            FailureSignal::Stderr("API error (529): overloaded".into()),
            Instant::now(),
            &no_sleep,
            |f| *seen.lock().unwrap() = Some(f.clone()),
        );
        let seen = seen.lock().unwrap().clone().unwrap();
        assert_eq!(seen.id, recorded.id);
        assert_eq!(seen.kind, FailureKind::QuotaExhausted);
        let announced = sink.notices();
        assert_eq!(announced[0].failure, seen, "hook and notice agree");
        remove_where(key, |_| true);
    }

    /// Record → announce; a repeat of the same kind updates the same id; the
    /// right evidence clears it and announces the clear; wrong evidence does
    /// nothing.
    #[test]
    fn record_upsert_and_clear_by_evidence() {
        let sink = Recorder::default();
        let key = "fr-test-term-1";
        let target = pty(key);
        let first = record_and_announce(
            &sink,
            &target,
            provider(),
            Some("/acct".into()),
            &grid("usage limit reached"),
        )
        .unwrap();
        assert_eq!(first.kind, FailureKind::QuotaExhausted);
        assert_eq!(first.account.as_deref(), Some("/acct"));
        let again = record_and_announce(
            &sink,
            &target,
            provider(),
            None,
            &grid("usage limit reached"),
        )
        .unwrap();
        assert_eq!(again.id, first.id, "a repeat keeps the row's id");
        assert_eq!(active(key).len(), 1);
        assert_eq!(
            sink.take(),
            vec![
                (FailureKind::QuotaExhausted, true),
                (FailureKind::QuotaExhausted, true)
            ]
        );

        // A different kind stacks beside it.
        record_and_announce(
            &sink,
            &target,
            provider(),
            None,
            &FailureSignal::HandshakeTimeout,
        )
        .unwrap();
        assert_eq!(active(key).len(), 2);
        sink.take();

        clear_on_evidence_with(&sink, key, Evidence::HandshakeVerified);
        assert_eq!(sink.take(), vec![(FailureKind::ResumeFailed, false)]);
        assert_eq!(active(key).len(), 1);

        // The probe confirms: re-worded, re-announced, still active.
        confirm(&sink, key, FailureKind::QuotaExhausted);
        let confirmed = active(key);
        assert_eq!(
            confirmed[0].evidence.confidence,
            FailureConfidence::Confirmed
        );
        assert_eq!(confirmed[0].title, "Usage quota exhausted");
        assert_eq!(sink.take(), vec![(FailureKind::QuotaExhausted, true)]);

        clear_on_evidence_with(&sink, key, Evidence::SessionReplaced);
        assert!(active(key).is_empty());
        assert_eq!(sink.take(), vec![(FailureKind::QuotaExhausted, false)]);
    }

    /// U2: a replacement pane does not clear the failure it took over when it
    /// SPAWNS — the failure moves to it and waits. Only that pane's session
    /// confirming (its `SessionStart` hook) clears it; a confirmation on a
    /// pane that is no replacement clears nothing.
    #[test]
    fn a_replacement_clears_the_failure_only_once_its_session_confirms() {
        let sink = Recorder::default();
        let old = "fr-test-u2-old";
        let new = "fr-test-u2-new";
        let f = record_and_announce(
            &sink,
            &pty(old),
            provider(),
            None,
            &FailureSignal::Exit { code: Some(1) },
        )
        .unwrap();
        sink.take();

        hand_over(&sink, old, new);
        assert!(active(old).is_empty());
        let moved = active(new);
        assert_eq!(moved.len(), 1, "still active after the spawn");
        assert_eq!(moved[0].id, f.id);
        assert_eq!(
            sink.take(),
            vec![
                (FailureKind::ProcessExited, false),
                (FailureKind::ProcessExited, true)
            ]
        );

        // A failure observed on the new pane itself is not the replacement's
        // to clear.
        record_and_announce(
            &sink,
            &pty(new),
            provider(),
            None,
            &grid("usage limit reached"),
        )
        .unwrap();
        sink.take();
        on_pty_session_confirmed_with(&sink, new);
        assert_eq!(sink.take(), vec![(FailureKind::ProcessExited, false)]);
        assert_eq!(
            active(new).iter().map(|f| f.kind).collect::<Vec<_>>(),
            vec![FailureKind::QuotaExhausted]
        );

        // A pane that is no replacement keeps its failures on a confirm.
        on_pty_session_confirmed_with(&sink, new);
        assert_eq!(active(new).len(), 1);
        remove_where(new, |_| true);
    }

    /// A replacement whose SessionStart confirmed BEFORE the hand-over (the
    /// spawn returned after the CLI was up) clears the failures it takes over
    /// at once — no second confirm is coming to clear them.
    #[test]
    fn a_replacement_confirmed_before_the_hand_over_clears_at_once() {
        let sink = Recorder::default();
        let old = "fr-test-early-old";
        let new = "fr-test-early-new";
        record_and_announce(
            &sink,
            &pty(old),
            provider(),
            None,
            &FailureSignal::Exit { code: Some(1) },
        )
        .unwrap();
        sink.take();

        on_pty_session_confirmed_with(&sink, new); // early: nothing there yet
        assert!(sink.take().is_empty());
        hand_over(&sink, old, new);
        assert!(active(old).is_empty());
        assert!(
            active(new).is_empty(),
            "not left awaiting a confirm that came already"
        );
        assert_eq!(
            sink.take(),
            vec![
                (FailureKind::ProcessExited, false),
                (FailureKind::ProcessExited, true),
                (FailureKind::ProcessExited, false)
            ]
        );
    }

    /// A signal that reports no failure records and announces nothing.
    #[test]
    fn a_non_failure_records_nothing() {
        let sink = Recorder::default();
        let key = "fr-test-term-2";
        assert!(record_and_announce(
            &sink,
            &pty(key),
            provider(),
            None,
            &FailureSignal::Exit { code: Some(0) }
        )
        .is_none());
        assert!(active(key).is_empty());
        assert!(sink.take().is_empty());
    }

    /// Dismissal and dead-session pruning remove, and only for their lane
    /// and key.
    #[test]
    fn dismiss_and_prune() {
        let sink = Recorder::default();
        let live_key = "fr-prune-live".to_string();
        let dead_key = "fr-prune-dead".to_string();
        let structured_key = "fr-prune-structured".to_string();
        let dead_structured_key = "fr-prune-structured-gone".to_string();
        let f = record_and_announce(
            &sink,
            &pty(&live_key),
            provider(),
            None,
            &FailureSignal::HandshakeTimeout,
        )
        .unwrap();
        record_and_announce(
            &sink,
            &pty(&dead_key),
            provider(),
            None,
            &FailureSignal::HandshakeTimeout,
        )
        .unwrap();
        for key in [&structured_key, &dead_structured_key] {
            record_and_announce(
                &sink,
                &structured(key, Counting::new()),
                provider(),
                None,
                &FailureSignal::Stderr("API error (529): overloaded".into()),
            )
            .unwrap();
        }

        let live: HashSet<&String> = [&live_key].into_iter().collect();
        prune_dead(Lane::Pty, &live, |k| k.starts_with("fr-prune-"));
        assert!(
            active(&dead_key).is_empty(),
            "a closed terminal's failures go with it"
        );
        assert_eq!(active(&live_key).len(), 1);
        assert_eq!(
            active(&structured_key).len(),
            1,
            "structured keys are not terminals"
        );

        // A structured session the SessionManager no longer holds.
        let live: HashSet<&String> = [&structured_key].into_iter().collect();
        prune_dead(Lane::Structured, &live, |k| k.starts_with("fr-prune-"));
        assert!(active(&dead_structured_key).is_empty());
        assert_eq!(active(&structured_key).len(), 1);
        assert_eq!(active(&live_key).len(), 1, "terminals are not sessions");

        assert!(!dismiss(&live_key, "not-an-id"));
        assert!(dismiss(&live_key, &f.id));
        assert!(active(&live_key).is_empty());
        remove_where(&structured_key, |_| true);
    }

    /// C1: a PTY exit is an AI session's failure only when the exiting
    /// process was an AI CLI AND a CONFIRMED session of that provider is open
    /// on the pane. The spawn-time provisional row every pane gets — a plain
    /// shell's included — never is, so a shell's `exit 1` resumes nothing.
    #[test]
    fn a_pty_exit_needs_an_ai_child_and_a_confirmed_session() {
        use crate::session::session_lifecycle_store::SessionLifecycleStore;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        let claude = qontinui_runner_lib::cli_profile::claude::ID;
        let open = |id: &str, term: &str| {
            crate::commands::terminal::record_pinned_session_open(
                &store,
                id.to_string(),
                term.to_string(),
                Some("/acct".to_string()),
                "/w".to_string(),
                "t".to_string(),
                "default".to_string(),
                0,
                claude.to_string(),
            )
        };

        // The identity seam's provisional row on a plain shell pane.
        open("minted-uuid", "shell-pane");
        assert!(
            pty_exit_session(&store, "shell-pane", Some(claude)).is_none(),
            "an unconfirmed spawn-time row is not a session"
        );
        assert!(pty_exit_session(&store, "shell-pane", None).is_none());

        // A real, confirmed claude session on a pane whose child is a shell
        // (`claude`, then its own `/exit`, then the shell's `exit 3`): the
        // PTY exit is the shell's.
        open("real-sess", "shell-with-claude");
        store.confirm_session("real-sess");
        assert!(
            pty_exit_session(&store, "shell-with-claude", None).is_none(),
            "a shell's exit is not the AI session's"
        );

        // A direct-exec claude whose session is confirmed: its exit is.
        open("direct-sess", "direct-pane");
        store.confirm_session("direct-sess");
        assert_eq!(
            pty_exit_session(&store, "direct-pane", Some(claude)).map(|r| r.claude_session_id),
            Some("direct-sess".to_string())
        );
        // ...but not when the confirmed session is another CLI's.
        assert!(pty_exit_session(
            &store,
            "direct-pane",
            Some(qontinui_runner_lib::cli_profile::codex::ID)
        )
        .is_none());
    }

    /// U1: a respawned `--resume` that fails — its screen shows the profile's
    /// resume-failure marker, or it exits within seconds — is `resume_failed`
    /// (never retried), not `process_exited` (resumed again, up to the cap).
    #[test]
    fn a_failed_resume_exit_is_resume_failed_not_a_crash_to_resume() {
        let profile = qontinui_runner_lib::cli_profile::profile_for(provider()).unwrap();
        let exit = |resumed: bool, lifetime: u64, screen: &'static str, code: i32| PtyExit {
            code: Some(code),
            provider: Some(provider()),
            resumed,
            lifetime: Duration::from_secs(lifetime),
            screen,
        };
        let kind_of = |e: &PtyExit<'_>| {
            classify(&pty_exit_signal(Some(profile), e), profile)
                .map(|f| (f.kind, f.recovery_policy))
        };

        let marker =
            "No conversation found with session ID: 0b7e2a3c-1d4f-4a5b-9c8d-7e6f5a4b3c2d\n$ ";
        // The marker on screen, however long it ran.
        assert_eq!(
            kind_of(&exit(true, 300, marker, 1)),
            Some((FailureKind::ResumeFailed, RecoveryPolicy::Never))
        );
        // No marker, but gone within seconds.
        assert_eq!(
            kind_of(&exit(true, 2, "", 1)),
            Some((FailureKind::ResumeFailed, RecoveryPolicy::Never))
        );
        // A resumed session that ran a while and then crashed is a crash.
        assert_eq!(
            kind_of(&exit(true, 300, "", 1)),
            Some((FailureKind::ProcessExited, RecoveryPolicy::ResumeSameId))
        );
        // A fresh launch is never a failed resume, marker or not.
        assert_eq!(
            kind_of(&exit(false, 2, marker, 1)),
            Some((FailureKind::ProcessExited, RecoveryPolicy::ResumeSameId))
        );
        // A clean exit (an operator's own `/exit`) is no failure at all.
        assert_eq!(kind_of(&exit(true, 2, marker, 0)), None);
    }

    /// C2: the PTY resume relaunches the Claude CLI, so it refuses any other
    /// provider's record (and an id unsafe on a command line) instead of
    /// typing `claude --resume <codex id>`.
    #[test]
    fn in_place_resume_refuses_a_session_it_cannot_relaunch() {
        use crate::session::session_lifecycle_store::SessionLifecycleStore;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        let record = |id: &str, provider: &str| {
            crate::commands::terminal::record_pinned_session_open(
                &store,
                id.to_string(),
                format!("term-{id}"),
                Some("/acct".to_string()),
                "/w".to_string(),
                "t".to_string(),
                "default".to_string(),
                0,
                provider.to_string(),
            );
            store.get(id).unwrap()
        };
        let claude = record(
            "0b7e2a3c-1d4f-4a5b-9c8d-7e6f5a4b3c2d",
            qontinui_runner_lib::cli_profile::claude::ID,
        );
        assert_eq!(resume_in_place_supported(&claude), Ok(()));
        let codex = record(
            "01a0ef49-1234-7abc-8def-0123456789ab",
            qontinui_runner_lib::cli_profile::codex::ID,
        );
        let why = resume_in_place_supported(&codex).unwrap_err();
        assert!(why.contains("codex"), "{why}");
        let mut unsafe_id = claude.clone();
        unsafe_id.claude_session_id = "x;rm -rf ~".to_string();
        assert!(resume_in_place_supported(&unsafe_id).is_err());

        // And the decision says so up front: a codex exit is never planned
        // as a resume, even under a policy that asks for one.
        let mut f = classify(
            &FailureSignal::Exit { code: Some(1) },
            qontinui_runner_lib::cli_profile::profile_for(
                qontinui_runner_lib::cli_profile::claude::ID,
            )
            .unwrap(),
        )
        .unwrap();
        f.recovery_policy = RecoveryPolicy::ResumeSameId;
        let target = RecoveryTarget::Pty {
            terminal_id: "term-codex".into(),
            record: Some(Box::new(codex)),
        };
        assert!(matches!(
            plan_for(&target, &f, Instant::now()),
            Plan::Declined(why) if why.contains("codex")
        ));
    }

    /// The notice's wire shape: camelCase envelope around the schemas type.
    #[test]
    fn notice_wire_shape() {
        let profile = qontinui_runner_lib::cli_profile::profile_for(provider()).unwrap();
        let failure = classify(&FailureSignal::HandshakeTimeout, profile).unwrap();
        let v = serde_json::to_value(SessionFailureNotice {
            terminal_id: "t".into(),
            failure,
            active: true,
        })
        .unwrap();
        assert_eq!(v["terminalId"], "t");
        assert_eq!(v["active"], true);
        assert_eq!(v["failure"]["kind"], "resume_failed");
        assert_eq!(v["failure"]["evidence"]["confidence"], "hint");
        assert_eq!(v["failure"]["recoveryPolicy"], "never");
    }
}
