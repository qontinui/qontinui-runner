//! Tests for [`super`]: the Phase 2b stall decision, its evidence, and the
//! tail event.

use super::*;

/// The resumed pane as the 2026-10-22 hypothesis has it: the CLI's own
/// limit-wait banner, re-entered on `--resume`.
const LIMIT_WAIT: &str = "● Working on phase 3…\n\n  ⎿  Usage limit reached · continuing automatically at 6pm · esc to cancel\n\n────────\n❯ \n────────\n";

/// An ordinary CLI dialog: `Esc to cancel`, but no limit.
const PLAIN_DIALOG: &str =
    "Resume a conversation\n  ❯ session one\n    session two\n  Enter to select · Esc to cancel\n";

/// Something else entirely: a resume that never drew its prompt.
const BLANK_RESUME: &str = "$ claude --permission-mode bypassPermissions --resume abc\n\n";

#[test]
fn the_limit_wait_banner_is_recognised_and_a_plain_dialog_is_not() {
    assert!(screen_shows_limit_wait(LIMIT_WAIT));
    assert!(screen_shows_limit_wait(
        "5-hour limit reached ∙ resets 3am · esc to cancel"
    ));
    assert!(!screen_shows_limit_wait(PLAIN_DIALOG));
    assert!(!screen_shows_limit_wait(BLANK_RESUME));
}

/// Mutation check per arm: swapping the `NotExhausted` and `Exhausted` arms
/// fails the first two asserts; dropping the `retried` guard fails the last.
#[test]
fn banner_on_a_not_exhausted_account_is_one_esc_and_one_retry() {
    assert_eq!(
        decide_stall(Some(LIMIT_WAIT), Exhaustion::NotExhausted, false),
        StallAction::EscThenRetry
    );
}

#[test]
fn banner_on_an_exhausted_account_sends_no_esc() {
    assert_eq!(
        decide_stall(Some(LIMIT_WAIT), Exhaustion::Exhausted, false),
        StallAction::Stalled(StallReason::LimitWaitOnExhaustedAccount)
    );
}

#[test]
fn banner_with_an_unknown_probe_sends_no_esc() {
    assert_eq!(
        decide_stall(Some(LIMIT_WAIT), Exhaustion::Unknown, false),
        StallAction::Stalled(StallReason::LimitWaitExhaustionUnknown)
    );
}

#[test]
fn any_other_screen_is_a_stall() {
    for screen in [PLAIN_DIALOG, BLANK_RESUME] {
        assert_eq!(
            decide_stall(Some(screen), Exhaustion::NotExhausted, false),
            StallAction::Stalled(StallReason::UnrecognisedScreen),
            "{screen:?}"
        );
    }
    assert_eq!(
        decide_stall(None, Exhaustion::NotExhausted, false),
        StallAction::Stalled(StallReason::ScreenUnreadable)
    );
}

/// At most one `Esc` and one retry: the retry's give-up is a stall even on the
/// very screen that earned the first `Esc`.
#[test]
fn a_second_give_up_is_a_stall_never_a_second_esc() {
    assert_eq!(
        decide_stall(Some(LIMIT_WAIT), Exhaustion::NotExhausted, true),
        StallAction::Stalled(StallReason::RetryGaveUp)
    );
}

#[test]
fn the_evidence_line_is_the_banner_row_on_one_bounded_line() {
    assert_eq!(
        evidence_line(LIMIT_WAIT),
        "⎿ Usage limit reached · continuing automatically at 6pm · esc to cancel"
    );
    assert_eq!(
        evidence_line(BLANK_RESUME),
        "$ claude --permission-mode bypassPermissions --resume abc"
    );
    assert_eq!(evidence_line(""), "");
    let long = format!("{}\u{1b}[0m\n", "y".repeat(400));
    let line = evidence_line(&long);
    assert!(!line.chars().any(char::is_control));
    assert_eq!(
        line.chars().count(),
        EVIDENCE_MAX_CHARS + 1,
        "bounded + ellipsis"
    );
}

#[test]
fn the_screen_excerpt_keeps_the_bottom_rows_and_is_bounded() {
    let screen: String = (0..40).map(|i| format!("row {i}\n")).collect();
    let excerpt = screen_excerpt(&screen);
    assert!(excerpt.starts_with("row 25 ⏎"), "got {excerpt}");
    assert!(excerpt.ends_with("row 39"), "got {excerpt}");

    let wide: String = (0..20)
        .map(|i| format!("{i}{}\n", "z".repeat(300)))
        .collect();
    let excerpt = screen_excerpt(&wide);
    assert_eq!(excerpt.chars().count(), EXCERPT_MAX_CHARS + 1);
    assert!(excerpt.starts_with('…'));
    assert!(
        excerpt.ends_with(&"z".repeat(300)),
        "the bottom row survives"
    );
}

