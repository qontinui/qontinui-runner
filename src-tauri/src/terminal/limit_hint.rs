//! The usage-limit hint's decision path, from the settings to the action
//! (plan
//! `2026-11-05-runner-terminal-pages-collide-crowd-and-overfill-and-limit-stalled-sessions-never-resume`,
//! Phase 2). [`run_hint`] is what
//! [`super::account_migration::handle_usage_limit_hint`] runs once it has the
//! session's record and account. It reads the transcript, picks and reads
//! the probe, decides ([`super::limit_reset::decide`]), and then skips, parks
//! or continues. A migration is handed back to the handler, which owns it.
//!
//! Every effect goes through [`HintEnv`], so a test drives the real path with
//! a fake env. That is how the wiring is pinned: the single-account continue,
//! the split kill switch, the finished-session skip and the park report.

use std::future::Future;

use crate::commands::ai_settings::AccountUsageInfo;

use super::limit_reset::{
    decide, hint_wanted, probe_source, roster_probe_needed, row_for, unknown_park_backoff_ms,
    HintInputs, LimitAction, ParkReason, ProbeSource, ProbeVerdict, SkipReason,
};
use super::limit_stop::{LimitStop, LimitStopRead};

/// What the hint path reads and does, for one session.
pub(crate) trait HintEnv {
    /// `(auto_migrate_on_token_exhaustion, auto_continue_after_limit_reset)`,
    /// as [`hint_switches`] reads them.
    fn switches(&self) -> (bool, bool);
    /// The session's lifecycle record has `finished_at` set.
    fn finished(&self) -> bool;
    /// How many Claude config dirs (accounts) are configured.
    fn config_dir_count(&self) -> usize;
    /// The session transcript's limit-stop read.
    fn read_stop(&self) -> impl Future<Output = LimitStopRead> + Send;
    /// The roster probe (every configured account; none below two).
    fn roster_probe(&self) -> impl Future<Output = Vec<AccountUsageInfo>> + Send;
    /// A probe of the session's own account.
    fn own_probe(&self) -> impl Future<Output = AccountUsageInfo> + Send;
    /// The cached exhaustion verdict for the session's account.
    fn known_exhausted(&self) -> bool;
    /// Whether one probe row reads exhausted.
    fn row_exhausted(&self, row: &AccountUsageInfo) -> bool;
    /// When the session last moved to another account, in this process.
    fn migrated_at(&self) -> Option<i64>;
    /// Count one more probe-unknown park of this stop; returns the count.
    fn note_unknown_park(&self, entry_uuid: &str) -> u32;
    /// A probe answered for this stop: restart its unknown-park backoff.
    fn clear_unknown_parks(&self, entry_uuid: &str);
    fn now_ms(&self) -> i64;
    /// Nothing to do: log why.
    fn skip(&self, why: SkipReason, read: &LimitStopRead);
    /// Park the session and report the park.
    fn park(&self, due_at_ms: i64, reason: ParkReason);
    /// Continue the session in place.
    fn continue_stop(&self, stop: LimitStop) -> impl Future<Output = ()> + Send;
}

/// How [`run_hint`] ended.
#[derive(Debug, Clone)]
pub(crate) enum HintFlow {
    /// Skipped, parked or continued: nothing left for the handler.
    Done,
    /// The handler's existing migration runs. `usage` is the roster probe it
    /// read (for the cooldown); `park_fallback` parks a stop the cap or the
    /// target pick refuses.
    Migrate {
        park_fallback: Option<i64>,
        usage: Vec<AccountUsageInfo>,
    },
}

/// The two switches the hint path reads, from the Claude CLI settings:
/// `(auto_migrate_on_token_exhaustion, auto_continue_after_limit_reset)`.
pub(crate) fn hint_switches(cli: &crate::settings::ClaudeCliSettings) -> (bool, bool) {
    (
        cli.auto_migrate_on_token_exhaustion,
        cli.auto_continue_after_limit_reset,
    )
}

/// The hint path for a session whose record is open; `config_dir` is its
/// account.
pub(crate) async fn run_hint(env: &impl HintEnv, config_dir: &str) -> HintFlow {
    let (auto_migrate, auto_continue) = env.switches();
    let no_stop = LimitStopRead::NotStopped;
    if !hint_wanted(auto_migrate, auto_continue) {
        env.skip(SkipReason::Disabled, &no_stop);
        return HintFlow::Done;
    }
    // Did the session STOP at a limit? The transcript says; the screen match
    // that brought the hint here is only a hint.
    let read = env.read_stop().await;
    let config_dir_count = env.config_dir_count();
    // CONFIRM with a fresh probe: conversation text can quote a limit, and a
    // resume repaint can re-render a historical one.
    let usage = if roster_probe_needed(&read, auto_migrate, config_dir_count) {
        env.roster_probe().await
    } else {
        Vec::new()
    };
    let roster_row = row_for(&usage, config_dir);
    let probe = match probe_source(roster_row.is_some(), &read, auto_continue) {
        ProbeSource::Roster => ProbeVerdict::from_row(roster_row, env.known_exhausted()),
        ProbeSource::OwnAccount => {
            let row = env.own_probe().await;
            ProbeVerdict::from_row(Some(&row), env.row_exhausted(&row))
        }
        ProbeSource::NoRow => ProbeVerdict::from_row(None, env.known_exhausted()),
    };
    if let (false, LimitStopRead::Stopped(stop)) = (probe.unknown, &read) {
        env.clear_unknown_parks(&stop.entry_uuid);
    }
    let now_ms = env.now_ms();
    let action = decide(&HintInputs {
        finished: env.finished(),
        read: read.clone(),
        probe,
        now_ms,
        auto_migrate,
        auto_continue,
        config_dir_count,
        migrated_at_ms: env.migrated_at(),
    });
    match action {
        LimitAction::Skip(why) => {
            env.skip(why, &read);
            HintFlow::Done
        }
        LimitAction::Park {
            due_at_ms,
            reason: ParkReason::ProbeUnknown,
        } => {
            // A probe that keeps failing is re-tried less and less often.
            let due_at_ms = match &read {
                LimitStopRead::Stopped(stop) => {
                    now_ms + unknown_park_backoff_ms(env.note_unknown_park(&stop.entry_uuid))
                }
                _ => due_at_ms,
            };
            env.park(due_at_ms, ParkReason::ProbeUnknown);
            HintFlow::Done
        }
        LimitAction::Park { due_at_ms, reason } => {
            env.park(due_at_ms, reason);
            HintFlow::Done
        }
        LimitAction::Continue(stop) => {
            env.continue_stop(stop).await;
            HintFlow::Done
        }
        LimitAction::Migrate { park_fallback } => HintFlow::Migrate {
            park_fallback,
            usage,
        },
    }
}

#[cfg(test)]
#[path = "limit_hint_tests.rs"]
mod tests;
