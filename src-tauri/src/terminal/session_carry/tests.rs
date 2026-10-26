//! Tests for [`super`]: the session carry (`copy_transcript`, adopted from
//! qontinui/qontinui-runner#2167 and moved here from `account_migration.rs`)
//! and the Phase 2 additions (subagent list, `delta_sync`, the exit wait).

use super::*;

#[test]
fn copy_transcript_copies_and_is_idempotent() {
    let tmp = std::env::temp_dir().join(format!("qontinui-acctmig-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src_cfg = tmp.join(".claude-hotmail");
    let dst_cfg = tmp.join(".claude-gmail");
    let working_dir = "D:\\qontinui-root";
    let sid = "11111111-2222-3333-4444-555555555555";

    let src_path = crate::terminal::transcript::session_transcript_path(&src_cfg, working_dir, sid);
    std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    std::fs::write(&src_path, b"{\"type\":\"user\"}\n").unwrap();

    let first = copy_transcript(
        src_cfg.to_str().unwrap(),
        dst_cfg.to_str().unwrap(),
        working_dir,
        sid,
    )
    .expect("copy should succeed");
    assert_eq!(first.main, MainCarry::Copied);
    let dst = first.path;
    assert!(dst.exists());
    assert_eq!(
        std::fs::read(&dst).unwrap(),
        std::fs::read(&src_path).unwrap()
    );
    // Source untouched.
    assert!(src_path.exists());

    // Second call (repeated hint / retry) is a no-op success.
    let second = copy_transcript(
        src_cfg.to_str().unwrap(),
        dst_cfg.to_str().unwrap(),
        working_dir,
        sid,
    )
    .expect("idempotent retry should succeed");
    assert_eq!(second.main, MainCarry::Present);
    assert_eq!(dst, second.path);

    let _ = std::fs::remove_dir_all(&tmp);
}

/// A cwd whose encoded name exceeds 200 chars lives in a `<prefix>-<hash>`
/// dir. The copy lands in a dir of the SAME name in the target account,
/// not the untruncated name `claude --resume` would never look in — and so
/// does the `<sid>/` side-car carry.
#[test]
fn copy_transcript_keeps_a_long_project_dir_name() {
    let tmp = tempfile::tempdir().unwrap();
    let src_cfg = tmp.path().join("src");
    let dst_cfg = tmp.path().join("dst");
    let working_dir = format!("/home/u/{}", "d".repeat(250));
    let sid = "11111111-2222-3333-4444-555555555555";
    let prefix = crate::terminal::transcript::encode_project_path(&working_dir)
        .get(..200)
        .unwrap()
        .to_string();
    let src_dir = src_cfg.join("projects").join(format!("{prefix}-k3j2h1"));
    std::fs::create_dir_all(src_dir.join(sid).join("subagents")).unwrap();
    std::fs::write(
        src_dir.join(format!("{sid}.jsonl")),
        b"{\"type\":\"user\"}
",
    )
    .unwrap();
    std::fs::write(
        src_dir.join(sid).join("subagents").join("agent-a1.jsonl"),
        b"{\"type\":\"assistant\"}
",
    )
    .unwrap();

    let dst = copy_transcript(
        src_cfg.to_str().unwrap(),
        dst_cfg.to_str().unwrap(),
        &working_dir,
        sid,
    )
    .expect("copy should succeed")
    .path;
    let dst_project = dst_cfg.join("projects").join(format!("{prefix}-k3j2h1"));
    assert_eq!(dst, dst_project.join(format!("{sid}.jsonl")));
    assert!(dst.exists());
    assert!(
        dst_project
            .join(sid)
            .join("subagents")
            .join("agent-a1.jsonl")
            .is_file(),
        "the side-car lands beside the transcript, in the same long-name dir"
    );
    // The retry resolves the same file through the target's own dir.
    let dst2 = copy_transcript(
        src_cfg.to_str().unwrap(),
        dst_cfg.to_str().unwrap(),
        &working_dir,
        sid,
    )
    .unwrap()
    .path;
    assert_eq!(dst, dst2);
}

/// Lay out `<cfg>/projects/<slug>/<sid>.jsonl` plus the files given as
/// `(path relative to <sid>/, bytes)`; returns the `<sid>/` directory.
fn seed_session(
    cfg: &Path,
    working_dir: &str,
    sid: &str,
    files: &[(&str, &[u8])],
) -> std::path::PathBuf {
    let transcript = crate::terminal::transcript::session_transcript_path(cfg, working_dir, sid);
    let project = transcript.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(&transcript, b"{\"type\":\"user\"}\n").unwrap();
    let session_dir = project.join(sid);
    for (rel, bytes) in files {
        let path = session_dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
    }
    session_dir
}

fn session_dir_of(cfg: &Path, working_dir: &str, sid: &str) -> std::path::PathBuf {
    crate::terminal::transcript::session_transcript_path(cfg, working_dir, sid)
        .parent()
        .unwrap()
        .join(sid)
}

#[test]
fn copy_transcript_carries_subagent_transcripts_and_sidecars() {
    let tmp = std::env::temp_dir().join(format!("qontinui-acctmig-sidecar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let (src_cfg, dst_cfg) = (tmp.join(".claude-erik"), tmp.join(".claude-qontinui"));
    let working_dir = "D:\\qontinui-root";
    let sid = "beeae8aa-7d4d-4843-87fb-8d71dcce6f2e";
    let files: &[(&str, &[u8])] = &[
        (
            "subagents/agent-a0a89f0b0f9dfeb83.jsonl",
            b"{\"type\":\"assistant\"}\n",
        ),
        (
            "subagents/agent-a0a89f0b0f9dfeb83.meta.json",
            b"{\"agentType\":\"x\"}",
        ),
        ("tool-results/hook-1-stdout.txt", b"large tool output"),
    ];
    seed_session(&src_cfg, working_dir, sid, files);

    let carried = copy_transcript(
        src_cfg.to_str().unwrap(),
        dst_cfg.to_str().unwrap(),
        working_dir,
        sid,
    )
    .expect("copy should succeed");
    assert_eq!(
        carried.session_dir,
        SessionDirCarry {
            copied: 3,
            ..SessionDirCarry::default()
        }
    );
    let dst_dir = session_dir_of(&dst_cfg, working_dir, sid);
    for (rel, bytes) in files {
        assert_eq!(
            std::fs::read(dst_dir.join(rel)).unwrap(),
            *bytes,
            "{rel} must land under the destination account byte-for-byte"
        );
    }

    // A retry (the main transcript is now present) still runs the carry,
    // and finds everything already there.
    let again = copy_transcript(
        src_cfg.to_str().unwrap(),
        dst_cfg.to_str().unwrap(),
        working_dir,
        sid,
    )
    .expect("retry should succeed");
    assert_eq!(
        again.session_dir,
        SessionDirCarry {
            skipped_present: 3,
            ..SessionDirCarry::default()
        }
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn copy_transcript_without_session_dir_carries_nothing() {
    let tmp = std::env::temp_dir().join(format!("qontinui-acctmig-nodir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let (src_cfg, dst_cfg) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    let working_dir = "D:\\qontinui-root";
    let sid = "22222222-3333-4444-5555-666666666666";
    seed_session(&src_cfg, working_dir, sid, &[]);

    let carried = copy_transcript(
        src_cfg.to_str().unwrap(),
        dst_cfg.to_str().unwrap(),
        working_dir,
        sid,
    )
    .expect("a session that spawned no subagent still migrates");
    assert!(carried.path.exists());
    assert_eq!(carried.session_dir, SessionDirCarry::default());
    assert!(!session_dir_of(&dst_cfg, working_dir, sid).exists());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn copy_transcript_never_truncates_a_newer_destination_subagent_file() {
    let tmp = std::env::temp_dir().join(format!("qontinui-acctmig-newer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let (src_cfg, dst_cfg) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    let working_dir = "D:\\qontinui-root";
    let sid = "33333333-4444-5555-6666-777777777777";
    seed_session(
        &src_cfg,
        working_dir,
        sid,
        &[
            ("subagents/agent-old.jsonl", b"short"),
            (
                "subagents/agent-grown.jsonl",
                b"a much longer source transcript",
            ),
        ],
    );
    // Ping-pong: the destination already holds a LONGER copy of one file
    // (the session ran there after an earlier migration) and a SHORTER,
    // stale copy of the other — an older prefix of the source's bytes.
    seed_session(
        &dst_cfg,
        working_dir,
        sid,
        &[
            ("subagents/agent-old.jsonl", b"short, then appended to"),
            ("subagents/agent-grown.jsonl", b"a much"),
        ],
    );

    // Outside a migration (the respawn's `ExtendPrefix`); a migration moves
    // the larger one aside instead — see
    // `a_migration_moves_a_larger_diverged_subagent_aside`.
    let carried = carry_session(
        src_cfg.to_str().unwrap(),
        dst_cfg.to_str().unwrap(),
        working_dir,
        sid,
        MainTranscript::ExtendPrefix,
    )
    .expect("copy should succeed");
    assert_eq!(
        carried.session_dir,
        SessionDirCarry {
            copied: 1,
            skipped_present: 1,
            ..SessionDirCarry::default()
        }
    );
    let dst_dir = session_dir_of(&dst_cfg, working_dir, sid);
    assert_eq!(
        std::fs::read(dst_dir.join("subagents/agent-old.jsonl")).unwrap(),
        b"short, then appended to",
        "a larger destination file is the newer copy and is left alone"
    );
    assert_eq!(
        std::fs::read(dst_dir.join("subagents/agent-grown.jsonl")).unwrap(),
        b"a much longer source transcript",
        "a smaller destination file is replaced with the source bytes"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn carry_session_dir_refuses_a_session_id_that_is_not_one_path_component() {
    let tmp =
        std::env::temp_dir().join(format!("qontinui-acctmig-traverse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src_project = tmp.join("src").join("project");
    let dst_project = tmp.join("dst").join("project");
    // A sibling directory a hostile id could reach with `..`.
    std::fs::create_dir_all(&src_project).unwrap();
    std::fs::create_dir_all(tmp.join("src").join("secret")).unwrap();
    std::fs::write(tmp.join("src").join("secret").join("f"), b"x").unwrap();
    let mut bad_ids = vec!["..", "../secret", "a/b", ""];
    if cfg!(windows) {
        bad_ids.extend(["..\\secret", "C:", "C:secret"]);
    }
    for bad in bad_ids {
        let (carried, files) = carry_session_dir(
            &src_project.join("t.jsonl"),
            &dst_project.join("t.jsonl"),
            bad,
            true,
            true,
            &mut |from: &Path, to: &Path| std::fs::rename(from, to),
        );
        assert!(files.is_empty());
        assert_eq!(
            carried,
            SessionDirCarry {
                skipped_other: 1,
                ..SessionDirCarry::default()
            },
            "id {bad:?} must carry nothing, and say so rather than read as 'no subagents'"
        );
    }
    assert!(!tmp.join("dst").exists());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn copy_transcript_replaces_a_stale_non_transcript_file_even_when_larger() {
    let tmp = std::env::temp_dir().join(format!("qontinui-acctmig-title-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let (src_cfg, dst_cfg) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    let working_dir = "D:\\qontinui-root";
    let sid = "44444444-5555-6666-7777-888888888888";
    // Renamed to something SHORTER on the account the session just ran on;
    // the destination holds the older, longer title from a prior migration.
    seed_session(
        &src_cfg,
        working_dir,
        sid,
        &[("custom-title.json", b"{\"customTitle\":\"x\"}")],
    );
    seed_session(
        &dst_cfg,
        working_dir,
        sid,
        &[(
            "custom-title.json",
            b"{\"customTitle\":\"a much longer old title\"}",
        )],
    );
    let carried = copy_transcript(
        src_cfg.to_str().unwrap(),
        dst_cfg.to_str().unwrap(),
        working_dir,
        sid,
    )
    .expect("copy should succeed");
    assert_eq!(carried.session_dir.copied, 1);
    assert_eq!(
        std::fs::read(session_dir_of(&dst_cfg, working_dir, sid).join("custom-title.json"))
            .unwrap(),
        b"{\"customTitle\":\"x\"}",
        "only append-only .jsonl files use the size rule"
    );
    // Identical bytes afterwards: a retry leaves it alone.
    let again = copy_transcript(
        src_cfg.to_str().unwrap(),
        dst_cfg.to_str().unwrap(),
        working_dir,
        sid,
    )
    .expect("retry should succeed");
    assert_eq!(again.session_dir.skipped_present, 1);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn copy_transcript_keeps_the_destination_sidecar_when_the_destination_ran_last() {
    let tmp =
        std::env::temp_dir().join(format!("qontinui-acctmig-newer-dst-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let (src_cfg, dst_cfg) = (tmp.join(".claude-old"), tmp.join(".claude-new"));
    let working_dir = "D:\\qontinui-root";
    let sid = "88888888-9999-aaaa-bbbb-cccccccccccc";
    // A respawn topping up from an OLDER holder: the target's main
    // transcript is the larger one, so its rewritten-in-place sidecar wins.
    seed_session(
        &src_cfg,
        working_dir,
        sid,
        &[("custom-title.json", b"{\"customTitle\":\"old\"}")],
    );
    seed_session(
        &dst_cfg,
        working_dir,
        sid,
        &[("custom-title.json", b"{\"customTitle\":\"new\"}")],
    );
    let dst_main = crate::terminal::transcript::session_transcript_path(&dst_cfg, working_dir, sid);
    std::fs::write(
        &dst_main,
        b"{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n",
    )
    .unwrap();

    // The respawn's mode (`ExtendPrefix`): a migration (`LiveSourceWins`)
    // would instead move this larger destination aside — the live source wins.
    let carried = carry_session(
        src_cfg.to_str().unwrap(),
        dst_cfg.to_str().unwrap(),
        working_dir,
        sid,
        MainTranscript::ExtendPrefix,
    )
    .expect("copy should succeed");
    assert_eq!(carried.session_dir.skipped_present, 1);
    assert_eq!(
        std::fs::read(session_dir_of(&dst_cfg, working_dir, sid).join("custom-title.json"))
            .unwrap(),
        b"{\"customTitle\":\"new\"}",
        "the side that ran the session last keeps its sidecar"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn copy_transcript_refuses_a_session_id_that_is_not_a_uuid() {
    let tmp = std::env::temp_dir().join(format!("qontinui-acctmig-notuuid-{}", std::process::id()));
    let err = copy_transcript(
        tmp.join("src").to_str().unwrap(),
        tmp.join("dst").to_str().unwrap(),
        "D:\\qontinui-root",
        "../11111111-2222-3333-4444-555555555555",
    )
    .unwrap_err();
    assert!(err.contains("expected a uuid"), "got: {err}");
    assert!(!tmp.exists(), "nothing is written for a refused id");
}

#[test]
fn copy_transcript_never_carries_a_leftover_partial_file() {
    let tmp = std::env::temp_dir().join(format!("qontinui-acctmig-partial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let (src_cfg, dst_cfg) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    let working_dir = "D:\\qontinui-root";
    let sid = "55555555-6666-7777-8888-999999999999";
    seed_session(
        &src_cfg,
        working_dir,
        sid,
        &[
            ("subagents/agent-a.jsonl", b"{}\n"),
            ("subagents/agent-b.jsonl.partial", b"half a co"),
        ],
    );
    let carried = copy_transcript(
        src_cfg.to_str().unwrap(),
        dst_cfg.to_str().unwrap(),
        working_dir,
        sid,
    )
    .expect("copy should succeed");
    assert_eq!(
        carried.session_dir,
        SessionDirCarry {
            copied: 1,
            skipped_other: 1,
            ..SessionDirCarry::default()
        }
    );
    let dst_sub = session_dir_of(&dst_cfg, working_dir, sid).join("subagents");
    assert!(dst_sub.join("agent-a.jsonl").exists());
    assert!(!dst_sub.join("agent-b.jsonl.partial").exists());
    let leftovers: Vec<_> = std::fs::read_dir(&dst_sub)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(PARTIAL_SUFFIX))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no temp file left behind: {leftovers:?}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Link `link` -> `target` as a directory symlink (unix) or a junction
/// (Windows; `mklink /J` needs no privilege). `false` when the platform
/// refused, so the caller can say the case went unexercised.
///
/// `#[cfg(test)]` is redundant here (this whole file is a test module) but is
/// what `scripts/check_untimed_subprocess.py` reads to skip a fixture: its
/// untimed `mklink` was exempt while it lived in `account_migration.rs`'s
/// inline `mod tests`.
#[cfg(test)]
fn link_dir(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|out| out.status.success())
    }
}

#[test]
fn carry_never_follows_a_linked_directory() {
    let tmp = std::env::temp_dir().join(format!("qontinui-acctmig-links-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let (src_cfg, dst_cfg) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    let working_dir = "D:\\qontinui-root";
    let sid = "66666666-7777-8888-9999-aaaaaaaaaaaa";
    let session_dir = seed_session(
        &src_cfg,
        working_dir,
        sid,
        &[("subagents/agent-a.jsonl", b"{}\n")],
    );
    let outside = tmp.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"must not be copied").unwrap();
    assert!(
        link_dir(&outside, &session_dir.join("linked")),
        "could not create a directory link to exercise the rule"
    );

    let carried = copy_transcript(
        src_cfg.to_str().unwrap(),
        dst_cfg.to_str().unwrap(),
        working_dir,
        sid,
    )
    .expect("copy should succeed");
    assert_eq!(
        carried.session_dir,
        SessionDirCarry {
            copied: 1,
            skipped_other: 1,
            ..SessionDirCarry::default()
        }
    );
    let dst_dir = session_dir_of(&dst_cfg, working_dir, sid);
    assert!(
        !dst_dir.join("linked").exists(),
        "a linked dir is never followed"
    );

    // `<sid>` itself being a link is refused and counted, not read as "none".
    let other_sid = "77777777-8888-9999-aaaa-bbbbbbbbbbbb";
    let other_transcript =
        crate::terminal::transcript::session_transcript_path(&src_cfg, working_dir, other_sid);
    std::fs::write(&other_transcript, b"{}\n").unwrap();
    assert!(link_dir(
        &session_dir,
        &other_transcript.parent().unwrap().join(other_sid)
    ));
    let linked_root = copy_transcript(
        src_cfg.to_str().unwrap(),
        dst_cfg.to_str().unwrap(),
        working_dir,
        other_sid,
    )
    .expect("main transcript still migrates");
    assert_eq!(
        linked_root.session_dir,
        SessionDirCarry {
            skipped_other: 1,
            ..SessionDirCarry::default()
        }
    );
    assert!(!session_dir_of(&dst_cfg, working_dir, other_sid).exists());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn copy_transcript_missing_source_errors() {
    let tmp = std::env::temp_dir().join(format!("qontinui-acctmig-missing-{}", std::process::id()));
    let err = copy_transcript(
        tmp.join("nope-src").to_str().unwrap(),
        tmp.join("nope-dst").to_str().unwrap(),
        "D:\\qontinui-root",
        "00000000-0000-0000-0000-000000000000",
    )
    .unwrap_err();
    assert!(err.contains("source transcript missing"), "got: {err}");
}

// ---- Phase 2 (plan 2026-10-21-account-migration-strands-subagent-transcripts-and-forks-live-sessions)

/// A fresh, unique scratch root.
fn scratch(label: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "qontinui-session-carry-{label}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

const WD: &str = "D:\\qontinui-root";
const SID: &str = "0c4e3025-8c5c-4792-a3ca-201ad43e0113";

/// `<cfg>/projects/D--qontinui-root/<SID>/` spelled out literally, so a drift
/// in the path rule fails here rather than agreeing with itself.
fn literal_sidecar(cfg: &Path) -> PathBuf {
    cfg.join("projects").join("D--qontinui-root").join(SID)
}

fn carry(src: &Path, dst: &Path) -> TranscriptCopy {
    copy_transcript(src.to_str().unwrap(), dst.to_str().unwrap(), WD, SID).unwrap()
}

fn sync(since: &TranscriptCopy) -> DeltaSync {
    delta_sync(since).unwrap()
}

fn append(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(bytes).unwrap();
}

#[test]
fn the_carry_records_the_two_sidecar_dirs_it_used() {
    let tmp = scratch("dirs");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(&src, WD, SID, &[]);
    let carried = carry(&src, &dst);
    assert_eq!(carried.src_session_dir, literal_sidecar(&src));
    assert_eq!(carried.dst_session_dir, literal_sidecar(&dst));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn a_session_with_no_subagents_lists_none_but_still_carries_its_sidecars() {
    let tmp = scratch("zero");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(&src, WD, SID, &[("tool-results/r1.txt", b"out")]);

    let carried = carry(&src, &dst);
    assert!(carried.subagents.is_empty());
    assert_eq!(carried.session_dir.copied, 1);
    assert_eq!(
        std::fs::read(literal_sidecar(&dst).join("tool-results").join("r1.txt")).unwrap(),
        b"out"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn one_subagent_is_carried_and_listed_with_its_description() {
    let tmp = scratch("one");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(
        &src,
        WD,
        SID,
        &[
            ("subagents/agent-a10fe2a6718228f32.jsonl", b"{}\n"),
            (
                "subagents/agent-a10fe2a6718228f32.meta.json",
                br#"{"agentType":"general-purpose","description":"Finish Phase 2"}"#,
            ),
        ],
    );

    let carried = carry(&src, &dst);
    assert_eq!(
        carried.subagents,
        vec![CarriedSubagent {
            agent_id: "a10fe2a6718228f32".to_string(),
            description: Some("Finish Phase 2".to_string()),
        }]
    );
    assert!(literal_sidecar(&dst)
        .join("subagents")
        .join("agent-a10fe2a6718228f32.jsonl")
        .is_file());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn many_subagents_and_nested_tool_results_are_all_carried() {
    let tmp = scratch("many");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(
        &src,
        WD,
        SID,
        &[
            ("subagents/agent-c3.jsonl", b"c\n"),
            ("subagents/agent-a1.jsonl", b"a\n"),
            (
                "subagents/agent-a1.meta.json",
                br#"{"description":"first"}"#,
            ),
            ("subagents/agent-b2.jsonl", b"b\n"),
            ("subagents/agent-b2.meta.json", b"not json"),
            ("tool-results/deep/nested/r.txt", b"nested"),
            ("custom-title.json", br#"{"title":"t"}"#),
        ],
    );

    let carried = carry(&src, &dst);
    let ids: Vec<&str> = carried
        .subagents
        .iter()
        .map(|a| a.agent_id.as_str())
        .collect();
    assert_eq!(ids, ["a1", "b2", "c3"], "every subagent, sorted by id");
    assert_eq!(carried.subagents[0].description.as_deref(), Some("first"));
    assert_eq!(carried.subagents[1].description, None, "unparseable meta");
    assert_eq!(carried.session_dir.copied, 7);
    assert_eq!(carried.files.len(), 7, "every file recorded for delta_sync");
    let side = literal_sidecar(&dst);
    for rel in [
        "subagents/agent-a1.jsonl",
        "subagents/agent-b2.jsonl",
        "subagents/agent-c3.jsonl",
        "tool-results/deep/nested/r.txt",
        "custom-title.json",
    ] {
        assert!(side.join(rel).is_file(), "{rel} carried");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn a_missing_session_dir_records_nothing() {
    let tmp = scratch("missing");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(&src, WD, SID, &[]);
    let carried = carry(&src, &dst);
    assert!(carried.files.is_empty() && carried.subagents.is_empty());
    assert_eq!(
        sync(&carried),
        DeltaSync::default(),
        "no <sid>/ on the source: nothing to top up"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn a_repeat_carry_copies_nothing_and_records_the_same_destination() {
    let tmp = scratch("repeat");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(
        &src,
        WD,
        SID,
        &[
            ("subagents/agent-a1.jsonl", b"a\n"),
            ("tool-results/r.txt", b"r"),
        ],
    );
    let first = carry(&src, &dst);
    let second = carry(&src, &dst);
    assert_eq!(second.session_dir.copied, 0);
    assert_eq!(second.session_dir.skipped_present, 2);
    let stamps = |c: &TranscriptCopy| {
        let mut v: Vec<(PathBuf, Option<FileStamp>)> =
            c.files.iter().map(|f| (f.rel.clone(), f.dst)).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    };
    assert_eq!(stamps(&first), stamps(&second));
    assert_eq!(first.subagents, second.subagents);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Outside a migration ([`MainTranscript::ExtendPrefix`]) a larger destination
/// subagent is the newer copy and is left alone.
#[test]
fn a_destination_subagent_larger_than_the_source_is_skipped() {
    let tmp = scratch("dst-larger");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(&src, WD, SID, &[("subagents/agent-a1.jsonl", b"a\n")]);
    let dst_file = literal_sidecar(&dst)
        .join("subagents")
        .join("agent-a1.jsonl");
    std::fs::create_dir_all(dst_file.parent().unwrap()).unwrap();
    std::fs::write(&dst_file, b"a\nand more\n").unwrap();

    let carried = carry_session(
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
        WD,
        SID,
        MainTranscript::ExtendPrefix,
    )
    .unwrap();
    assert_eq!(carried.session_dir.skipped_present, 1);
    assert_eq!(std::fs::read(&dst_file).unwrap(), b"a\nand more\n");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn delta_sync_copies_a_grown_source_and_a_new_file() {
    let tmp = scratch("delta-grow");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    let src_side = seed_session(&src, WD, SID, &[("subagents/agent-a1.jsonl", b"a\n")]);
    let carried = carry(&src, &dst);

    // The old pane keeps writing between the carry and its exit.
    append(&src_side.join("subagents").join("agent-a1.jsonl"), b"a2\n");
    std::fs::write(src_side.join("subagents").join("agent-n9.jsonl"), b"new\n").unwrap();

    let done = sync(&carried);
    assert_eq!(done.copied, 2, "{done:?}");
    let dst_sub = literal_sidecar(&dst).join("subagents");
    assert_eq!(
        std::fs::read(dst_sub.join("agent-a1.jsonl")).unwrap(),
        b"a\na2\n"
    );
    assert_eq!(
        std::fs::read(dst_sub.join("agent-n9.jsonl")).unwrap(),
        b"new\n"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn delta_sync_leaves_an_unchanged_source_alone() {
    let tmp = scratch("delta-same");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(
        &src,
        WD,
        SID,
        &[
            ("subagents/agent-a1.jsonl", b"a\n"),
            ("custom-title.json", b"{}"),
        ],
    );
    let carried = carry(&src, &dst);
    assert_eq!(
        sync(&carried),
        DeltaSync {
            unchanged: 2,
            ..DeltaSync::default()
        }
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn delta_sync_refuses_a_destination_written_after_the_carry() {
    let tmp = scratch("delta-refuse");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    let src_side = seed_session(&src, WD, SID, &[("subagents/agent-a1.jsonl", b"a\n")]);
    let carried = carry(&src, &dst);
    let dst_sub = literal_sidecar(&dst).join("subagents");
    let dst_file = dst_sub.join("agent-a1.jsonl");

    // Both sides moved on: the old pane AND the resumed process.
    append(
        &src_side.join("subagents").join("agent-a1.jsonl"),
        b"old pane, much longer\n",
    );
    append(&dst_file, b"resumed\n");
    // A file the carry never put there, at a path the source now also has.
    std::fs::write(dst_sub.join("agent-r7.jsonl"), b"r\n").unwrap();
    std::fs::write(
        src_side.join("subagents").join("agent-r7.jsonl"),
        b"old r, longer\n",
    )
    .unwrap();

    let done = sync(&carried);
    assert_eq!(done.refused_destination_changed, 2, "{done:?}");
    assert_eq!(done.copied, 0);
    assert_eq!(std::fs::read(&dst_file).unwrap(), b"a\nresumed\n");
    assert_eq!(
        std::fs::read(dst_sub.join("agent-r7.jsonl")).unwrap(),
        b"r\n"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn delta_sync_retries_a_file_the_carry_failed_to_write() {
    let tmp = scratch("delta-retry");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(&src, WD, SID, &[("subagents/agent-a1.jsonl", b"a\n")]);
    let mut carried = carry(&src, &dst);
    // As if the carry's write had failed: no destination, recorded absent
    // and `Failed` — exactly what a real failed carry records.
    let dst_file = literal_sidecar(&dst)
        .join("subagents")
        .join("agent-a1.jsonl");
    std::fs::remove_file(&dst_file).unwrap();
    carried.files[0].dst = None;
    carried.files[0].wrote = CarryWrite::Failed;

    let done = sync(&carried);
    assert_eq!(done.copied, 1, "{done:?}");
    assert_eq!(std::fs::read(&dst_file).unwrap(), b"a\n");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn delta_sync_never_touches_the_main_transcript() {
    let tmp = scratch("delta-main");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(&src, WD, SID, &[("subagents/agent-a1.jsonl", b"a\n")]);
    let carried = carry(&src, &dst);
    let src_main = crate::terminal::transcript::session_transcript_path(&src, WD, SID);
    append(&src_main, b"{\"type\":\"assistant\"}\n");
    let before = std::fs::read(&carried.path).unwrap();

    sync(&carried);
    assert_eq!(std::fs::read(&carried.path).unwrap(), before);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Overwrite `<cfg>/projects/<slug>/<SID>.jsonl` with `bytes`.
fn write_main(cfg: &Path, bytes: &[u8]) {
    let main = crate::terminal::transcript::session_transcript_path(cfg, WD, SID);
    std::fs::write(main, bytes).unwrap();
}

fn read_main(cfg: &Path) -> Vec<u8> {
    std::fs::read(crate::terminal::transcript::session_transcript_path(
        cfg, WD, SID,
    ))
    .unwrap()
}

const SRC_MAIN: &[u8] =
    b"{\"type\":\"user\"}\n{\"type\":\"assistant\",\"tail\":\"source, longer\"}\n";
const FORK_MAIN: &[u8] = b"{\"type\":\"user\"}\n{\"fork\":1}\n";

/// A destination that FORKED: its `.jsonl` files (main and subagent) are
/// smaller than the source's but not a prefix of them, so each carries a tail
/// of its own; its `custom-title.json` differs too. Returns the source's
/// `<sid>/`.
fn seed_forked(src: &Path, dst: &Path) -> PathBuf {
    let src_side = seed_session(
        src,
        WD,
        SID,
        &[
            ("subagents/agent-a1.jsonl", b"a\nsource tail, longer\n"),
            ("custom-title.json", br#"{"customTitle":"src"}"#),
        ],
    );
    seed_session(
        dst,
        WD,
        SID,
        &[
            ("subagents/agent-a1.jsonl", b"a\nfork\n"),
            ("custom-title.json", br#"{"customTitle":"fork"}"#),
        ],
    );
    write_main(src, SRC_MAIN);
    write_main(dst, FORK_MAIN);
    src_side
}

/// Every `<SID>.jsonl.diverged-*` file in the destination's project dir.
fn diverged_backups(cfg: &Path) -> Vec<PathBuf> {
    let main = crate::terminal::transcript::session_transcript_path(cfg, WD, SID);
    let prefix = format!("{SID}.jsonl.diverged-");
    std::fs::read_dir(main.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .map(|e| e.path())
        .collect()
}

/// W1 — on a MIGRATION the source is the live conversation and is what
/// resumes. A destination `<sid>.jsonl` that diverged from it is moved aside
/// (its bytes survive at `<sid>.jsonl.diverged-<ms>`), and the destination
/// then holds the source's bytes. A diverged SUBAGENT transcript is treated
/// the same way (W2): its fork survives as `<file>.diverged-<ms>` beside it,
/// the destination ends up equal to the source, and the delta sync then tops
/// it up like any carried file. The main transcript is not counted in the
/// `<sid>/` tally.
#[test]
fn a_migration_moves_a_diverged_main_transcript_aside_and_copies_the_live_one() {
    let tmp = scratch("diverged");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    let src_side = seed_forked(&src, &dst);

    let carried = carry(&src, &dst);
    let MainCarry::DivergedPreserved { backup } = &carried.main else {
        panic!("expected the fork preserved, got {:?}", carried.main);
    };
    assert_eq!(
        std::fs::read(backup).unwrap(),
        FORK_MAIN,
        "the fork's bytes survive at the backup path"
    );
    assert_eq!(
        backup.parent(),
        carried.path.parent(),
        "beside the transcript"
    );
    assert!(backup
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with(&format!("{SID}.jsonl.diverged-")));
    assert_eq!(
        read_main(&dst),
        SRC_MAIN,
        "the live conversation now resumes"
    );
    assert_eq!(diverged_backups(&dst), vec![backup.clone()]);
    assert_eq!(carried.main.event_name(), "diverged_preserved");
    assert_eq!(carried.main.backup(), Some(backup.as_path()));
    assert_eq!(
        carried.session_dir,
        SessionDirCarry {
            copied: 1,
            diverged_preserved: 1,
            ..SessionDirCarry::default()
        },
        "the subagent fork is preserved aside; the main transcript is not counted here; \
         the live side's in-place custom-title follows the live transcript"
    );
    let dst_side = literal_sidecar(&dst);
    let dst_sub = dst_side.join("subagents").join("agent-a1.jsonl");
    assert_eq!(
        std::fs::read(&dst_sub).unwrap(),
        b"a\nsource tail, longer\n",
        "the destination subagent now equals the live source"
    );
    let sub_backups = subagent_backups(&dst, "agent-a1.jsonl");
    assert_eq!(sub_backups.len(), 1, "{sub_backups:?}");
    assert_eq!(
        std::fs::read(&sub_backups[0]).unwrap(),
        b"a\nfork\n",
        "the subagent fork survives as a backup beside it"
    );
    let recorded = carried
        .files
        .iter()
        .find(|f| f.rel == Path::new("subagents").join("agent-a1.jsonl"))
        .unwrap();
    assert_eq!(recorded.wrote, CarryWrite::Copied);
    assert_eq!(agent_ids(&carried), ["a1"]);

    // The old pane writes more; the delta sync tops up the live copy and
    // never touches the backup.
    append(
        &src_side.join("subagents").join("agent-a1.jsonl"),
        b"and the old pane wrote even more\n",
    );
    let done = sync(&carried);
    assert_eq!(done.copied, 1, "{done:?}");
    assert_eq!(
        std::fs::read(&dst_sub).unwrap(),
        b"a\nsource tail, longer\nand the old pane wrote even more\n"
    );
    assert_eq!(std::fs::read(&sub_backups[0]).unwrap(), b"a\nfork\n");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Every `<name>.diverged-*` file in the destination's `<SID>/subagents/`.
fn subagent_backups(cfg: &Path, name: &str) -> Vec<PathBuf> {
    let prefix = format!("{name}.diverged-");
    std::fs::read_dir(literal_sidecar(cfg).join("subagents"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .map(|e| e.path())
        .collect()
}

fn agent_ids(carried: &TranscriptCopy) -> Vec<&str> {
    carried
        .subagents
        .iter()
        .map(|a| a.agent_id.as_str())
        .collect()
}

/// Seed a source subagent `agent-a1.jsonl` = `src_bytes` and a destination
/// copy = `dst_bytes` (main transcripts identical), then migrate.
fn migrate_subagent(label: &str, src_bytes: &[u8], dst_bytes: &[u8]) -> (PathBuf, TranscriptCopy) {
    let tmp = scratch(label);
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(&src, WD, SID, &[("subagents/agent-a1.jsonl", src_bytes)]);
    seed_session(&dst, WD, SID, &[("subagents/agent-a1.jsonl", dst_bytes)]);
    let carried = carry(&src, &dst);
    (tmp, carried)
}

/// W2 — a destination subagent LARGER than the live source's (a stale fork
/// that grew past it) is moved aside on a migration, and the destination then
/// holds the source's bytes; the subagent is listed as carried.
#[test]
fn a_migration_moves_a_larger_diverged_subagent_aside() {
    let fork: &[u8] = b"a\nfork that kept running and grew past the live one\n";
    let (tmp, carried) = migrate_subagent("sub-larger", b"a\nlive\n", fork);
    let dst = tmp.join(".claude-b");
    assert_eq!(carried.main, MainCarry::Present);
    assert_eq!(
        carried.session_dir,
        SessionDirCarry {
            diverged_preserved: 1,
            ..SessionDirCarry::default()
        }
    );
    let dst_sub = literal_sidecar(&dst)
        .join("subagents")
        .join("agent-a1.jsonl");
    assert_eq!(std::fs::read(&dst_sub).unwrap(), b"a\nlive\n");
    let backups = subagent_backups(&dst, "agent-a1.jsonl");
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(std::fs::read(&backups[0]).unwrap(), fork);
    assert_eq!(agent_ids(&carried), ["a1"]);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// W2 — the same size with other bytes is a fork too: a size check alone
/// would call it current.
#[test]
fn a_migration_moves_a_same_size_different_bytes_subagent_aside() {
    let (tmp, carried) = migrate_subagent("sub-same-size", b"a\nlive\n", b"a\nfork\n");
    let dst = tmp.join(".claude-b");
    assert_eq!(carried.session_dir.diverged_preserved, 1);
    assert_eq!(
        std::fs::read(
            literal_sidecar(&dst)
                .join("subagents")
                .join("agent-a1.jsonl")
        )
        .unwrap(),
        b"a\nlive\n"
    );
    let backups = subagent_backups(&dst, "agent-a1.jsonl");
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(std::fs::read(&backups[0]).unwrap(), b"a\nfork\n");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// W2 — byte-identical stays `Present` (untouched, no backup); a strict
/// prefix is extended in place (no backup).
#[test]
fn a_migration_keeps_an_identical_subagent_and_extends_a_prefix_one() {
    let (tmp, identical) = migrate_subagent("sub-identical", b"a\nlive\n", b"a\nlive\n");
    assert_eq!(identical.session_dir.skipped_present, 1);
    assert!(subagent_backups(&tmp.join(".claude-b"), "agent-a1.jsonl").is_empty());
    let _ = std::fs::remove_dir_all(&tmp);

    let (tmp, prefix) = migrate_subagent("sub-prefix", b"a\nlive\n", b"a\n");
    assert_eq!(prefix.session_dir.copied, 1);
    assert_eq!(prefix.session_dir.diverged_preserved, 0);
    let dst = tmp.join(".claude-b");
    assert!(subagent_backups(&dst, "agent-a1.jsonl").is_empty());
    assert_eq!(
        std::fs::read(
            literal_sidecar(&dst)
                .join("subagents")
                .join("agent-a1.jsonl")
        )
        .unwrap(),
        b"a\nlive\n"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// W2 — a diverged subagent that cannot be moved aside is skipped, not
/// fatal: the carry succeeds, the file is counted `failed`, the fork is left
/// exactly as it was (no backup, no placeholder), and the subagent is NOT
/// listed as resumable.
#[test]
fn a_subagent_that_cannot_be_moved_aside_is_skipped_and_not_listed() {
    let tmp = scratch("sub-rename-fails");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(
        &src,
        WD,
        SID,
        &[
            ("subagents/agent-a1.jsonl", b"a\nlive\n"),
            ("subagents/agent-b2.jsonl", b"b\n"),
        ],
    );
    seed_session(&dst, WD, SID, &[("subagents/agent-a1.jsonl", b"a\nfork\n")]);

    let carried = carry_session_with(
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
        WD,
        SID,
        MainTranscript::LiveSourceWins,
        |from, to| {
            if from.ends_with("agent-a1.jsonl") {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                std::fs::rename(from, to)
            }
        },
        || {},
    )
    .expect("a subagent never fails the carry");
    assert_eq!(
        carried.session_dir,
        SessionDirCarry {
            copied: 1,
            failed: 1,
            ..SessionDirCarry::default()
        }
    );
    assert_eq!(
        std::fs::read(
            literal_sidecar(&dst)
                .join("subagents")
                .join("agent-a1.jsonl")
        )
        .unwrap(),
        b"a\nfork\n",
        "the fork is not overwritten"
    );
    assert!(subagent_backups(&dst, "agent-a1.jsonl").is_empty());
    assert_eq!(agent_ids(&carried), ["b2"], "a1 is not called resumable");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// W1(c) — two backups claimed for the same file in the same millisecond get
/// distinct names, and neither replaces an earlier backup.
#[test]
fn two_backups_in_the_same_millisecond_get_distinct_names() {
    let tmp = scratch("backup-names");
    let dst = tmp.join(format!("{SID}.jsonl"));
    std::fs::write(&dst, b"x").unwrap();
    let earlier = tmp.join(format!("{SID}.jsonl.diverged-1000"));
    std::fs::write(&earlier, b"an earlier backup").unwrap();

    let first = claim_diverged_backup_at(&dst, 1000).unwrap();
    let second = claim_diverged_backup_at(&dst, 1000).unwrap();
    assert_ne!(first, second);
    assert_ne!(first, earlier);
    assert_ne!(second, earlier);
    assert!(
        first.is_file() && second.is_file(),
        "both names are claimed"
    );
    assert_eq!(
        std::fs::read(&earlier).unwrap(),
        b"an earlier backup",
        "an earlier backup is never replaced"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// S1 — the main transcript is classified prefix-first from one read. The
/// live source grows between the carry's stat and its comparison (`after_stat`
/// appends to it): a destination identical to the source as stat'ed is a
/// strict prefix of the grown one and is EXTENDED, never moved aside as a
/// same-size fork.
#[test]
fn a_source_that_grows_after_the_stat_is_extended_not_forked() {
    let tmp = scratch("growing-source");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(&src, WD, SID, &[]);
    seed_session(&dst, WD, SID, &[]);
    let src_main = crate::terminal::transcript::session_transcript_path(&src, WD, SID);

    let carried = carry_session_with(
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
        WD,
        SID,
        MainTranscript::LiveSourceWins,
        |from, to| std::fs::rename(from, to),
        || append(&src_main, b"{\"type\":\"assistant\"}\n"),
    )
    .unwrap();
    assert_eq!(carried.main, MainCarry::ExtendedPrefix);
    assert!(diverged_backups(&dst).is_empty(), "no fork was invented");
    assert_eq!(
        read_main(&dst),
        b"{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// W1 — when the fork cannot be moved aside the carry FAILS (so the migration
/// leaves the old pane untouched) and nothing is overwritten.
#[test]
fn a_failed_rename_aside_fails_the_carry_and_overwrites_nothing() {
    let tmp = scratch("diverged-rename-fails");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_forked(&src, &dst);

    let err = carry_session_with(
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
        WD,
        SID,
        MainTranscript::LiveSourceWins,
        |_, _| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        || {},
    )
    .unwrap_err();
    assert!(err.contains("could not be moved aside"), "got: {err}");
    assert_eq!(read_main(&dst), FORK_MAIN, "the fork is untouched");
    assert!(
        diverged_backups(&dst).is_empty(),
        "the claimed placeholder is removed again"
    );
    assert_eq!(
        std::fs::read(literal_sidecar(&dst).join("custom-title.json")).unwrap(),
        br#"{"customTitle":"fork"}"#,
        "a failed step 1 carries no side-car either"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Outside a migration ([`MainTranscript::ExtendPrefix`], the respawn's mode
/// for an absent target) a diverged main transcript is KEPT, as before: no
/// backup, and the fork keeps its in-place sidecar.
#[test]
fn extend_prefix_keeps_a_diverged_main_transcript() {
    let tmp = scratch("diverged-keep");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_forked(&src, &dst);

    let carried = carry_session(
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
        WD,
        SID,
        MainTranscript::ExtendPrefix,
    )
    .unwrap();
    assert_eq!(carried.main, MainCarry::KeptExisting);
    assert_eq!(read_main(&dst), FORK_MAIN);
    assert!(diverged_backups(&dst).is_empty());
    assert_eq!(
        carried.session_dir,
        SessionDirCarry {
            skipped_present: 1,
            diverged_kept: 1,
            ..SessionDirCarry::default()
        },
        "a kept subagent fork is `diverged_kept`, not `skipped_other` (not followed)"
    );
    assert_eq!(
        std::fs::read(
            literal_sidecar(&dst)
                .join("subagents")
                .join("agent-a1.jsonl")
        )
        .unwrap(),
        b"a\nfork\n"
    );
    assert_eq!(
        std::fs::read(literal_sidecar(&dst).join("custom-title.json")).unwrap(),
        br#"{"customTitle":"fork"}"#
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// On a migration the live source wins WHATEVER the sizes: a destination
/// LARGER than the source whose bytes are not the source's (a stale fork that
/// grew past the live one) is moved aside, and the destination then holds the
/// source's bytes — previously it read as current, was resumed, and the live
/// conversation was dropped. The live side's in-place sidecar follows.
#[test]
fn a_migration_moves_a_larger_diverged_main_transcript_aside() {
    let tmp = scratch("diverged-larger");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_forked(&src, &dst);
    let larger_fork: &[u8] = b"{\"type\":\"user\"}\n{\"fork\":\"a stale fork that kept running and grew past the live one\"}\n";
    assert!(larger_fork.len() > SRC_MAIN.len());
    write_main(&dst, larger_fork);

    let carried = carry(&src, &dst);
    let MainCarry::DivergedPreserved { backup } = &carried.main else {
        panic!("expected the larger fork preserved, got {:?}", carried.main);
    };
    assert_eq!(std::fs::read(backup).unwrap(), larger_fork);
    assert_eq!(diverged_backups(&dst), vec![backup.clone()]);
    assert_eq!(read_main(&dst), SRC_MAIN, "the live conversation resumes");
    assert_eq!(
        std::fs::read(literal_sidecar(&dst).join("custom-title.json")).unwrap(),
        br#"{"customTitle":"src"}"#,
        "the live side's in-place sidecar follows the live transcript"
    );
    let title = carried
        .files
        .iter()
        .find(|f| f.rel == Path::new("custom-title.json"))
        .unwrap();
    assert!(!title.kept_destination_newer, "{title:?}");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A destination the SAME size as the source but with other bytes is a fork
/// too: a size check alone would call it current.
#[test]
fn a_migration_moves_a_same_size_different_bytes_main_transcript_aside() {
    let tmp = scratch("diverged-same-size");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_forked(&src, &dst);
    // Same length as SRC_MAIN, one byte different near the end.
    let mut same_size = SRC_MAIN.to_vec();
    let at = same_size.len() - 3;
    same_size[at] = b'X';
    assert_ne!(same_size, SRC_MAIN);
    write_main(&dst, &same_size);

    let carried = carry(&src, &dst);
    let MainCarry::DivergedPreserved { backup } = &carried.main else {
        panic!(
            "expected the same-size fork preserved, got {:?}",
            carried.main
        );
    };
    assert_eq!(std::fs::read(backup).unwrap(), same_size);
    assert_eq!(diverged_backups(&dst), vec![backup.clone()]);
    assert_eq!(read_main(&dst), SRC_MAIN);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A destination byte-identical to the source is left untouched: `Present`,
/// no backup file, the same bytes and the same modification time.
#[test]
fn a_migration_leaves_a_byte_identical_main_transcript_untouched() {
    let tmp = scratch("identical");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_forked(&src, &dst);
    write_main(&dst, SRC_MAIN);
    let dst_main = crate::terminal::transcript::session_transcript_path(&dst, WD, SID);
    let mtime_before = std::fs::metadata(&dst_main).unwrap().modified().unwrap();

    let carried = carry(&src, &dst);
    assert_eq!(carried.main, MainCarry::Present);
    assert!(diverged_backups(&dst).is_empty(), "no backup file");
    assert_eq!(read_main(&dst), SRC_MAIN);
    assert_eq!(
        std::fs::metadata(&dst_main).unwrap().modified().unwrap(),
        mtime_before,
        "the destination was not rewritten"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// W2 — outside a migration ([`MainTranscript::ExtendPrefix`], a respawn
/// topping up from an older holder) the destination ran the session last (its
/// main transcript is the larger), so the carry keeps its in-place
/// `custom-title.json`. When the older source side rewrites its own copy
/// before the delta sync, the sync must not overwrite the destination's either.
#[test]
fn delta_sync_never_overwrites_a_sidecar_the_carry_kept_as_newer() {
    let tmp = scratch("delta-kept-newer");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    let src_side = seed_session(
        &src,
        WD,
        SID,
        &[("custom-title.json", br#"{"customTitle":"old"}"#)],
    );
    seed_session(
        &dst,
        WD,
        SID,
        &[("custom-title.json", br#"{"customTitle":"new"}"#)],
    );
    write_main(&dst, b"{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n");

    let carried = carry_session(
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
        WD,
        SID,
        MainTranscript::ExtendPrefix,
    )
    .unwrap();
    assert_eq!(carried.main, MainCarry::Present);
    let title = carried
        .files
        .iter()
        .find(|f| f.rel == Path::new("custom-title.json"))
        .unwrap();
    assert!(title.kept_destination_newer, "{title:?}");

    std::fs::write(
        src_side.join("custom-title.json"),
        br#"{"customTitle":"old side, renamed again"}"#,
    )
    .unwrap();
    let done = sync(&carried);
    assert_eq!(
        done,
        DeltaSync {
            kept_destination_newer: 1,
            ..DeltaSync::default()
        }
    );
    assert_eq!(
        std::fs::read(literal_sidecar(&dst).join("custom-title.json")).unwrap(),
        br#"{"customTitle":"new"}"#,
        "the destination's copy survives the sync"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// S3 — under [`MainTranscript::KeepExisting`] a target that already holds
/// the transcript resumes ITS copy, so its in-place sidecars stay with it even
/// when a holder's main transcript is a larger extension of the target's.
#[test]
fn keep_existing_keeps_the_targets_in_place_sidecars() {
    let tmp = scratch("keep-existing-sidecar");
    let (holder, target) = (tmp.join(".claude-h"), tmp.join(".claude-t"));
    seed_session(
        &holder,
        WD,
        SID,
        &[("custom-title.json", br#"{"customTitle":"holder"}"#)],
    );
    seed_session(
        &target,
        WD,
        SID,
        &[("custom-title.json", br#"{"customTitle":"target"}"#)],
    );
    // The holder's main transcript extends the target's (a strict prefix).
    write_main(&holder, b"{\"type\":\"user\"}\n{\"more\":1}\n");

    let carried = carry_session(
        holder.to_str().unwrap(),
        target.to_str().unwrap(),
        WD,
        SID,
        MainTranscript::KeepExisting,
    )
    .unwrap();
    assert_eq!(carried.main, MainCarry::KeptExisting);
    assert_eq!(read_main(&target), b"{\"type\":\"user\"}\n");
    assert_eq!(
        std::fs::read(literal_sidecar(&target).join("custom-title.json")).unwrap(),
        br#"{"customTitle":"target"}"#,
        "a holder's in-place sidecar must not overwrite the target's"
    );
    assert!(carried.files.iter().all(|f| f.kept_destination_newer));
    let _ = std::fs::remove_dir_all(&tmp);
}

/// S1 — with a cwd over 200 encoded chars the session lives in a
/// `<prefix>-<hash>` project dir, and the target's copy lands in a dir of the
/// same name. The delta sync tops up exactly those two dirs — the ones the
/// carry used — rather than a re-derived name.
#[test]
fn delta_sync_tops_up_the_long_cwd_dirs_the_carry_used() {
    let tmp = scratch("delta-long-cwd");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    let working_dir = format!("/home/u/{}", "d".repeat(250));
    let prefix = crate::terminal::transcript::encode_project_path(&working_dir)
        .get(..200)
        .unwrap()
        .to_string();
    let src_project = src.join("projects").join(format!("{prefix}-k3j2h1"));
    let src_sub = src_project.join(SID).join("subagents");
    std::fs::create_dir_all(&src_sub).unwrap();
    std::fs::write(src_project.join(format!("{SID}.jsonl")), b"{}\n").unwrap();
    std::fs::write(src_sub.join("agent-a1.jsonl"), b"a\n").unwrap();

    let carried = copy_transcript(
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
        &working_dir,
        SID,
    )
    .unwrap();
    let dst_project = dst.join("projects").join(format!("{prefix}-k3j2h1"));
    assert_eq!(carried.src_session_dir, src_project.join(SID));
    assert_eq!(carried.dst_session_dir, dst_project.join(SID));

    append(&src_sub.join("agent-a1.jsonl"), b"a2\n");
    std::fs::write(src_sub.join("agent-n9.jsonl"), b"new\n").unwrap();
    let done = sync(&carried);
    assert_eq!(done.copied, 2, "{done:?}");
    let dst_sub = dst_project.join(SID).join("subagents");
    assert_eq!(
        std::fs::read(dst_sub.join("agent-a1.jsonl")).unwrap(),
        b"a\na2\n"
    );
    assert_eq!(
        std::fs::read(dst_sub.join("agent-n9.jsonl")).unwrap(),
        b"new\n"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A destination whose `.jsonl` files are an older PREFIX of the source's (an
/// append-only extension) is replaced: main transcript and subagent alike.
#[test]
fn an_append_only_prefix_destination_jsonl_is_extended() {
    let tmp = scratch("prefix");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(&src, WD, SID, &[("subagents/agent-a1.jsonl", b"a\nb\n")]);
    seed_session(&dst, WD, SID, &[("subagents/agent-a1.jsonl", b"a\n")]);
    write_main(&src, b"{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n");

    let carried = carry(&src, &dst);
    assert_eq!(carried.main, MainCarry::ExtendedPrefix);
    assert_eq!(
        carried.session_dir,
        SessionDirCarry {
            copied: 1,
            ..SessionDirCarry::default()
        }
    );
    assert_eq!(
        read_main(&dst),
        b"{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n"
    );
    assert_eq!(
        std::fs::read(
            literal_sidecar(&dst)
                .join("subagents")
                .join("agent-a1.jsonl")
        )
        .unwrap(),
        b"a\nb\n"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A carry whose overwrite of an EXISTING (smaller) destination failed records
/// that destination's stamp with `wrote: Failed`; the delta sync retries it
/// while the destination still has that stamp, even though the source did
/// not move on.
#[test]
fn delta_sync_retries_a_failed_overwrite_of_an_existing_destination() {
    let tmp = scratch("delta-retry-overwrite");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(&src, WD, SID, &[("subagents/agent-a1.jsonl", b"a\nb\n")]);
    let mut carried = carry(&src, &dst);
    // As if the overwrite had failed: the destination still holds the older,
    // smaller copy it would have replaced, and the record says so.
    let dst_file = literal_sidecar(&dst)
        .join("subagents")
        .join("agent-a1.jsonl");
    std::fs::write(&dst_file, b"a\n").unwrap();
    let entry = carried
        .files
        .iter_mut()
        .find(|f| f.rel == Path::new("subagents").join("agent-a1.jsonl"))
        .unwrap();
    entry.dst = FileStamp::of(&dst_file);
    entry.wrote = CarryWrite::Failed;
    assert!(
        entry.dst.is_some(),
        "the failed overwrite left a destination"
    );

    let done = sync(&carried);
    assert_eq!(done.copied, 1, "{done:?}");
    assert_eq!(std::fs::read(&dst_file).unwrap(), b"a\nb\n");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The rename's one retry re-checks the destination first: when the first
/// rename fails with `PermissionDenied` and the destination changes before
/// the retry, the copy backs out — `Ok(false)`, the destination as the other
/// writer left it, and no `*.partial` behind.
#[test]
fn the_rename_retry_backs_out_when_the_destination_changed_during_the_pause() {
    let tmp = scratch("rename-retry");
    let (from, to) = (tmp.join("src.jsonl"), tmp.join("dst.jsonl"));
    std::fs::write(&from, b"a\nb\nfrom the old pane\n").unwrap();
    std::fs::write(&to, b"a\n").unwrap();
    let expected = FileStamp::of(&to);
    let mut calls = 0;

    let outcome = copy_if_destination_is_with(&from, &to, expected, |a, b| {
        calls += 1;
        if calls == 1 {
            // The resumed process writes while this rename is refused.
            append(b, b"resumed wrote this\n");
            return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        }
        std::fs::rename(a, b)
    });

    assert!(matches!(outcome, Ok(false)), "{outcome:?}");
    assert_eq!(
        calls, 1,
        "the retry must not rename over a changed destination"
    );
    assert_eq!(std::fs::read(&to).unwrap(), b"a\nresumed wrote this\n");
    let partials: Vec<_> = std::fs::read_dir(&tmp)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(PARTIAL_SUFFIX))
        .collect();
    assert!(partials.is_empty(), "left behind: {partials:?}");
    let _ = std::fs::remove_dir_all(&tmp);
}

// ---- the detached driver: waits for the old writer -------------------------

fn scripted(
    looks: Vec<WriterProbe>,
    seen: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> impl FnMut() -> std::future::Ready<WriterProbe> {
    let mut looks = looks.into_iter();
    move || {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::future::ready(looks.next().unwrap_or(WriterProbe::Alive))
    }
}

fn ok_sync(
    ran: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> impl FnOnce() -> std::future::Ready<Result<DeltaSync, String>> {
    move || {
        ran.store(true, std::sync::atomic::Ordering::SeqCst);
        std::future::ready(Ok(DeltaSync {
            copied: 1,
            ..DeltaSync::default()
        }))
    }
}

#[tokio::test(start_paused = true)]
async fn the_sync_runs_only_after_two_consecutive_gone_looks() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    let (seen, ran) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
    );
    let outcome = run_delta_sync_after_exit(
        scripted(
            vec![
                WriterProbe::Alive,
                WriterProbe::Gone,
                WriterProbe::Unreadable,
                WriterProbe::Gone,
                WriterProbe::Gone,
            ],
            seen.clone(),
        ),
        DELTA_SYNC_EXIT_WAIT,
        Duration::from_millis(500),
        ok_sync(ran.clone()),
    )
    .await;
    assert_eq!(
        outcome,
        DeltaSyncOutcome::Synced(DeltaSync {
            copied: 1,
            ..DeltaSync::default()
        })
    );
    assert!(ran.load(Ordering::SeqCst));
    assert_eq!(
        seen.load(Ordering::SeqCst),
        5,
        "an unreadable look breaks the gone streak"
    );
}

#[tokio::test(start_paused = true)]
async fn a_writer_still_live_at_the_deadline_skips_the_sync() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    let (seen, ran) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
    );
    let started = tokio::time::Instant::now();
    let outcome = run_delta_sync_after_exit(
        scripted(Vec::new(), seen.clone()),
        DELTA_SYNC_EXIT_WAIT,
        Duration::from_millis(500),
        ok_sync(ran.clone()),
    )
    .await;
    assert_eq!(outcome, DeltaSyncOutcome::WriterNotGone);
    assert!(
        !ran.load(std::sync::atomic::Ordering::SeqCst),
        "never copies while the writer may be live"
    );
    assert!(started.elapsed() >= DELTA_SYNC_EXIT_WAIT);
    assert!(started.elapsed() < DELTA_SYNC_EXIT_WAIT + Duration::from_secs(1));
    assert!(
        seen.load(Ordering::SeqCst) > 1,
        "it kept looking until the deadline"
    );
}

/// W3(b) — a look that completes AFTER the deadline is discarded, whatever it
/// saw. Each look here takes 300 ms and the pause is 600 ms against a 1 s
/// deadline: the first look (done at 0.3 s) sees the writer gone, the second
/// starts at 0.9 s and only returns at 1.2 s — past the deadline. Accepting it
/// would start the sync after the deadline, outside the window the continue
/// nudge's wait is sized for.
#[tokio::test(start_paused = true)]
async fn a_gone_look_that_completes_after_the_deadline_does_not_count() {
    let started = tokio::time::Instant::now();
    let looks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = looks.clone();
    let gone = wait_for_writer_exit(
        move || {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                WriterProbe::Gone
            }
        },
        Duration::from_secs(1),
        Duration::from_millis(600),
    )
    .await;
    assert!(!gone, "the second gone look landed after the deadline");
    assert_eq!(looks.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(started.elapsed(), Duration::from_millis(1200));
}

/// W3(b) — no look STARTS after the deadline: the pause is cut short so the
/// last look lands on it, and a gone streak completed exactly at the deadline
/// still counts.
#[tokio::test(start_paused = true)]
async fn the_last_look_lands_on_the_deadline_and_still_counts() {
    let started = tokio::time::Instant::now();
    let gone = wait_for_writer_exit(
        move || {
            std::future::ready(if started.elapsed() >= Duration::from_millis(600) {
                WriterProbe::Gone
            } else {
                WriterProbe::Alive
            })
        },
        Duration::from_secs(1),
        Duration::from_millis(600),
    )
    .await;
    assert!(gone);
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(1),
        "looks at 0, 0.6 and 1.0 s — the last one cut short onto the deadline"
    );
}

// ---- review round 5 (S1, S4, S5) ---------------------------------------------

/// S1 — the production rename-aside is retried once on `PermissionDenied` (a
/// transient Windows filter hold): a rename that fails once and then succeeds
/// still preserves the fork and copies the live transcript in its place, and
/// the retry renames over the claimed placeholder.
#[test]
fn a_rename_aside_that_fails_once_with_permission_denied_is_retried() {
    let tmp = scratch("rename-aside-retry");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_forked(&src, &dst);
    let calls = std::cell::Cell::new(0usize);

    let carried = carry_session_with_rename(
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
        WD,
        SID,
        MainTranscript::LiveSourceWins,
        |from, to| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                assert!(
                    std::fs::metadata(to).is_ok_and(|m| m.len() == 0),
                    "the claimed backup is an empty placeholder when the first attempt fails"
                );
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                std::fs::rename(from, to)
            }
        },
    )
    .expect("the retry moves the fork aside");
    let MainCarry::DivergedPreserved { backup } = &carried.main else {
        panic!("expected the fork preserved, got {:?}", carried.main);
    };
    assert_eq!(std::fs::read(backup).unwrap(), FORK_MAIN);
    assert_eq!(read_main(&dst), SRC_MAIN, "the live conversation resumes");
    assert_eq!(
        diverged_backups(&dst),
        vec![backup.clone()],
        "one backup, no stray placeholder"
    );
    // Main: one failure + one retry; the subagent fork: one rename.
    assert_eq!(calls.get(), 3);
    assert_eq!(carried.session_dir.diverged_preserved, 1);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// S5 — a subagent transcript moved aside keeps its fork's `.meta.json` beside
/// it under the SAME suffix, taken before the source's meta replaces the
/// destination's; the destination meta then is the live one.
#[test]
fn a_moved_aside_subagent_keeps_its_meta_json_under_the_same_suffix() {
    let tmp = scratch("sub-meta");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    let live_meta: &[u8] = br#"{"description":"live run"}"#;
    let fork_meta: &[u8] = br#"{"description":"forked run"}"#;
    seed_session(
        &src,
        WD,
        SID,
        &[
            ("subagents/agent-a1.jsonl", b"a\nlive\n"),
            ("subagents/agent-a1.meta.json", live_meta),
        ],
    );
    seed_session(
        &dst,
        WD,
        SID,
        &[
            ("subagents/agent-a1.jsonl", b"a\nfork\n"),
            ("subagents/agent-a1.meta.json", fork_meta),
        ],
    );

    let carried = carry(&src, &dst);
    assert_eq!(carried.session_dir.diverged_preserved, 1);
    let backups = subagent_backups(&dst, "agent-a1.jsonl");
    assert_eq!(backups.len(), 1, "{backups:?}");
    let suffix = backups[0]
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .strip_prefix("agent-a1.jsonl")
        .unwrap()
        .to_string();
    assert!(suffix.starts_with(".diverged-"), "{suffix}");
    let sub = literal_sidecar(&dst).join("subagents");
    assert_eq!(
        std::fs::read(sub.join(format!("agent-a1.meta.json{suffix}"))).unwrap(),
        fork_meta,
        "the fork's meta survives beside its transcript, same suffix"
    );
    assert_eq!(subagent_backups(&dst, "agent-a1.meta.json").len(), 1);
    assert_eq!(
        std::fs::read(sub.join("agent-a1.meta.json")).unwrap(),
        live_meta
    );
    assert_eq!(
        carried.subagents,
        vec![CarriedSubagent {
            agent_id: "a1".to_string(),
            description: Some("live run".to_string()),
        }]
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// S4 — after the delta sync, the resumable list is recomputed from disk: a
/// subagent the sync first copied is named; one new at the source but not
/// (yet) at the destination is not; one the carry left diverged is not.
#[test]
fn resumable_subagents_now_names_what_the_sync_copied_and_nothing_still_missing() {
    let tmp = scratch("resumable-now");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    let src_side = seed_session(&src, WD, SID, &[("subagents/agent-a1.jsonl", b"a\n")]);
    let carried = carry(&src, &dst);
    assert_eq!(agent_ids(&carried), ["a1"]);

    // The old pane spawns n9 after the carry; the sync copies it.
    std::fs::write(src_side.join("subagents").join("agent-n9.jsonl"), b"n\n").unwrap();
    assert_eq!(sync(&carried).copied, 1);
    // And m5 after the sync: never reached the destination.
    std::fs::write(src_side.join("subagents").join("agent-m5.jsonl"), b"m\n").unwrap();

    let now: Vec<String> = resumable_subagents_now(&carried)
        .unwrap()
        .into_iter()
        .map(|a| a.agent_id)
        .collect();
    assert_eq!(now, ["a1", "n9"]);

    // A file the carry recorded as left diverged stays out.
    let mut diverged = carried.clone();
    diverged.files[0].wrote = CarryWrite::Diverged;
    let now: Vec<String> = resumable_subagents_now(&diverged)
        .unwrap()
        .into_iter()
        .map(|a| a.agent_id)
        .collect();
    assert_eq!(now, ["n9"]);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// S4 — a subagent whose carry write failed is named once the sync has
/// replaced it, and not while the destination is still as the failure left it.
#[test]
fn resumable_subagents_now_names_a_failed_carry_only_once_the_sync_wrote_it() {
    let tmp = scratch("resumable-failed");
    let (src, dst) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
    seed_session(&src, WD, SID, &[("subagents/agent-a1.jsonl", b"a\n")]);
    let dst_file = literal_sidecar(&dst)
        .join("subagents")
        .join("agent-a1.jsonl");
    std::fs::create_dir_all(dst_file.parent().unwrap()).unwrap();
    // An older prefix the carry "failed" to extend: still at the destination.
    let mut carried = carry(&src, &dst);
    std::fs::write(&dst_file, b"").unwrap();
    carried.files[0].wrote = CarryWrite::Failed;
    carried.files[0].dst = FileStamp::of(&dst_file);
    assert!(
        resumable_subagents_now(&carried).unwrap().is_empty(),
        "still exactly what the failed write left"
    );
    assert_eq!(sync(&carried).copied, 1);
    let now: Vec<String> = resumable_subagents_now(&carried)
        .unwrap()
        .into_iter()
        .map(|a| a.agent_id)
        .collect();
    assert_eq!(now, ["a1"]);
    let _ = std::fs::remove_dir_all(&tmp);
}
