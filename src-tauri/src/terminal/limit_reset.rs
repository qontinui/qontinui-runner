//! What the usage-limit hint does with a session whose TRANSCRIPT says it
//! stopped at a usage limit (plan
//! `2026-11-05-runner-terminal-pages-collide-crowd-and-overfill-and-limit-stalled-sessions-never-resume`,
//! Phase 2, fix 5).
//!
//! Until this phase, [`super::account_migration::handle_usage_limit_hint`]
//! read "this account is no longer exhausted" as "the banner was echoed
//! text" and returned with nothing scheduled. A session that stopped at a
//! limit whose reset has since passed therefore sat at its prompt forever:
//! 95 of the 97 hints after the 2026-11-04 restart were dropped that way.
//! [`super::limit_stop::transcript_limit_stop`] (Phase 1) now says whether
//! the session really stopped at a limit, and [`decide_hint`] turns that,
//! the fresh usage probe and the clock into one [`HintAction`]:
//!
//! - a stop whose account has reset is CONTINUED in place, once per stop
//!   entry ([`claim_continue`] / [`settle_continue`]);
//! - a stop that cannot move yet (its reset is still ahead, the probe could
//!   not answer, or a migration was wanted but cannot run) is PARKED
//!   ([`park`]) until its reset plus [`LIMIT_RESET_MARGIN_MS`];
//! - everything else is what the handler did before: migrate an exhausted
//!   session, or skip.
//!
//! Every decision here is pure and enters through [`decide_hint`],
//! [`ContinueClaims`], [`ParkList`] and [`screen_shows_rate_limit_options_menu`];
//! the handler is wiring.
//!
//! The park list is in memory. The grid scanner's tick takes due entries
//! ([`take_due`]) and re-enters the handler (`usage_limit::scan_grids_once`,
//! plan Phase 3's due dispatch, landed here so that a park is acted on). A
//! restart loses the list; Phase 3's first-sight check is what re-derives
//! every park from the transcripts.
//!
//! Continues are paced: at most [`LIMIT_CONTINUE_PER_MINUTE`] are typed per
//! minute across the process ([`ContinuePacer`]), so a restart that brings
//! back a hundred reset sessions does not start a hundred turns at once.
//!
//! Interim arm: plan `2026-10-28-one-session-lifecycle-authority` Phase 99
//! (the Migrator) is to carry this decision into its `decide()`.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};
use tracing::{info, warn};

use super::account_migration::IdlePromptOutcome;
use super::limit_stop::{LimitStop, LimitStopRead};
use crate::terminal::graceful_exit::ScreenText;

/// How long after a stop's `resetsAt` a parked session is due. The CLI's
/// reset time is to the minute and the usage endpoint can lag it, so waiting
/// a little past it avoids continuing into a limit that has not yet lifted.
pub(crate) const LIMIT_RESET_MARGIN_MS: i64 = 120_000;

/// When a park has no reset time to wait for — the probe could not answer
/// after the reset passed, or the stop carries no `resetsAt` and nothing
/// probed the account — it is due again this long after it was parked.
pub(crate) const PARK_RETRY_MS: i64 = 15 * 60 * 1000;

/// Continue attempts per stop entry. An attempt is spent when the idle
/// watcher gives up, finds the terminal gone or cannot write; a submitted
/// continue, or a session that was already working, keeps its claim.
pub(crate) const MAX_CONTINUE_ATTEMPTS: u32 = 3;

/// Continues typed per minute across the process (the plan's
/// `LIMIT_REENTRY_PER_MINUTE`, applied where the prompt is typed so every
/// route to a continue — a screen hint or a due park — shares one bound).
pub(crate) const LIMIT_CONTINUE_PER_MINUTE: i64 = 6;

/// A claim untouched this long is forgotten, so the claim map stays bounded on
/// a runner that lives for weeks. Only a settled claim ages out: an
/// `InFlight` one is held by a live attempt (and released by its guard).
const CLAIM_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// The text typed into a session whose limit has reset. One line: it is typed
/// into a PTY, where a newline would submit early. Follows
/// `account_migration::CONTINUE_NUDGE`, minus the account move.
pub(crate) const LIMIT_RESET_NUDGE: &str = "This session stopped when this Claude account hit its \
usage limit, and that limit has now reset. Continue the task you were working on from where it \
left off; if you were idle awaiting operator input, say so and wait. Subagents interrupted by the \
limit still have their transcripts on this account: resume each one with SendMessage to its agent \
id instead of spawning a replacement (cancel any resume gate you booked for it first), and only if \
SendMessage reports \"No transcript found\" fall back to its checkpoint or worktree.";

// ── Inputs ───────────────────────────────────────────────────────────────────

/// What the fresh usage probe returned for the session's account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FreshProbe {
    /// The refresh probed nothing for this account. Below two configured
    /// accounts the selection refresh probes no account at all
    /// (`commands::ai_settings::plan_usage_refresh`).
    NotProbed,
    /// The probe of this account returned an error.
    Errored,
    /// The probe of this account answered.
    Answered,
}

