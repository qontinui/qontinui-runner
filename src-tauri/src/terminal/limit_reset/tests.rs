//! Plan 2026-11-05 Phase 2: the hint's branch table, the continue dedup, the
//! park list and the `/rate-limit-options` menu gate. Each test names the
//! mutation it is the partner of.

use super::*;
use crate::terminal::graceful_exit::ScreenText;

const NOW: i64 = 1_762_300_000_000;
const HOUR: i64 = 60 * 60 * 1000;

fn stop(resets_at_ms: Option<i64>) -> LimitStopRead {
    LimitStopRead::Stopped(LimitStop {
        entry_uuid: "e-1".to_string(),
        resets_at_ms,
        rate_limit_type: Some("seven_day".to_string()),
    })
}

fn inputs(transcript: &LimitStopRead, probe: ProbeVerdict) -> HintInputs<'_> {
    HintInputs {
        finished: false,
        transcript,
        probe,
        now_ms: NOW,
        auto_continue: true,
        migration_block: None,
    }
}

fn continue_of(t: &LimitStopRead) -> HintAction {
    match t {
        LimitStopRead::Stopped(s) => HintAction::Continue(s.clone()),
        other => panic!("not a stop: {other:?}"),
    }
}

// ---- the branch table ------------------------------------------------------

/// The table, row by row. Literals: margin 120000 ms, retry 15 min.
#[test]
fn the_branch_table() {
    let passed = stop(Some(NOW - HOUR));
    let ahead = stop(Some(NOW + HOUR));
    let absent = stop(None);
    let unreadable = LimitStopRead::Unreadable("truncated".to_string());
    use ProbeVerdict::*;
    let rows: Vec<(&str, HintInputs<'_>, HintAction)> = vec![
        (
            "stopped, reset passed, account answered not exhausted: continue",
            inputs(&passed, NotExhausted),
            continue_of(&passed),
        ),
        (
            "stopped, no resetsAt, account answered not exhausted: continue",
            inputs(&absent, NotExhausted),
            continue_of(&absent),
        ),
        (
            "stopped, reset ahead, probe says not exhausted: wait for the reset",
            inputs(&ahead, NotExhausted),
            HintAction::Park {
                due_at_ms: NOW + HOUR + 120_000,
                reason: ParkReason::ResetPending,
            },
        ),
        (
            "stopped, reset passed, probe errored: park and retry, never migrate",
            inputs(&passed, Errored),
            HintAction::Park {
                due_at_ms: NOW + 15 * 60 * 1000,
                reason: ParkReason::ProbeUnknown,
            },
        ),
        (
            "stopped, reset ahead, probe errored: an error reads exhausted, so migrate",
            inputs(&ahead, Errored),
            HintAction::Migrate {
                park_due_at_ms: Some(NOW + HOUR + 120_000),
            },
        ),
        (
            "stopped and exhausted: migrate, parking until the reset if it cannot",
            inputs(&ahead, Exhausted),
            HintAction::Migrate {
                park_due_at_ms: Some(NOW + HOUR + 120_000),
            },
        ),
        (
            "stopped and exhausted with the reset passed: migrate, retry park",
            inputs(&passed, Exhausted),
            HintAction::Migrate {
                park_due_at_ms: Some(NOW + 15 * 60 * 1000),
            },
        ),
        (
            "not stopped, not exhausted: echoed text",
            inputs(&LimitStopRead::NotStopped, NotExhausted),
            HintAction::SkipEchoed,
        ),
        (
            "not stopped, exhausted: the existing migration",
            inputs(&LimitStopRead::NotStopped, Exhausted),
            HintAction::Migrate {
                park_due_at_ms: None,
            },
        ),
        (
            "unreadable, not exhausted: UNKNOWN, nothing typed",
            inputs(&unreadable, NotExhausted),
            HintAction::SkipUnreadable("truncated".to_string()),
        ),
        (
            "unreadable, exhausted: the existing migration",
            inputs(&unreadable, Exhausted),
            HintAction::Migrate {
                park_due_at_ms: None,
            },
        ),
    ];
    for (name, i, want) in rows {
        assert_eq!(decide_hint(&i), want, "{name}");
    }
}

/// Partner: continue on `NotStopped` + not exhausted.
#[test]
fn an_echoed_banner_is_never_continued() {
    let t = LimitStopRead::NotStopped;
    for probe in [ProbeVerdict::NotExhausted, ProbeVerdict::NotProbed] {
        assert_eq!(decide_hint(&inputs(&t, probe)), HintAction::SkipEchoed);
    }
}

