//! What the usage-limit hint does with a session that STOPPED at a limit:
//! continue it in place once its account has reset, migrate it while the
//! account is still exhausted, or park it until it can be one of those (plan
//! `2026-11-05-runner-terminal-pages-collide-crowd-and-overfill-and-limit-stalled-sessions-never-resume`,
//! Phase 2, design decision D5).
//!
//! Before this, [`super::account_migration::handle_usage_limit_hint`] read a
//! probe saying "this account is no longer exhausted" as "the limit message
//! was echoed text" and returned with nothing scheduled. It could not tell a
//! quoted banner from a real stop whose reset had passed: after the 2026-11-04
//! restart 95 hints were dropped as echoed text, 53 of them on paktis after its
//! reset, and those sessions stayed idle. The transcript says which it is
//! ([`super::limit_stop::transcript_limit_stop`]), and [`decide`] combines that
//! read with a fresh probe and the clock.
//!
//! Everything here that decides is pure ([`decide`], [`park_due_at`],
//! [`ContinueLedger`], [`shows_rate_limit_menu`], [`parked_event`]); the handler
//! in `account_migration` is the wiring. [`run_continue`] takes the idle
//! watcher as a parameter so its dedup is testable without a terminal.
//!
//! Interim arm: plan `2026-10-28-one-session-lifecycle-authority` Phase 99 (the
//! Migrator) is to carry this decision, with the transcript classifier, into
//! its `decide()`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;

use serde_json::{json, Value};

use super::account_migration::IdlePromptOutcome;
use super::limit_stop::{LimitStop, LimitStopRead};

/// How long after an account's `resetsAt` a parked stop is handed back. The
/// probe and the CLI's own clock need not agree to the second.
pub(crate) const LIMIT_RESET_MARGIN_MS: i64 = 120_000;

/// Continue attempts per stop (one `entry_uuid`) before the runner stops
/// trying. An attempt is spent only when the idle watcher ends without the
/// prompt in the pane (it gave up, the pane went away, or the write failed).
pub(crate) const MAX_CONTINUE_ATTEMPTS: u32 = 3;

/// The park of a stop that is still exhausted but records no `resetsAt`: when
/// to look again. The same hour the account cooldown uses when the probe
/// carries no reset time.
pub(crate) const PARK_WITHOUT_RESET_MS: i64 = 60 * 60 * 1000;

/// How long after a migration its own continue nudge can still be in flight:
/// the nudge waits up to `NUDGE_SYNC_WAIT` (45 s) for the delta sync, then up
/// to 180 s for an idle prompt; one more minute of slack. Inside it, the
/// carried stop is left to that nudge; after it, a stop the transcript still
/// ends at was not answered, and is continued like any other.
pub(crate) const MIGRATION_NUDGE_WINDOW_MS: i64 = 300_000;

/// Typed into a session that stopped at its account's usage limit, once the
/// limit has reset. Same shape as the migration's continue nudge; here the
/// session stays on its own account, so its subagents' transcripts are where
/// they were.
pub(crate) const LIMIT_RESET_NUDGE: &str = "This session stopped when its Claude account hit a \
usage limit, and it can run again now: the limit has reset, or the session was moved to an account \
with capacity. Continue the task you were working on from where it left off; if you were idle \
awaiting operator input, say so and wait. Subagents interrupted by the limit should still have \
their transcripts on this account: resume each one with SendMessage to its agent id instead of \
spawning a replacement (cancel any resume gate you booked for it first), and only if SendMessage \
reports \"No transcript found\" fall back to its checkpoint or worktree.";

/// The event a park emits. Not `session-account-migration-skipped`: that one
/// is shown as an ERROR toast reading "out of tokens … NOT migrated", which is
/// false for a stop whose account has already reset.
pub(crate) const PARKED_EVENT: &str = "session-limit-parked";

/// The event a submitted reset-continue emits.
pub(crate) const CONTINUED_EVENT: &str = "session-limit-reset-continued";

