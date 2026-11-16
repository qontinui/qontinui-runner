//! The reset-continue decision, its dedup and the menu gate (plan 2026-11-05
//! Phase 2). The probe, the clock and the transcript read are inputs, so the
//! branch table is tested row by row.
#![cfg(test)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::terminal::graceful_exit::ScreenText;

const NOW: i64 = 1_800_000_000_000;

fn stop(resets_at_ms: Option<i64>) -> LimitStop {
    LimitStop {
        entry_uuid: "7d3c2a51-0000-4000-8000-000000000001".to_string(),
        resets_at_ms,
        rate_limit_type: Some("seven_day".to_string()),
        stopped_at_ms: Some(STOPPED_AT),
    }
}

/// When the test stop was hit: well before [`NOW`].
const STOPPED_AT: i64 = NOW - 10_000_000;

fn probe(unknown: bool, known_exhausted: bool) -> ProbeVerdict {
    ProbeVerdict {
        unknown,
        known_exhausted,
        resets_at_ms: None,
    }
}

const FINE: ProbeVerdict = ProbeVerdict {
    unknown: false,
    known_exhausted: false,
    resets_at_ms: None,
};
const DRY: ProbeVerdict = ProbeVerdict {
    unknown: false,
    known_exhausted: true,
    resets_at_ms: None,
};

/// Both toggles on, two accounts, not finished.
fn inputs(read: LimitStopRead, probe: ProbeVerdict) -> HintInputs {
    HintInputs {
        finished: false,
        read,
        probe,
        now_ms: NOW,
        auto_migrate: true,
        auto_continue: true,
        config_dir_count: 2,
        migrated_at_ms: None,
    }
}

fn stopped(resets_at_ms: Option<i64>) -> LimitStopRead {
    LimitStopRead::Stopped(stop(resets_at_ms))
}

// ── The branch table ─────────────────────────────────────────────────────────

#[test]
fn the_margin_attempts_and_fallback_are_the_planned_literals() {
    assert_eq!(LIMIT_RESET_MARGIN_MS, 120_000);
    assert_eq!(MAX_CONTINUE_ATTEMPTS, 3);
    assert_eq!(PARK_WITHOUT_RESET_MS, 3_600_000);
}

#[test]
fn a_finished_session_is_never_continued() {
    let mut i = inputs(stopped(Some(NOW - 1_000)), FINE);
    i.finished = true;
    assert_eq!(decide(&i), LimitAction::Skip(SkipReason::Finished));
}

#[test]
fn a_stop_whose_reset_passed_on_an_account_that_reset_is_continued() {
    let i = inputs(stopped(Some(NOW - 1_000)), FINE);
    assert_eq!(decide(&i), LimitAction::Continue(stop(Some(NOW - 1_000))));
}

#[test]
fn a_stop_with_no_reset_time_on_an_account_that_reset_is_continued() {
    let i = inputs(stopped(None), FINE);
    assert_eq!(decide(&i), LimitAction::Continue(stop(None)));
}

#[test]
fn a_stop_whose_reset_is_still_ahead_is_parked_until_reset_plus_margin() {
    let resets = NOW + 3_600_000;
    let i = inputs(stopped(Some(resets)), FINE);
    assert_eq!(
        decide(&i),
        LimitAction::Park {
            due_at_ms: NOW + 3_600_000 + 120_000,
            reason: ParkReason::NotYetReset,
        }
    );
}

#[test]
fn an_unknown_probe_parks_a_stop_and_never_migrates_it() {
    // An errored probe row: the cached snapshot reads it as exhausted, so
    // `known_exhausted` is true here too, and still nothing migrates.
    let i = inputs(stopped(Some(NOW - 1_000)), probe(true, true));
    assert_eq!(
        decide(&i),
        LimitAction::Park {
            due_at_ms: NOW + 120_000,
            reason: ParkReason::ProbeUnknown,
        }
    );
    // With the reset still ahead the stop itself says the account is limited
    // (as an errored header-probe 429 does), so it takes the migration.
    let ahead = inputs(stopped(Some(NOW + 600_000)), probe(true, true));
    assert_eq!(
        decide(&ahead),
        LimitAction::Migrate {
            park_fallback: Some(NOW + 600_000 + 120_000),
        }
    );
    // A missing row (unknown, not read exhausted) with the reset ahead too.
    let missing = inputs(stopped(Some(NOW + 600_000)), probe(true, false));
    assert_eq!(
        decide(&missing),
        LimitAction::Migrate {
            park_fallback: Some(NOW + 600_000 + 120_000),
        }
    );
}