/// Partner: drop the `resetsAt` comparison — a stop whose reset is still
/// ahead is parked, never continued early, even when the probe says the
/// account is not exhausted.
#[test]
fn a_stop_whose_reset_is_ahead_is_parked_not_continued() {
    let t = stop(Some(NOW + 1));
    for probe in [ProbeVerdict::NotExhausted, ProbeVerdict::NotProbed] {
        assert_eq!(
            decide_hint(&inputs(&t, probe)),
            HintAction::Park {
                due_at_ms: NOW + 1 + LIMIT_RESET_MARGIN_MS,
                reason: ParkReason::ResetPending,
            }
        );
    }
    // At the reset instant the margin is still ahead; at reset + margin it
    // has passed.
    let at = stop(Some(NOW));
    assert_eq!(
        decide_hint(&inputs(&at, ProbeVerdict::NotExhausted)),
        HintAction::Park {
            due_at_ms: NOW + LIMIT_RESET_MARGIN_MS,
            reason: ParkReason::ResetPending,
        }
    );
    let after = stop(Some(NOW - LIMIT_RESET_MARGIN_MS));
    assert_eq!(
        decide_hint(&inputs(&after, ProbeVerdict::NotExhausted)),
        continue_of(&after)
    );
}

/// Partner: gate the continue on `auto_migrate_on_token_exhaustion`. The
/// migration switch being off must not stop a continue, and the continue
/// switch being off must.
#[test]
fn the_continue_has_its_own_switch() {
    let t = stop(Some(NOW - HOUR));
    let mut i = inputs(&t, ProbeVerdict::NotExhausted);
    i.migration_block = migration_block(false, 3);
    assert_eq!(i.migration_block, Some(ParkReason::MigrationDisabled));
    assert_eq!(decide_hint(&i), continue_of(&t));
    i.auto_continue = false;
    assert_eq!(decide_hint(&i), HintAction::SkipContinueDisabled);
    // And a migration still runs with the continue switched off.
    let mut m = inputs(&t, ProbeVerdict::Exhausted);
    m.auto_continue = false;
    assert!(matches!(decide_hint(&m), HintAction::Migrate { .. }));
}

/// Partner: restore the early fewer-than-two-accounts return. A lone
/// account's refresh probes nothing, and its stop is continued once the reset
/// has passed.
#[test]
fn a_single_account_runner_continues_after_the_reset() {
    let t = stop(Some(NOW - HOUR));
    let mut i = inputs(&t, probe_verdict(FreshProbe::NotProbed, false));
    i.migration_block = migration_block(true, 1);
    assert_eq!(i.migration_block, Some(ParkReason::SingleAccount));
    assert!(needs_probe(&t, i.migration_block, true));
    assert_eq!(decide_hint(&i), continue_of(&t));
    // With no resetsAt and no probe, nothing says the account came back:
    // nothing typed, and nothing parked to re-read forever.
    let absent = stop(None);
    let mut j = inputs(&absent, ProbeVerdict::NotProbed);
    j.migration_block = Some(ParkReason::SingleAccount);
    assert_eq!(decide_hint(&j), HintAction::SkipNoEvidence);
}

/// Partner: migrate on a probe `error`. Once the stop's reset has passed the
/// error is no evidence the account is still exhausted.
#[test]
fn an_unknown_probe_after_the_reset_parks_and_never_migrates() {
    let t = stop(Some(NOW - 1));
    let i = inputs(&t, probe_verdict(FreshProbe::Errored, true));
    assert_eq!(i.probe, ProbeVerdict::Errored);
    assert_eq!(
        decide_hint(&i),
        HintAction::Park {
            due_at_ms: NOW + PARK_RETRY_MS,
            reason: ParkReason::ProbeUnknown,
        }
    );
}

/// Partner: continue a session whose record has `finished_at`.
#[test]
fn a_finished_session_is_never_continued_or_migrated() {
    for (t, probe) in [
        (stop(Some(NOW - HOUR)), ProbeVerdict::NotExhausted),
        (stop(Some(NOW - HOUR)), ProbeVerdict::Exhausted),
        (LimitStopRead::NotStopped, ProbeVerdict::Exhausted),
    ] {
        let mut i = inputs(&t, probe);
        i.finished = true;
        assert_eq!(decide_hint(&i), HintAction::SkipFinished, "{t:?} {probe:?}");
    }
}

