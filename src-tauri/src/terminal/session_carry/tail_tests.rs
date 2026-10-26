//! Tests for [`super`]: the Phase 2b transcript-tail rescue.

use super::*;

const WD: &str = "D:\\qontinui-root";
const SID: &str = "4d7ecc60-83c1-4398-a5eb-d3640fb5417a";

/// The 2026-10-22 incident's tail, SHAPE-for-shape: the last 5 lines `.claude-lukas`
/// wrote after the carry — 4 `queue-operation` lines and ONE `type: user`
/// `<task-notification>` for agent `a48b10a51f491857f` with `status=failed`.
/// Long content trimmed and paths redacted.
const INCIDENT_TAIL: &str = concat!(
    r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-10-22T12:03:35.527Z","sessionId":"4d7ecc60-83c1-4398-a5eb-d3640fb5417a","content":"<task-notification>\n<task-id>bsunvqvjj</task-id>\n<tool-use-id>toolu_REDACTED</tool-use-id>\n<output-file>C:\\Temp\\tasks\\bsunvqvjj.output</output-file>\n<status>completed</status>\n<summary>Background command \"Push with lease, full log\" completed (exit code 0)</summary>\n</task-notification>"}"#,
    "\n",
    r#"{"type":"queue-operation","operation":"remove","timestamp":"2026-10-22T12:03:35.602Z","sessionId":"4d7ecc60-83c1-4398-a5eb-d3640fb5417a","content":"<task-notification>\n<task-id>bsunvqvjj</task-id>\n<status>completed</status>\n<summary>Background command \"Push with lease, full log\" completed (exit code 0)</summary>\n</task-notification>","reason":"delivered_to_agent"}"#,
    "\n",
    r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-10-22T12:03:36.044Z","sessionId":"4d7ecc60-83c1-4398-a5eb-d3640fb5417a","content":"<task-notification>\n<task-id>a48b10a51f491857f</task-id>\n<output-file>C:\\Temp\\tasks\\a48b10a51f491857f.output</output-file>\n<status>failed</status>\n<summary>Agent \"Phase 3 wiring stacked on Phase 2\" failed: Agent terminated early due to an API error: You've hit your weekly limit · resets 6pm (Europe/Berlin)</summary>\n<note>trimmed</note>\n</task-notification>"}"#,
    "\n",
    r#"{"type":"queue-operation","operation":"dequeue","timestamp":"2026-10-22T12:03:36.055Z","sessionId":"4d7ecc60-83c1-4398-a5eb-d3640fb5417a"}"#,
    "\n",
    r#"{"parentUuid":"cbff0f76-9e6b-48ce-a307-a4d976667375","isSidechain":false,"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>a48b10a51f491857f</task-id>\n<output-file>C:\\Temp\\tasks\\a48b10a51f491857f.output</output-file>\n<status>failed</status>\n<summary>Agent \"Phase 3 wiring stacked on Phase 2\" failed: Agent terminated early due to an API error: You've hit your weekly limit · resets 6pm (Europe/Berlin)</summary>\n<note>trimmed</note>\n</task-notification>"},"uuid":"6961f83b-6b77-4dbb-a778-342e70a6a5bb","timestamp":"2026-10-22T12:03:36.079Z","permissionMode":"bypassPermissions","origin":{"kind":"task-notification","producer":"session-task"},"promptSource":"system","userType":"external"}"#,
    "\n",
);

fn incident_queue_operations_only() -> String {
    INCIDENT_TAIL
        .lines()
        .take(4)
        .map(|l| format!("{l}\n"))
        .collect()
}

fn notification_line(task_id: &str, status: &str, summary: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": format!(
            "<task-notification>\n<task-id>{task_id}</task-id>\n<status>{status}</status>\n<summary>{summary}</summary>\n</task-notification>"
        )},
        "promptSource": "system",
        "origin": {"kind": "task-notification"},
    })
    .to_string()
}

fn typed_prompt_line(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": text},
        "promptSource": "typed",
        "origin": {"kind": "human"},
    })
    .to_string()
}

// ---- scan_tail ---------------------------------------------------------------

#[test]
fn an_empty_tail_delivers_nothing() {
    let scan = scan_tail(b"");
    assert_eq!(scan, TailScan::default());
    assert_eq!(catch_up_message(&scan, Path::new("t.jsonl")), None);
    // Blank lines are not lines.
    assert_eq!(scan_tail(b"\n \r\n\n").lines, 0);
}