/// The probe's verdict on the session's account, as [`decide_hint`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeVerdict {
    /// The account will not serve a request now.
    Exhausted,
    /// The account answered and is not exhausted.
    NotExhausted,
    /// The probe returned an error. `probe_result_exhausted`
    /// (`commands/ai_settings.rs`) counts that as exhausted, because a probe
    /// refused by the account's own quota looks the same — so it still reads
    /// as exhausted, EXCEPT where the stop's own reset has passed: there the
    /// error is no evidence either way (see [`decide_hint`]).
    Errored,
    /// No fresh probe of this account, and no unexpired sample saying it is
    /// exhausted.
    NotProbed,
}

/// Combine the fresh probe with the selection snapshot's `exhausted` flag
/// (`ai_provider::account_known_exhausted`), which the same refresh just
/// recorded.
pub(crate) fn probe_verdict(fresh: FreshProbe, known_exhausted: bool) -> ProbeVerdict {
    match fresh {
        FreshProbe::Errored => ProbeVerdict::Errored,
        _ if known_exhausted => ProbeVerdict::Exhausted,
        FreshProbe::Answered => ProbeVerdict::NotExhausted,
        FreshProbe::NotProbed => ProbeVerdict::NotProbed,
    }
}

/// Why a park, or a migration that cannot run, happened. The string is the
/// `reason` field of `session-limit-parked`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParkReason {
    /// The stop's reset is still ahead.
    ResetPending,
    /// The probe could not say whether the account has reset.
    ProbeUnknown,
    /// The account is exhausted and `auto_migrate_on_token_exhaustion` is off.
    MigrationDisabled,
    /// The account is exhausted and there is no second account to move to.
    SingleAccount,
    /// The account is exhausted and the session spent its migration cap.
    MigrationCapReached,
    /// The account is exhausted and no other account can take the session.
    NoUsableTarget,
    /// A continue attempt was not delivered (the idle watcher gave up, the
    /// write failed); attempts remain.
    ContinueRetry,
}

impl ParkReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ResetPending => "reset-pending",
            Self::ProbeUnknown => "probe-unknown",
            Self::MigrationDisabled => "migration-disabled",
            Self::SingleAccount => "single-account",
            Self::MigrationCapReached => "migration-cap-reached",
            Self::NoUsableTarget => "no-usable-target",
            Self::ContinueRetry => "continue-retry",
        }
    }
}

/// Why no migration can run, decided before the probe: the kill switch, then
/// the account count. `None` means a migration may run.
pub(crate) fn migration_block(auto_migrate: bool, account_count: usize) -> Option<ParkReason> {
    if !auto_migrate {
        Some(ParkReason::MigrationDisabled)
    } else if account_count < 2 {
        Some(ParkReason::SingleAccount)
    } else {
        None
    }
}

/// Whether the hint needs the usage probe at all: only when a migration can
/// run, or when the transcript holds a stop and the continue is switched on.
/// Otherwise the probe would decide nothing, and the handler returns before
/// it, as it did before this phase.
pub(crate) fn needs_probe(
    transcript: &LimitStopRead,
    block: Option<ParkReason>,
    auto_continue: bool,
) -> bool {
    block.is_none() || (auto_continue && matches!(transcript, LimitStopRead::Stopped(_)))
}

/// Everything [`decide_hint`] reads.
#[derive(Debug, Clone)]
pub(crate) struct HintInputs<'a> {
    /// The session's lifecycle record has `finished_at` set.
    pub finished: bool,
    pub transcript: &'a LimitStopRead,
    pub probe: ProbeVerdict,
    pub now_ms: i64,
    /// `claude_cli.auto_continue_after_limit_reset`.
    pub auto_continue: bool,
    /// [`migration_block`].
    pub migration_block: Option<ParkReason>,
}

/// What the handler does with one hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HintAction {
    /// The session is finished: never continue or migrate it.
    SkipFinished,
    /// No limit stop in the transcript and the account is not exhausted: the
    /// banner on screen was quoted text.
    SkipEchoed,
    /// The transcript could not be read and the account is not exhausted.
    /// UNKNOWN: nothing is typed.
    SkipUnreadable(String),
    /// The stop's only way out is a continue (now or after a wait), and
    /// `auto_continue_after_limit_reset` is off.
    SkipContinueDisabled,
    /// The stop carries no reset time and nothing probed the account: no
    /// evidence the limit lifted. UNKNOWN: nothing is typed, nothing parked.
    SkipNoEvidence,
    /// No limit stop in the transcript (or an unreadable one), the account
    /// is exhausted, and no migration can run. Exactly the pre-Phase-2
    /// behaviour for these sessions.
    SkipMigrationBlocked(ParkReason),
    /// Type [`LIMIT_RESET_NUDGE`] into the session, once for this stop.
    Continue(LimitStop),
    /// Wait until `due_at_ms`, then decide again.
    Park { due_at_ms: i64, reason: ParkReason },
    /// The existing migration. `park_due_at_ms` is set for a session that
    /// stopped at a limit: when the cap or the target refuses, it is parked
    /// until then instead of left with nothing scheduled.
    Migrate { park_due_at_ms: Option<i64> },
}

