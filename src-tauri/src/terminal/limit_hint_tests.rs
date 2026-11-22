//! The hint path through a fake [`HintEnv`] (plan 2026-11-05 Phase 2): the
//! wiring the pure `decide` tests cannot see.
#![cfg(test)]

use std::sync::Mutex;

use super::*;

const NOW: i64 = 1_800_000_000_000;
const SRC: &str = "/home/u/.claude-jan";

fn stop(resets_at_ms: Option<i64>) -> LimitStop {
    LimitStop {
        entry_uuid: "7d3c2a51-0000-4000-8000-000000000001".to_string(),
        resets_at_ms,
        rate_limit_type: Some("seven_day".to_string()),
        stopped_at_ms: Some(NOW - 10_000_000),
    }
}

fn clean_row() -> AccountUsageInfo {
    AccountUsageInfo {
        config_dir: SRC.to_string(),
        utilization: 0.2,
        ..Default::default()
    }
}

/// What the fake env was asked to do.
#[derive(Debug, Default)]
struct Calls {
    roster_probes: u32,
    own_probes: u32,
    skips: Vec<SkipReason>,
    parks: Vec<(i64, ParkReason)>,
    forgets: u32,
    continued: Vec<LimitStop>,
}

struct Fake {
    auto_migrate: bool,
    auto_continue: bool,
    dirs: usize,
    read: LimitStopRead,
    roster: Vec<AccountUsageInfo>,
    own: AccountUsageInfo,
    known_exhausted: bool,
    finished: bool,
    migrated_at: Option<i64>,
    unknown_parks: Mutex<u32>,
    calls: Mutex<Calls>,
}

impl Fake {
    /// Both switches on, two accounts, a reset stop, a clean roster row.
    fn new(read: LimitStopRead) -> Self {
        Self {
            auto_migrate: true,
            auto_continue: true,
            dirs: 2,
            read,
            roster: vec![clean_row()],
            own: clean_row(),
            known_exhausted: false,
            finished: false,
            migrated_at: None,
            unknown_parks: Mutex::new(0),
            calls: Mutex::new(Calls::default()),
        }
    }

    fn calls(&self) -> std::sync::MutexGuard<'_, Calls> {
        self.calls.lock().unwrap()
    }
}

impl HintEnv for Fake {
    fn switches(&self) -> HintSwitches {
        HintSwitches {
            auto_migrate: self.auto_migrate,
            auto_continue: self.auto_continue,
            continue_after_migration: true,
        }
    }
    fn finished(&self) -> bool {
        self.finished
    }
    fn config_dir_count(&self) -> usize {
        self.dirs
    }
    async fn read_stop(&self) -> LimitStopRead {
        self.read.clone()
    }
    async fn roster_probe(&self) -> Vec<AccountUsageInfo> {
        self.calls().roster_probes += 1;
        self.roster.clone()
    }
    async fn own_probe(&self) -> AccountUsageInfo {
        self.calls().own_probes += 1;
        self.own.clone()
    }
    fn known_exhausted(&self) -> bool {
        self.known_exhausted
    }
    fn row_exhausted(&self, row: &AccountUsageInfo) -> bool {
        row.utilization >= 0.99
    }
    fn migrated_at(&self) -> Option<i64> {
        self.migrated_at
    }
    fn note_unknown_park(&self, _entry_uuid: &str) -> u32 {
        let mut n = self.unknown_parks.lock().unwrap();
        *n += 1;
        *n
    }
    fn clear_unknown_parks(&self, _entry_uuid: &str) {
        *self.unknown_parks.lock().unwrap() = 0;
    }
    fn now_ms(&self) -> i64 {
        NOW
    }
    fn skip(&self, why: SkipReason, _read: &LimitStopRead) {
        self.calls().skips.push(why);
    }
    fn park(&self, due_at_ms: i64, reason: ParkReason) {
        self.calls().parks.push((due_at_ms, reason));
    }
    fn forget_park(&self) {
        self.calls().forgets += 1;
    }
    async fn continue_stop(&self, stop: LimitStop) {
        self.calls().continued.push(stop);
    }
}