/// The queue-operation lines name the notification but are not its delivery;
/// only the `type: user` line is.
#[test]
fn queue_operation_lines_alone_deliver_nothing() {
    let scan = scan_tail(incident_queue_operations_only().as_bytes());
    assert_eq!(scan.lines, 4);
    assert_eq!(scan.unparseable, 0);
    assert!(scan.items.is_empty(), "got {:?}", scan.items);
    assert_eq!(catch_up_message(&scan, Path::new("t.jsonl")), None);
}

/// The real incident's shape: one failed-agent notification is delivered.
#[test]
fn the_incident_tail_yields_the_one_failed_agent_notification() {
    let scan = scan_tail(INCIDENT_TAIL.as_bytes());
    assert_eq!(scan.lines, 5);
    assert_eq!(scan.unparseable, 0);
    assert_eq!(
        scan.items,
        vec![CatchUpItem::TaskNotification {
            task_ids: vec!["a48b10a51f491857f".to_string()],
            status: Some("failed".to_string()),
            summary: Some(
                "Agent \"Phase 3 wiring stacked on Phase 2\" failed: Agent terminated early due to an API error: You've hit your weekly limit · resets 6pm (Europe/Berlin)".to_string()
            ),
        }]
    );
    let msg = catch_up_message(&scan, Path::new("C:/x/migration-tail/claude-lukas-1.jsonl"))
        .expect("one item → a message");
    assert!(
        msg.starts_with("1 message(s) reached the old account after the move: task a48b10a51f491857f failed: Agent \"Phase 3 wiring"),
        "got {msg}"
    );
    assert!(
        msg.ends_with("Full lines: C:/x/migration-tail/claude-lukas-1.jsonl."),
        "got {msg}"
    );
}

#[test]
fn many_notifications_are_listed_up_to_the_bound_and_counted_past_it() {
    let lines: String = (0..14)
        .map(|i| {
            format!(
                "{}\n",
                notification_line(&format!("agent{i:02}"), "completed", "ok")
            )
        })
        .collect();
    let scan = scan_tail(lines.as_bytes());
    assert_eq!(scan.items.len(), 14);
    let msg = catch_up_message(&scan, Path::new("t.jsonl")).unwrap();
    assert!(msg.starts_with("14 message(s) reached"), "got {msg}");
    assert!(
        msg.contains("task agent11 completed: ok"),
        "the 12th is listed: {msg}"
    );
    assert!(!msg.contains("agent12"), "the 13th is not listed: {msg}");
    assert!(msg.contains("; and 2 more."), "got {msg}");
}

#[test]
fn a_repeated_notification_is_listed_once() {
    let line = notification_line("a1", "failed", "boom");
    let scan = scan_tail(format!("{line}\n{line}\n").as_bytes());
    assert_eq!(scan.lines, 2);
    assert_eq!(scan.items.len(), 1);
}

#[test]
fn an_unparseable_line_is_counted_and_named_never_dropped_silently() {
    let tail = format!(
        "{{\"type\":\"user\",\"message\n{}\n[1,2]\n",
        notification_line("a1", "failed", "boom")
    );
    let scan = scan_tail(tail.as_bytes());
    assert_eq!(scan.lines, 3);
    assert_eq!(
        scan.unparseable, 2,
        "a cut line and a non-object both count"
    );
    assert_eq!(scan.items.len(), 1);
    let msg = catch_up_message(&scan, Path::new("t.jsonl")).unwrap();
    assert!(
        msg.contains("(2 tail line(s) could not be parsed)"),
        "got {msg}"
    );
}

#[test]
fn an_operator_typed_prompt_is_delivered_but_system_and_meta_lines_are_not() {
    let tool_result = serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t", "content": "x"}]},
    })
    .to_string();
    let meta = serde_json::json!({
        "type": "user", "isMeta": true, "promptSource": "typed",
        "message": {"role": "user", "content": "<system-reminder>x</system-reminder>"},
    })
    .to_string();
    let untyped = serde_json::json!({
        "type": "user", "message": {"role": "user", "content": "This session is being continued…"},
    })
    .to_string();
    let human_blocks = serde_json::json!({
        "type": "user", "origin": {"kind": "human"},
        "message": {"role": "user", "content": [{"type": "text", "text": "and also check B"}]},
    })
    .to_string();
    let tail = format!(
        "{tool_result}\n{meta}\n{untyped}\n{}\n{human_blocks}\n",
        typed_prompt_line("please stop after phase 2")
    );
    let scan = scan_tail(tail.as_bytes());
    assert_eq!(
        scan.items,
        vec![
            CatchUpItem::OperatorPrompt {
                text: "please stop after phase 2".to_string()
            },
            CatchUpItem::OperatorPrompt {
                text: "and also check B".to_string()
            },
        ]
    );
    let msg = catch_up_message(&scan, Path::new("t.jsonl")).unwrap();
    assert!(
        msg.contains(
            "operator prompt \"please stop after phase 2\"; operator prompt \"and also check B\""
        ),
        "got {msg}"
    );
}

