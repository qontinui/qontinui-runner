//! The one place a session failure is recorded, announced and acted on.
//!
//! Plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 7. Producers (the usage-limit grid scan, the stream-json lane's exit
//! path, the resume-handshake verifier, the PTY waiter) no longer act on what
//! they see: each hands a [`FailureSignal`] to [`report`], which
//!
//! 1. classifies it ([`super::failure::classify`]),
//! 2. records it as the session's ACTIVE failure of that kind — readable at
//!    `GET /terminals/{id}/failures` so a headless consumer does not depend on
//!    having been subscribed,
//! 3. announces it on the `session-failure` Tauri event plus the WS
//!    re-broadcast `terminal::exit_notice` uses, and
//! 4. executes the kind's [`RecoveryPolicy`] by calling the mechanism that
//!    already owns it — never a second copy of one:
//!
//! | policy | PTY lane | structured (stream-json) lane |
//! |---|---|---|
//! | `migrate_account` | `terminal::account_migration::handle_usage_limit_hint` — its usage probe confirms (Hint → Confirmed) or refutes (clears) the failure before anything moves | the in-place restart on a rotated account ([`StructuredRestart`]) |
//! | `backoff_then_retry` | none: the CLI's TUI owns its own retry, and the runner holds the account (no rotation) | in-place restart on the SAME account after [`backoff_delay`] |
//! | `handoff_new_session` | `terminal::context_watcher`'s trigger (its flag, threshold and once-per-session debounce apply) | none: the lane has no handoff machinery |
//! | `resume_same_id` | respawn `--resume <id>` on the same account through `account_migration::spawn_resumed_pane` | in-place restart on the same account |
//! | `never` (and the two unproduced policies) | none | none |
//!
//! Everything runs in the runner process. Nothing here touches the dev
//! supervisor (`:9875`): an end user has no supervisor.
//!
//! **A failure clears by its policy's own evidence, never by a timer**
//! ([`Evidence`], [`cleared_by`]): a verified resume handshake, a usage probe
//! that finds the account not exhausted, a working replacement session, or a
//! real successful turn on the structured lane.
//! An operator may also dismiss one explicitly, and a terminal's failures go
//! when the terminal does.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use qontinui_types::cli_session::{
    FailureConfidence, FailureEvidenceSource, FailureKind, RecoveryPolicy, SessionFailure,
};
use serde::Serialize;
use tracing::{info, warn};

use super::failure::{classify_for, title_for, FailureSignal};
use crate::session::session_lifecycle_store::TerminalSessionRecord;

/// The Tauri event (and WS channel) every failure notice travels on.
pub const SESSION_FAILURE_EVENT: &str = "session-failure";

/// Which lane a failure key belongs to. Decides pruning: a PTY key goes when
/// its terminal leaves the `TerminalManager`.
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
    /// Restart the session under its existing id. With `rotate_account` the
    /// runner first moves to another account; without it the account stays.
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
    /// A working replacement took the session over (migrated, resumed or
    /// restarted), so every failure recorded under the old key is over.
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

// ============================================================================
// The active-failure store
// ============================================================================

struct Entry {
    lane: Lane,
    failures: Vec<SessionFailure>,
}

/// Active failures per session key. Const-initializable (the `Option` wrap),
/// like the grid scanners' state maps.
static STORE: Mutex<Option<HashMap<String, Entry>>> = Mutex::new(None);

fn with_store<R>(f: impl FnOnce(&mut HashMap<String, Entry>) -> R) -> R {
    let mut guard = STORE.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(HashMap::new))
}

/// Record `failure` as `key`'s active failure of its kind. A repeat of a kind
/// already active replaces it but keeps the original id, so a surface updates
/// one row instead of stacking duplicates. Returns what was stored.
fn upsert(key: &str, lane: Lane, mut failure: SessionFailure) -> SessionFailure {
    with_store(|store| {
        let entry = store.entry(key.to_string()).or_insert_with(|| Entry {
            lane,
            failures: Vec::new(),
        });
        if let Some(existing) = entry.failures.iter_mut().find(|f| f.kind == failure.kind) {
            failure.id = existing.id.clone();
            *existing = failure.clone();
        } else {
            entry.failures.push(failure.clone());
        }
        failure
    })
}