#[test]
fn with_the_continue_off_a_stop_waiting_only_for_a_continue_is_not_parked() {
    let off = |read, probe| {
        let mut i = inputs(read, probe);
        i.auto_continue = false;
        i
    };
    // Not yet reset, on a fine account: only a continue could follow.
    assert_eq!(
        decide(&off(stopped(Some(NOW + 60_000)), FINE)),
        LimitAction::Skip(SkipReason::ContinueDisabled)
    );
    // Unknown probe after the reset: likewise.
    assert_eq!(
        decide(&off(stopped(Some(NOW - 1_000)), probe(true, true))),
        LimitAction::Skip(SkipReason::ContinueDisabled)
    );
    // Still exhausted on one account: nothing can ever move it.
    let mut single = off(stopped(Some(NOW + 60_000)), DRY);
    single.config_dir_count = 1;
    assert_eq!(
        decide(&single),
        LimitAction::Skip(SkipReason::ContinueDisabled)
    );
    // Still exhausted with a target possible: it migrates, and a cap or
    // no-target refusal reports as before instead of parking for a continue
    // that is switched off.
    assert_eq!(
        decide(&off(stopped(Some(NOW + 60_000)), DRY)),
        LimitAction::Migrate {
            park_fallback: Some(NOW + 60_000 + 120_000)
        }
    );
}

#[test]
fn a_still_exhausted_account_parks_until_its_own_window_resets() {
    // The stop's five-hour reset has passed, but the weekly window is out:
    // park until the probe's reset, not one margin from now.
    let weekly = ProbeVerdict {
        resets_at_ms: Some(NOW + 3 * 86_400_000),
        ..DRY
    };
    let mut single = inputs(stopped(Some(NOW - 60_000)), weekly);
    single.config_dir_count = 1;
    assert_eq!(
        decide(&single),
        LimitAction::Park {
            due_at_ms: NOW + 3 * 86_400_000 + 120_000,
            reason: ParkReason::SingleAccount,
        }
    );
    // The later of the two resets wins.
    assert_eq!(
        exhausted_park_due_at(&stop(Some(NOW + 9_000_000)), &weekly, NOW),
        NOW + 3 * 86_400_000 + 120_000
    );
    assert_eq!(
        exhausted_park_due_at(&stop(None), &DRY, NOW),
        NOW + 3_600_000
    );
}

#[test]
fn the_exhausted_window_reset_is_the_session_window_only_when_only_it_is_full() {
    use crate::commands::ai_settings::AccountUsageInfo;
    let session_full = AccountUsageInfo {
        utilization: 0.4,
        resets_at: Some(1_800_500_000),
        session_utilization: Some(1.0),
        session_resets_at: Some(1_800_010_000),
        ..Default::default()
    };
    assert_eq!(
        exhausted_window_reset_ms(&session_full),
        Some(1_800_010_000_000)
    );
    let weekly_full = AccountUsageInfo {
        utilization: 1.0,
        ..session_full.clone()
    };
    assert_eq!(
        exhausted_window_reset_ms(&weekly_full),
        Some(1_800_500_000_000)
    );
    assert_eq!(
        exhausted_window_reset_ms(&AccountUsageInfo::default()),
        None
    );
    // Exhausted by status alone (neither number at the threshold): the
    // earlier reset, never the later.
    let status_only = AccountUsageInfo {
        utilization: 0.6,
        session_utilization: Some(0.7),
        status: Some("blocked".to_string()),
        ..session_full.clone()
    };
    assert_eq!(
        exhausted_window_reset_ms(&status_only),
        Some(1_800_010_000_000)
    );
}

#[test]
fn the_handler_probes_its_own_account_only_for_a_stop_it_could_continue() {
    let stop_read = stopped(None);
    assert_eq!(probe_source(true, &stop_read, true), ProbeSource::Roster);
    assert_eq!(
        probe_source(true, &LimitStopRead::NotStopped, false),
        ProbeSource::Roster
    );
    assert_eq!(
        probe_source(false, &stop_read, true),
        ProbeSource::OwnAccount
    );
    assert_eq!(probe_source(false, &stop_read, false), ProbeSource::NoRow);
    assert_eq!(
        probe_source(false, &LimitStopRead::NotStopped, true),
        ProbeSource::NoRow
    );
    assert_eq!(
        probe_source(false, &LimitStopRead::Unreadable("x".to_string()), true),
        ProbeSource::NoRow
    );
}