#[test]
fn the_stall_finding_has_the_door_shape_and_the_session_key() {
    let b = stall_finding_body(
        "4d7ecc60-83c1-4398-a5eb-d3640fb5417a",
        "bfca9a32",
        "C:\\claude\\.claude-niklas",
        StallReason::UnrecognisedScreen,
        "❯",
        false,
    );
    let mut keys: Vec<&str> = b.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["body", "kind", "resource_keys", "title", "topic"]);
    assert_eq!(
        b["resource_keys"],
        json!(["session:4d7ecc60-83c1-4398-a5eb-d3640fb5417a"])
    );
    assert!(b["title"]
        .as_str()
        .unwrap()
        .ends_with("(unrecognised-screen)"));
    assert!(b["body"].as_str().unwrap().contains("bfca9a32"));
}

fn post_migration() -> PostMigration {
    PostMigration {
        src_config_dir: "C:\\claude\\.claude-lukas".to_string(),
        dst_config_dir: "C:\\claude\\.claude-niklas".to_string(),
        claude_session_id: "4d7ecc60-83c1-4398-a5eb-d3640fb5417a".to_string(),
        new_terminal_id: "bfca9a32".to_string(),
        carried: TranscriptCopy {
            path: std::path::PathBuf::new(),
            main: session_carry::MainCarry::Copied,
            src_session_dir: std::path::PathBuf::new(),
            dst_session_dir: std::path::PathBuf::new(),
            transcript_offset: Some(0),
            session_dir: Default::default(),
            files: Vec::new(),
            subagents: Vec::new(),
        },
        auto_continue: false,
    }
}

/// An unread or ambiguous tail is UNKNOWN (`null`), never zero lines.
#[test]
fn the_tail_event_reports_unknown_as_null_never_zero() {
    let p = post_migration();
    let not_read = tail_event(&p, None, None);
    assert_eq!(not_read["transcriptTail"], "not-read");
    assert!(not_read["transcriptTailLines"].is_null());
    assert_eq!(not_read["transcriptTailDelivered"], false);

    let shorter = tail_event(
        &p,
        Some(&TailRescue::SourceShorter { len: 8, offset: 16 }),
        None,
    );
    assert_eq!(shorter["transcriptTail"], "source-shorter-unknown");
    assert!(shorter["transcriptTailLines"].is_null());

    let empty = tail_event(
        &p,
        Some(&TailRescue::Empty),
        Some(TailDelivery::NoneToDeliver),
    );
    assert_eq!(empty["transcriptTailLines"], 0);

    let scan = session_carry::tail::scan_tail(
        b"{\"type\":\"user\",\"promptSource\":\"typed\",\"message\":{\"content\":\"hi\"}}\nnot json\n",
    );
    let rescued = tail_event(
        &p,
        Some(&TailRescue::Rescued {
            path: std::path::PathBuf::from("C:/t/migration-tail/claude-lukas-1.jsonl"),
            bytes: 10,
            truncated: false,
            scan,
        }),
        Some(TailDelivery::Delivered),
    );
    assert_eq!(rescued["transcriptTail"], "rescued");
    assert_eq!(rescued["transcriptTailLines"], 2);
    assert_eq!(rescued["transcriptTailUnparseable"], 1);
    assert_eq!(rescued["transcriptTailDelivered"], true);
    assert_eq!(rescued["newTerminalId"], "bfca9a32");
}

// ---- 2026-10-23 review fixes ------------------------------------------------

/// Fix 1: a limit banner HIGH up with a dialog (or a draft) in the bottom rows
/// is not the limit-wait — an `Esc` there would cancel the dialog.
///
/// Mutation check: reading the whole screen in `screen_shows_limit_wait`
/// fails both assertions.
#[test]
fn a_banner_above_a_bottom_dialog_is_not_the_limit_wait() {
    let mut screen = String::from(
        "  ⎿  Usage limit reached · continuing automatically at 6pm · esc to cancel\n",
    );
    for i in 0..10 {
        screen.push_str(&format!("● later output {i}\n"));
    }
    let with_dialog = format!("{screen}{PLAIN_DIALOG}");
    assert!(!screen_shows_limit_wait(&with_dialog));
    let with_draft = format!("{screen}────────\n❯ half-typed operator draft\n────────\n");
    assert!(!screen_shows_limit_wait(&with_draft));
    assert_eq!(
        decide_stall(Some(&with_dialog), Exhaustion::NotExhausted, false),
        StallAction::Stalled(StallReason::UnrecognisedScreen)
    );
}