/// The event a stop emits when its last continue attempt did not reach the
/// pane: nothing more is tried, so the operator is told.
pub(crate) const CONTINUE_GAVE_UP_EVENT: &str = "session-limit-reset-continue-gave-up";

/// What a fresh usage probe said about the session's account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProbeVerdict {
    /// The probe returned no clean row for the account (none at all, or one
    /// carrying an `error`). The cached snapshot reads an errored row as
    /// exhausted (`probe_result_exhausted`), so for a STOP whose reset has
    /// passed this is UNKNOWN.
    pub unknown: bool,
    /// Whether the account reads exhausted after the probe: the verdict the
    /// handler has always migrated on.
    pub known_exhausted: bool,
    /// When the probe says the exhausted window resets, in milliseconds since
    /// the epoch ([`exhausted_window_reset_ms`]). A stop whose own reset has
    /// passed on an account exhausted again (another window, other sessions)
    /// is parked until THIS time, not one margin from now.
    pub resets_at_ms: Option<i64>,
}

impl ProbeVerdict {
    /// The verdict from the account's own probe row (`None`: no row).
    pub(crate) fn from_row(
        row: Option<&crate::commands::ai_settings::AccountUsageInfo>,
        known_exhausted: bool,
    ) -> Self {
        Self {
            unknown: row.is_none_or(|r| r.error.is_some()),
            known_exhausted,
            resets_at_ms: row
                .filter(|r| r.error.is_none())
                .and_then(exhausted_window_reset_ms),
        }
    }
}

/// The reset of the window a probe row reads as exhausted: the weekly window
/// when it is at the exhaustion threshold, the five-hour session window when
/// only that one is, and the EARLIER of the two when neither number is (a
/// row exhausted by its status alone): parking too early costs one re-probe,
/// parking too late costs a whole window. Seconds in the row, milliseconds
/// out.
pub(crate) fn exhausted_window_reset_ms(
    row: &crate::commands::ai_settings::AccountUsageInfo,
) -> Option<i64> {
    let threshold = crate::commands::ai_settings::EXHAUSTION_UTILIZATION;
    let weekly_full = row.utilization >= threshold;
    let session_full = row.session_utilization.is_some_and(|u| u >= threshold);
    let secs = match (weekly_full, session_full) {
        (true, _) => row.resets_at,
        (false, true) => row.session_resets_at,
        (false, false) => match (row.resets_at, row.session_resets_at) {
            (Some(w), Some(s)) => Some(w.min(s)),
            (w, s) => w.or(s),
        },
    };
    secs.and_then(|s| i64::try_from(s).ok())
        .and_then(|s| s.checked_mul(1000))
}

/// Whether the hint needs the roster probe at all. A stop always does (the
/// continue and the migration both read it); any other read only when a
/// migration could follow, since every other outcome for it is a skip.
pub(crate) fn roster_probe_needed(
    read: &LimitStopRead,
    auto_migrate: bool,
    config_dir_count: usize,
) -> bool {
    matches!(read, LimitStopRead::Stopped(_)) || (auto_migrate && config_dir_count >= 2)
}

/// How long the `n`th consecutive probe-unknown park of one stop waits:
/// one margin, doubling, at most [`PARK_WITHOUT_RESET_MS`]. A probe that keeps
/// failing (a signed-out account) is re-tried less and less often.
pub(crate) fn unknown_park_backoff_ms(n: u32) -> i64 {
    let doublings = n.saturating_sub(1).min(16);
    (LIMIT_RESET_MARGIN_MS << doublings).min(PARK_WITHOUT_RESET_MS)
}

/// Which probe the handler reads for the session's account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeSource {
    /// The roster probe's row for the account.
    Roster,
    /// Probe the session's own account: the roster returned no row for it
    /// (below two accounts it returns none at all), and a stop whose continue
    /// is on needs the verdict.
    OwnAccount,
    /// No row, and nothing would read one: decide on the cached verdict.
    NoRow,
}