#[test]
fn a_stop_from_before_the_latest_migration_is_left_to_that_migration() {
    // The carried transcript ends at the stop the migration answered, with
    // the OLD account's reset still ahead: never park it until then. While
    // the migration's own nudge can still be running, leave it to that nudge.
    let mut carried = inputs(stopped(Some(NOW + 86_400_000)), FINE);
    carried.migrated_at_ms = Some(NOW - 60_000);
    assert_eq!(
        decide(&carried),
        LimitAction::Park {
            due_at_ms: NOW - 60_000 + 300_000 + 120_000,
            reason: ParkReason::MigrationNudgeWait,
        }
    );
    // Past that window the stop was never answered: continue it, whatever
    // the OLD account's reset says.
    carried.migrated_at_ms = Some(NOW - 300_001);
    assert_eq!(
        decide(&carried),
        LimitAction::Continue(stop(Some(NOW + 86_400_000)))
    );
    assert_eq!(MIGRATION_NUDGE_WINDOW_MS, 300_000);
    // A stop with no timestamp beside a recorded migration: the same.
    let mut untimed = inputs(
        LimitStopRead::Stopped(LimitStop {
            stopped_at_ms: None,
            ..stop(None)
        }),
        FINE,
    );
    untimed.migrated_at_ms = Some(NOW - 60_000);
    assert!(matches!(
        decide(&untimed),
        LimitAction::Park {
            reason: ParkReason::MigrationNudgeWait,
            ..
        }
    ));
    // A destination that is itself dry still takes the next hop.
    // Refused, it parks until the DESTINATION's reset, not the old one's.
    let dry_here = ProbeVerdict {
        resets_at_ms: Some(NOW + 5_000_000),
        ..DRY
    };
    let mut dry_target = inputs(stopped(Some(NOW + 86_400_000)), dry_here);
    dry_target.migrated_at_ms = Some(NOW - 60_000);
    assert_eq!(
        decide(&dry_target),
        LimitAction::Migrate {
            park_fallback: Some(NOW + 5_000_000 + 120_000)
        }
    );
    assert_eq!(probe_park_due_at(&DRY, NOW), NOW + 3_600_000);
    // A NEW stop, hit on the target account after the migration, is decided.
    let mut later = inputs(stopped(Some(NOW - 1_000)), FINE);
    later.migrated_at_ms = Some(STOPPED_AT - 5_000);
    assert_eq!(
        decide(&later),
        LimitAction::Continue(stop(Some(NOW - 1_000)))
    );
}

#[test]
fn the_probe_verdict_reads_a_missing_or_errored_row_as_unknown() {
    use crate::commands::ai_settings::AccountUsageInfo;
    let clean = AccountUsageInfo {
        config_dir: "/a".to_string(),
        ..Default::default()
    };
    let errored = AccountUsageInfo {
        config_dir: "/b".to_string(),
        error: Some("API error (429)".to_string()),
        ..Default::default()
    };
    assert_eq!(
        ProbeVerdict::from_row(Some(&clean), false),
        probe(false, false)
    );
    let clean_with_reset = AccountUsageInfo {
        utilization: 1.0,
        resets_at: Some(1_800_500_000),
        ..clean.clone()
    };
    assert_eq!(
        ProbeVerdict::from_row(Some(&clean_with_reset), true).resets_at_ms,
        Some(1_800_500_000_000)
    );
    // An errored row carries no reset to park on.
    let errored_with_reset = AccountUsageInfo {
        error: Some("API error (429)".to_string()),
        ..clean_with_reset.clone()
    };
    assert_eq!(
        ProbeVerdict::from_row(Some(&errored_with_reset), true).resets_at_ms,
        None
    );
    assert_eq!(
        ProbeVerdict::from_row(Some(&clean), true),
        probe(false, true)
    );
    assert_eq!(
        ProbeVerdict::from_row(Some(&errored), true),
        probe(true, true)
    );
    assert_eq!(ProbeVerdict::from_row(None, false), probe(true, false));
    let usage = vec![clean.clone(), errored.clone()];
    assert_eq!(
        row_for(&usage, "/b").map(|r| r.config_dir.as_str()),
        Some("/b")
    );
    assert!(row_for(&usage, "/c").is_none());
    // The roster probe returns no rows at all below two accounts.
    assert!(row_for(&[], "/a").is_none());
}