/// Round-11 W3: the banner phrase and `esc to cancel` must share ONE row. A
/// replayed limit line directly above a short dialog (all of it inside the
/// bottom rows) is the dialog, and an `Esc` would cancel the operator's
/// choice; so is a real banner row with a numbered choice or dialog chrome
/// on another bottom row.
///
/// Mutation checks: matching the phrase and `esc to cancel` across the whole
/// bottom window again fails the first assertion; dropping the other-row
/// dialog check fails the second and third.
#[test]
fn a_limit_line_above_a_short_dialog_is_not_the_limit_wait() {
    let banner_over_dialog = format!("5-hour limit reached ∙ resets 3am\n{PLAIN_DIALOG}");
    assert!(!screen_shows_limit_wait(&banner_over_dialog));
    assert_eq!(
        decide_stall(Some(&banner_over_dialog), Exhaustion::NotExhausted, false),
        StallAction::Stalled(StallReason::UnrecognisedScreen)
    );
    let wait_beside_dialog = format!(
        "  ⎿  Usage limit reached · continuing automatically at 6pm · esc to cancel\n{PLAIN_DIALOG}"
    );
    assert!(!screen_shows_limit_wait(&wait_beside_dialog));
    let wait_beside_choice =
        "  ⎿  5-hour limit reached ∙ resets 3am · esc to cancel\n❯ 1. Yes\n  2. No\n";
    assert!(!screen_shows_limit_wait(wait_beside_choice));
    assert!(
        screen_shows_limit_wait(LIMIT_WAIT),
        "the real banner over the idle input box still is"
    );
}

/// Round-12 S6: the real banner directly over an input box holding an
/// unsent draft is not the limit wait, so no `Esc` (it could wipe the
/// draft); the placeholder is not a draft.
///
/// Mutation check: dropping the draft term fails the first assertion.
#[test]
fn a_banner_over_an_unsent_draft_sends_no_esc() {
    let with_draft = "  ⎿  Usage limit reached · continuing automatically at 6pm · esc to cancel\n────────\n❯ half-typed operator draft\n────────\n";
    assert!(!screen_shows_limit_wait(with_draft));
    assert!(!esc_still_warranted(Some(with_draft)));
    assert_eq!(
        decide_stall(Some(with_draft), Exhaustion::NotExhausted, false),
        StallAction::Stalled(StallReason::UnrecognisedScreen)
    );
    let placeholder = "  ⎿  Usage limit reached · continuing automatically at 6pm · esc to cancel\n────────\n❯ Try \"fix the tests\"\n────────\n";
    assert!(screen_shows_limit_wait(placeholder));
}

/// Round-13 S4: the draft rule reads only rows BELOW the banner. A prior
/// prompt echoed above it, or a `❯ npm test` quoted in output, is history —
/// the wait is still recognised; the real draft in the input box still
/// blocks.
///
/// Mutation check: applying the draft rule to every bottom row (round 12)
/// fails the first two assertions.
#[test]
fn a_prompt_echoed_in_the_history_is_not_a_draft() {
    let echoed = "❯ run the tests and push\n  ⎿  Usage limit reached · continuing automatically at 6pm · esc to cancel\n────────\n❯\u{a0}\n────────\n";
    assert!(screen_shows_limit_wait(echoed));
    let quoted = "  Run it with:\n    ❯ npm test\n  ⎿  Usage limit reached · continuing automatically at 6pm · esc to cancel\n────────\n❯ \n────────\n";
    assert!(screen_shows_limit_wait(quoted));
    let drafted = "❯ run the tests and push\n  ⎿  Usage limit reached · continuing automatically at 6pm · esc to cancel\n────────\n❯ half-typed operator draft\n────────\n";
    assert!(!screen_shows_limit_wait(drafted));
}