/// Pure: where the handler's probe verdict comes from.
pub(crate) fn probe_source(
    roster_has_row: bool,
    read: &LimitStopRead,
    auto_continue: bool,
) -> ProbeSource {
    if roster_has_row {
        ProbeSource::Roster
    } else if auto_continue && matches!(read, LimitStopRead::Stopped(_)) {
        ProbeSource::OwnAccount
    } else {
        ProbeSource::NoRow
    }
}

/// `config_dir`'s row in one probe's results. The roster probe returns no
/// rows at all on a runner with fewer than two accounts, which is why the
/// handler then probes the session's own account instead.
pub(crate) fn row_for<'a>(
    usage: &'a [crate::commands::ai_settings::AccountUsageInfo],
    config_dir: &str,
) -> Option<&'a crate::commands::ai_settings::AccountUsageInfo> {
    usage.iter().find(|i| i.config_dir == config_dir)
}

/// Whether a hint is worth reading a transcript and probing for at all: not
/// when both the migration and the reset-continue are switched off.
pub(crate) fn hint_wanted(auto_migrate: bool, auto_continue: bool) -> bool {
    auto_migrate || auto_continue
}

/// Everything [`decide`] reads.
#[derive(Debug, Clone)]
pub(crate) struct HintInputs {
    /// The session's lifecycle record has `finished_at` set.
    pub finished: bool,
    pub read: LimitStopRead,
    pub probe: ProbeVerdict,
    pub now_ms: i64,
    /// `claude_cli.auto_migrate_on_token_exhaustion`.
    pub auto_migrate: bool,
    /// `claude_cli.auto_continue_after_limit_reset`.
    pub auto_continue: bool,
    /// `claude_cli.auto_continue_after_migration`: whether a migration types
    /// its own continue nudge. Off, a stop the migration carried is left
    /// alone, as the operator chose.
    pub continue_after_migration: bool,
    /// How many Claude config dirs (accounts) are configured.
    pub config_dir_count: usize,
    /// When this session's last migration in this process left it on another
    /// account (`account_migration::note_migrated`, stamped after the
    /// respawn), if one did. A migration carries the transcript, so the copy
    /// still ends at the stop that caused it, with the OLD account's reset.
    pub migrated_at_ms: Option<i64>,
}

/// Why a stop was parked. The string is the event's `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParkReason {
    /// The probe could not say whether the account is exhausted.
    ProbeUnknown,
    /// The probe says not exhausted but the stop's `resetsAt` is still ahead.
    NotYetReset,
    /// Still exhausted and auto-migration is off.
    AutoMigrateOff,
    /// Still exhausted and there is no other account to move to.
    SingleAccount,
    /// Still exhausted and the session's migration cap is spent.
    MigrationCapReached,
    /// Still exhausted and every other account is exhausted, cooled or
    /// signed out.
    NoUsableTarget,
    /// A continue attempt ended without the prompt in the pane.
    ContinueRetry,
    /// A stop carried by a migration whose own continue nudge may still be
    /// running: looked at again once that nudge cannot be.
    MigrationNudgeWait,
}

impl ParkReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ProbeUnknown => "probe-unknown",
            Self::NotYetReset => "not-yet-reset",
            Self::AutoMigrateOff => "auto-migrate-off",
            Self::SingleAccount => "single-account",
            Self::MigrationCapReached => "migration-cap-reached",
            Self::NoUsableTarget => "no-usable-target",
            Self::ContinueRetry => "continue-retry",
            Self::MigrationNudgeWait => "migration-nudge-wait",
        }
    }
}

/// Why the hint does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkipReason {
    /// The session was marked finished: never continue it.
    Finished,
    /// Not a stop, and the account is fine: the limit text was echoed.
    Echoed,
    /// The transcript could not be read, and the account is fine. UNKNOWN,
    /// so nothing is typed.
    TranscriptUnreadable,
    /// A stop that only a reset-continue could resolve (its account has
    /// reset, or will before anything else can move it), and the operator
    /// turned the reset-continue off.
    ContinueDisabled,
    /// Not a stop, the account is exhausted, and auto-migration is off.
    MigrateDisabled,
    /// Not a stop, the account is exhausted, and there is no other account.
    NoOtherAccount,
    /// The stop came before this session's latest migration, onto an account
    /// that is fine, and `auto_continue_after_migration` is off: the operator
    /// chose that a migrated session is not prompted.
    MigratedSinceStop,
    /// Both the migration and the reset-continue are switched off.
    Disabled,
}