/// An exhausted stop that no migration can serve is parked until its reset,
/// where before Phase 2 the handler returned with nothing scheduled.
#[test]
fn an_exhausted_stop_that_cannot_migrate_is_parked() {
    let t = stop(Some(NOW + HOUR));
    for (auto_migrate, accounts, reason) in [
        (false, 3, ParkReason::MigrationDisabled),
        (true, 1, ParkReason::SingleAccount),
    ] {
        let mut i = inputs(&t, ProbeVerdict::Exhausted);
        i.migration_block = migration_block(auto_migrate, accounts);
        assert_eq!(
            decide_hint(&i),
            HintAction::Park {
                due_at_ms: NOW + HOUR + LIMIT_RESET_MARGIN_MS,
                reason,
            }
        );
    }
    // A session that did not stop at a limit keeps the old skip.
    let mut n = inputs(&LimitStopRead::NotStopped, ProbeVerdict::Exhausted);
    n.migration_block = migration_block(false, 3);
    assert_eq!(
        decide_hint(&n),
        HintAction::SkipMigrationBlocked(ParkReason::MigrationDisabled)
    );
}

/// Partner: park nothing on `no-usable-target` (or the cap).
#[test]
fn a_refused_migration_of_a_stop_parks() {
    let due = NOW + HOUR;
    for reason in [ParkReason::NoUsableTarget, ParkReason::MigrationCapReached] {
        assert_eq!(
            on_refused_migration(Some(due), reason),
            RefusedMigration::Park {
                due_at_ms: due,
                reason
            }
        );
        assert_eq!(
            on_refused_migration(None, reason),
            RefusedMigration::ReportSkipped
        );
    }
    assert_eq!(ParkReason::NoUsableTarget.as_str(), "no-usable-target");
    assert_eq!(
        ParkReason::MigrationCapReached.as_str(),
        "migration-cap-reached"
    );
}

/// The probe is skipped only where it can decide nothing: no stop, and no
/// migration possible.
#[test]
fn the_probe_runs_only_where_it_decides_something() {
    let s = stop(Some(NOW));
    assert!(needs_probe(&s, Some(ParkReason::MigrationDisabled), true));
    assert!(needs_probe(&LimitStopRead::NotStopped, None, true));
    assert!(needs_probe(&s, None, false), "a migration can still run");
    assert!(!needs_probe(&s, Some(ParkReason::SingleAccount), false));
    assert!(!needs_probe(
        &LimitStopRead::NotStopped,
        Some(ParkReason::MigrationDisabled),
        true
    ));
    assert!(!needs_probe(
        &LimitStopRead::Unreadable("x".into()),
        Some(ParkReason::SingleAccount),
        true
    ));
}

#[test]
fn the_probe_verdict() {
    assert_eq!(
        probe_verdict(FreshProbe::Answered, false),
        ProbeVerdict::NotExhausted
    );
    assert_eq!(
        probe_verdict(FreshProbe::Answered, true),
        ProbeVerdict::Exhausted
    );
    assert_eq!(
        probe_verdict(FreshProbe::Errored, true),
        ProbeVerdict::Errored
    );
    assert_eq!(
        probe_verdict(FreshProbe::NotProbed, false),
        ProbeVerdict::NotProbed
    );
    // An unexpired sample still counts when this refresh probed nothing.
    assert_eq!(
        probe_verdict(FreshProbe::NotProbed, true),
        ProbeVerdict::Exhausted
    );
}

// ---- the dedup --------------------------------------------------------------

/// Partner: continue twice for one `entry_uuid`.
#[test]
fn one_stop_is_continued_once() {
    let mut c = ContinueClaims::default();
    assert_eq!(c.try_claim("s", "e", NOW), Claim::Granted { attempt: 1 });
    c.settle("s", "e", true, NOW);
    assert_eq!(c.try_claim("s", "e", NOW + 1), Claim::Delivered);
    // A new stop entry of the same session is a new claim.
    assert_eq!(
        c.try_claim("s", "e2", NOW + 2),
        Claim::Granted { attempt: 1 }
    );
}

/// Partner: claim only on submit. Two hints for one stop arrive before either
/// watcher reports; only the first may type.
#[test]
fn two_concurrent_hints_for_one_stop_submit_once() {
    let mut c = ContinueClaims::default();
    assert_eq!(c.try_claim("s", "e", NOW), Claim::Granted { attempt: 1 });
    assert_eq!(c.try_claim("s", "e", NOW), Claim::InFlight);
}

/// A give-up releases the claim and spends an attempt; three spent attempts
/// end the retries for that stop.
#[test]
fn a_stop_is_retried_at_most_three_times() {
    let mut c = ContinueClaims::default();
    for attempt in 1..=MAX_CONTINUE_ATTEMPTS {
        assert_eq!(c.try_claim("s", "e", NOW), Claim::Granted { attempt });
        let may_retry = c.settle("s", "e", false, NOW);
        assert_eq!(
            may_retry,
            attempt < MAX_CONTINUE_ATTEMPTS,
            "attempt {attempt}"
        );
    }
    assert_eq!(MAX_CONTINUE_ATTEMPTS, 3);
    assert_eq!(c.try_claim("s", "e", NOW), Claim::AttemptsSpent);
}

