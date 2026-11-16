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
    fn switches(&self) -> (bool, bool) {
        (self.auto_migrate, self.auto_continue)
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
    fn now_ms(&self) -> i64 {
        NOW
    }
    fn skip(&self, why: SkipReason, _read: &LimitStopRead) {
        self.calls().skips.push(why);
    }
    fn park(&self, due_at_ms: i64, reason: ParkReason) {
        self.calls().parks.push((due_at_ms, reason));
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
    let flow = run_hint(&env, false, SRC).await;
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
    run_hint(&env, false, SRC).await;
    assert_eq!(env.calls().continued.len(), 1);
}

#[tokio::test]
async fn the_continue_switch_off_skips_a_reset_stop() {
    let mut env = Fake::new(reset_stop());
    env.auto_continue = false;
    run_hint(&env, false, SRC).await;
    let calls = env.calls();
    assert!(calls.continued.is_empty());
    assert_eq!(calls.skips, vec![SkipReason::ContinueDisabled]);
}

#[tokio::test]
async fn both_switches_off_read_nothing() {
    let mut env = Fake::new(reset_stop());
    env.auto_migrate = false;
    env.auto_continue = false;
    run_hint(&env, false, SRC).await;
    let calls = env.calls();
    assert_eq!(calls.skips, vec![SkipReason::Disabled]);
    assert_eq!(calls.roster_probes, 0);
}

#[tokio::test]
async fn a_finished_session_is_never_continued() {
    let env = Fake::new(reset_stop());
    run_hint(&env, true, SRC).await;
    let calls = env.calls();
    assert!(calls.continued.is_empty());
    assert_eq!(calls.skips, vec![SkipReason::Finished]);
}

#[tokio::test]
async fn a_stop_not_yet_reset_is_parked_not_skipped() {
    let env = Fake::new(LimitStopRead::Stopped(stop(Some(NOW + 600_000))));
    run_hint(&env, false, SRC).await;
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
    let flow = run_hint(&env, false, SRC).await;
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
    run_hint(&env, false, SRC).await;
    let calls = env.calls();
    assert_eq!(calls.roster_probes, 0);
    assert_eq!(calls.own_probes, 0);
    assert_eq!(calls.skips, vec![SkipReason::Echoed]);
}

#[tokio::test]
async fn a_carried_stop_is_left_to_a_recent_migration_and_continued_after() {
    let mut env = Fake::new(LimitStopRead::Stopped(stop(Some(NOW + 86_400_000))));
    env.migrated_at = Some(NOW - 60_000);
    run_hint(&env, false, SRC).await;
    assert_eq!(env.calls().skips, vec![SkipReason::MigratedSinceStop]);

    // Past the nudge window the transcript says the nudge never landed.
    let mut late = Fake::new(LimitStopRead::Stopped(stop(Some(NOW + 86_400_000))));
    late.migrated_at = Some(NOW - 300_001);
    run_hint(&late, false, SRC).await;
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
        run_hint(&env, false, SRC).await;
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