/// What the hint does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LimitAction {
    Skip(SkipReason),
    /// Continue the session in place: its account has reset.
    Continue(LimitStop),
    /// Park the session until `due_at_ms`.
    Park {
        due_at_ms: i64,
        reason: ParkReason,
    },
    /// The existing migration. `park_fallback` is the stop's park due time,
    /// used when the cap or the target pick refuses; `None` (not a stop)
    /// keeps the old report-and-return.
    Migrate {
        park_fallback: Option<i64>,
    },
}

/// When a parked stop is handed back: `resetsAt` plus the margin, never
/// sooner than one margin from now (a hot re-entry loop helps no one), and
/// [`PARK_WITHOUT_RESET_MS`] from now when the stop records no reset time.
pub(crate) fn park_due_at(stop: &LimitStop, now_ms: i64) -> i64 {
    let floor = now_ms + LIMIT_RESET_MARGIN_MS;
    match stop.resets_at_ms {
        Some(resets) => (resets + LIMIT_RESET_MARGIN_MS).max(floor),
        None => now_ms + PARK_WITHOUT_RESET_MS,
    }
}

/// When a stop on an account that is (still, or again) exhausted is handed
/// back: the later of the stop's own reset and the probe's exhausted-window
/// reset, plus the margin, never sooner than one margin from now, and
/// [`PARK_WITHOUT_RESET_MS`] from now when neither is known.
pub(crate) fn exhausted_park_due_at(stop: &LimitStop, probe: &ProbeVerdict, now_ms: i64) -> i64 {
    let latest = match (stop.resets_at_ms, probe.resets_at_ms) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    match latest {
        Some(resets) => (resets + LIMIT_RESET_MARGIN_MS).max(now_ms + LIMIT_RESET_MARGIN_MS),
        None => now_ms + PARK_WITHOUT_RESET_MS,
    }
}

/// When a carried stop on an exhausted destination is handed back: the
/// probe's own reset plus the margin (never sooner than one margin from now),
/// or [`PARK_WITHOUT_RESET_MS`] from now when the probe gives none.
pub(crate) fn probe_park_due_at(probe: &ProbeVerdict, now_ms: i64) -> i64 {
    probe
        .resets_at_ms
        .map_or(now_ms + PARK_WITHOUT_RESET_MS, |r| {
            (r + LIMIT_RESET_MARGIN_MS).max(now_ms + LIMIT_RESET_MARGIN_MS)
        })
}