/// The hint's branch table, rows in order. "Reset passed" means `now` is at
/// least `resetsAt` + [`LIMIT_RESET_MARGIN_MS`]; before that the reset is
/// "ahead".
///
/// | transcript | probe | `resetsAt` | action |
/// |---|---|---|---|
/// | any | — | — | finished → [`HintAction::SkipFinished`] |
/// | `Stopped` | `Errored` | passed | park, retry after [`PARK_RETRY_MS`] (`probe-unknown`); never migrate on an unknown probe |
/// | `Stopped` | `Exhausted` / `Errored` | — | migrate; if no migration can run, park until reset + margin (else retry) |
/// | `Stopped` | `NotExhausted` / `NotProbed` | ahead | park until reset + margin (`reset-pending`) |
/// | `Stopped` | `NotProbed` | absent | skip: no evidence the limit lifted |
/// | `Stopped` | `NotExhausted` / `NotProbed` | passed, or absent with `NotExhausted` | continue in place |
/// | `NotStopped` / `Unreadable` | `Exhausted` / `Errored` | — | migrate, or skip when no migration can run |
/// | `NotStopped` | `NotExhausted` / `NotProbed` | — | skip: echoed text |
/// | `Unreadable` | `NotExhausted` / `NotProbed` | — | skip: UNKNOWN |
///
/// `NotProbed` continues once the reset has passed so that a single-account
/// runner, whose refresh probes nothing, still continues its sessions.
///
/// With `auto_continue` off, a stop whose only way out is a continue is
/// skipped rather than parked: a park that can only end in a skip would probe
/// and toast for nothing.
pub(crate) fn decide_hint(i: &HintInputs<'_>) -> HintAction {
    let action = decide_ignoring_switch(i);
    if i.auto_continue {
        return action;
    }
    match action {
        HintAction::Continue(_) => HintAction::SkipContinueDisabled,
        // Only a continue ends these parks.
        HintAction::Park {
            reason:
                ParkReason::ResetPending
                | ParkReason::MigrationDisabled
                | ParkReason::SingleAccount
                | ParkReason::ContinueRetry,
            ..
        } => HintAction::SkipContinueDisabled,
        // A probe-unknown park can still end in a migration — unless none can run.
        HintAction::Park {
            reason: ParkReason::ProbeUnknown,
            ..
        } if i.migration_block.is_some() => HintAction::SkipContinueDisabled,
        other => other,
    }
}

fn decide_ignoring_switch(i: &HintInputs<'_>) -> HintAction {
    if i.finished {
        return HintAction::SkipFinished;
    }
    let exhausted = matches!(i.probe, ProbeVerdict::Exhausted | ProbeVerdict::Errored);
    match i.transcript {
        LimitStopRead::Stopped(stop) => {
            let retry_due = i.now_ms + PARK_RETRY_MS;
            let due = stop
                .resets_at_ms
                .map(|r| r.saturating_add(LIMIT_RESET_MARGIN_MS));
            let reset_passed = due.is_some_and(|d| d <= i.now_ms);
            let wait_due = due.filter(|d| *d > i.now_ms);
            if i.probe == ProbeVerdict::Errored && reset_passed {
                return HintAction::Park {
                    due_at_ms: retry_due,
                    reason: ParkReason::ProbeUnknown,
                };
            }
            if exhausted {
                let due = wait_due.unwrap_or(retry_due);
                return match i.migration_block {
                    None => HintAction::Migrate {
                        park_due_at_ms: Some(due),
                    },
                    Some(reason) => HintAction::Park {
                        due_at_ms: due,
                        reason,
                    },
                };
            }
            if let Some(due) = wait_due {
                return HintAction::Park {
                    due_at_ms: due,
                    reason: ParkReason::ResetPending,
                };
            }
            if stop.resets_at_ms.is_none() && i.probe == ProbeVerdict::NotProbed {
                return HintAction::SkipNoEvidence;
            }
            HintAction::Continue(stop.clone())
        }
        LimitStopRead::NotStopped | LimitStopRead::Unreadable(_) if exhausted => {
            match i.migration_block {
                None => HintAction::Migrate {
                    park_due_at_ms: None,
                },
                Some(reason) => HintAction::SkipMigrationBlocked(reason),
            }
        }
        LimitStopRead::NotStopped => HintAction::SkipEchoed,
        LimitStopRead::Unreadable(why) => HintAction::SkipUnreadable(why.clone()),
    }
}