/// With the continue switched off, a stop whose only way out is a continue is
/// skipped, not parked: such a park could only ever end in a skip.
#[test]
fn with_the_continue_off_nothing_is_parked_for_a_continue() {
    let ahead = stop(Some(NOW + HOUR));
    let passed = stop(Some(NOW - HOUR));
    fn off(t: &LimitStopRead, probe: ProbeVerdict, block: Option<ParkReason>) -> HintInputs<'_> {
        HintInputs {
            auto_continue: false,
            migration_block: block,
            ..inputs(t, probe)
        }
    }
    let blocked = Some(ParkReason::MigrationDisabled);
    for (i, name) in [
        (
            off(&ahead, ProbeVerdict::NotExhausted, None),
            "reset pending",
        ),
        (
            off(&ahead, ProbeVerdict::Exhausted, blocked),
            "exhausted, migration off",
        ),
        (
            off(&passed, ProbeVerdict::Errored, blocked),
            "probe unknown, migration off",
        ),
    ] {
        assert_eq!(decide_hint(&i), HintAction::SkipContinueDisabled, "{name}");
    }
    // A probe-unknown park can still end in a migration: it is kept.
    assert_eq!(
        decide_hint(&off(&passed, ProbeVerdict::Errored, None)),
        HintAction::Park {
            due_at_ms: NOW + PARK_RETRY_MS,
            reason: ParkReason::ProbeUnknown,
        }
    );
}

/// An in-flight claim is held by a live attempt and never ages out under it.
#[test]
fn an_in_flight_claim_is_not_forgotten() {
    let mut c = ContinueClaims::default();
    assert_eq!(c.try_claim("s", "e", NOW), Claim::Granted { attempt: 1 });
    assert_eq!(c.try_claim("s", "e", NOW + 8 * 24 * HOUR), Claim::InFlight);
}

/// A guard dropped unsettled — a panic mid-attempt — spends the attempt and
/// releases the claim; `release` gives it back without spending one.
#[test]
fn a_dropped_claim_guard_never_wedges_the_stop() {
    let claims = Mutex::new(ContinueClaims::default());
    assert_eq!(
        lock(&claims).try_claim("s", "e", NOW),
        Claim::Granted { attempt: 1 }
    );
    drop(ClaimGuard::new(&claims, "s", "e"));
    assert!(!lock(&claims).handled("s", "e"));
    assert_eq!(
        lock(&claims).try_claim("s", "e", NOW),
        Claim::Granted { attempt: 2 }
    );
    ClaimGuard::new(&claims, "s", "e").release(NOW);
    assert_eq!(
        lock(&claims).try_claim("s", "e", NOW),
        Claim::Granted { attempt: 2 }
    );
}

/// Partner: dispatch every continue at once. 97 reset sessions are typed at
/// six a minute.
#[test]
fn continues_are_paced_six_a_minute() {
    assert_eq!(LIMIT_CONTINUE_PER_MINUTE, 6);
    let mut p = ContinuePacer::default();
    let waits: Vec<i64> = (0..97).map(|_| p.reserve(NOW)).collect();
    assert_eq!(&waits[..3], &[0, 10_000, 20_000]);
    assert_eq!(waits[96], 96 * 10_000);
    // Once the queue has drained, the next one goes at once.
    assert_eq!(p.reserve(NOW + 97 * 10_000), 0);
}

#[test]
fn an_old_claim_is_forgotten() {
    let mut c = ContinueClaims::default();
    assert_eq!(c.try_claim("s", "e", NOW), Claim::Granted { attempt: 1 });
    c.settle("s", "e", true, NOW);
    let later = NOW + 8 * 24 * HOUR;
    assert_eq!(c.try_claim("s", "e", later), Claim::Granted { attempt: 1 });
}

// ---- the park list ------------------------------------------------------------

fn a_park(terminal: &str, due: i64) -> Park {
    Park {
        terminal_id: terminal.to_string(),
        claude_session_id: format!("sid-{terminal}"),
        due_at_ms: due,
        reason: ParkReason::ResetPending,
    }
}