fn reset_stop() -> LimitStopRead {
    LimitStopRead::Stopped(stop(Some(NOW - 1_000)))
}

#[tokio::test]
async fn a_single_account_runner_probes_its_own_account_and_continues() {
    let mut env = Fake::new(reset_stop());
    env.dirs = 1;
    env.roster = Vec::new(); // the roster probe returns no rows below two accounts
    let flow = run_hint(&env, SRC).await;
    assert!(matches!(flow, HintFlow::Done));
    let calls = env.calls();
    assert_eq!(calls.own_probes, 1);
    assert_eq!(calls.continued, vec![stop(Some(NOW - 1_000))]);
    assert!(calls.skips.is_empty() && calls.parks.is_empty());
}

#[tokio::test]
async fn the_continue_runs_with_auto_migration_off() {
    let mut env = Fake::new(reset_stop());
    env.auto_migrate = false;
    run_hint(&env, SRC).await;
    assert_eq!(env.calls().continued.len(), 1);
}

#[tokio::test]
async fn the_continue_switch_off_skips_a_reset_stop() {
    let mut env = Fake::new(reset_stop());
    env.auto_continue = false;
    run_hint(&env, SRC).await;
    let calls = env.calls();
    assert!(calls.continued.is_empty());
    assert_eq!(calls.skips, vec![SkipReason::ContinueDisabled]);
}

#[tokio::test]
async fn both_switches_off_read_nothing() {
    let mut env = Fake::new(reset_stop());
    env.auto_migrate = false;
    env.auto_continue = false;
    run_hint(&env, SRC).await;
    let calls = env.calls();
    assert_eq!(calls.skips, vec![SkipReason::Disabled]);
    assert_eq!(calls.roster_probes, 0);
}

#[tokio::test]
async fn a_finished_session_is_never_continued() {
    let mut env = Fake::new(reset_stop());
    env.finished = true;
    run_hint(&env, SRC).await;
    let calls = env.calls();
    assert!(calls.continued.is_empty());
    assert_eq!(calls.skips, vec![SkipReason::Finished]);
    // Final for this stop: a re-entry's retry park is dropped (plan Phase 3).
    assert_eq!(calls.forgets, 1);
}

#[tokio::test]
async fn an_unreadable_transcript_keeps_the_pending_park() {
    let env = Fake::new(LimitStopRead::Unreadable(
        "cannot open transcript".to_string(),
    ));
    run_hint(&env, SRC).await;
    let calls = env.calls();
    assert_eq!(calls.skips, vec![SkipReason::TranscriptUnreadable]);
    assert_eq!(calls.forgets, 0);
}

#[tokio::test]
async fn an_unreadable_transcript_on_an_exhausted_account_keeps_the_pending_park() {
    let mut env = Fake::new(LimitStopRead::Unreadable(
        "cannot open transcript".to_string(),
    ));
    env.auto_migrate = false;
    env.known_exhausted = true;
    env.roster = vec![AccountUsageInfo {
        config_dir: SRC.to_string(),
        utilization: 1.0,
        ..Default::default()
    }];
    env.own = env.roster[0].clone();
    run_hint(&env, SRC).await;
    let calls = env.calls();
    assert_eq!(calls.skips, vec![SkipReason::MigrateDisabled]);
    assert_eq!(calls.forgets, 0);
}

#[tokio::test]
async fn a_stop_not_yet_reset_is_parked_not_skipped() {
    let env = Fake::new(LimitStopRead::Stopped(stop(Some(NOW + 600_000))));
    run_hint(&env, SRC).await;
    let calls = env.calls();
    assert_eq!(
        calls.parks,
        vec![(NOW + 600_000 + 120_000, ParkReason::NotYetReset)]
    );
    assert!(calls.skips.is_empty());
}

#[tokio::test]
async fn an_exhausted_stop_is_handed_back_for_the_migration() {
    let mut env = Fake::new(LimitStopRead::Stopped(stop(Some(NOW + 600_000))));
    env.known_exhausted = true;
    let flow = run_hint(&env, SRC).await;
    match flow {
        HintFlow::Migrate {
            park_fallback,
            usage,
        } => {
            assert_eq!(park_fallback, Some(NOW + 600_000 + 120_000));
            assert_eq!(usage.len(), 1);
        }
        HintFlow::Done => panic!("expected the migration"),
    }
}