/// Round-14 W1: the draft below the banner blocks with NO box rules around
/// it (an unfamiliar rule glyph, or a single rule): the `Esc` could wipe it.
///
/// Mutation check: reading the draft only inside a two-rule box (round 13)
/// fails both assertions.
#[test]
fn a_draft_below_the_banner_blocks_without_box_rules() {
    let bare = "  ⎿  Usage limit reached · continuing automatically at 6pm · esc to cancel\n❯ half-typed operator draft\n";
    assert!(!screen_shows_limit_wait(bare));
    let one_rule = "  ⎿  Usage limit reached · continuing automatically at 6pm · esc to cancel\n════════\n❯ half-typed operator draft\n";
    assert!(!screen_shows_limit_wait(one_rule));
}

/// Round-16 W1: the banner drawn UNDER the input box (a status line) still
/// has the box's draft seen — the lowest `❯` row decides, wherever the
/// banner is; an empty box there is still the wait.
///
/// Mutation check: reading the draft only on rows below the banner (round
/// 14) fails the first assertion.
#[test]
fn a_draft_above_a_status_line_banner_blocks() {
    let drafted = "────────\n❯ half-typed operator draft\n────────\n  Usage limit reached · continuing automatically at 6pm · esc to cancel\n";
    assert!(!screen_shows_limit_wait(drafted));
    let idle = "────────\n❯ \n────────\n  Usage limit reached · continuing automatically at 6pm · esc to cancel\n";
    assert!(screen_shows_limit_wait(idle));
}

/// Fix 2: the `Esc` needs the banner STILL in the bottom rows on the re-read,
/// and no turn under way; an unreadable re-read never authorizes it.
///
/// Mutation checks: dropping the `screen_shows_turn` term fails the
/// turn-started case; returning `true` for `None` fails the last.
#[test]
fn the_esc_is_sent_only_while_the_re_read_still_shows_the_banner() {
    assert!(esc_still_warranted(Some(LIMIT_WAIT)));
    let turn_started = "  ⎿  Usage limit reached · continuing automatically at 6pm · esc to cancel\n✻ Working… (esc to interrupt)\n────────\n❯ \n────────\n";
    assert!(!esc_still_warranted(Some(turn_started)));
    assert!(!esc_still_warranted(Some(PLAIN_DIALOG)));
    assert!(!esc_still_warranted(Some(BLANK_RESUME)));
    assert!(!esc_still_warranted(None));
}

/// Fix 5: a failed PTY write of the nudge is a stall with its own reason,
/// never a silent end; a give-up carries its refusal on.
///
/// Mutation check: mapping `SubmitFailed` to `Done` fails the first assertion.
#[test]
fn a_failed_nudge_write_is_a_stall() {
    assert_eq!(
        nudge_step(IdlePromptOutcome::SubmitFailed("broken pipe".into())),
        NudgeStep::Stalled(StallReason::SubmitFailed)
    );
    assert_eq!(StallReason::SubmitFailed.as_str(), "submit-failed");
    for done in [
        IdlePromptOutcome::Submitted,
        IdlePromptOutcome::AlreadyBusy,
        IdlePromptOutcome::TerminalGone,
    ] {
        assert_eq!(nudge_step(done), NudgeStep::Done);
    }
    assert_eq!(
        nudge_step(IdlePromptOutcome::GaveUp {
            last_refusal: "x".into()
        }),
        NudgeStep::GaveUp {
            last_refusal: "x".into()
        }
    );
}