#[test]
fn a_hint_is_read_unless_both_switches_are_off() {
    assert!(hint_wanted(true, true));
    assert!(hint_wanted(false, true));
    assert!(hint_wanted(true, false));
    assert!(!hint_wanted(false, false));
}

#[test]
fn a_stop_still_exhausted_takes_the_migration_with_its_park_as_fallback() {
    let resets = NOW + 7_200_000;
    let i = inputs(stopped(Some(resets)), DRY);
    assert_eq!(
        decide(&i),
        LimitAction::Migrate {
            park_fallback: Some(NOW + 7_200_000 + 120_000),
        }
    );
}

#[test]
fn a_stop_still_exhausted_with_migration_off_is_parked() {
    let mut i = inputs(stopped(Some(NOW + 60_000)), DRY);
    i.auto_migrate = false;
    assert_eq!(
        decide(&i),
        LimitAction::Park {
            due_at_ms: NOW + 60_000 + 120_000,
            reason: ParkReason::AutoMigrateOff,
        }
    );
}

#[test]
fn a_stop_still_exhausted_on_a_single_account_runner_is_parked() {
    let mut i = inputs(stopped(Some(NOW + 60_000)), DRY);
    i.config_dir_count = 1;
    assert_eq!(
        decide(&i),
        LimitAction::Park {
            due_at_ms: NOW + 60_000 + 120_000,
            reason: ParkReason::SingleAccount,
        }
    );
}

#[test]
fn echoed_limit_text_on_a_fine_account_is_skipped() {
    let i = inputs(LimitStopRead::NotStopped, FINE);
    assert_eq!(decide(&i), LimitAction::Skip(SkipReason::Echoed));
}

#[test]
fn not_a_stop_on_an_exhausted_account_migrates_as_before() {
    let i = inputs(LimitStopRead::NotStopped, DRY);
    assert_eq!(
        decide(&i),
        LimitAction::Migrate {
            park_fallback: None
        }
    );
    // An errored probe still migrates a non-stop, as it always has.
    let errored = inputs(LimitStopRead::NotStopped, probe(true, true));
    assert_eq!(
        decide(&errored),
        LimitAction::Migrate {
            park_fallback: None
        }
    );
    let mut off = inputs(LimitStopRead::NotStopped, DRY);
    off.auto_migrate = false;
    assert_eq!(decide(&off), LimitAction::Skip(SkipReason::MigrateDisabled));
    let mut single = inputs(LimitStopRead::NotStopped, DRY);
    single.config_dir_count = 1;
    assert_eq!(
        decide(&single),
        LimitAction::Skip(SkipReason::NoOtherAccount)
    );
}

#[test]
fn an_unreadable_transcript_is_never_continued() {
    let fine = inputs(LimitStopRead::Unreadable("truncated".to_string()), FINE);
    assert_eq!(
        decide(&fine),
        LimitAction::Skip(SkipReason::TranscriptUnreadable)
    );
    let dry = inputs(LimitStopRead::Unreadable("truncated".to_string()), DRY);
    assert_eq!(
        decide(&dry),
        LimitAction::Migrate {
            park_fallback: None
        }
    );
}

#[test]
fn the_continue_is_gated_by_its_own_setting_not_by_auto_migration() {
    let mut migrate_off = inputs(stopped(Some(NOW - 1_000)), FINE);
    migrate_off.auto_migrate = false;
    assert_eq!(
        decide(&migrate_off),
        LimitAction::Continue(stop(Some(NOW - 1_000)))
    );
    let mut continue_off = inputs(stopped(Some(NOW - 1_000)), FINE);
    continue_off.auto_continue = false;
    assert_eq!(
        decide(&continue_off),
        LimitAction::Skip(SkipReason::ContinueDisabled)
    );
}

#[test]
fn a_single_account_runner_still_continues_a_reset_stop() {
    let mut i = inputs(stopped(Some(NOW - 1_000)), FINE);
    i.config_dir_count = 1;
    assert_eq!(decide(&i), LimitAction::Continue(stop(Some(NOW - 1_000))));
}