/// The branch table (plan Phase 2), top to bottom.
pub(crate) fn decide(i: &HintInputs) -> LimitAction {
    if i.finished {
        return LimitAction::Skip(SkipReason::Finished);
    }
    match &i.read {
        LimitStopRead::Stopped(stop) => {
            if let Some(migrated_at) = migrated_since(stop, i.migrated_at_ms) {
                // The stop was hit on the OLD account. A destination that is
                // itself dry still takes the next hop, as it always has. On a
                // fine one the migration's nudge answers it; once that nudge
                // can no longer be running, a transcript still ending at the
                // stop says it never landed, and the old account's reset time
                // is not this account's.
                return if i.probe.unknown {
                    // Never migrate, and never continue, on an unknown probe.
                    retry_or_skip(
                        i,
                        LimitAction::Park {
                            due_at_ms: i.now_ms + LIMIT_RESET_MARGIN_MS,
                            reason: ParkReason::ProbeUnknown,
                        },
                    )
                } else if i.probe.known_exhausted {
                    // Parked, if the hop is refused, until THIS account's
                    // reset: the stop's own reset is the old account's.
                    migrate_or_park(i, Some(probe_park_due_at(&i.probe, i.now_ms)))
                } else if !i.continue_after_migration {
                    LimitAction::Skip(SkipReason::MigratedSinceStop)
                } else if i.now_ms - migrated_at <= MIGRATION_NUDGE_WINDOW_MS {
                    // Looked at again once the nudge cannot be running: a
                    // landed nudge then reads NotStopped, a lost one is
                    // continued.
                    continue_or_skip(
                        i,
                        LimitAction::Park {
                            due_at_ms: migrated_at
                                + MIGRATION_NUDGE_WINDOW_MS
                                + LIMIT_RESET_MARGIN_MS,
                            reason: ParkReason::MigrationNudgeWait,
                        },
                    )
                } else {
                    continue_or_skip(i, LimitAction::Continue(stop.clone()))
                };
            }
            let reset_passed = stop.resets_at_ms.is_none_or(|r| r <= i.now_ms);
            if i.probe.unknown && reset_passed {
                // Never migrate, and never continue, on an unknown probe.
                return retry_or_skip(
                    i,
                    LimitAction::Park {
                        due_at_ms: i.now_ms + LIMIT_RESET_MARGIN_MS,
                        reason: ParkReason::ProbeUnknown,
                    },
                );
            }
            if i.probe.known_exhausted || i.probe.unknown {
                // An unknown probe beside a reset still ahead: the stop itself
                // says the account is limited, as an errored probe row does.
                return migrate_or_park(i, Some(exhausted_park_due_at(stop, &i.probe, i.now_ms)));
            }
            if !reset_passed {
                // The probe and the stop disagree: wait for the stop's reset.
                return continue_or_skip(
                    i,
                    LimitAction::Park {
                        due_at_ms: park_due_at(stop, i.now_ms),
                        reason: ParkReason::NotYetReset,
                    },
                );
            }
            continue_or_skip(i, LimitAction::Continue(stop.clone()))
        }
        LimitStopRead::NotStopped if i.probe.known_exhausted => migrate_or_park(i, None),
        LimitStopRead::NotStopped => LimitAction::Skip(SkipReason::Echoed),
        LimitStopRead::Unreadable(_) if i.probe.known_exhausted => migrate_or_park(i, None),
        LimitStopRead::Unreadable(_) => LimitAction::Skip(SkipReason::TranscriptUnreadable),
    }
}

/// The stop was hit before this session's latest migration. A stop with no
/// timestamp beside a recorded migration is read the same way: the migration
/// path owns that session's continue, and a second prompt would race it.
/// Returns the migration's time when it came after the stop.
fn migrated_since(stop: &LimitStop, migrated_at_ms: Option<i64>) -> Option<i64> {
    migrated_at_ms.filter(|m| stop.stopped_at_ms.is_none_or(|s| s <= *m))
}

/// `action` (a probe-unknown park) when its re-entry can lead anywhere: a
/// continue, or a migration once the probe answers exhausted (an errored probe
/// is often the limit itself).
fn retry_or_skip(i: &HintInputs, action: LimitAction) -> LimitAction {
    if i.auto_continue || (i.auto_migrate && i.config_dir_count >= 2) {
        action
    } else {
        LimitAction::Skip(SkipReason::ContinueDisabled)
    }
}

/// `action` when it can end in a continue, which needs the reset-continue
/// switched on; otherwise nothing.
fn continue_or_skip(i: &HintInputs, action: LimitAction) -> LimitAction {
    if i.auto_continue {
        action
    } else {
        LimitAction::Skip(SkipReason::ContinueDisabled)
    }
}