/// What a migration the cap or the target refused leads to: a park for a
/// session that stopped at a limit (`park_due_at_ms` set by [`decide_hint`]),
/// otherwise the `session-account-migration-skipped` report it always got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefusedMigration {
    Park { due_at_ms: i64, reason: ParkReason },
    ReportSkipped,
}

pub(crate) fn on_refused_migration(
    park_due_at_ms: Option<i64>,
    reason: ParkReason,
) -> RefusedMigration {
    match park_due_at_ms {
        Some(due_at_ms) => RefusedMigration::Park { due_at_ms, reason },
        None => RefusedMigration::ReportSkipped,
    }
}

// ── Continue claims (the dedup) ──────────────────────────────────────────────

/// What [`ContinueClaims::try_claim`] answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Claim {
    /// The caller owns this attempt (1-based) and must settle or release it
    /// (a [`ClaimGuard`] does, even on a panic).
    Granted { attempt: u32 },
    /// Another hint holds the claim: its attempt has not ended yet.
    InFlight,
    /// A continue was submitted for this stop, the session was already
    /// working when one was about to be, or a migration carried the stop to
    /// another account (whose own nudge continues it).
    Delivered,
    /// [`MAX_CONTINUE_ATTEMPTS`] attempts gave up.
    AttemptsSpent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimState {
    InFlight,
    Delivered,
    Released,
}

#[derive(Debug, Clone, Copy)]
struct ClaimEntry {
    state: ClaimState,
    /// Attempts that gave up.
    spent: u32,
    touched_ms: i64,
}

/// One claim per `(claude_session_id, entry_uuid)`: the stop entry is the
/// unit a continue is owed for. The claim is taken BEFORE the watcher starts,
/// so two hints for the same stop — at boot, Phase 3's first-sight check and
/// the screen hint fire together — type one prompt, not two. The transcript
/// is the durable half of the dedup: a submitted continue adds a later user
/// entry, so the next read of it is `NotStopped`.
#[derive(Debug, Default)]
pub(crate) struct ContinueClaims {
    map: HashMap<(String, String), ClaimEntry>,
}

impl ContinueClaims {
    pub(crate) fn try_claim(&mut self, session: &str, entry_uuid: &str, now_ms: i64) -> Claim {
        self.map.retain(|_, e| {
            e.state == ClaimState::InFlight || now_ms.saturating_sub(e.touched_ms) <= CLAIM_TTL_MS
        });
        let entry = self
            .map
            .entry((session.to_string(), entry_uuid.to_string()))
            .or_insert(ClaimEntry {
                state: ClaimState::Released,
                spent: 0,
                touched_ms: now_ms,
            });
        match entry.state {
            ClaimState::InFlight => Claim::InFlight,
            ClaimState::Delivered => Claim::Delivered,
            ClaimState::Released if entry.spent >= MAX_CONTINUE_ATTEMPTS => Claim::AttemptsSpent,
            ClaimState::Released => {
                entry.state = ClaimState::InFlight;
                entry.touched_ms = now_ms;
                Claim::Granted {
                    attempt: entry.spent + 1,
                }
            }
        }
    }

    /// Whether this stop is being continued or already was: a hint for it
    /// does nothing, and in particular does not park it against a reset that
    /// belongs to the account a migration moved it off.
    pub(crate) fn handled(&self, session: &str, entry_uuid: &str) -> bool {
        self.map
            .get(&(session.to_string(), entry_uuid.to_string()))
            .is_some_and(|e| matches!(e.state, ClaimState::InFlight | ClaimState::Delivered))
    }

    /// Settle a granted attempt: `delivered` keeps the claim for good;
    /// otherwise it is released and the attempt counts as spent. Returns
    /// whether attempts remain.
    pub(crate) fn settle(
        &mut self,
        session: &str,
        entry_uuid: &str,
        delivered: bool,
        now_ms: i64,
    ) -> bool {
        let Some(entry) = self
            .map
            .get_mut(&(session.to_string(), entry_uuid.to_string()))
        else {
            return false;
        };
        entry.touched_ms = now_ms;
        if delivered {
            entry.state = ClaimState::Delivered;
            return false;
        }
        entry.state = ClaimState::Released;
        entry.spent = entry.spent.saturating_add(1);
        entry.spent < MAX_CONTINUE_ATTEMPTS
    }

    /// Give a granted claim back without spending an attempt: nothing was
    /// tried (a migration that did not run).
    pub(crate) fn release(&mut self, session: &str, entry_uuid: &str, now_ms: i64) {
        if let Some(entry) = self
            .map
            .get_mut(&(session.to_string(), entry_uuid.to_string()))
        {
            entry.state = ClaimState::Released;
            entry.touched_ms = now_ms;
        }
    }
}

/// A granted claim that is settled exactly once. Dropped unsettled — a panic,
/// or the future dropped mid-attempt — it settles as not delivered, so a stop
/// can never be left `InFlight` for the process lifetime.
pub(crate) struct ClaimGuard<'a> {
    claims: &'a Mutex<ContinueClaims>,
    session: String,
    entry_uuid: String,
    open: bool,
}