/// Remove every failure under `key` that `pred` selects; returns them.
fn remove_where(key: &str, pred: impl Fn(&SessionFailure) -> bool) -> Vec<SessionFailure> {
    with_store(|store| {
        let Some(entry) = store.get_mut(key) else {
            return Vec::new();
        };
        let (gone, kept): (Vec<_>, Vec<_>) = entry.failures.drain(..).partition(|f| pred(f));
        entry.failures = kept;
        if entry.failures.is_empty() {
            store.remove(key);
        }
        gone
    })
}

/// The failures currently active under `key` (a terminal id on the PTY lane,
/// an AI session id on the structured lane). Empty when there are none.
pub fn active(key: &str) -> Vec<SessionFailure> {
    with_store(|store| {
        store
            .get(key)
            .map(|e| e.failures.clone())
            .unwrap_or_default()
    })
}

fn announce_cleared(sink: &dyn FailureNoticeSink, key: &str, gone: Vec<SessionFailure>) {
    for failure in gone {
        sink.notify(&SessionFailureNotice {
            terminal_id: key.to_string(),
            failure,
            active: false,
        });
    }
}

/// Clear every failure under `key` that `evidence` ends, and announce each.
pub fn clear_on_evidence(key: &str, evidence: Evidence) {
    clear_on_evidence_with(&TauriFailureSink, key, evidence);
}

fn clear_on_evidence_with(sink: &dyn FailureNoticeSink, key: &str, evidence: Evidence) {
    let gone = remove_where(key, |f| cleared_by(f.kind, evidence));
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
    let gone = remove_where(key, |f| f.id == failure_id);
    let found = !gone.is_empty();
    announce_cleared(&TauriFailureSink, key, gone);
    found
}

/// Drop the failures of PTY terminals that are no longer live — the session a
/// failure described is gone. Called from the grid-scan tick, which already
/// holds the live set.
pub fn retain_live_terminals(live: &HashSet<&String>) {
    prune_dead_terminals(live, |_| true);
}

/// [`retain_live_terminals`] over the keys `in_scope` selects — the whole
/// store in production; one test's own keys under the parallel test runner.
fn prune_dead_terminals(live: &HashSet<&String>, in_scope: impl Fn(&str) -> bool) {
    let gone: Vec<(String, Vec<SessionFailure>)> = with_store(|store| {
        let dead: Vec<String> = store
            .iter()
            .filter(|(k, e)| e.lane == Lane::Pty && in_scope(k) && !live.contains(k))
            .map(|(k, _)| k.clone())
            .collect();
        dead.into_iter()
            .filter_map(|k| store.remove(&k).map(|e| (k, e.failures)))
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
        let failure = store
            .get_mut(key)?
            .failures
            .iter_mut()
            .find(|f| f.kind == kind)?;
        failure.evidence.confidence = FailureConfidence::Confirmed;
        failure.title = title_for(kind, FailureConfidence::Confirmed).to_string();
        Some(failure.clone())
    });
    if let Some(failure) = updated {
        sink.notify(&SessionFailureNotice {
            terminal_id: key.to_string(),
            failure,
            active: true,
        });
    }
}

// ============================================================================
// Report: classify → record → announce → execute
// ============================================================================