#[test]
fn parks_are_taken_when_due_earliest_first_and_bounded() {
    let mut l = ParkList::default();
    l.park(a_park("b", NOW - 1));
    l.park(a_park("a", NOW - 5));
    l.park(a_park("c", NOW + 1));
    l.park(a_park("d", NOW));
    let taken: Vec<String> = l
        .take_due(NOW, 2)
        .into_iter()
        .map(|p| p.terminal_id)
        .collect();
    assert_eq!(taken, ["a", "b"]);
    let rest: Vec<String> = l
        .take_due(NOW, 10)
        .into_iter()
        .map(|p| p.terminal_id)
        .collect();
    assert_eq!(rest, ["d"]);
    assert!(l.get("c").is_some(), "not yet due");
}

#[test]
fn a_newer_park_replaces_the_older_and_unpark_drops_it() {
    let mut l = ParkList::default();
    l.park(a_park("t", NOW + HOUR));
    l.park(a_park("t", NOW + 2 * HOUR));
    assert_eq!(l.get("t").map(|p| p.due_at_ms), Some(NOW + 2 * HOUR));
    assert!(l.unpark("t").is_some());
    assert!(l.get("t").is_none());
}

#[test]
fn a_closed_terminal_leaves_the_park_list() {
    let mut l = ParkList::default();
    l.park(a_park("open", NOW));
    l.park(a_park("closed", NOW));
    l.retain_live(|t| t == "open");
    assert!(l.get("closed").is_none());
    assert!(l.get("open").is_some());
}

/// Partner: send parks through `emit_skipped`.
#[test]
fn a_park_emits_session_limit_parked_not_the_skipped_error() {
    assert_eq!(PARKED_EVENT, "session-limit-parked");
    assert_ne!(PARKED_EVENT, "session-account-migration-skipped");
    let p = parked_payload(
        "sid",
        "tid",
        "/home/u/.claude-a",
        NOW,
        ParkReason::ResetPending,
    );
    assert_eq!(p["claudeSessionId"], "sid");
    assert_eq!(p["terminalId"], "tid");
    assert_eq!(p["configDir"], "/home/u/.claude-a");
    assert_eq!(p["dueAtMs"], NOW);
    assert_eq!(p["reason"], "reset-pending");
    assert_eq!(CONTINUED_EVENT, "session-limit-reset-continued");
    let c = continued_payload("sid", "tid", "/d", "e-1");
    assert_eq!(c["entryUuid"], "e-1");
}

// ---- the `/rate-limit-options` menu gate --------------------------------------

const MENU: &str = include_str!("../fixtures/parked_screens/rate_limit_options_menu.txt");
const IDLE: &str = include_str!("../fixtures/parked_screens/idle_prompt_under_weekly_banner.txt");

fn screen(text: &str) -> ScreenText {
    ScreenText {
        lines: text.lines().map(str::to_string).collect(),
        cursor_row: 4,
        cursor_col: 3,
        cursor_row_dim: None,
    }
}

/// The pinned menu, also under a long transcript and above trailing blank
/// rows, reads as the menu: one `Esc` is warranted.
#[test]
fn the_pinned_menu_reads_as_the_menu() {
    assert!(screen_shows_rate_limit_options_menu(&screen(MENU)));
    let mut tall = String::new();
    for i in 0..40 {
        tall.push_str(&format!("● transcript row {i}\n"));
    }
    tall.push_str(MENU);
    tall.push_str("\n\n\n");
    assert!(screen_shows_rate_limit_options_menu(&screen(&tall)));
}

/// The idle prompt under the banner needs no `Esc`, and neither does a
/// screen quoting the menu text far above an idle box.
#[test]
fn an_idle_prompt_is_not_the_menu() {
    assert!(!screen_shows_rate_limit_options_menu(&screen(IDLE)));
    let mut quoted = String::from(MENU);
    quoted.push('\n');
    for i in 0..20 {
        quoted.push_str(&format!("● later output {i}\n"));
    }
    quoted.push_str(IDLE);
    assert!(!screen_shows_rate_limit_options_menu(&screen(&quoted)));
}

/// An `Esc` during a turn would interrupt it.
#[test]
fn a_busy_screen_never_gets_the_esc() {
    let busy = format!("{MENU}\n  ⏵⏵ bypass permissions on · esc to interrupt");
    assert!(!screen_shows_rate_limit_options_menu(&screen(&busy)));
}

/// Menu text quoted in the bottom rows above an idle box is not the menu:
/// the footer must be at the bottom.
#[test]
fn menu_text_above_an_idle_box_is_not_the_menu() {
    let quoted = format!("{MENU}\n{IDLE}");
    assert!(!screen_shows_rate_limit_options_menu(&screen(&quoted)));
}