impl<'a> ClaimGuard<'a> {
    pub(crate) fn new(claims: &'a Mutex<ContinueClaims>, session: &str, entry_uuid: &str) -> Self {
        Self {
            claims,
            session: session.to_string(),
            entry_uuid: entry_uuid.to_string(),
            open: true,
        }
    }

    /// [`ContinueClaims::settle`]; returns whether attempts remain.
    pub(crate) fn settle(mut self, delivered: bool, now_ms: i64) -> bool {
        self.open = false;
        lock(self.claims).settle(&self.session, &self.entry_uuid, delivered, now_ms)
    }

    /// [`ContinueClaims::release`].
    pub(crate) fn release(mut self, now_ms: i64) {
        self.open = false;
        lock(self.claims).release(&self.session, &self.entry_uuid, now_ms);
    }
}

impl Drop for ClaimGuard<'_> {
    fn drop(&mut self) {
        if self.open {
            let now = chrono::Utc::now().timestamp_millis();
            lock(self.claims).settle(&self.session, &self.entry_uuid, false, now);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Hands out continue slots no closer than one per
/// `60 s / LIMIT_CONTINUE_PER_MINUTE`.
#[derive(Debug, Default)]
pub(crate) struct ContinuePacer {
    next_slot_ms: i64,
}

impl ContinuePacer {
    /// Reserve the next slot; returns how long the caller waits for it.
    pub(crate) fn reserve(&mut self, now_ms: i64) -> i64 {
        let slot = self.next_slot_ms.max(now_ms);
        self.next_slot_ms = slot + 60_000 / LIMIT_CONTINUE_PER_MINUTE;
        slot - now_ms
    }
}

/// The process-wide claims, park list and pacer the running app uses
/// (`account_migration`'s [`HintEnv`] implementation).
pub(crate) static CLAIMS: std::sync::LazyLock<Mutex<ContinueClaims>> =
    std::sync::LazyLock::new(|| Mutex::new(ContinueClaims::default()));
pub(crate) static PARKS: std::sync::LazyLock<Mutex<ParkList>> =
    std::sync::LazyLock::new(|| Mutex::new(ParkList::default()));
pub(crate) static PACER: std::sync::LazyLock<Mutex<ContinuePacer>> =
    std::sync::LazyLock::new(|| Mutex::new(ContinuePacer::default()));

// ── The park list ────────────────────────────────────────────────────────────

/// One parked session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Park {
    pub terminal_id: String,
    pub claude_session_id: String,
    pub due_at_ms: i64,
    pub reason: ParkReason,
}

/// Parked sessions, one entry per terminal: a newer park for the same
/// terminal replaces the older one, because the newer hint read the newer
/// transcript.
#[derive(Debug, Default)]
pub(crate) struct ParkList {
    by_terminal: HashMap<String, Park>,
}

impl ParkList {
    pub(crate) fn park(&mut self, park: Park) {
        self.by_terminal.insert(park.terminal_id.clone(), park);
    }

    pub(crate) fn unpark(&mut self, terminal_id: &str) -> Option<Park> {
        self.by_terminal.remove(terminal_id)
    }

    pub(crate) fn get(&self, terminal_id: &str) -> Option<&Park> {
        self.by_terminal.get(terminal_id)
    }

    /// Drop the parks of terminals that are gone: a closed pane is never
    /// re-entered.
    pub(crate) fn retain_live(&mut self, is_live: impl Fn(&str) -> bool) {
        self.by_terminal.retain(|tid, _| is_live(tid));
    }

    /// Remove and return up to `max` entries due at `now_ms`, earliest due
    /// first; the rest stay for a later take.
    pub(crate) fn take_due(&mut self, now_ms: i64, max: usize) -> Vec<Park> {
        let mut due: Vec<Park> = self
            .by_terminal
            .values()
            .filter(|p| p.due_at_ms <= now_ms)
            .cloned()
            .collect();
        due.sort_by(|a, b| {
            a.due_at_ms
                .cmp(&b.due_at_ms)
                .then_with(|| a.terminal_id.cmp(&b.terminal_id))
        });
        due.truncate(max);
        for p in &due {
            self.by_terminal.remove(&p.terminal_id);
        }
        due
    }
}

/// Up to `max` parks due at `now_ms`, removed from the list, after dropping
/// the parks of terminals that are gone. The grid scanner's tick is the caller.
pub(crate) fn take_due(now_ms: i64, max: usize, is_live: impl Fn(&str) -> bool) -> Vec<Park> {
    let mut parks = lock(&PARKS);
    parks.retain_live(is_live);
    parks.take_due(now_ms, max)
}

// ── Events ───────────────────────────────────────────────────────────────────

/// The event a park emits. NOT `session-account-migration-skipped`: that one
/// is an error toast saying the session is "out of tokens", and a park is
/// neither an error nor, for a `reset-pending` session, out of tokens.
pub(crate) const PARKED_EVENT: &str = "session-limit-parked";

/// The event a submitted continue emits.
pub(crate) const CONTINUED_EVENT: &str = "session-limit-reset-continued";

pub(crate) fn parked_payload(
    claude_session_id: &str,
    terminal_id: &str,
    config_dir: &str,
    due_at_ms: i64,
    reason: ParkReason,
) -> Value {
    json!({
        "claudeSessionId": claude_session_id,
        "terminalId": terminal_id,
        "configDir": config_dir,
        "dueAtMs": due_at_ms,
        "reason": reason.as_str(),
    })
}

pub(crate) fn continued_payload(
    claude_session_id: &str,
    terminal_id: &str,
    config_dir: &str,
    entry_uuid: &str,
) -> Value {
    json!({
        "claudeSessionId": claude_session_id,
        "terminalId": terminal_id,
        "configDir": config_dir,
        "entryUuid": entry_uuid,
    })
}

// ── The hint's sequence ──────────────────────────────────────────────────────

/// The session a hint is about.
#[derive(Debug, Clone)]
pub(crate) struct HintSession {
    pub terminal_id: String,
    pub claude_session_id: String,
    /// Its lifecycle record has `finished_at` set.
    pub finished: bool,
    /// The account it runs on.
    pub config_dir: String,
}

/// The settings the sequence reads.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HintSettings {
    pub auto_migrate: bool,
    pub auto_continue: bool,
    pub account_count: usize,
}