#[test]
fn park_due_never_lands_inside_one_margin_and_falls_back_an_hour() {
    assert_eq!(
        park_due_at(&stop(Some(NOW - 5_000_000)), NOW),
        NOW + 120_000
    );
    assert_eq!(park_due_at(&stop(None), NOW), NOW + 3_600_000);
    assert_eq!(
        park_due_at(&stop(Some(NOW + 1_000)), NOW),
        NOW + 1_000 + 120_000
    );
}

// ── Refusals and events ──────────────────────────────────────────────────────

#[test]
fn a_refused_migration_of_a_stop_parks_it_and_a_non_stop_reports_as_before() {
    assert_eq!(
        migration_refused(Some(NOW + 9_000), ParkReason::NoUsableTarget, NOW),
        RefusalReport::Park {
            due_at_ms: NOW + 9_000,
            reason: ParkReason::NoUsableTarget,
        }
    );
    assert_eq!(
        migration_refused(Some(NOW + 9_000), ParkReason::MigrationCapReached, NOW),
        RefusalReport::Park {
            due_at_ms: NOW + 9_000,
            reason: ParkReason::MigrationCapReached,
        }
    );
    assert_eq!(
        migration_refused(None, ParkReason::NoUsableTarget, NOW),
        RefusalReport::Skipped("no-usable-target")
    );
    assert_eq!(
        migration_refused(None, ParkReason::MigrationCapReached, NOW),
        RefusalReport::Skipped("migration-cap-reached")
    );
}

#[test]
fn report_park_parks_the_terminal_and_emits_session_limit_parked() {
    let mut parked = None;
    let mut emitted = Vec::new();
    report_park(
        "term-1",
        "sid-1",
        "/home/u/.claude-jan",
        NOW + 120_000,
        ParkReason::NoUsableTarget,
        |t, s, due, why| {
            parked = Some((t.to_string(), s.to_string(), due, why));
            true
        },
        |name, payload| emitted.push((name, payload)),
    );
    assert_eq!(
        parked,
        Some((
            "term-1".to_string(),
            "sid-1".to_string(),
            NOW + 120_000,
            "no-usable-target"
        ))
    );
    assert_eq!(emitted.len(), 1);
    assert_eq!(emitted[0].0, "session-limit-parked");
    assert_eq!(emitted[0].1["reason"], "no-usable-target");
    // A re-park the park list calls not worth reporting emits nothing.
    let mut quiet = Vec::new();
    report_park(
        "term-1",
        "sid-1",
        "/home/u/.claude-jan",
        NOW + 240_000,
        ParkReason::NoUsableTarget,
        |_, _, _, _| false,
        |name, payload| quiet.push((name, payload)),
    );
    assert!(quiet.is_empty());
}

#[test]
fn a_refused_migration_park_is_due_within_the_hour() {
    // The source's weekly reset is days away; another account or a cap slot
    // can free up long before.
    assert_eq!(
        migration_refused(Some(NOW + 4 * 86_400_000), ParkReason::NoUsableTarget, NOW),
        RefusalReport::Park {
            due_at_ms: NOW + 3_600_000,
            reason: ParkReason::NoUsableTarget,
        }
    );
}

#[test]
fn a_failing_probe_backs_off_up_to_the_hour() {
    assert_eq!(unknown_park_backoff_ms(1), 120_000);
    assert_eq!(unknown_park_backoff_ms(2), 240_000);
    assert_eq!(unknown_park_backoff_ms(3), 480_000);
    assert_eq!(unknown_park_backoff_ms(40), 3_600_000);
    let mut l = ContinueLedger::default();
    assert_eq!(l.note_unknown_park("s", "e"), 1);
    assert_eq!(l.note_unknown_park("s", "e"), 2);
    assert_eq!(l.note_unknown_park("s", "other"), 1);
}

#[test]
fn only_a_stop_or_a_possible_migration_needs_the_roster_probe() {
    assert!(roster_probe_needed(&stopped(None), false, 1));
    assert!(roster_probe_needed(&LimitStopRead::NotStopped, true, 2));
    assert!(!roster_probe_needed(&LimitStopRead::NotStopped, false, 2));
    assert!(!roster_probe_needed(&LimitStopRead::NotStopped, true, 1));
    assert!(!roster_probe_needed(
        &LimitStopRead::Unreadable("x".to_string()),
        false,
        3
    ));
}