/// A stop option a narrow pane wrapped onto two rows still reads.
#[test]
fn a_wrapped_stop_option_still_reads() {
    let wrapped = MENU.replace(
        "1. Stop and wait for limit to reset",
        "1. Stop and wait for limit\n    to reset",
    );
    assert_ne!(wrapped, MENU);
    assert!(screen_shows_rate_limit_options_menu(&screen(&wrapped)));
}

/// Other menus with `Esc to cancel` are not this one.
#[test]
fn another_dialog_is_not_the_menu() {
    let other =
        " Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Enter to confirm · Esc to cancel";
    assert!(!screen_shows_rate_limit_options_menu(&screen(other)));
}

/// One line, so typing it into a PTY submits it once.
#[test]
fn the_nudge_is_one_line() {
    assert!(!LIMIT_RESET_NUDGE.contains('\n'));
    assert!(LIMIT_RESET_NUDGE.contains("limit has now reset"));
}

// ---- the sequence (`run_hint`), through a fake app ---------------------------
//
// These enter where the handler hands over, so the wiring the plan's
// mutation partners name — the switches and account count read before the
// probe, the claim taken before the watcher, parks reported on their own
// event, the migration's claim on the stop — is what is under test.

use std::sync::atomic::{AtomicUsize, Ordering};

struct Fake {
    settings: HintSettings,
    transcript: LimitStopRead,
    probe: ProbeVerdict,
    prompt_outcome: IdlePromptOutcome,
    migration: MigrationAttempt,
    claims: Mutex<ContinueClaims>,
    parks: Mutex<ParkList>,
    pacer: Mutex<ContinuePacer>,
    events: Mutex<Vec<(String, Value)>>,
    skipped: Mutex<Vec<String>>,
    sleeps: Mutex<Vec<i64>>,
    probes: AtomicUsize,
    prompts: AtomicUsize,
    menu_closes: AtomicUsize,
    migrations: AtomicUsize,
}

impl Fake {
    fn new(transcript: LimitStopRead, probe: ProbeVerdict) -> Self {
        Self {
            settings: HintSettings {
                auto_migrate: true,
                auto_continue: true,
                account_count: 3,
            },
            transcript,
            probe,
            prompt_outcome: IdlePromptOutcome::Submitted,
            migration: MigrationAttempt::Migrated,
            claims: Mutex::new(ContinueClaims::default()),
            parks: Mutex::new(ParkList::default()),
            pacer: Mutex::new(ContinuePacer::default()),
            events: Mutex::new(Vec::new()),
            skipped: Mutex::new(Vec::new()),
            sleeps: Mutex::new(Vec::new()),
            probes: AtomicUsize::new(0),
            prompts: AtomicUsize::new(0),
            menu_closes: AtomicUsize::new(0),
            migrations: AtomicUsize::new(0),
        }
    }

    fn event_names(&self) -> Vec<String> {
        lock(&self.events).iter().map(|(n, _)| n.clone()).collect()
    }
}

impl HintEnv for Fake {
    fn settings(&self) -> HintSettings {
        self.settings
    }
    async fn read_transcript(&self) -> LimitStopRead {
        self.transcript.clone()
    }
    async fn probe(&self) -> ProbeVerdict {
        self.probes.fetch_add(1, Ordering::SeqCst);
        tokio::task::yield_now().await;
        self.probe
    }
    fn now_ms(&self) -> i64 {
        NOW
    }
    fn claims(&self) -> &Mutex<ContinueClaims> {
        &self.claims
    }
    fn parks(&self) -> &Mutex<ParkList> {
        &self.parks
    }
    fn pacer(&self) -> &Mutex<ContinuePacer> {
        &self.pacer
    }
    async fn sleep_ms(&self, ms: i64) {
        lock(&self.sleeps).push(ms);
    }
    fn close_rate_limit_menu(&self) {
        self.menu_closes.fetch_add(1, Ordering::SeqCst);
    }
    async fn prompt_when_idle(&self, prompt: &str) -> IdlePromptOutcome {
        assert_eq!(prompt, LIMIT_RESET_NUDGE);
        self.prompts.fetch_add(1, Ordering::SeqCst);
        // A real watcher waits seconds; let a concurrent hint run meanwhile.
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        self.prompt_outcome.clone()
    }
    fn emit(&self, event: &'static str, payload: Value) {
        lock(&self.events).push((event.to_string(), payload));
    }
    fn emit_skipped(&self, reason: &str) {
        lock(&self.skipped).push(reason.to_string());
    }
    async fn migrate(&self) -> MigrationAttempt {
        self.migrations.fetch_add(1, Ordering::SeqCst);
        self.migration
    }
}