/// Classify `signal` for a session of `provider`, record and announce the
/// result, and execute its recovery policy. `None` when the signal reports no
/// failure. Structured-lane recovery runs on the calling thread (the lane's
/// waiter thread, which exists for this); PTY-lane recovery is spawned.
pub fn report(
    target: RecoveryTarget,
    provider: &str,
    account: Option<String>,
    signal: FailureSignal,
) -> Option<SessionFailure> {
    let failure = match exit_tail_of_structured_failure(&target, &signal) {
        Some(prior) => {
            info!(
                key = target.key(),
                kind = ?prior.kind,
                "stream-json exit follows a typed failure already reported on the stream — one failure, executing its policy"
            );
            prior
        }
        None => record_and_announce(&TauriFailureSink, &target, provider, account, &signal)?,
    };
    execute(target, &failure);
    Some(failure)
}

/// One failure, not two. On the structured lane an errored turn is stated
/// twice: by a typed frame on the stream (recorded then by
/// `claude_session::dispatcher`, without recovery, since the child is still
/// running) and by the child's exit that follows it — the CLI exits non-zero
/// after an API-errored turn (Phase 2 probe Q3). When the lane's exit
/// signal (its exit status or its stderr) arrives for a session that already
/// has an active failure stated by a structured event, the exit IS that
/// failure's tail: it returns that failure, the most recent if several, so
/// [`report`] executes its policy instead of recording a second, weaker one.
fn exit_tail_of_structured_failure(
    target: &RecoveryTarget,
    signal: &FailureSignal,
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
    active(session_id)
        .into_iter()
        .rev()
        .find(|f| f.evidence.source == FailureEvidenceSource::StructuredEvent)
}

/// The PTY waiter's report: pane `terminal_id`'s child exited with `code`.
///
/// Not a failure when the runner closed the pane itself, when the exit was
/// clean (the classifier's `Exit { code: Some(0) }` is `None`), or when the
/// pane hosted no registered AI session — a plain shell's exit is nobody's
/// failure. The lifecycle row is read HERE, synchronously on the waiter
/// thread, before the exit notice lets the frontend close it.
pub fn report_pty_exit(terminal_id: &str, code: Option<i32>, closed_deliberately: bool) {
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
    let Some(record) = store.find_open_by_terminal(terminal_id) else {
        return;
    };
    let provider = record.provider.clone();
    let account = record.config_dir.clone();
    report(
        RecoveryTarget::Pty {
            terminal_id: terminal_id.to_string(),
            record: Some(Box::new(record)),
        },
        &provider,
        account,
        FailureSignal::Exit { code },
    );
}

/// Steps 1–3 of [`report`], without executing anything. Also the door for a
/// producer whose recovery is someone else's (the frontend's resume
/// verifier owns its own retry).
pub fn record_only(
    key: &str,
    lane: Lane,
    provider: &str,
    signal: &FailureSignal,
) -> Option<SessionFailure> {
    let profile = qontinui_runner_lib::cli_profile::profile_for(provider);
    let failure = classify_for(signal, provider, profile)?;
    let stored = upsert(key, lane, failure);
    TauriFailureSink.notify(&SessionFailureNotice {
        terminal_id: key.to_string(),
        failure: stored.clone(),
        active: true,
    });
    Some(stored)
}

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
    info!(
        key = target.key(),
        kind = ?stored.kind,
        confidence = ?stored.evidence.confidence,
        policy = ?stored.recovery_policy,
        "session failure recorded"
    );
    sink.notify(&SessionFailureNotice {
        terminal_id: target.key().to_string(),
        failure: stored.clone(),
        active: true,
    });
    Some(stored)
}

fn execute(target: RecoveryTarget, failure: &SessionFailure) {
    let policy = failure.recovery_policy;
    match target {
        RecoveryTarget::Pty {
            terminal_id,
            record,
        } => execute_pty(terminal_id, record, failure, policy),
        RecoveryTarget::Structured {
            session_id,
            restart,
        } => execute_structured(&session_id, restart.as_ref(), policy),
    }
}