/// Typed into a PTY: one line, no control characters, every item bounded.
#[test]
fn the_message_is_one_bounded_scrubbed_line() {
    let hostile = format!("line1\nline2\r\x1b[2J\u{7}{}", "x".repeat(500));
    let scan = scan_tail(
        format!(
            "{}\n{}\n",
            notification_line("a1", "failed", &hostile),
            typed_prompt_line(&hostile)
        )
        .as_bytes(),
    );
    assert_eq!(scan.items.len(), 2);
    let msg = catch_up_message(&scan, Path::new("C:\\a\nb\\t.jsonl")).unwrap();
    assert!(!msg.chars().any(char::is_control), "got {msg:?}");
    assert!(
        msg.contains("line1 line2 [2J"),
        "whitespace collapsed: {msg}"
    );
    assert!(msg.contains('…'), "a long item is cut: {msg}");
    assert!(
        msg.chars().count() < 2 * 230 + 200,
        "bounded: {} chars",
        msg.chars().count()
    );
}

#[test]
fn account_file_label_is_the_last_component_without_its_dot() {
    assert_eq!(
        account_file_label("C:\\claude\\.claude-lukas"),
        "claude-lukas"
    );
    assert_eq!(account_file_label("/home/u/.claude-a/"), "claude-a");
    assert_eq!(account_file_label("C:/x/we ird:name"), "we_ird_name");
    assert_eq!(account_file_label(""), "account");
}

// ---- rescue_tail (disk) ------------------------------------------------------

fn scratch(label: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "qontinui-session-tail-{label}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

/// `<cfg>/projects/D--qontinui-root/<SID>.jsonl`, spelled out literally.
fn literal_transcript(cfg: &Path) -> PathBuf {
    cfg.join("projects")
        .join("D--qontinui-root")
        .join(format!("{SID}.jsonl"))
}

/// Seed the source transcript and carry it to the destination.
fn seeded_carry(root: &Path, body: &[u8]) -> (PathBuf, PathBuf, TranscriptCopy) {
    let (src, dst) = (root.join(".claude-lukas"), root.join(".claude-niklas"));
    let src_t = literal_transcript(&src);
    std::fs::create_dir_all(src_t.parent().unwrap()).unwrap();
    std::fs::write(&src_t, body).unwrap();
    let carried =
        super::super::copy_transcript(src.to_str().unwrap(), dst.to_str().unwrap(), WD, SID)
            .unwrap();
    (src, dst, carried)
}

fn rescue(src: &Path, _dst: &Path, since: &TranscriptCopy) -> TailRescue {
    rescue_tail(src.to_str().unwrap(), SID, since, 1_761_134_616_000).unwrap()
}

fn append(path: &Path, bytes: &[u8]) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(bytes).unwrap();
}