fn session() -> HintSession {
    HintSession {
        terminal_id: "t-1".to_string(),
        claude_session_id: "s-1".to_string(),
        finished: false,
        config_dir: "/home/u/.claude-jan".to_string(),
    }
}

/// Partner: restore the early fewer-than-two-accounts return before the
/// probe. A single-account runner's reset stop is continued.
#[tokio::test]
async fn sequence_a_single_account_runner_continues() {
    let mut f = Fake::new(stop(Some(NOW - HOUR)), ProbeVerdict::NotProbed);
    f.settings.account_count = 1;
    run_hint(&f, &session()).await;
    assert_eq!(f.prompts.load(Ordering::SeqCst), 1);
    assert_eq!(f.menu_closes.load(Ordering::SeqCst), 1);
    assert_eq!(f.migrations.load(Ordering::SeqCst), 0);
    assert_eq!(f.event_names(), [CONTINUED_EVENT]);
    assert!(lock(&f.claims).handled("s-1", "e-1"));
}

/// Partner: gate the continue on `auto_migrate_on_token_exhaustion`.
#[tokio::test]
async fn sequence_migration_off_still_continues() {
    let mut f = Fake::new(stop(Some(NOW - HOUR)), ProbeVerdict::NotExhausted);
    f.settings.auto_migrate = false;
    run_hint(&f, &session()).await;
    assert_eq!(f.prompts.load(Ordering::SeqCst), 1);
}

/// Partner: claim only on submit. Two hints for one stop arrive together
/// (the boot shape); one prompt is typed.
#[tokio::test]
async fn sequence_two_concurrent_hints_type_one_prompt() {
    let f = Fake::new(stop(Some(NOW - HOUR)), ProbeVerdict::NotExhausted);
    let s = session();
    tokio::join!(run_hint(&f, &s), run_hint(&f, &s));
    assert_eq!(f.prompts.load(Ordering::SeqCst), 1);
    // And a later hint for the delivered stop does nothing, not even probe.
    let probes = f.probes.load(Ordering::SeqCst);
    run_hint(&f, &s).await;
    assert_eq!(f.prompts.load(Ordering::SeqCst), 1);
    assert_eq!(f.probes.load(Ordering::SeqCst), probes);
}

/// Partner: send parks through `emit_skipped`.
#[tokio::test]
async fn sequence_a_park_emits_session_limit_parked() {
    let f = Fake::new(stop(Some(NOW + HOUR)), ProbeVerdict::NotExhausted);
    run_hint(&f, &session()).await;
    assert_eq!(f.event_names(), [PARKED_EVENT]);
    assert!(lock(&f.skipped).is_empty());
    assert_eq!(f.prompts.load(Ordering::SeqCst), 0);
    let park = lock(&f.parks).get("t-1").cloned().expect("parked");
    assert_eq!(park.due_at_ms, NOW + HOUR + LIMIT_RESET_MARGIN_MS);
    assert_eq!(park.reason, ParkReason::ResetPending);
}

/// Partner: park nothing on `no-usable-target`. A stopped session is parked;
/// one that did not stop keeps the skipped report.
#[tokio::test]
async fn sequence_a_refused_migration_parks_a_stop_and_reports_the_rest() {
    let mut f = Fake::new(stop(Some(NOW + HOUR)), ProbeVerdict::Exhausted);
    f.migration = MigrationAttempt::Blocked(ParkReason::NoUsableTarget);
    run_hint(&f, &session()).await;
    assert_eq!(f.event_names(), [PARKED_EVENT]);
    assert!(lock(&f.skipped).is_empty());
    assert!(!lock(&f.claims).handled("s-1", "e-1"), "claim given back");

    let mut g = Fake::new(LimitStopRead::NotStopped, ProbeVerdict::Exhausted);
    g.migration = MigrationAttempt::Blocked(ParkReason::NoUsableTarget);
    run_hint(&g, &session()).await;
    assert!(g.event_names().is_empty());
    assert_eq!(*lock(&g.skipped), ["no-usable-target"]);
}

/// The resumed pane of a migrated session repaints the old account's banner,
/// and its transcript still ends at the old account's stop: that hint must
/// neither park the session against the old reset nor type a second nudge.
#[tokio::test]
async fn sequence_a_migrated_stop_is_not_parked_or_continued_on_the_new_pane() {
    let f = Fake::new(stop(Some(NOW + 5 * 24 * HOUR)), ProbeVerdict::Exhausted);
    run_hint(&f, &session()).await;
    assert_eq!(f.migrations.load(Ordering::SeqCst), 1);
    assert!(lock(&f.claims).handled("s-1", "e-1"));
    // The new pane, on an account that is not exhausted.
    let mut new_pane = session();
    new_pane.terminal_id = "t-2".to_string();
    new_pane.config_dir = "/home/u/.claude-tiohorst".to_string();
    let probes = f.probes.load(Ordering::SeqCst);
    run_hint(&f, &new_pane).await;
    assert!(f.event_names().is_empty(), "{:?}", f.event_names());
    assert_eq!(f.prompts.load(Ordering::SeqCst), 0);
    assert_eq!(f.probes.load(Ordering::SeqCst), probes);
}