/// The migration branch's own gates. A stop (`park_due` set) that cannot
/// migrate is parked until its reset, when only a continue can follow; any
/// other read keeps the old return.
fn migrate_or_park(i: &HintInputs, park_due: Option<i64>) -> LimitAction {
    let refusal = if !i.auto_migrate {
        Some((ParkReason::AutoMigrateOff, SkipReason::MigrateDisabled))
    } else if i.config_dir_count < 2 {
        Some((ParkReason::SingleAccount, SkipReason::NoOtherAccount))
    } else {
        None
    };
    match (refusal, park_due) {
        // A refusal by the cap or the target pick parks the stop for at most
        // an hour (`migration_refused`): a re-entry can migrate it then,
        // whether or not the continue is on.
        (None, park_fallback) => LimitAction::Migrate { park_fallback },
        (Some((reason, _)), Some(due_at_ms)) => {
            continue_or_skip(i, LimitAction::Park { due_at_ms, reason })
        }
        (Some((_, skip)), None) => LimitAction::Skip(skip),
    }
}

/// What the migration branch reports when its cap or its target pick refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RefusalReport {
    /// A stop: park it until `due_at_ms` (`session-limit-parked`).
    Park { due_at_ms: i64, reason: ParkReason },
    /// Not a stop: the old `session-account-migration-skipped` report.
    Skipped(&'static str),
}

/// The report for a migration refused by the cap
/// ([`ParkReason::MigrationCapReached`]) or by the target pick
/// ([`ParkReason::NoUsableTarget`]), given the action's `park_fallback`. The
/// park is due no later than [`PARK_WITHOUT_RESET_MS`] from now: another
/// account can free up, or a cap slot age out, well before the source
/// account's own reset, and the re-entry then migrates.
pub(crate) fn migration_refused(
    park_fallback: Option<i64>,
    reason: ParkReason,
    now_ms: i64,
) -> RefusalReport {
    match park_fallback {
        Some(due_at_ms) => RefusalReport::Park {
            due_at_ms: due_at_ms.min(now_ms + PARK_WITHOUT_RESET_MS),
            reason,
        },
        None => RefusalReport::Skipped(reason.as_str()),
    }
}

/// The `session-limit-parked` event for one park.
pub(crate) fn parked_event(
    claude_session_id: &str,
    config_dir: &str,
    due_at_ms: i64,
    reason: ParkReason,
) -> (&'static str, Value) {
    (
        PARKED_EVENT,
        json!({
            "claudeSessionId": claude_session_id,
            "configDir": config_dir,
            "dueAtMs": due_at_ms,
            "reason": reason.as_str(),
        }),
    )
}

/// Park a stopped session and report it: `park` records it on the park list
/// and answers whether it is worth reporting (a re-park for the same reason is
/// not), and `emit` sends [`parked_event`]. Both are parameters so a test sees
/// what a park reports, and that it is never
/// `session-account-migration-skipped`.
pub(crate) fn report_park(
    terminal_id: &str,
    claude_session_id: &str,
    config_dir: &str,
    due_at_ms: i64,
    reason: ParkReason,
    park: impl FnOnce(&str, &str, i64, &'static str) -> bool,
    emit: impl FnOnce(&'static str, Value),
) {
    if park(terminal_id, claude_session_id, due_at_ms, reason.as_str()) {
        let (event, payload) = parked_event(claude_session_id, config_dir, due_at_ms, reason);
        emit(event, payload);
    }
}

/// The `session-limit-reset-continue-gave-up` event for a stop whose last
/// allowed continue attempt did not reach the pane.
pub(crate) fn gave_up_event(
    claude_session_id: &str,
    config_dir: &str,
    terminal_id: &str,
    attempts: u32,
) -> (&'static str, Value) {
    (
        CONTINUE_GAVE_UP_EVENT,
        json!({
            "claudeSessionId": claude_session_id,
            "configDir": config_dir,
            "terminalId": terminal_id,
            "attempts": attempts,
        }),
    )
}

/// The `session-limit-reset-continued` event for one submitted continue.
pub(crate) fn continued_event(
    claude_session_id: &str,
    config_dir: &str,
    terminal_id: &str,
    entry_uuid: &str,
) -> (&'static str, Value) {
    (
        CONTINUED_EVENT,
        json!({
            "claudeSessionId": claude_session_id,
            "configDir": config_dir,
            "terminalId": terminal_id,
            "entryUuid": entry_uuid,
        }),
    )
}

// ── Dedup ─────────────────────────────────────────────────────────────────────

/// What [`ContinueLedger::claim`] answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Claim {
    /// This caller now owns the continue of this stop.
    Claimed,
    /// Another caller holds it right now.
    InFlight,
    /// A continue for this stop already reached the pane.
    Done,
    /// [`MAX_CONTINUE_ATTEMPTS`] attempts ended without reaching the pane.
    OutOfAttempts,
}

/// What [`ContinueLedger::settle`] did with a finished attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Settled {
    /// The prompt reached the pane, or the session was already working: the
    /// claim is kept, so no later hint types a second prompt for this stop.
    Kept,
    /// The attempt ended without the prompt in the pane: the claim is
    /// released and the attempt counted. `retry` is whether another is owed.
    Released { attempts: u32, retry: bool },
}