/// Fix 10: each reason's finding says what the runner actually did — the
/// `Esc` it sent (or attempted, or withheld), or that it typed nothing.
///
/// Mutation check: one shared wording for every reason fails the
/// distinctness assertion; wording `RetryGaveUp` without `esc_sent` fails
/// the withheld-Esc assertion.
#[test]
fn the_stall_finding_wording_follows_the_reason() {
    let body = |reason: StallReason, esc_sent: bool| {
        stall_finding_body("s", "t", "C:/a", reason, "row", esc_sent)["body"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let all = [
        StallReason::ScreenUnreadable,
        StallReason::UnrecognisedScreen,
        StallReason::LimitWaitOnExhaustedAccount,
        StallReason::LimitWaitExhaustionUnknown,
        StallReason::EscNotDelivered,
        StallReason::RetryGaveUp,
        StallReason::SubmitFailed,
    ];
    let bodies: std::collections::HashSet<String> = all.iter().map(|r| body(*r, true)).collect();
    assert_eq!(bodies.len(), all.len(), "one wording per reason");
    assert!(body(StallReason::EscNotDelivered, false).contains("attempted ONE Esc"));
    assert!(body(StallReason::RetryGaveUp, true).contains("sent ONE Esc"));
    let withheld = body(StallReason::RetryGaveUp, false);
    assert!(withheld.contains("none was sent"), "{withheld}");
    assert!(!withheld.contains("sent ONE Esc"));
    for r in [
        StallReason::ScreenUnreadable,
        StallReason::UnrecognisedScreen,
        StallReason::LimitWaitOnExhaustedAccount,
        StallReason::LimitWaitExhaustionUnknown,
        StallReason::SubmitFailed,
    ] {
        let b = body(r, false);
        assert!(b.contains("typed nothing further"), "{r:?}: {b}");
        assert!(!b.contains("sent ONE Esc"), "{r:?}: {b}");
    }
}

fn rescued_with_prompt(truncated: bool) -> TailRescue {
    TailRescue::Rescued {
        path: std::path::PathBuf::from("C:/t/migration-tail/claude-lukas-1.jsonl"),
        bytes: 10,
        truncated,
        scan: session_carry::tail::scan_tail(
            b"{\"type\":\"user\",\"promptSource\":\"typed\",\"message\":{\"content\":\"hi\"}}\n",
        ),
    }
}

/// Fix 13: with `auto_continue_after_migration` OFF nothing is typed — the
/// tail is still saved and reported, as `skipped_auto_continue_off`.
///
/// Mutation check: ignoring `auto_continue` in `catch_up_plan` returns
/// `Deliver` for the off case.
#[test]
fn auto_continue_off_reports_the_tail_but_types_nothing() {
    let rescued = rescued_with_prompt(false);
    assert_eq!(
        catch_up_plan(Some(&rescued), false),
        CatchUpPlan::SkipAutoContinueOff
    );
    assert!(matches!(
        catch_up_plan(Some(&rescued), true),
        CatchUpPlan::Deliver(ref m) if m.contains("operator prompt \"hi\"")
    ));
    assert_eq!(
        catch_up_plan(Some(&TailRescue::Empty), false),
        CatchUpPlan::NoneToDeliver
    );
    for unknown in [
        None,
        Some(TailRescue::OffsetUnknown),
        Some(TailRescue::SourceShorter { len: 1, offset: 2 }),
    ] {
        assert_eq!(catch_up_plan(unknown.as_ref(), true), CatchUpPlan::Unknown);
    }
    let ev = tail_event(
        &post_migration(),
        Some(&rescued),
        Some(TailDelivery::SkippedAutoContinueOff),
    );
    assert_eq!(ev["transcriptTail"], "rescued");
    assert_eq!(ev["transcriptTailLines"], 1);
    assert_eq!(ev["transcriptTailDelivery"], "skipped_auto_continue_off");
    assert_eq!(ev["transcriptTailDelivered"], false);
}

/// Fix 9: `transcriptTailTruncated` and the `transcriptTailDelivery` enum —
/// `null` when the tail is UNKNOWN, never `false` / a delivery.
///
/// Mutation check: reporting `false` for an unread tail fails the null
/// assertions.
#[test]
fn the_tail_event_carries_truncation_and_the_delivery_enum() {
    let p = post_migration();
    let truncated = tail_event(
        &p,
        Some(&rescued_with_prompt(true)),
        Some(TailDelivery::GaveUp),
    );
    assert_eq!(truncated["transcriptTailTruncated"], true);
    assert_eq!(truncated["transcriptTailDelivery"], "gave_up");
    let empty = tail_event(
        &p,
        Some(&TailRescue::Empty),
        Some(TailDelivery::NoneToDeliver),
    );
    assert_eq!(empty["transcriptTailTruncated"], false);
    assert_eq!(empty["transcriptTailDelivery"], "none_to_deliver");
    let unknown = tail_event(&p, Some(&TailRescue::OffsetUnknown), None);
    assert_eq!(unknown["transcriptTail"], "offset-unknown");
    assert!(unknown["transcriptTailLines"].is_null());
    assert!(unknown["transcriptTailTruncated"].is_null());
    assert!(unknown["transcriptTailDelivery"].is_null());
    assert_eq!(
        tail_delivery(&IdlePromptOutcome::SubmitFailed("x".into())),
        TailDelivery::SubmitFailed
    );
    assert_eq!(
        tail_delivery(&IdlePromptOutcome::Submitted),
        TailDelivery::Delivered
    );
    assert_eq!(
        tail_delivery(&IdlePromptOutcome::TerminalGone),
        TailDelivery::GaveUp
    );
}