/// A migration that did not run gives the stop's claim back.
#[tokio::test]
async fn sequence_a_migration_that_did_not_run_releases_the_stop() {
    let mut f = Fake::new(stop(Some(NOW + HOUR)), ProbeVerdict::Exhausted);
    f.migration = MigrationAttempt::NotMigrated;
    run_hint(&f, &session()).await;
    assert!(!lock(&f.claims).handled("s-1", "e-1"));
}

/// Partner: drop the retry park. An attempt that gave up is parked for a
/// retry while attempts remain, and not after the last.
#[tokio::test]
async fn sequence_a_give_up_is_retried_until_the_attempts_run_out() {
    let mut f = Fake::new(stop(Some(NOW - HOUR)), ProbeVerdict::NotExhausted);
    f.prompt_outcome = IdlePromptOutcome::GaveUp {
        last_refusal: "a numbered choice".to_string(),
    };
    for attempt in 1..=MAX_CONTINUE_ATTEMPTS {
        lock(&f.parks).unpark("t-1");
        run_hint(&f, &session()).await;
        let park = lock(&f.parks).get("t-1").cloned();
        if attempt < MAX_CONTINUE_ATTEMPTS {
            let park = park.expect("retry parked");
            assert_eq!(park.reason, ParkReason::ContinueRetry);
            assert_eq!(park.due_at_ms, NOW + PARK_RETRY_MS);
        } else {
            assert!(park.is_none(), "no retry after the last attempt");
        }
    }
    assert_eq!(f.prompts.load(Ordering::SeqCst), 3);
    run_hint(&f, &session()).await;
    assert_eq!(f.prompts.load(Ordering::SeqCst), 3, "attempts spent");
}

/// Partner: continue a session whose record has `finished_at`.
#[tokio::test]
async fn sequence_a_finished_session_is_left_alone() {
    let f = Fake::new(stop(Some(NOW - HOUR)), ProbeVerdict::Exhausted);
    let mut s = session();
    s.finished = true;
    run_hint(&f, &s).await;
    assert_eq!(f.prompts.load(Ordering::SeqCst), 0);
    assert_eq!(f.migrations.load(Ordering::SeqCst), 0);
    assert!(f.event_names().is_empty());
}

/// Partner: migrate on a probe `error` after the reset.
#[tokio::test]
async fn sequence_an_unknown_probe_after_the_reset_parks() {
    let f = Fake::new(stop(Some(NOW - HOUR)), ProbeVerdict::Errored);
    run_hint(&f, &session()).await;
    assert_eq!(f.migrations.load(Ordering::SeqCst), 0);
    assert_eq!(f.event_names(), [PARKED_EVENT]);
}

/// Nothing the probe could decide: no probe is spent, as before Phase 2.
#[tokio::test]
async fn sequence_no_probe_when_nothing_can_follow() {
    let mut f = Fake::new(LimitStopRead::NotStopped, ProbeVerdict::Exhausted);
    f.settings.auto_migrate = false;
    run_hint(&f, &session()).await;
    let mut g = Fake::new(stop(Some(NOW - HOUR)), ProbeVerdict::NotExhausted);
    g.settings.auto_continue = false;
    g.settings.account_count = 1;
    run_hint(&g, &session()).await;
    assert_eq!(f.probes.load(Ordering::SeqCst), 0);
    assert_eq!(g.probes.load(Ordering::SeqCst), 0);
    assert_eq!(g.prompts.load(Ordering::SeqCst), 0);
}

/// The continue waits for its paced slot.
#[tokio::test]
async fn sequence_continues_wait_for_their_slot() {
    let f = Fake::new(stop(Some(NOW - HOUR)), ProbeVerdict::NotExhausted);
    let mut other = session();
    other.claude_session_id = "s-2".to_string();
    other.terminal_id = "t-2".to_string();
    run_hint(&f, &session()).await;
    run_hint(&f, &other).await;
    assert_eq!(*lock(&f.sleeps), [10_000]);
    assert_eq!(f.prompts.load(Ordering::SeqCst), 2);
}