fn execute_pty(
    terminal_id: String,
    record: Option<Box<TerminalSessionRecord>>,
    failure: &SessionFailure,
    policy: RecoveryPolicy,
) {
    match policy {
        RecoveryPolicy::MigrateAccount => {
            let kind = failure.kind;
            // The matched phrase, for the migration's own log lines.
            let phrase = failure.reason.clone().unwrap_or_default();
            tauri::async_runtime::spawn(async move {
                use crate::terminal::account_migration::{handle_usage_limit_hint, HintOutcome};
                match handle_usage_limit_hint(terminal_id.clone(), phrase).await {
                    HintOutcome::NotConfirmed => {
                        clear_on_evidence(&terminal_id, Evidence::QuotaNotExhausted)
                    }
                    HintOutcome::Confirmed { migrated_to } => {
                        confirm(&TauriFailureSink, &terminal_id, kind);
                        if migrated_to.is_some() {
                            clear_on_evidence(&terminal_id, Evidence::SessionReplaced);
                        }
                    }
                    HintOutcome::NotApplicable(why) => {
                        info!(terminal_id, why, "quota failure left for the operator");
                    }
                }
            });
        }
        RecoveryPolicy::HandoffNewSession => {
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
        }
        RecoveryPolicy::ResumeSameId => {
            let Some(record) = record else {
                info!(
                    terminal_id,
                    "exited pane has no session record — nothing to resume"
                );
                return;
            };
            tauri::async_runtime::spawn(async move {
                match resume_in_place(&record) {
                    Ok(new_terminal_id) => {
                        info!(
                            terminal_id,
                            new_terminal_id, "exited session resumed under the same id"
                        );
                        clear_on_evidence(&terminal_id, Evidence::SessionReplaced);
                    }
                    Err(why) => {
                        warn!(terminal_id, why, "exited session not resumed automatically")
                    }
                }
            });
        }
        // The CLI's TUI retries a rate limit or an overload itself; the
        // runner's whole policy there is to NOT move the account.
        RecoveryPolicy::BackoffThenRetry => info!(
            terminal_id,
            kind = ?failure.kind,
            "transient provider failure on a PTY session — the CLI retries, the account stays"
        ),
        RecoveryPolicy::Never
        | RecoveryPolicy::WaitUntilReset
        | RecoveryPolicy::LoginThenResume => {}
    }
}

fn execute_structured(session_id: &str, restart: &dyn StructuredRestart, policy: RecoveryPolicy) {
    let rotate = match policy {
        RecoveryPolicy::MigrateAccount => true,
        RecoveryPolicy::ResumeSameId => false,
        RecoveryPolicy::BackoffThenRetry => {
            let Some(delay) = take_backoff_slot(session_id, Instant::now()) else {
                warn!(
                    session_id,
                    cap = BACKOFF_CAP,
                    "backoff restarts exhausted for this session — leaving the failure active"
                );
                return;
            };
            info!(
                session_id,
                delay_secs = delay.as_secs(),
                "backing off before an in-place restart"
            );
            std::thread::sleep(delay);
            false
        }
        RecoveryPolicy::HandoffNewSession
        | RecoveryPolicy::Never
        | RecoveryPolicy::WaitUntilReset
        | RecoveryPolicy::LoginThenResume => return,
    };
    match restart.restart(rotate) {
        Ok(()) => clear_on_evidence(session_id, Evidence::SessionReplaced),
        Err(e) => warn!(session_id, error = %e, rotate, "in-place restart after a failure failed"),
    }
}

// ============================================================================
// Bounded backoff (structured lane)
// ============================================================================

/// Most backoff restarts one session gets within [`BACKOFF_WINDOW`].
pub const BACKOFF_CAP: usize = 3;
/// The rolling window [`BACKOFF_CAP`] counts over.
const BACKOFF_WINDOW: Duration = Duration::from_secs(60 * 60);
/// First backoff delay; each further attempt in the window doubles it.
const BACKOFF_BASE: Duration = Duration::from_secs(30);

