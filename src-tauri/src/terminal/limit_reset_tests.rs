//! The reset-continue decision, its dedup and the menu gate (plan 2026-11-05
//! Phase 2). The probe, the clock and the transcript read are inputs, so the
//! branch table is tested row by row.

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
    }
}

fn probe(unknown: bool, known_exhausted: bool) -> ProbeVerdict {
    ProbeVerdict {
        unknown,
        known_exhausted,
    }
}

const FINE: ProbeVerdict = ProbeVerdict {
    unknown: false,
    known_exhausted: false,
};
const DRY: ProbeVerdict = ProbeVerdict {
    unknown: false,
    known_exhausted: true,
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
    // With the reset still ahead, it waits for the reset.
    let ahead = inputs(stopped(Some(NOW + 600_000)), probe(true, true));
    assert_eq!(
        decide(&ahead),
        LimitAction::Park {
            due_at_ms: NOW + 600_000 + 120_000,
            reason: ParkReason::ProbeUnknown,
        }
    );
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
        migration_refused(Some(NOW + 9_000), ParkReason::NoUsableTarget),
        RefusalReport::Park {
            due_at_ms: NOW + 9_000,
            reason: ParkReason::NoUsableTarget,
        }
    );
    assert_eq!(
        migration_refused(Some(NOW + 9_000), ParkReason::MigrationCapReached),
        RefusalReport::Park {
            due_at_ms: NOW + 9_000,
            reason: ParkReason::MigrationCapReached,
        }
    );
    assert_eq!(
        migration_refused(None, ParkReason::NoUsableTarget),
        RefusalReport::Skipped("no-usable-target")
    );
    assert_eq!(
        migration_refused(None, ParkReason::MigrationCapReached),
        RefusalReport::Skipped("migration-cap-reached")
    );
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
    let (a, b) = tokio::join!(first, second);
    assert_eq!(watched.load(Ordering::SeqCst), 1);
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