/// Fixes 7 and 11: the offset is the copied bytes' last LINE BOUNDARY, not
/// the source's size. The source here ends mid-line (the old writer was
/// half-way through an append when the carry copied it), so the offset (16)
/// differs from the stat (20) — a stat-based offset fails this test. After
/// the writer finishes the line, the rescued tail is that line WHOLE.
///
/// Mutation checks: using the source's stat (or the copy's length) as the
/// offset fails the first assertion; dropping the `+ 1` past the `\n` fails
/// it too.
#[test]
fn the_carry_records_the_copys_last_line_boundary_as_the_tail_offset() {
    let root = scratch("offset");
    let (src, dst, carried) = seeded_carry(&root, b"{\"a\":1}\n{\"b\":2}\n{\"c\"");
    assert_eq!(carried.transcript_offset, Some(16));
    append(&literal_transcript(&src), b":3}\n");
    match rescue(&src, &dst, &carried) {
        TailRescue::Rescued { bytes, scan, .. } => {
            assert_eq!(bytes, 8, "the whole torn line");
            assert_eq!(scan.lines, 1);
            assert_eq!(scan.unparseable, 0, "rescued whole, not as a fragment");
        }
        other => panic!("expected Rescued, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Fix 7: a copy holding no `\n` at all starts the tail at 0, and a boundary
/// further back than one read chunk is still found.
#[test]
fn line_boundary_offset_finds_the_last_newline_or_zero() {
    let root = scratch("no-newline");
    let f = root.join("t.jsonl");
    std::fs::write(&f, b"{\"partial\"").unwrap();
    assert_eq!(super::super::line_boundary_offset(&f).unwrap(), 0);
    std::fs::write(&f, b"").unwrap();
    assert_eq!(super::super::line_boundary_offset(&f).unwrap(), 0);
    let mut big = b"{\"x\":1}\n".to_vec();
    big.extend(std::iter::repeat_n(b'y', 200 * 1024));
    std::fs::write(&f, &big).unwrap();
    assert_eq!(super::super::line_boundary_offset(&f).unwrap(), 8);
    let _ = std::fs::remove_dir_all(&root);
}

/// Fix 8: when the destination already held the transcript, the carry copied
/// nothing and records NO offset; the rescue then reports UNKNOWN and writes
/// nothing — even though the source has grown.
///
/// Mutation check: recording an offset for `MainCarry::Present` (a boundary or
/// the source size) rescues the appended line instead.
#[test]
fn an_already_present_destination_leaves_the_tail_unknown() {
    let root = scratch("present");
    let (src, dst, _) = seeded_carry(&root, b"{\"a\":1}\n");
    // A second carry (a retry): the destination already holds it all.
    let again =
        super::super::copy_transcript(src.to_str().unwrap(), dst.to_str().unwrap(), WD, SID)
            .unwrap();
    assert_eq!(again.transcript_offset, None);
    append(&literal_transcript(&src), b"{\"b\":2}\n");
    let got = rescue(&src, &dst, &again);
    assert_eq!(got, TailRescue::OffsetUnknown);
    assert_eq!(got.message(), None);
    assert!(!dst
        .join("projects")
        .join("D--qontinui-root")
        .join(SID)
        .join(TAIL_DIR)
        .exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// A migration onto a destination that diverged moves the fork aside and
/// copies the live source in its place, so the destination then equals the
/// source and the offset is the copy's line boundary — the tail after it is
/// rescued. A respawn top-up (`KeepExisting`) that keeps a shorter prefix in
/// place copies nothing, so its offset is UNKNOWN.
///
/// Mutation checks: mapping `DivergedPreserved` to `None` fails the first
/// assertion; mapping `KeptExisting` to a boundary fails the last.
#[test]
fn a_preserved_fork_records_the_copys_boundary_and_a_kept_prefix_none() {
    let root = scratch("diverged");
    let (src, dst) = (root.join(".claude-lukas"), root.join(".claude-niklas"));
    let (src_t, dst_t) = (literal_transcript(&src), literal_transcript(&dst));
    std::fs::create_dir_all(src_t.parent().unwrap()).unwrap();
    std::fs::create_dir_all(dst_t.parent().unwrap()).unwrap();
    std::fs::write(&src_t, b"{\"a\":1}\n{\"b\"").unwrap();
    std::fs::write(&dst_t, b"{\"fork\":1}\n{\"fork\":2}\n").unwrap();
    let carried =
        super::super::copy_transcript(src.to_str().unwrap(), dst.to_str().unwrap(), WD, SID)
            .unwrap();
    assert!(matches!(
        carried.main,
        super::super::MainCarry::DivergedPreserved { .. }
    ));
    assert_eq!(carried.transcript_offset, Some(8));
    append(&src_t, b":2}\n");
    match rescue(&src, &dst, &carried) {
        TailRescue::Rescued { bytes, scan, .. } => {
            assert_eq!(bytes, 8);
            assert_eq!(scan.unparseable, 0);
        }
        other => panic!("expected Rescued, got {other:?}"),
    }

    let prefix_root = scratch("kept-prefix");
    let (src, dst) = (
        prefix_root.join(".claude-lukas"),
        prefix_root.join(".claude-niklas"),
    );
    let (src_t, dst_t) = (literal_transcript(&src), literal_transcript(&dst));
    std::fs::create_dir_all(src_t.parent().unwrap()).unwrap();
    std::fs::create_dir_all(dst_t.parent().unwrap()).unwrap();
    std::fs::write(&src_t, b"{\"a\":1}\n{\"b\":2}\n").unwrap();
    std::fs::write(&dst_t, b"{\"a\":1}\n").unwrap();
    let kept = super::super::carry_session(
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
        WD,
        SID,
        super::super::MainTranscript::KeepExisting,
    )
    .unwrap();
    assert_eq!(kept.main, super::super::MainCarry::KeptExisting);
    assert_eq!(kept.transcript_offset, None);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&prefix_root);
}

/// Fix 12: a notification naming several tasks reports EVERY task id (a
/// repeat once).
///
/// Mutation check: collecting only the first `<task-id>` fails.
#[test]
fn a_notification_naming_several_tasks_reports_every_id() {
    let line = serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content":
            "<task-notification>\n<task-id>aaa</task-id>\n<task-id>bbb</task-id>\n<task-id>aaa</task-id>\n<status>failed</status>\n</task-notification>"},
        "promptSource": "system",
    })
    .to_string();
    let scan = scan_tail(format!("{line}\n").as_bytes());
    assert_eq!(
        scan.items,
        vec![CatchUpItem::TaskNotification {
            task_ids: vec!["aaa".to_string(), "bbb".to_string()],
            status: Some("failed".to_string()),
            summary: None,
        }]
    );
    let msg = catch_up_message(&scan, Path::new("C:/t.jsonl")).unwrap();
    assert!(msg.contains("task aaa, bbb failed"), "{msg}");
}

/// The live incident end to end on disk: the old writer appends its 5 lines
/// after the carry; they land byte-verbatim under `migration-tail/`, and the
/// destination's own `<sid>.jsonl` is never touched.
#[test]
fn an_appended_tail_is_written_verbatim_beside_the_session_never_into_it() {
    let root = scratch("rescued");
    let head = b"{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n";
    let (src, dst, carried) = seeded_carry(&root, head);
    // The resumed process writes its own chain to the destination.
    let dst_t = literal_transcript(&dst);
    append(&dst_t, b"{\"type\":\"assistant\",\"resumed\":true}\n");
    let dst_before = std::fs::read(&dst_t).unwrap();
    append(&literal_transcript(&src), INCIDENT_TAIL.as_bytes());

    let got = rescue(&src, &dst, &carried);
    let expected_path = dst
        .join("projects")
        .join("D--qontinui-root")
        .join(SID)
        .join("migration-tail")
        .join("claude-lukas-1761134616000.jsonl");
    match &got {
        TailRescue::Rescued {
            path,
            bytes,
            truncated,
            scan,
        } => {
            assert_eq!(path, &expected_path);
            assert_eq!(*bytes, INCIDENT_TAIL.len());
            assert!(!truncated);
            assert_eq!(scan.lines, 5);
            assert_eq!(scan.items.len(), 1);
        }
        other => panic!("expected Rescued, got {other:?}"),
    }
    assert_eq!(
        std::fs::read(&expected_path).unwrap(),
        INCIDENT_TAIL.as_bytes()
    );
    assert_eq!(got.lines(), 5);
    assert!(got.message().unwrap().contains("a48b10a51f491857f failed"));
    assert_eq!(
        std::fs::read(&dst_t).unwrap(),
        dst_before,
        "the destination transcript is never spliced"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn nothing_appended_after_the_carry_is_empty_and_writes_nothing() {
    let root = scratch("empty");
    let (src, dst, carried) = seeded_carry(&root, b"{\"a\":1}\n");
    assert_eq!(rescue(&src, &dst, &carried), TailRescue::Empty);
    assert!(!dst
        .join("projects")
        .join("D--qontinui-root")
        .join(SID)
        .join(TAIL_DIR)
        .exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// A source SHORTER than the recorded offset was truncated or replaced: which
/// lines are new is UNKNOWN, so nothing is written.
#[test]
fn a_source_shorter_than_the_offset_is_unknown_and_writes_nothing() {
    let root = scratch("shorter");
    let (src, dst, carried) = seeded_carry(&root, b"{\"a\":1}\n{\"b\":2}\n");
    std::fs::write(literal_transcript(&src), b"{\"a\":1}\n").unwrap();
    assert_eq!(
        rescue(&src, &dst, &carried),
        TailRescue::SourceShorter { len: 8, offset: 16 }
    );
    assert!(!dst
        .join("projects")
        .join("D--qontinui-root")
        .join(SID)
        .join(TAIL_DIR)
        .exists());
    assert_eq!(rescue(&src, &dst, &carried).message(), None);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_tail_of_queue_operations_only_is_saved_but_delivers_nothing() {
    let root = scratch("queue-only");
    let (src, dst, carried) = seeded_carry(&root, b"{\"a\":1}\n");
    append(
        &literal_transcript(&src),
        incident_queue_operations_only().as_bytes(),
    );
    let got = rescue(&src, &dst, &carried);
    assert!(matches!(got, TailRescue::Rescued { .. }), "got {got:?}");
    assert_eq!(got.lines(), 4);
    assert_eq!(got.message(), None);
    let _ = std::fs::remove_dir_all(&root);
}
