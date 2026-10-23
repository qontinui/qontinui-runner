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
        old_writer: None,
        src_config_dir: "C:\\claude\\.claude-lukas".to_string(),
        dst_config_dir: "C:\\claude\\.claude-niklas".to_string(),
        working_dir: "D:\\qontinui-root".to_string(),
        claude_session_id: "4d7ecc60-83c1-4398-a5eb-d3640fb5417a".to_string(),
        new_terminal_id: "bfca9a32".to_string(),
        carried: TranscriptCopy {
            path: std::path::PathBuf::new(),
            transcript_offset: 0,
            session_dir: Default::default(),
            files: Vec::new(),
            subagents: Vec::new(),
        },
        nudge: None,
    }
}

/// An unread or ambiguous tail is UNKNOWN (`null`), never zero lines.
#[test]
fn the_tail_event_reports_unknown_as_null_never_zero() {
    let p = post_migration();
    let not_read = tail_event(&p, None, false);
    assert_eq!(not_read["transcriptTail"], "not-read");
    assert!(not_read["transcriptTailLines"].is_null());
    assert_eq!(not_read["transcriptTailDelivered"], false);

    let shorter = tail_event(
        &p,
        Some(&TailRescue::SourceShorter { len: 8, offset: 16 }),
        false,
    );
    assert_eq!(shorter["transcriptTail"], "source-shorter-unknown");
    assert!(shorter["transcriptTailLines"].is_null());

    let empty = tail_event(&p, Some(&TailRescue::Empty), false);
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
        true,
    );
    assert_eq!(rescued["transcriptTail"], "rescued");
    assert_eq!(rescued["transcriptTailLines"], 2);
    assert_eq!(rescued["transcriptTailUnparseable"], 1);
    assert_eq!(rescued["transcriptTailDelivered"], true);
    assert_eq!(rescued["newTerminalId"], "bfca9a32");
}