#[tokio::test]
async fn a_dropped_continue_run_does_not_leave_its_stop_in_flight() {
    let ledger: Mutex<Option<ContinueLedger>> = Mutex::new(None);
    let run = run_continue(&ledger, "s", "e", || {}, || std::future::pending());
    // Poll it once (the claim is taken), then drop it mid-watch.
    let mut run = Box::pin(run);
    assert!(futures::poll!(run.as_mut()).is_pending());
    assert_eq!(with_ledger(&ledger, |l| l.claim("s", "e")), Claim::InFlight);
    drop(run);
    assert_eq!(with_ledger(&ledger, |l| l.claim("s", "e")), Claim::Claimed);
}

#[test]
fn a_continue_that_runs_out_of_attempts_emits_its_own_event() {
    let (name, payload) = gave_up_event("sid-1", "/home/u/.claude-jan", "term-1", 3);
    assert_eq!(name, "session-limit-reset-continue-gave-up");
    assert_eq!(payload["attempts"], 3);
    assert_eq!(payload["terminalId"], "term-1");
}

#[test]
fn a_park_emits_session_limit_parked_not_the_skipped_error_event() {
    let (name, payload) = parked_event(
        "0c4e3025-8c5c-4792-a3ca-201ad43e0113",
        "/home/u/.claude-jan",
        NOW + 120_000,
        ParkReason::NotYetReset,
    );
    assert_eq!(name, "session-limit-parked");
    assert_ne!(name, "session-account-migration-skipped");
    assert_eq!(
        payload,
        serde_json::json!({
            "claudeSessionId": "0c4e3025-8c5c-4792-a3ca-201ad43e0113",
            "configDir": "/home/u/.claude-jan",
            "dueAtMs": 1_800_000_120_000_i64,
            "reason": "not-yet-reset",
        })
    );
    let (name, payload) = continued_event("sid", "/home/u/.claude-jan", "term-1", "uuid-1");
    assert_eq!(name, "session-limit-reset-continued");
    assert_eq!(payload["terminalId"], "term-1");
    assert_eq!(payload["entryUuid"], "uuid-1");
}

// ── Dedup ────────────────────────────────────────────────────────────────────

#[test]
fn a_submitted_continue_keeps_its_claim_so_one_stop_is_continued_once() {
    let mut l = ContinueLedger::default();
    assert_eq!(l.claim("s", "e"), Claim::Claimed);
    assert_eq!(l.claim("s", "e"), Claim::InFlight);
    assert_eq!(
        l.settle("s", "e", &IdlePromptOutcome::Submitted),
        Settled::Kept
    );
    assert_eq!(l.claim("s", "e"), Claim::Done);
    // A NEW stop of the same session is a new key.
    assert_eq!(l.claim("s", "e2"), Claim::Claimed);
}

#[test]
fn a_busy_session_counts_as_continued() {
    let mut l = ContinueLedger::default();
    assert_eq!(l.claim("s", "e"), Claim::Claimed);
    assert_eq!(
        l.settle("s", "e", &IdlePromptOutcome::AlreadyBusy),
        Settled::Kept
    );
    assert_eq!(l.claim("s", "e"), Claim::Done);
}

#[test]
fn a_continue_that_does_not_reach_the_pane_is_retried_at_most_three_times() {
    let mut l = ContinueLedger::default();
    let ends = [
        IdlePromptOutcome::GaveUp {
            last_refusal: "numbered choice".to_string(),
        },
        IdlePromptOutcome::TerminalGone,
        IdlePromptOutcome::SubmitFailed("pty closed".to_string()),
    ];
    for (n, end) in ends.iter().enumerate() {
        assert_eq!(l.claim("s", "e"), Claim::Claimed, "attempt {}", n + 1);
        let attempts = n as u32 + 1;
        assert_eq!(
            l.settle("s", "e", end),
            Settled::Released {
                attempts,
                retry: attempts < 3,
            }
        );
    }
    assert_eq!(l.claim("s", "e"), Claim::OutOfAttempts);
}