/// What a migration attempt came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MigrationAttempt {
    Migrated,
    /// The cap or the target refused before anything ran.
    Blocked(ParkReason),
    /// Not migrated, and already reported by the migration path: another
    /// migration holds the session, or the migration failed.
    NotMigrated,
}

/// Everything [`run_hint`] needs from the running app, for one hint. The
/// app's implementation is `account_migration::AppHintEnv`; the tests' is a
/// fake that records what the sequence did.
pub(crate) trait HintEnv {
    fn settings(&self) -> HintSettings;
    /// [`super::limit_stop::transcript_limit_stop`] on the session's transcript.
    async fn read_transcript(&self) -> LimitStopRead;
    /// Refresh the usage probe; the verdict on the session's account.
    async fn probe(&self) -> ProbeVerdict;
    fn now_ms(&self) -> i64;
    fn claims(&self) -> &Mutex<ContinueClaims>;
    fn parks(&self) -> &Mutex<ParkList>;
    fn pacer(&self) -> &Mutex<ContinuePacer>;
    async fn sleep_ms(&self, ms: i64);
    /// One `Esc` when [`screen_shows_rate_limit_options_menu`] reads the pane.
    fn close_rate_limit_menu(&self);
    async fn prompt_when_idle(&self, prompt: &str) -> IdlePromptOutcome;
    fn emit(&self, event: &'static str, payload: Value);
    /// `session-account-migration-skipped`, as the migration always reported.
    fn emit_skipped(&self, reason: &str);
    /// The existing migration (cooldown, cap, target, respawn).
    async fn migrate(&self) -> MigrationAttempt;
}

/// The usage-limit hint, from the transcript read to the action. The handler
/// (`account_migration::handle_usage_limit_hint`) resolves the session and
/// hands over.
pub(crate) async fn run_hint<E: HintEnv>(env: &E, s: &HintSession) {
    let settings = env.settings();
    let block = migration_block(settings.auto_migrate, settings.account_count);
    let transcript = env.read_transcript().await;
    if let LimitStopRead::Stopped(stop) = &transcript {
        if lock(env.claims()).handled(&s.claude_session_id, &stop.entry_uuid) {
            info!(
                terminal_id = %s.terminal_id,
                session = %s.claude_session_id,
                entry_uuid = %stop.entry_uuid,
                "usage-limit hint for a stop that is being continued, was continued, or was carried by a migration — nothing to do"
            );
            return;
        }
    }
    if !needs_probe(&transcript, block, settings.auto_continue) {
        info!(
            terminal_id = %s.terminal_id,
            session = %s.claude_session_id,
            transcript = ?transcript,
            blocked = block.map(ParkReason::as_str),
            "usage-limit hint: no migration can run, and there is no limit stop to continue (or the continue is off) — nothing to do"
        );
        return;
    }
    let probe = env.probe().await;
    let action = decide_hint(&HintInputs {
        finished: s.finished,
        transcript: &transcript,
        probe,
        now_ms: env.now_ms(),
        auto_continue: settings.auto_continue,
        migration_block: block,
    });
    let (terminal_id, session, account) = (&s.terminal_id, &s.claude_session_id, &s.config_dir);
    match action {
        HintAction::SkipFinished => info!(
            %terminal_id, %session,
            "usage-limit hint on a FINISHED session — never continued or migrated"
        ),
        HintAction::SkipEchoed => info!(
            %terminal_id, %session, %account,
            "usage-limit message NOT confirmed by probe — skipping (likely echoed text)"
        ),
        HintAction::SkipUnreadable(why) => info!(
            %terminal_id, %session, %account, why = %why,
            "limit stop UNKNOWN — transcript unreadable; the account is not exhausted, so nothing is typed"
        ),
        HintAction::SkipContinueDisabled => info!(
            %terminal_id, %session, %account,
            "limit stop whose only way out is a continue, and auto-continue after a limit reset is disabled in settings"
        ),
        HintAction::SkipNoEvidence => info!(
            %terminal_id, %session, %account,
            "limit stop with no reset time on an account nothing probed — UNKNOWN whether the limit lifted; nothing typed"
        ),
        HintAction::SkipMigrationBlocked(reason) => info!(
            %terminal_id, %session, %account, reason = reason.as_str(),
            "usage-limit confirmed-candidate but no migration can run (auto-migration disabled, or fewer than two accounts)"
        ),
        HintAction::Park { due_at_ms, reason } => park_session(env, s, due_at_ms, reason),
        HintAction::Continue(stop) => {
            lock(env.parks()).unpark(terminal_id);
            continue_session(env, s, &stop).await;
        }
        HintAction::Migrate { park_due_at_ms } => {
            lock(env.parks()).unpark(terminal_id);
            migrate_session(env, s, &transcript, park_due_at_ms).await;
        }
    }
}

/// Park the session and say so on [`PARKED_EVENT`] — never on
/// `session-account-migration-skipped`, whose error toast says "out of tokens".
fn park_session<E: HintEnv>(env: &E, s: &HintSession, due_at_ms: i64, reason: ParkReason) {
    lock(env.parks()).park(Park {
        terminal_id: s.terminal_id.clone(),
        claude_session_id: s.claude_session_id.clone(),
        due_at_ms,
        reason,
    });
    info!(
        terminal_id = %s.terminal_id,
        session = %s.claude_session_id,
        account = %s.config_dir,
        due_at_ms,
        reason = reason.as_str(),
        "limit-stopped session parked"
    );
    env.emit(
        PARKED_EVENT,
        parked_payload(
            &s.claude_session_id,
            &s.terminal_id,
            &s.config_dir,
            due_at_ms,
            reason,
        ),
    );
}

/// Continue the session in place, once for this stop: claim it, wait for a
/// paced slot, close the `/rate-limit-options` menu if it holds the input
/// box, type [`LIMIT_RESET_NUDGE`] through the idle watcher, and settle the
/// claim on the outcome. An attempt that was not delivered is parked for a
/// retry while attempts remain. No migration cap slot is spent.
async fn continue_session<E: HintEnv>(env: &E, s: &HintSession, stop: &LimitStop) {
    let (terminal_id, session) = (&s.terminal_id, s.claude_session_id.as_str());
    let entry_uuid = stop.entry_uuid.as_str();
    let claim = lock(env.claims()).try_claim(session, entry_uuid, env.now_ms());
    let attempt = match claim {
        Claim::Granted { attempt } => attempt,
        Claim::InFlight | Claim::Delivered => {
            info!(
                %terminal_id, session, entry_uuid,
                "limit-reset-continue: already in flight or delivered for this stop — nothing to do"
            );
            return;
        }
        Claim::AttemptsSpent => {
            warn!(
                %terminal_id, session, entry_uuid, attempts = MAX_CONTINUE_ATTEMPTS,
                "limit-reset-continue: every attempt for this stop gave up — not trying again"
            );
            return;
        }
    };
    let guard = ClaimGuard::new(env.claims(), session, entry_uuid);
    let wait_ms = lock(env.pacer()).reserve(env.now_ms());
    info!(
        %terminal_id, session, entry_uuid, attempt, wait_ms,
        account = %s.config_dir,
        rate_limit_type = stop.rate_limit_type.as_deref().unwrap_or("unknown"),
        resets_at_ms = stop.resets_at_ms,
        "limit stop's account has reset — continuing the session in place"
    );
    if wait_ms > 0 {
        env.sleep_ms(wait_ms).await;
    }
    env.close_rate_limit_menu();
    let outcome = env.prompt_when_idle(LIMIT_RESET_NUDGE).await;
    let delivered = matches!(
        outcome,
        IdlePromptOutcome::Submitted | IdlePromptOutcome::AlreadyBusy
    );
    let may_retry = guard.settle(delivered, env.now_ms());
    match outcome {
        IdlePromptOutcome::Submitted => {
            info!(%terminal_id, session, entry_uuid, "limit-reset-continue submitted");
            env.emit(
                CONTINUED_EVENT,
                continued_payload(session, terminal_id, &s.config_dir, entry_uuid),
            );
        }
        IdlePromptOutcome::AlreadyBusy => info!(
            %terminal_id, session, entry_uuid,
            "limit-reset-continue: the session was already working — nothing typed"
        ),
        other => {
            warn!(
                %terminal_id, session, entry_uuid, attempt, may_retry, outcome = ?other,
                "limit-reset-continue: not delivered — the attempt is spent"
            );
            if may_retry {
                park_session(
                    env,
                    s,
                    env.now_ms() + PARK_RETRY_MS,
                    ParkReason::ContinueRetry,
                );
            }
        }
    }
}

/// The existing migration. A stop it carries is claimed for the migration
/// first, so the resumed pane's own hint — the resumed CLI repaints the old
/// account's banner — finds it handled, instead of parking it against the old
/// account's reset or typing a second nudge beside the migration's. The claim
/// is given back when no migration ran. When the cap or the target refuses, a
/// stopped session is parked; any other session is reported skipped as before.
async fn migrate_session<E: HintEnv>(
    env: &E,
    s: &HintSession,
    transcript: &LimitStopRead,
    park_due_at_ms: Option<i64>,
) {
    let guard = match transcript {
        LimitStopRead::Stopped(stop) => {
            let claim =
                lock(env.claims()).try_claim(&s.claude_session_id, &stop.entry_uuid, env.now_ms());
            matches!(claim, Claim::Granted { .. })
                .then(|| ClaimGuard::new(env.claims(), &s.claude_session_id, &stop.entry_uuid))
        }
        _ => None,
    };
    match env.migrate().await {
        MigrationAttempt::Migrated => {
            if let Some(g) = guard {
                g.settle(true, env.now_ms());
            }
        }
        MigrationAttempt::Blocked(reason) => {
            if let Some(g) = guard {
                g.release(env.now_ms());
            }
            match on_refused_migration(park_due_at_ms, reason) {
                RefusedMigration::Park { due_at_ms, reason } => {
                    park_session(env, s, due_at_ms, reason)
                }
                RefusedMigration::ReportSkipped => env.emit_skipped(reason.as_str()),
            }
        }
        MigrationAttempt::NotMigrated => {
            if let Some(g) = guard {
                g.release(env.now_ms());
            }
        }
    }
}

// ── The `/rate-limit-options` menu ───────────────────────────────────────────

/// How many of the screen's last non-blank rows the menu check reads.
const MENU_BOTTOM_ROWS: usize = 12;

/// The menu's stop option, as `usage_limit::USAGE_LIMIT_PATTERNS` and
/// claude-config's `LIMIT_MENU` read it. The question above it ("What do you
/// want to do?") titles unrelated menus too.
const MENU_STOP_OPTION: &str = "stop and wait for limit to reset";

/// Pure: whether the CLI's `/rate-limit-options` menu is open at the bottom of
/// the screen and nothing is running, so that ONE `Esc` closes the menu and
/// leaves the idle input box the continue is typed into.
///
/// Phase 1 pinned the menu (`fixtures/parked_screens/rate_limit_options_menu.txt`):
/// the idle watcher reads it as a numbered-choice dialog, never `Ready`, so a
/// continue on a menu-parked pane would give up after 3 minutes every time.
///
/// The plan named runner#2417's `screen_shows_limit_wait` for this, but that
/// gate recognises a different screen — the "continuing automatically at … ·
/// esc to cancel" wait banner over an idle input box — and refuses exactly
/// this one by its own rules: a numbered option, or an `esc to …` hint on a
/// row other than the banner. So this gate reads the menu itself.
///
/// It requires, in the bottom rows read in screen order, the stop option and,
/// BELOW it among the last [`MENU_FOOTER_ROWS`] non-blank rows, the menu's
/// `Esc to cancel` hint — the menu is the last thing drawn, not text quoted
/// above an idle box. It refuses on Claude's busy marker anywhere on screen:
/// an `Esc` during a turn would interrupt it. The rows are normalized joined,
/// so a stop option a narrow pane wrapped onto two rows still matches.
pub(crate) fn screen_shows_rate_limit_options_menu(screen: &ScreenText) -> bool {
    use crate::terminal::output_scan::normalize;
    let rows: Vec<&str> = screen
        .lines
        .iter()
        .map(String::as_str)
        .filter(|r| !r.trim().is_empty())
        .collect();
    let bottom = &rows[rows.len().saturating_sub(MENU_BOTTOM_ROWS)..];
    let footer_at = bottom.len().saturating_sub(MENU_FOOTER_ROWS);
    let Some(footer) = bottom
        .iter()
        .rposition(|r| normalize(r).contains("esc to cancel"))
        .filter(|i| *i >= footer_at)
    else {
        return false;
    };
    if !normalize(&bottom[..footer].join("\n")).contains(MENU_STOP_OPTION) {
        return false;
    }
    !crate::terminal::graceful_exit::shows_busy_marker(screen)
}

/// The menu's `Esc to cancel` footer sits within this many of the last
/// non-blank rows (a status line may follow it).
const MENU_FOOTER_ROWS: usize = 3;

#[cfg(test)]
mod tests;