#[derive(Debug, Default, Clone, Copy)]
struct LedgerEntry {
    in_flight: bool,
    done: bool,
    attempts: u32,
    /// Probe-unknown parks of this stop so far ([`unknown_park_backoff_ms`]).
    unknown_parks: u32,
}

/// The in-flight and attempt record for reset-continues, keyed by
/// `(claude_session_id, entry_uuid)`. The claim is taken BEFORE the idle
/// watcher starts, so two hints for one stop (a screen hint and a re-entry
/// arriving together) type one prompt. The durable dedup is the transcript: a
/// submitted continue appends a later user entry, so the next read is
/// `NotStopped`. One small entry per stop, held for the process's life.
#[derive(Debug, Default)]
pub(crate) struct ContinueLedger {
    entries: HashMap<(String, String), LedgerEntry>,
}

impl ContinueLedger {
    /// A probe answered for this stop: the next unknown park backs off from
    /// the start again.
    pub(crate) fn clear_unknown_parks(&mut self, claude_session_id: &str, entry_uuid: &str) {
        if let Some(e) = self
            .entries
            .get_mut(&(claude_session_id.to_string(), entry_uuid.to_string()))
        {
            e.unknown_parks = 0;
        }
    }

    /// Count one more probe-unknown park of this stop; returns the count.
    pub(crate) fn note_unknown_park(&mut self, claude_session_id: &str, entry_uuid: &str) -> u32 {
        let e = self
            .entries
            .entry((claude_session_id.to_string(), entry_uuid.to_string()))
            .or_default();
        e.unknown_parks = e.unknown_parks.saturating_add(1);
        e.unknown_parks
    }

    pub(crate) fn claim(&mut self, claude_session_id: &str, entry_uuid: &str) -> Claim {
        let e = self
            .entries
            .entry((claude_session_id.to_string(), entry_uuid.to_string()))
            .or_default();
        if e.done {
            Claim::Done
        } else if e.in_flight {
            Claim::InFlight
        } else if e.attempts >= MAX_CONTINUE_ATTEMPTS {
            Claim::OutOfAttempts
        } else {
            e.in_flight = true;
            Claim::Claimed
        }
    }

    pub(crate) fn settle(
        &mut self,
        claude_session_id: &str,
        entry_uuid: &str,
        outcome: &IdlePromptOutcome,
    ) -> Settled {
        let e = self
            .entries
            .entry((claude_session_id.to_string(), entry_uuid.to_string()))
            .or_default();
        e.in_flight = false;
        match outcome {
            IdlePromptOutcome::Submitted | IdlePromptOutcome::AlreadyBusy => {
                e.done = true;
                Settled::Kept
            }
            IdlePromptOutcome::TerminalGone
            | IdlePromptOutcome::SubmitFailed(_)
            | IdlePromptOutcome::GaveUp { .. } => {
                e.attempts += 1;
                Settled::Released {
                    attempts: e.attempts,
                    retry: e.attempts < MAX_CONTINUE_ATTEMPTS,
                }
            }
        }
    }
}

/// The process's ledger.
pub(crate) static LEDGER: Mutex<Option<ContinueLedger>> = Mutex::new(None);