#[tokio::test]
async fn a_non_stop_that_cannot_migrate_pays_no_probe() {
    let mut env = Fake::new(LimitStopRead::NotStopped);
    env.auto_migrate = false;
    run_hint(&env, SRC).await;
    let calls = env.calls();
    assert_eq!(calls.roster_probes, 0);
    assert_eq!(calls.own_probes, 0);
    assert_eq!(calls.skips, vec![SkipReason::Echoed]);
}

#[tokio::test]
async fn a_carried_stop_is_left_to_a_recent_migration_and_continued_after() {
    let mut env = Fake::new(LimitStopRead::Stopped(stop(Some(NOW + 86_400_000))));
    env.migrated_at = Some(NOW - 60_000);
    run_hint(&env, SRC).await;
    // Looked at again once the nudge cannot be running, not dropped.
    assert_eq!(
        env.calls().parks,
        vec![(
            NOW - 60_000 + 300_000 + 120_000,
            ParkReason::MigrationNudgeWait
        )]
    );

    // Past the nudge window the transcript says the nudge never landed.
    let mut late = Fake::new(LimitStopRead::Stopped(stop(Some(NOW + 86_400_000))));
    late.migrated_at = Some(NOW - 300_001);
    run_hint(&late, SRC).await;
    assert_eq!(late.calls().continued.len(), 1);
}

#[tokio::test]
async fn a_failing_probe_backs_off() {
    let mut env = Fake::new(reset_stop());
    env.roster = vec![AccountUsageInfo {
        error: Some("API error (401)".to_string()),
        ..clean_row()
    }];
    env.known_exhausted = true;
    for _ in 0..3 {
        run_hint(&env, SRC).await;
    }
    assert_eq!(
        env.calls().parks,
        vec![
            (NOW + 120_000, ParkReason::ProbeUnknown),
            (NOW + 240_000, ParkReason::ProbeUnknown),
            (NOW + 480_000, ParkReason::ProbeUnknown),
        ]
    );
}

#[tokio::test]
async fn an_answered_probe_restarts_the_backoff() {
    let mut env = Fake::new(reset_stop());
    env.roster = vec![AccountUsageInfo {
        error: Some("API error (401)".to_string()),
        ..clean_row()
    }];
    env.known_exhausted = true;
    run_hint(&env, SRC).await;
    run_hint(&env, SRC).await;
    assert_eq!(*env.unknown_parks.lock().unwrap(), 2);
    env.roster = vec![clean_row()];
    env.known_exhausted = false;
    run_hint(&env, SRC).await; // the probe answers: continued
    assert_eq!(*env.unknown_parks.lock().unwrap(), 0);
}

#[test]
fn the_switches_are_read_from_their_own_settings() {
    let sw = |m, c, n| HintSwitches {
        auto_migrate: m,
        auto_continue: c,
        continue_after_migration: n,
    };
    let mut cli = crate::settings::ClaudeCliSettings::default();
    assert_eq!(hint_switches(&cli), sw(true, true, true));
    cli.auto_migrate_on_token_exhaustion = false;
    assert_eq!(hint_switches(&cli), sw(false, true, true));
    cli.auto_migrate_on_token_exhaustion = true;
    cli.auto_continue_after_limit_reset = false;
    assert_eq!(hint_switches(&cli), sw(true, false, true));
    cli.auto_continue_after_limit_reset = true;
    cli.auto_continue_after_migration = false;
    assert_eq!(hint_switches(&cli), sw(true, true, false));
}

#[tokio::test]
async fn a_session_no_longer_stopped_drops_its_pending_park() {
    let env = Fake::new(LimitStopRead::NotStopped);
    run_hint(&env, SRC).await;
    assert_eq!(env.calls().forgets, 1);
    let stopped_env = Fake::new(reset_stop());
    run_hint(&stopped_env, SRC).await;
    assert_eq!(stopped_env.calls().forgets, 0);
}