/// The delay before backoff attempt `attempt` (0-based): 30 s, 60 s, 120 s,
/// then `None` — the cap.
pub fn backoff_delay(attempt: usize) -> Option<Duration> {
    (attempt < BACKOFF_CAP).then(|| BACKOFF_BASE * 2u32.pow(attempt as u32))
}

static BACKOFF_HISTORY: Mutex<Option<HashMap<String, Vec<Instant>>>> = Mutex::new(None);

/// Claim the next backoff slot for `key` at `now`: its delay, or `None` when
/// the window's cap is spent.
fn take_backoff_slot(key: &str, now: Instant) -> Option<Duration> {
    let mut guard = BACKOFF_HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    let history = guard
        .get_or_insert_with(HashMap::new)
        .entry(key.to_string())
        .or_default();
    history.retain(|t| now.duration_since(*t) < BACKOFF_WINDOW);
    let delay = backoff_delay(history.len())?;
    history.push(now);
    Some(delay)
}

// ============================================================================
// resume_same_id on the PTY lane
// ============================================================================

/// Respawn an exited PTY session with `--resume <id>` on the account it ran
/// under, through the shared resume seam
/// ([`crate::terminal::account_migration::spawn_resumed_pane`], the one the
/// account migration and the cross-machine respawn use), then retire the dead
/// pane the way the migration retires its old one.
///
/// Refuses (and leaves the failure active, with its Resume action) when the
/// runner is draining, when the row has no account or working dir, or when
/// the session's migration cap is spent — a respawn is another hop for the
/// same session, and the cap is what stops a crash loop.
fn resume_in_place(record: &TerminalSessionRecord) -> Result<String, String> {
    use tauri::Manager;

    if crate::drain::is_draining() {
        return Err("runner is draining".into());
    }
    let config_dir = record
        .config_dir
        .as_deref()
        .ok_or("session's account is unknown")?;
    let working_dir = record
        .working_dir
        .as_deref()
        .ok_or("session has no working dir")?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    if !crate::terminal::account_migration::migration_cap_permits(&record.claude_session_id, now_ms)
    {
        return Err("session's respawn cap is spent".into());
    }
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
    }

    struct NoRestart;
    impl StructuredRestart for NoRestart {
        fn restart(&self, _rotate: bool) -> Result<(), String> {
            Err("not in a test".into())
        }
    }

    fn pty(key: &str) -> RecoveryTarget {
        RecoveryTarget::Pty {
            terminal_id: key.to_string(),
            record: None,
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
        struct Counting(Mutex<Vec<bool>>);
        impl StructuredRestart for Counting {
            fn restart(&self, rotate: bool) -> Result<(), String> {
                self.0.lock().unwrap().push(rotate);
                Err("not in a test".into())
            }
        }
        let key = "fr-test-structured-dedupe";
        let restart = Arc::new(Counting(Mutex::new(Vec::new())));
        let target = || RecoveryTarget::Structured {
            session_id: key.to_string(),
            restart: restart.clone(),
        };
        // The stream's rate_limit_event said `rejected`: recorded, not executed.
        let frame = record_only(
            key,
            Lane::Structured,
            provider(),
            &FailureSignal::StructuredEvent {
                error_code: None,
                api_status: None,
                rate_limit_status: Some("rejected".into()),
                message: None,
                reset_at: None,
            },
        )
        .unwrap();
        assert_eq!(frame.kind, FailureKind::QuotaExhausted);
        assert!(
            restart.0.lock().unwrap().is_empty(),
            "no recovery while the child runs"
        );

        // The exit, carrying a stderr the classifier would call a rate limit.
        let exit = report(
            target(),
            provider(),
            None,
            FailureSignal::Stderr("API error (429): rate limit".into()),
        )
        .unwrap();
        assert_eq!(exit.id, frame.id, "the exit is the recorded failure");
        assert_eq!(active(key).len(), 1, "one failure, not two");
        // Its policy ran once, at exit: migrate (rotate) for a quota.
        assert_eq!(*restart.0.lock().unwrap(), vec![true]);

        // A non-zero exit with no stderr folds the same way.
        report(
            target(),
            provider(),
            None,
            FailureSignal::Exit { code: Some(1) },
        )
        .unwrap();
        assert_eq!(active(key).len(), 1);

        // A successful turn clears it; a later exit is then its own failure.
        clear_on_evidence_with(&Recorder::default(), key, Evidence::TurnSucceeded);
        assert!(active(key).is_empty());
        let fresh = report(
            target(),
            provider(),
            None,
            FailureSignal::Exit { code: Some(1) },
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
            &FailureSignal::Exit { code: Some(1) }
        )
        .is_none());
        remove_where(pty_key, |_| true);
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

    /// Dismissal and dead-terminal pruning remove, and only for their key.
    #[test]
    fn dismiss_and_prune() {
        let sink = Recorder::default();
        let live_key = "fr-prune-live".to_string();
        let dead_key = "fr-prune-dead".to_string();
        let structured_key = "fr-prune-structured".to_string();
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
        record_and_announce(
            &sink,
            &RecoveryTarget::Structured {
                session_id: structured_key.clone(),
                restart: Arc::new(NoRestart),
            },
            provider(),
            None,
            &FailureSignal::Stderr("API error (529): overloaded".into()),
        )
        .unwrap();

        let live: HashSet<&String> = [&live_key].into_iter().collect();
        prune_dead_terminals(&live, |k| k.starts_with("fr-prune-"));
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

        assert!(!dismiss(&live_key, "not-an-id"));
        assert!(dismiss(&live_key, &f.id));
        assert!(active(&live_key).is_empty());
        remove_where(&structured_key, |_| true);
    }

    /// The structured lane's backoff is bounded: 30 s, 60 s, 120 s in an hour,
    /// then nothing until the window rolls.
    #[test]
    fn structured_backoff_is_bounded() {
        assert_eq!(backoff_delay(0), Some(Duration::from_secs(30)));
        assert_eq!(backoff_delay(1), Some(Duration::from_secs(60)));
        assert_eq!(backoff_delay(2), Some(Duration::from_secs(120)));
        assert_eq!(backoff_delay(3), None);

        let t0 = Instant::now();
        let key = "fr-test-backoff";
        assert_eq!(take_backoff_slot(key, t0), Some(Duration::from_secs(30)));
        assert_eq!(take_backoff_slot(key, t0), Some(Duration::from_secs(60)));
        assert_eq!(take_backoff_slot(key, t0), Some(Duration::from_secs(120)));
        assert_eq!(take_backoff_slot(key, t0), None);
        assert_eq!(
            take_backoff_slot(key, t0 + BACKOFF_WINDOW + Duration::from_secs(1)),
            Some(Duration::from_secs(30))
        );
    }

    /// Structured recovery arms: a policy with no structured mechanism does not
    /// restart, and a restart that fails leaves the failure active.
    #[test]
    fn structured_arms_that_do_not_restart() {
        struct Counting(Mutex<Vec<bool>>);
        impl StructuredRestart for Counting {
            fn restart(&self, rotate: bool) -> Result<(), String> {
                self.0.lock().unwrap().push(rotate);
                Err("refused".into())
            }
        }
        let r = Counting(Mutex::new(Vec::new()));
        for policy in [
            RecoveryPolicy::Never,
            RecoveryPolicy::HandoffNewSession,
            RecoveryPolicy::LoginThenResume,
            RecoveryPolicy::WaitUntilReset,
        ] {
            execute_structured("fr-test-arms", &r, policy);
        }
        assert!(r.0.lock().unwrap().is_empty());
        execute_structured("fr-test-arms", &r, RecoveryPolicy::MigrateAccount);
        execute_structured("fr-test-arms", &r, RecoveryPolicy::ResumeSameId);
        assert_eq!(*r.0.lock().unwrap(), vec![true, false]);
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