#[tokio::test]
async fn two_concurrent_hints_for_one_stop_submit_once() {
    let ledger: Mutex<Option<ContinueLedger>> = Mutex::new(None);
    let watched = Arc::new(AtomicUsize::new(0));
    let (release, gate) = tokio::sync::watch::channel(false);
    let watcher = || {
        let watched = watched.clone();
        let mut gate = gate.clone();
        move || async move {
            watched.fetch_add(1, Ordering::SeqCst);
            // Hold the first attempt open until the second hint has arrived.
            let _ = gate.wait_for(|open| *open).await;
            IdlePromptOutcome::Submitted
        }
    };
    let first = run_continue(&ledger, "s", "e", || {}, watcher());
    let second = async {
        tokio::task::yield_now().await;
        let run = run_continue(&ledger, "s", "e", || {}, watcher()).await;
        release.send(true).expect("gate receiver alive");
        run
    };
    // Bounded: with the claim taken only on submit, the second hint starts a
    // second watcher that waits on the gate for ever — a red, not a hang.
    let joined = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(first, second)
    })
    .await;
    assert_eq!(
        watched.load(Ordering::SeqCst),
        1,
        "a second watcher started for the same stop"
    );
    let (a, b) = joined.expect("the two continue runs did not finish");
    assert_eq!(
        a,
        ContinueRun::Ran {
            outcome: IdlePromptOutcome::Submitted,
            settled: Settled::Kept,
        }
    );
    assert_eq!(b, ContinueRun::NotClaimed(Claim::InFlight));
    // And a later hint for the same stop finds it done.
    let later = run_continue(&ledger, "s", "e", || {}, watcher()).await;
    assert_eq!(later, ContinueRun::NotClaimed(Claim::Done));
    assert_eq!(watched.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_menu_gate_runs_before_the_watcher_and_only_when_claimed() {
    let ledger: Mutex<Option<ContinueLedger>> = Mutex::new(None);
    let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let (o1, o2) = (order.clone(), order.clone());
    let run = run_continue(
        &ledger,
        "s",
        "e",
        move || o1.lock().unwrap().push("prepare"),
        move || async move {
            o2.lock().unwrap().push("watch");
            IdlePromptOutcome::GaveUp {
                last_refusal: "x".to_string(),
            }
        },
    )
    .await;
    assert_eq!(*order.lock().unwrap(), vec!["prepare", "watch"]);
    assert!(matches!(
        run,
        ContinueRun::Ran {
            settled: Settled::Released {
                attempts: 1,
                retry: true
            },
            ..
        }
    ));
    // Held by another caller: neither runs.
    with_ledger(&ledger, |l| l.claim("s", "e"));
    let o3 = order.clone();
    let blocked = run_continue(
        &ledger,
        "s",
        "e",
        move || o3.lock().unwrap().push("prepare-2"),
        || async { IdlePromptOutcome::Submitted },
    )
    .await;
    assert_eq!(blocked, ContinueRun::NotClaimed(Claim::InFlight));
    assert_eq!(order.lock().unwrap().len(), 2);
}

// ── The /rate-limit-options menu ─────────────────────────────────────────────

const RATE_LIMIT_OPTIONS_MENU: &str =
    include_str!("fixtures/parked_screens/rate_limit_options_menu.txt");
const IDLE_UNDER_WEEKLY_BANNER: &str =
    include_str!("fixtures/parked_screens/idle_prompt_under_weekly_banner.txt");

fn screen(text: &str) -> ScreenText {
    ScreenText {
        lines: text.lines().map(str::to_string).collect(),
        cursor_row: 0,
        cursor_col: 0,
        cursor_row_dim: None,
    }
}

#[test]
fn the_rate_limit_options_menu_is_recognized() {
    assert!(shows_rate_limit_menu(&screen(RATE_LIMIT_OPTIONS_MENU)));
}

#[test]
fn an_idle_prompt_under_the_banner_is_not_the_menu() {
    assert!(!shows_rate_limit_menu(&screen(IDLE_UNDER_WEEKLY_BANNER)));
}

#[test]
fn the_menu_is_not_read_while_a_turn_is_running() {
    let busy = format!(
        "{RATE_LIMIT_OPTIONS_MENU}\n  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt"
    );
    assert!(!shows_rate_limit_menu(&screen(&busy)));
}

#[test]
fn a_menu_quoted_above_the_bottom_rows_is_history() {
    let mut text = RATE_LIMIT_OPTIONS_MENU.to_string();
    for n in 0..12 {
        text.push_str(&format!("\n● later output line {n}"));
    }
    assert!(!shows_rate_limit_menu(&screen(&text)));
}