pub(crate) fn with_ledger<T>(
    ledger: &Mutex<Option<ContinueLedger>>,
    f: impl FnOnce(&mut ContinueLedger) -> T,
) -> T {
    let mut guard = ledger.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(ContinueLedger::default))
}

/// How one [`run_continue`] call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContinueRun {
    /// Not attempted: the ledger answered this claim.
    NotClaimed(Claim),
    /// Attempted: the watcher's outcome and what the ledger did with it.
    Ran {
        outcome: IdlePromptOutcome,
        settled: Settled,
    },
}

/// Claim the stop, run `prepare` then `watch`, and settle the claim on the
/// watcher's outcome. No lock is held across an await.
pub(crate) async fn run_continue<P, W, WF>(
    ledger: &Mutex<Option<ContinueLedger>>,
    claude_session_id: &str,
    entry_uuid: &str,
    prepare: P,
    watch: W,
) -> ContinueRun
where
    P: FnOnce(),
    W: FnOnce() -> WF,
    WF: Future<Output = IdlePromptOutcome>,
{
    match with_ledger(ledger, |l| l.claim(claude_session_id, entry_uuid)) {
        Claim::Claimed => {}
        other => return ContinueRun::NotClaimed(other),
    }
    let mut claim = ClaimGuard {
        ledger,
        claude_session_id,
        entry_uuid,
        settled: false,
    };
    prepare();
    let outcome = watch().await;
    let settled = with_ledger(ledger, |l| {
        l.settle(claude_session_id, entry_uuid, &outcome)
    });
    claim.settled = true;
    ContinueRun::Ran { outcome, settled }
}

/// Settles a claim [`run_continue`] took when the run ends without settling
/// it (its future dropped, its watcher panicked), as an attempt that did not
/// reach the pane, so the stop is not left in flight for the process's life.
struct ClaimGuard<'a> {
    ledger: &'a Mutex<Option<ContinueLedger>>,
    claude_session_id: &'a str,
    entry_uuid: &'a str,
    settled: bool,
}

impl Drop for ClaimGuard<'_> {
    fn drop(&mut self) {
        if !self.settled {
            with_ledger(self.ledger, |l| {
                l.settle(
                    self.claude_session_id,
                    self.entry_uuid,
                    &IdlePromptOutcome::TerminalGone,
                )
            });
        }
    }
}

// ── The /rate-limit-options menu ─────────────────────────────────────────────

/// How many of the screen's last non-blank rows the menu is looked for in.
const MENU_BOTTOM_ROWS: usize = 12;

/// Pure: the pane is sitting in Claude Code's `/rate-limit-options` menu
/// ("What do you want to do? ❯ 1. Stop and wait for limit to reset · Enter to
/// confirm · Esc to cancel"), which the CLI opens by itself after a limit
/// stop. The idle watcher never reads that menu as ready (Phase 1,
/// `the_rate_limit_options_menu_is_never_ready`), so a continue into such a
/// pane would always give up. The menu's own `Esc to cancel` closes it.
///
/// Read on the bottom rows only (a quoted menu higher up is history), and
/// never while a turn is running (`esc to interrupt` anywhere on screen).
///
/// runner#2417's `screen_shows_limit_wait` is a different screen: the CLI's
/// "continuing automatically at … · esc to cancel" banner over an idle box,
/// and it refuses any numbered option, so it does not admit this menu.
pub(crate) fn shows_rate_limit_menu(screen: &crate::terminal::graceful_exit::ScreenText) -> bool {
    if crate::terminal::graceful_exit::shows_busy_marker(screen) {
        return false;
    }
    let bottom: Vec<&str> = screen
        .lines
        .iter()
        .map(String::as_str)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let start = bottom.len().saturating_sub(MENU_BOTTOM_ROWS);
    let text = super::output_scan::normalize(&bottom[start..].join("\n"));
    text.contains("stop and wait for limit to reset") && text.contains("esc to cancel")
}

#[cfg(test)]
#[path = "limit_reset_tests.rs"]
mod tests;
